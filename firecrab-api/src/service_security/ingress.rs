//! Best-effort ingress denial counts; raw requests and client addresses are never collected.
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::state::AppState;

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// The ingress process must send a recent authenticated health probe.
pub(super) fn status(state: &AppState) -> &'static str {
    let seen = state.security_ingress_seen.load(Ordering::Relaxed);
    if seen == 0
        || seen > now_seconds().saturating_add(5)
        || now_seconds().saturating_sub(seen) >= 90
    {
        return "unavailable";
    }
    let loss = state.security_ingress_loss.load(Ordering::Relaxed);
    if loss != 0 && now_seconds().saturating_sub(loss) < 7 * 24 * 60 * 60 {
        "degraded"
    } else {
        "limited"
    }
}

#[cfg(target_os = "linux")]
pub(super) fn start(state: AppState) {
    tokio::spawn(async move {
        if let Err(error) = linux::collect(state.clone()).await {
            tracing::warn!(%error, "ingress security collection unavailable");
        }
        state.security_ingress_seen.store(0, Ordering::Relaxed);
    });
}

#[cfg(not(target_os = "linux"))]
pub(super) fn start(_state: AppState) {}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::{BTreeMap, HashMap};
    use std::io;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    use std::path::Path;
    use std::time::Duration;

    use serde::Deserialize;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixDatagram, UnixStream};
    use uuid::Uuid;

    use super::{AppState, Ordering, now_seconds};
    use crate::persistence::security_activity::IngressRecord;
    use firecrab_helper_protocol::pipe::{SECURITY_SOCKET, STATUS_SOCKET};

    const INGRESS_DIRECTORY: &str = "/run/firecrab-ingress";
    const MAX_KEYS: usize = 4096;
    type Key = (Uuid, Uuid, String, i64);

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Event {
        service_id: Uuid,
        deployment_id: Uuid,
        status: u16,
        upstream: String,
        at: f64,
    }

    fn parse_event(bytes: &[u8], now: u64) -> Option<Event> {
        let start = bytes.iter().position(|byte| *byte == b'{')?;
        let event: Event = serde_json::from_slice(&bytes[start..]).ok()?;
        if !matches!(event.status, 401 | 403)
            || event.upstream.len() > 256
            || !event.at.is_finite()
            || event.at < 0.0
        {
            return None;
        }
        let timestamp = event.at as u64;
        if timestamp.saturating_add(300) < now || timestamp > now.saturating_add(60) {
            return None;
        }
        Some(event)
    }

    fn bind(path: &Path) -> io::Result<UnixDatagram> {
        if let Ok(metadata) = std::fs::symlink_metadata(path) {
            if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() } {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "refusing existing event path",
                ));
            }
            std::fs::remove_file(path)?;
        }
        let socket = std::os::unix::net::UnixDatagram::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
        let enabled: libc::c_int = 1;
        // SAFETY: socket is open, and the option points to a valid integer.
        if unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PASSCRED,
                (&enabled as *const libc::c_int).cast(),
                std::mem::size_of_val(&enabled) as libc::socklen_t,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        socket.set_nonblocking(true)?;
        UnixDatagram::from_std(socket)
    }

    /// Read one datagram and the UID supplied by the kernel, not by its contents.
    fn receive(socket: &UnixDatagram, data: &mut [u8]) -> io::Result<(usize, u32)> {
        let mut control = [0usize; 16];
        let mut vector = libc::iovec {
            iov_base: data.as_mut_ptr().cast(),
            iov_len: data.len(),
        };
        // SAFETY: zero initializes a plain C struct, then all used pointers refer to live buffers.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut vector;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = std::mem::size_of_val(&control);
        // SAFETY: the fd is a live datagram socket and message points to writable buffers.
        let length = unsafe { libc::recvmsg(socket.as_raw_fd(), &mut message, libc::MSG_DONTWAIT) };
        if length < 0 {
            return Err(io::Error::last_os_error());
        }
        if message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated ingress event",
            ));
        }
        // SAFETY: CMSG helpers walk the ancillary buffer reported by recvmsg.
        let mut header = unsafe { libc::CMSG_FIRSTHDR(&message) };
        while !header.is_null() {
            // SAFETY: header is a valid control message returned by the helpers.
            let current = unsafe { &*header };
            if current.cmsg_level == libc::SOL_SOCKET
                && current.cmsg_type == libc::SCM_CREDENTIALS
                && current.cmsg_len
                    >= unsafe { libc::CMSG_LEN(std::mem::size_of::<libc::ucred>() as u32) } as usize
            {
                // SAFETY: cmsg_len was checked and read_unaligned avoids alignment assumptions.
                let credentials = unsafe {
                    std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<libc::ucred>())
                };
                return Ok((length as usize, credentials.uid));
            }
            // SAFETY: header belongs to this message and the helpers bound traversal.
            header = unsafe { libc::CMSG_NXTHDR(&message, header) };
        }
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "missing ingress sender credentials",
        ))
    }

    async fn probe(path: &Path) {
        let request = async {
            let mut socket = UnixStream::connect(path).await?;
            socket
                .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
                .await?;
            let mut response = [0u8; 128];
            let _ = socket.read(&mut response).await?;
            Ok::<_, io::Error>(())
        };
        let _ = tokio::time::timeout(Duration::from_secs(2), request).await;
    }

    fn persist(
        state: &AppState,
        pending: BTreeMap<Key, u64>,
    ) -> Result<(), crate::persistence::PersistenceError> {
        let mut releases = HashMap::new();
        let mut records = Vec::new();
        for ((service, deployment, upstream, bucket), count) in pending {
            if let std::collections::hash_map::Entry::Vacant(entry) = releases.entry(deployment) {
                entry.insert(state.store.service_attempt(deployment)?);
            }
            let release = releases
                .get(&deployment)
                .and_then(Option::as_ref)
                .filter(|release| release.service_id == service);
            let Some(release) = release else {
                continue;
            };
            // A retry may contain several upstreams; the last address served the response.
            let address = upstream
                .rsplit(',')
                .next()
                .unwrap_or("")
                .trim()
                .parse::<std::net::SocketAddr>()
                .ok();
            let vm_id = address.and_then(|address| {
                release
                    .instances
                    .iter()
                    .find(|instance| {
                        instance.ipv4.parse::<std::net::IpAddr>().ok() == Some(address.ip())
                    })
                    .map(|instance| instance.vm_id)
            });
            records.push(IngressRecord {
                service_id: service,
                deployment_id: deployment,
                vm_id,
                bucket,
                count,
            });
        }
        state.store.save_security_ingress(&records)
    }

    pub(super) async fn collect(state: AppState) -> io::Result<()> {
        collect_at(
            state,
            Path::new(SECURITY_SOCKET),
            Path::new(INGRESS_DIRECTORY),
            Path::new(STATUS_SOCKET),
        )
        .await
    }

    async fn collect_at(
        state: AppState,
        socket_path: &Path,
        ingress_directory: &Path,
        status_socket: &Path,
    ) -> io::Result<()> {
        let socket = bind(socket_path)?;
        let mut timer = tokio::time::interval(Duration::from_secs(5));
        let mut last_probe = 0;
        let mut pending = BTreeMap::<Key, u64>::new();
        let mut data = [0u8; 2048];
        loop {
            tokio::select! {
                ready = socket.readable() => {
                    ready?;
                    for _ in 0..256 {
                        match socket.try_io(tokio::io::Interest::READABLE, || receive(&socket, &mut data)) {
                            Ok((length, uid)) => {
                                // The systemd-owned runtime directory belongs to ingress's DynamicUser.
                                let expected = std::fs::metadata(ingress_directory).ok().map(|metadata| metadata.uid());
                                if expected != Some(uid) { continue; }
                                let message = &data[..length];
                                if std::str::from_utf8(message).is_ok_and(|line| line.trim_end().ends_with("fc_security: probe")) {
                                    state.security_ingress_seen.store(now_seconds(), Ordering::Relaxed);
                                } else if let Some(event) = parse_event(message, now_seconds()) {
                                    let bucket = (event.at as i64).div_euclid(60) * 60;
                                    let key = (event.service_id,event.deployment_id,event.upstream,bucket);
                                    if pending.contains_key(&key) || pending.len() < MAX_KEYS {
                                        let count = pending.entry(key).or_default();
                                        *count = count.saturating_add(1).min(1000000);
                                    } else {
                                        state.security_ingress_loss.store(now_seconds(), Ordering::Relaxed);
                                    }
                                }
                            }
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                                state.security_ingress_loss.store(now_seconds(), Ordering::Relaxed);
                            }
                            Err(_) => continue,
                        }
                    }
                }
                _ = timer.tick() => {
                    let now = now_seconds();
                    if now.saturating_sub(last_probe) >= 20 {
                        last_probe = now;
                        let path = status_socket.to_owned();
                        tokio::spawn(async move { probe(&path).await });
                    }
                    if !pending.is_empty() {
                        let batch = std::mem::take(&mut pending);
                        let store = state.clone();
                        if !matches!(tokio::task::spawn_blocking(move || persist(&store, batch)).await, Ok(Ok(()))) {
                            state.security_ingress_loss.store(now_seconds(), Ordering::Relaxed);
                            tracing::warn!("ingress security persistence failed");
                        }
                    }
                }
            }
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::templates::TemplateRegistry;
        use firecrab_api_types::{MicroServiceDeployment, MicroServiceResponse, MicroServiceSpec};
        use serde_json::json;

        #[tokio::test]
        async fn kernel_credentials_and_event_fields_are_validated() {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("events.sock");
            let receiver = bind(&path).unwrap();
            let sender = std::os::unix::net::UnixDatagram::unbound().unwrap();
            let id = Uuid::new_v4();
            let release = Uuid::new_v4();
            let event = format!(
                r#"<190>fc_security: {{"serviceId":"{id}","deploymentId":"{release}","status":403,"upstream":"127.0.0.2:8080","at":{}}}"#,
                now_seconds()
            );
            sender.send_to(event.as_bytes(), &path).unwrap();
            receiver.readable().await.unwrap();
            let mut data = [0u8; 2048];
            let (length, uid) = receiver
                .try_io(tokio::io::Interest::READABLE, || {
                    receive(&receiver, &mut data)
                })
                .unwrap();
            assert_eq!(uid, unsafe { libc::geteuid() });
            let parsed = parse_event(&data[..length], now_seconds()).unwrap();
            assert_eq!(parsed.service_id, id);
            assert_eq!(parsed.deployment_id, release);
            assert!(parse_event(event.replace("403", "401").as_bytes(), now_seconds()).is_some());
            assert!(parse_event(event.replace("403", "200").as_bytes(), now_seconds()).is_none());
            assert!(
                parse_event(
                    event
                        .replace("\"status\":403", "\"path\":\"/secret\",\"status\":403")
                        .as_bytes(),
                    now_seconds()
                )
                .is_none()
            );
        }

        #[tokio::test]
        async fn collector_accepts_only_the_ingress_uid_and_retains_counts() {
            let root = tempfile::tempdir().unwrap();
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o711)).unwrap();
            let state = AppState::with_db_file(
                TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap(),
                root.path().join("state.db"),
            )
            .await
            .unwrap();
            let service = Uuid::new_v4();
            let deployment = Uuid::new_v4();
            let vm = Uuid::new_v4();
            let spec: MicroServiceSpec = serde_json::from_value(
                json!({"name":"web","image":"ghcr.io/team/web:v1","port":8080,"ram":128,"cpu":1}),
            )
            .unwrap();
            state
                .store
                .insert_micro_service(&MicroServiceResponse {
                    id: service,
                    spec: spec.clone(),
                })
                .unwrap();
            let record: MicroServiceDeployment = serde_json::from_value(json!({"id":deployment,"serviceId":service,"phase":"running","spec":spec,"request":{"hostPort":0},"instances":[{"vmId":vm,"ipv4":"127.0.0.2","hostPort":0,"state":"running","healthy":true}],"log":[],"error":null})).unwrap();
            state.store.save_service_deployment(&record).unwrap();
            let path = root.path().join("events.sock");
            let event = format!(
                r#"<190>fc_security: {{"serviceId":"{service}","deploymentId":"{deployment}","status":403,"upstream":"127.0.0.2:8080","at":{}}}"#,
                now_seconds()
            );
            let receiver_state = state.clone();
            let receiver_path = path.clone();
            let ingress_dir = root.path().join("ingress-owner");
            std::fs::create_dir(&ingress_dir).unwrap();
            let running_as_root = unsafe { libc::geteuid() } == 0;
            if running_as_root {
                std::os::unix::fs::chown(&ingress_dir, Some(65534), Some(65534)).unwrap();
            }
            let probe_path = root.path().join("absent-status.sock");
            let task = tokio::spawn(async move {
                collect_at(receiver_state, &receiver_path, &ingress_dir, &probe_path).await
            });
            tokio::time::timeout(Duration::from_secs(2), async {
                while !path.exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            let sender = UnixDatagram::unbound().unwrap();
            if running_as_root {
                use std::os::unix::process::CommandExt;
                // A root sender does not match the ingress DynamicUser and is ignored.
                sender.send_to(event.as_bytes(), &path).await.unwrap();
                let target = path.clone();
                let body = event.clone();
                let status = tokio::task::spawn_blocking(move || {
                    let code = "import socket,sys; s=socket.socket(socket.AF_UNIX,socket.SOCK_DGRAM); s.sendto(b'<190>fc_security: probe',sys.argv[1]); [s.sendto(sys.argv[2].encode(),sys.argv[1]) for _ in range(10)]";
                    std::process::Command::new("python3").args(["-c",code]).arg(target).arg(body).uid(65534).gid(65534).status().unwrap()
                }).await.unwrap();
                assert!(status.success());
            } else {
                sender
                    .send_to(b"<190>fc_security: probe", &path)
                    .await
                    .unwrap();
                for _ in 0..10 {
                    sender.send_to(event.as_bytes(), &path).await.unwrap();
                }
            }
            tokio::time::timeout(Duration::from_secs(8), async {
                loop {
                    let rows = state.store.security_activity(service).unwrap();
                    if let Some(found) = rows.first() {
                        assert_eq!(found.count, 10);
                        assert_eq!(found.replica_ids, vec![vm.to_string()]);
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(super::super::status(&state), "limited");
            task.abort();
        }

        #[tokio::test]
        async fn ingress_counts_keep_the_matching_release_and_replica() {
            let root = tempfile::tempdir().unwrap();
            let state = AppState::with_db_file(
                TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap(),
                root.path().join("state.db"),
            )
            .await
            .unwrap();
            let service = Uuid::new_v4();
            let deployment = Uuid::new_v4();
            let vm = Uuid::new_v4();
            let spec: MicroServiceSpec = serde_json::from_value(
                json!({"name":"web","image":"ghcr.io/team/web:v1","port":8080,"ram":128,"cpu":1}),
            )
            .unwrap();
            state
                .store
                .insert_micro_service(&MicroServiceResponse {
                    id: service,
                    spec: spec.clone(),
                })
                .unwrap();
            let record: MicroServiceDeployment = serde_json::from_value(json!({"id":deployment,"serviceId":service,"phase":"running","spec":spec,"request":{"hostPort":0},"instances":[{"vmId":vm,"ipv4":"127.0.0.2","hostPort":0,"state":"running","healthy":true}],"log":[],"error":null})).unwrap();
            state.store.save_service_deployment(&record).unwrap();
            let bucket = (now_seconds() as i64).div_euclid(60) * 60;
            let pending = BTreeMap::from([(
                (
                    service,
                    deployment,
                    "127.0.0.1:9001, 127.0.0.2:8080".to_owned(),
                    bucket,
                ),
                10,
            )]);
            persist(&state, pending).unwrap();
            let findings = state.store.security_activity(service).unwrap();
            assert_eq!(findings.len(), 1);
            assert_eq!(findings[0].source, "ingress HTTP status codes");
            assert_eq!(findings[0].replica_ids, vec![vm.to_string()]);
            assert_eq!(findings[0].deployment_id, deployment.to_string());
            assert!(
                !serde_json::to_string(&findings)
                    .unwrap()
                    .contains("127.0.0.")
            );
        }
    }
}

#[cfg(test)]
mod availability_tests {
    use super::*;
    use crate::templates::TemplateRegistry;

    #[tokio::test]
    async fn missing_stale_and_lost_ingress_are_not_reported_as_available() {
        let root = tempfile::tempdir().unwrap();
        let state = AppState::with_db_file(
            TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap(),
            root.path().join("state.db"),
        )
        .await
        .unwrap();
        assert_eq!(status(&state), "unavailable");
        state
            .security_ingress_seen
            .store(now_seconds(), Ordering::Relaxed);
        assert_eq!(status(&state), "limited");
        state
            .security_ingress_loss
            .store(now_seconds(), Ordering::Relaxed);
        assert_eq!(status(&state), "degraded");
        state
            .security_ingress_seen
            .store(now_seconds().saturating_sub(100), Ordering::Relaxed);
        assert_eq!(status(&state), "unavailable");
    }
}
