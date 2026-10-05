//! TCP listeners for WSL's Windows localhost relay. DNAT alone has no bound
//! socket for WSL's port tracker to discover. Bind only loopback and relay to
//! the explicit VM/guest port, with the same lifetime as its firewall policy.

use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};

use crate::firewall::VmPolicy;

#[derive(Debug)]
pub struct PortRelays {
    enabled: bool,
    active: HashMap<u16, Relay>,
}

#[derive(Debug)]
struct Relay {
    target: watch::Sender<SocketAddr>,
    task: Option<JoinHandle<()>>,
}

impl Drop for Relay {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// New sockets are reserved before nft commits. A failed bind or nft
/// transaction drops these sockets and leaves the existing relays intact.
pub struct Prepared {
    wanted: HashMap<u16, SocketAddr>,
    listeners: HashMap<u16, TcpListener>,
}

impl Default for PortRelays {
    fn default() -> Self {
        let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
        Self::new(is_wsl2(&release))
    }
}

fn is_wsl2(release: &str) -> bool {
    release.to_ascii_lowercase().contains("microsoft-standard")
}

impl PortRelays {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            active: HashMap::new(),
        }
    }

    pub async fn prepare(&self, policies: &[VmPolicy]) -> io::Result<Prepared> {
        let mut wanted = HashMap::new();
        for policy in policies.iter().filter(|_| self.enabled) {
            for forward in &policy.port_forwards {
                if forward.protocol.eq_ignore_ascii_case("tcp") {
                    let target = SocketAddr::from((policy.ipv4, forward.guest_port));
                    if wanted.insert(forward.host_port, target).is_some() {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("duplicate WSL localhost port {}", forward.host_port),
                        ));
                    }
                }
            }
        }
        let mut listeners = HashMap::new();
        for &port in wanted.keys() {
            if !self.active.contains_key(&port) {
                let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
                    .await
                    .map_err(|error| {
                        io::Error::new(error.kind(), format!("WSL localhost port {port}: {error}"))
                    })?;
                listeners.insert(port, listener);
            }
        }
        Ok(Prepared { wanted, listeners })
    }

    pub async fn commit(&mut self, prepared: Prepared) {
        let obsolete: Vec<_> = self
            .active
            .keys()
            .filter(|port| !prepared.wanted.contains_key(port))
            .copied()
            .collect();
        for port in obsolete {
            if let Some(mut relay) = self.active.remove(&port)
                && let Some(task) = relay.task.take()
            {
                // Closing the target channel asks serve to drain its aborted
                // connection tasks, rather than merely scheduling their abort.
                drop(relay);
                // Release the port and every accepted connection before the
                // stop/remove response, including when another VM reuses it.
                let _ = task.await;
            }
        }
        for (port, listener) in prepared.listeners {
            let (target, receiver) = watch::channel(prepared.wanted[&port]);
            let task = tokio::spawn(serve(listener, receiver));
            self.active.insert(
                port,
                Relay {
                    target,
                    task: Some(task),
                },
            );
        }
        for (port, target) in prepared.wanted {
            let relay = &self.active[&port];
            relay.target.send_if_modified(|current| {
                if *current == target {
                    return false;
                }
                *current = target;
                true
            });
        }
    }
}

async fn serve(listener: TcpListener, mut target: watch::Receiver<SocketAddr>) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            changed = target.changed() => {
                // Retargeting never keeps a connection to the previous VM.
                connections.shutdown().await;
                if changed.is_err() { return; }
            }
            accepted = listener.accept() => {
                let Ok((mut incoming, _)) = accepted else { return; };
                if connections.len() >= 64 { continue; }
                let address = *target.borrow();
                connections.spawn(async move {
                    if let Ok(Ok(mut guest)) = tokio::time::timeout(
                        Duration::from_secs(5), TcpStream::connect(address),
                    ).await {
                        let _ = copy_bidirectional(&mut incoming, &mut guest).await;
                    }
                });
            }
            _ = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::firewall::EgressPolicy;
    use firecrab_helper_protocol::network::{MacAddr, PortForwardSpec};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use uuid::Uuid;

    fn policy(host_port: u16, guest_port: u16) -> VmPolicy {
        VmPolicy {
            vm_id: Uuid::new_v4(),
            ipv4: Ipv4Addr::LOCALHOST,
            ipv6: None,
            mac: MacAddr([2, 0, 0, 0, 0, 1]),
            egress: EgressPolicy::Internet,
            allow_host_ssh: false,
            port_forwards: vec![PortForwardSpec {
                host_port,
                guest_port,
                protocol: "tcp".into(),
            }],
        }
    }

    async fn echo_server() -> (u16, JoinHandle<()>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let (mut reader, mut writer) = stream.into_split();
                    let _ = tokio::io::copy(&mut reader, &mut writer).await;
                });
            }
        });
        (port, task)
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    async fn exchange(port: u16) {
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap();
            let sent = vec![b'x'; 128 * 1024];
            stream.write_all(&sent).await.unwrap();
            stream.shutdown().await.unwrap();
            let mut received = Vec::new();
            stream.read_to_end(&mut received).await.unwrap();
            assert_eq!(received, sent);
        })
        .await
        .unwrap();
    }

    #[test]
    fn only_wsl2_uses_listeners() {
        assert!(is_wsl2("6.18.33.2-microsoft-standard-WSL2"));
        assert!(is_wsl2("4.19.128-microsoft-standard"));
        assert!(!is_wsl2("7.2.2-generic"));
        assert!(!is_wsl2("4.4.0-19041-Microsoft"));
    }

    #[tokio::test]
    async fn forwards_half_closed_streams_and_releases_stopped_ports() {
        let (guest, echo) = echo_server().await;
        let port = free_port();
        let mut relays = PortRelays::new(true);
        let prepared = relays.prepare(&[policy(port, guest)]).await.unwrap();
        relays.commit(prepared).await;
        exchange(port).await;
        let prepared = relays.prepare(&[]).await.unwrap();
        relays.commit(prepared).await;
        // Reuse the relay's socket options: a drained connection can still
        // leave TCP TIME_WAIT, which a bare std listener does not reuse.
        let prepared = relays.prepare(&[policy(port, guest)]).await.unwrap();
        relays.commit(prepared).await;
        exchange(port).await;
        let prepared = relays.prepare(&[]).await.unwrap();
        relays.commit(prepared).await;
        echo.abort();
    }

    #[tokio::test]
    async fn failed_bind_preserves_the_existing_relay() {
        let (guest, echo) = echo_server().await;
        let busy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let busy_port = busy.local_addr().unwrap().port();
        let port = free_port();
        let original = policy(port, guest);
        let mut relays = PortRelays::new(true);
        let prepared = relays
            .prepare(std::slice::from_ref(&original))
            .await
            .unwrap();
        relays.commit(prepared).await;
        assert!(
            relays
                .prepare(&[original, policy(busy_port, guest)])
                .await
                .is_err()
        );
        exchange(port).await;
        echo.abort();
    }

    #[tokio::test]
    async fn unchanged_snapshot_keeps_connections_and_retargeting_closes_them() {
        let (guest, echo) = echo_server().await;
        let (next_guest, next_echo) = echo_server().await;
        let port = free_port();
        let mut current = policy(port, guest);
        let mut relays = PortRelays::new(true);
        let prepared = relays
            .prepare(std::slice::from_ref(&current))
            .await
            .unwrap();
        relays.commit(prepared).await;
        let mut connection = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        connection.write_all(b"a").await.unwrap();
        let mut byte = [0];
        connection.read_exact(&mut byte).await.unwrap();
        let prepared = relays
            .prepare(std::slice::from_ref(&current))
            .await
            .unwrap();
        relays.commit(prepared).await;
        connection.write_all(b"b").await.unwrap();
        connection.read_exact(&mut byte).await.unwrap();
        assert_eq!(byte, [b'b']);
        current.port_forwards[0].guest_port = next_guest;
        let prepared = relays.prepare(&[current]).await.unwrap();
        relays.commit(prepared).await;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), connection.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        exchange(port).await;
        echo.abort();
        next_echo.abort();
    }

    #[tokio::test]
    async fn non_wsl_and_udp_do_not_bind_ports() {
        let busy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = busy.local_addr().unwrap().port();
        let mut relays = PortRelays::new(false);
        let prepared = relays.prepare(&[policy(port, 80)]).await.unwrap();
        relays.commit(prepared).await;
        assert!(relays.active.is_empty());
        let mut udp = policy(port, 80);
        udp.port_forwards[0].protocol = "udp".into();
        let relays = PortRelays::new(true);
        let prepared = relays.prepare(&[udp]).await.unwrap();
        assert!(prepared.listeners.is_empty());
    }

    #[tokio::test]
    async fn abandoned_transaction_releases_reserved_ports() {
        let port = free_port();
        let relays = PortRelays::new(true);
        let prepared = relays.prepare(&[policy(port, 80)]).await.unwrap();
        assert!(std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_err());
        drop(prepared);
        assert!(std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok());
    }

    #[tokio::test]
    async fn stop_closes_accepted_connections_before_returning() {
        let (guest, echo) = echo_server().await;
        let port = free_port();
        let mut relays = PortRelays::new(true);
        let prepared = relays.prepare(&[policy(port, guest)]).await.unwrap();
        relays.commit(prepared).await;
        let mut connection = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        connection.write_all(b"a").await.unwrap();
        let mut byte = [0];
        connection.read_exact(&mut byte).await.unwrap();
        let prepared = relays.prepare(&[]).await.unwrap();
        relays.commit(prepared).await;
        assert_eq!(connection.read(&mut byte).await.unwrap(), 0);
        assert!(TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await.is_ok());
        echo.abort();
    }

    #[tokio::test]
    async fn duplicate_ports_do_not_retarget_an_existing_relay() {
        let (guest, echo) = echo_server().await;
        let port = free_port();
        let original = policy(port, guest);
        let mut relays = PortRelays::new(true);
        let prepared = relays
            .prepare(std::slice::from_ref(&original))
            .await
            .unwrap();
        relays.commit(prepared).await;
        let error = relays
            .prepare(&[original, policy(port, 1)])
            .await
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        exchange(port).await;
        echo.abort();
    }
}
