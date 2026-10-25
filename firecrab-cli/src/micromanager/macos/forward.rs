//! Serves running VMs' TCP port forwards on the Mac's loopback.
//!
//! The guest DNATs each forwarded host port, including connections to its own
//! `127.0.0.1`, so one `ssh -L 127.0.0.1:<port>:127.0.0.1:<port>` per forward
//! makes `127.0.0.1:<port>` on the Mac work as it does on a Linux host. It is
//! SSH rather than a direct connection to the VM's NAT address because macOS
//! local network privacy refuses that address to an unbundled launchd agent,
//! while `/usr/bin/ssh` is exempt. UDP forwards stay reachable at the manager
//! address from interactive processes only.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::thread;
use std::time::Duration;

use firecrab_api_types::{PortForward, PortProtocol, VmState};
use serde::Deserialize;

use crate::micromanager::dev::instance;
use crate::micromanager::runtime::Ssh;

const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not read development instance: {0}")]
    Instance(#[from] std::io::Error),
    #[error(transparent)]
    Manifest(#[from] crate::micromanager::manifest::Error),
    #[error("management SSH key must be inside its runtime directory")]
    CredentialsPath,
    #[error("could not build the port-forward API client: {0}")]
    Client(#[source] reqwest::Error),
}

/// Only the fields the relay reads from `GET /api/vms`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Vm {
    state: VmState,
    #[serde(default)]
    port_forwards: Vec<PortForward>,
}

/// How to reach the management VM over SSH, as the wrapper's API tunnel does.
pub struct Manager {
    pub ip: IpAddr,
    pub key: PathBuf,
    pub known_hosts: PathBuf,
}

pub fn run(manager: Manager) -> Result<i32, Error> {
    let home = manager
        .key
        .parent()
        .and_then(|path| path.parent())
        .ok_or(Error::CredentialsPath)?
        .to_owned();
    let settings = super::ssh::load(&home)?;
    let vms_url = format!(
        "{}/api/vms",
        instance::api_url(&home)?.trim_end_matches('/')
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(Error::Client)?;
    let mut relays = Relays::new(PathBuf::from("/usr/bin/ssh"), manager, settings);
    let mut last_error = None;
    loop {
        let reserved = relays.refresh_reserved(&home)?;
        match wanted_ports(&client, &vms_url, &reserved) {
            Ok(wanted) => {
                last_error = None;
                relays.sync(&wanted);
            }
            // Only forwards outside the current reservations survive an API outage.
            Err(error) => {
                let error = error.to_string();
                if last_error.as_ref() != Some(&error) {
                    eprintln!("port-forward: GET {vms_url}: {error}");
                }
                last_error = Some(error);
            }
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn wanted_ports(
    client: &reqwest::blocking::Client,
    vms_url: &str,
    reserved: &BTreeSet<u16>,
) -> Result<BTreeSet<u16>, reqwest::Error> {
    let vms: Vec<Vm> = client.get(vms_url).send()?.error_for_status()?.json()?;
    Ok(loopback_ports(&vms, reserved))
}

/// The DNAT rules exist only while a VM runs, and `ssh -L` carries TCP only.
fn loopback_ports(vms: &[Vm], reserved: &BTreeSet<u16>) -> BTreeSet<u16> {
    vms.iter()
        .filter(|vm| vm.state == VmState::Running)
        .flat_map(|vm| &vm.port_forwards)
        .filter(|forward| {
            forward.protocol == PortProtocol::Tcp && !reserved.contains(&forward.host_port)
        })
        .map(|forward| forward.host_port)
        .collect()
}

struct Relays {
    settings: Ssh,
    ssh: PathBuf,
    manager: Manager,
    active: HashMap<u16, Child>,
    /// Ports another Mac process holds; remembered so the failure is logged
    /// once rather than on every poll.
    unavailable: BTreeSet<u16>,
}

impl Relays {
    fn new(ssh: PathBuf, manager: Manager, settings: Ssh) -> Self {
        Self {
            settings,
            ssh,
            manager,
            active: HashMap::new(),
            unavailable: BTreeSet::new(),
        }
    }

    /// Read every registered backend before polling the API; unknown ownership closes all relays.
    fn refresh_reserved(&mut self, home: &Path) -> std::io::Result<BTreeSet<u16>> {
        let reserved = match instance::registered_backend_ports(home) {
            Ok(ports) => ports.into_iter().collect::<BTreeSet<_>>(),
            Err(error) => {
                self.sync(&BTreeSet::new());
                return Err(error);
            }
        };
        self.retain(|port| !reserved.contains(&port));
        Ok(reserved)
    }

    /// Close disallowed relays before checking for dead SSH children.
    fn retain(&mut self, keep: impl Fn(u16) -> bool) {
        self.active.retain(|port, ssh| {
            if !keep(*port) {
                let _ = ssh.kill();
                let _ = ssh.wait();
                eprintln!("port-forward: closed 127.0.0.1:{port}");
                return false;
            }
            match ssh.try_wait() {
                Ok(None) => true,
                // Respawned on this same pass, so a dropped SSH session heals.
                Ok(Some(status)) => {
                    eprintln!("port-forward: ssh for 127.0.0.1:{port} exited ({status})");
                    false
                }
                Err(_) => false,
            }
        });
        self.unavailable.retain(|port| keep(*port));
    }

    fn sync(&mut self, wanted: &BTreeSet<u16>) {
        self.retain(|port| wanted.contains(&port));
        for &port in wanted {
            if self.active.contains_key(&port) {
                continue;
            }
            // ssh would only exit on the conflict; probing first names the cause.
            if let Err(error) = TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
                if self.unavailable.insert(port) {
                    eprintln!("port-forward: cannot listen on 127.0.0.1:{port}: {error}");
                }
                continue;
            }
            match self.forward(port).spawn() {
                Ok(ssh) => {
                    self.active.insert(port, ssh);
                    self.unavailable.remove(&port);
                    eprintln!("port-forward: 127.0.0.1:{port} -> {}", self.manager.ip);
                }
                Err(error) => eprintln!("port-forward: could not start ssh for {port}: {error}"),
            }
        }
    }

    fn forward(&self, port: u16) -> ProcessCommand {
        let mut command = ProcessCommand::new(&self.ssh);
        command
            .args(super::ssh::arguments(
                self.manager
                    .key
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new(".")),
                &self.settings,
                super::ssh::Session::Forward,
            ))
            .arg("-i")
            .arg(&self.manager.key)
            .args(["-o", "BatchMode=yes"])
            .args(["-o", "StrictHostKeyChecking=accept-new"])
            .arg("-o")
            .arg(super::known_hosts_option(&self.manager.known_hosts))
            .args(["-o", "ExitOnForwardFailure=yes", "-o", "LogLevel=ERROR"])
            .args(["-T", "-L"])
            .arg(format!("127.0.0.1:{port}:127.0.0.1:{port}"))
            .arg(format!("root@{}", self.manager.ip))
            // The session lives until stdin closes, and the relay holds the
            // other end: however the relay dies, its forwards die with it
            // instead of holding the ports for a successor.
            .arg("cat >/dev/null")
            .stdin(Stdio::piped())
            .stdout(Stdio::null());
        command
    }
}

impl Drop for Relays {
    fn drop(&mut self) {
        for ssh in self.active.values_mut() {
            let _ = ssh.kill();
            let _ = ssh.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, Permissions};
    use std::os::unix::fs::PermissionsExt;

    fn manager() -> Manager {
        Manager {
            ip: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7)),
            key: PathBuf::from("/tmp/fc state/manager_ed25519"),
            known_hosts: PathBuf::from("/tmp/fc state/known_hosts"),
        }
    }

    /// Below the ephemeral range, so a parallel test's `:0` bind or outgoing
    /// connection cannot take it between this probe and the relay's.
    fn free_port() -> u16 {
        let start = 20_000 + (std::process::id() % 10_000) as u16;
        (start..40_000)
            .find(|&port| TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok())
            .expect("a free loopback port below the ephemeral range")
    }

    /// A sibling test's spawn can briefly inherit a probe socket (macOS has no
    /// atomic `SOCK_CLOEXEC`), so give the port a moment to come free.
    fn sync_until_active(relays: &mut Relays, wanted: &BTreeSet<u16>, port: u16) {
        let started = std::time::Instant::now();
        relays.sync(wanted);
        while !relays.active.contains_key(&port) && started.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(50));
            relays.sync(wanted);
        }
    }

    fn fake_relays(directory: &Path) -> Relays {
        let ssh = directory.join("ssh");
        fs::write(&ssh, "#!/bin/sh\nexec sleep 600\n").unwrap();
        fs::set_permissions(&ssh, Permissions::from_mode(0o755)).unwrap();
        Relays::new(ssh, manager(), Ssh::default())
    }

    fn process_alive(pid: u32) -> bool {
        ProcessCommand::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    #[test]
    fn only_running_tcp_forwards_reach_the_loopback() {
        let vms: Vec<Vm> = serde_json::from_str(
            r#"[
              {"state":"running","portForwards":[
                {"hostPort":8080,"guestPort":80,"protocol":"tcp"},
                {"hostPort":5353,"guestPort":53,"protocol":"udp"},
                {"hostPort":5523,"guestPort":80,"protocol":"tcp"},
                {"hostPort":2222,"guestPort":22}
              ]},
              {"state":"stopped","portForwards":[{"hostPort":9090,"guestPort":80,"protocol":"tcp"}]},
              {"state":"running"}
            ]"#,
        )
        .unwrap();
        assert_eq!(
            loopback_ports(&vms, &BTreeSet::from([5523, 5524, 8080])),
            BTreeSet::from([2222])
        );
    }

    #[test]
    fn primary_and_private_relays_reserve_every_registered_backend() {
        let directory = tempfile::tempdir().unwrap();
        let first = instance::test_instance(directory.path(), "aaaaaaaaaaaaaaaa", 60230, 60231);
        let second = instance::test_instance(directory.path(), "bbbbbbbbbbbbbbbb", 61230, 61231);
        let vms: Vec<Vm> = serde_json::from_str(
            r#"[{"state":"running","portForwards":[
                {"hostPort":60230,"guestPort":80},
                {"hostPort":60231,"guestPort":80},
                {"hostPort":61230,"guestPort":80},
                {"hostPort":61231,"guestPort":80},
                {"hostPort":5523,"guestPort":80},
                {"hostPort":5524,"guestPort":80},
                {"hostPort":8080,"guestPort":80},
                {"hostPort":2222,"guestPort":22}
            ]}]"#,
        )
        .unwrap();
        for home in [
            &first.primary_home,
            &first.managed_home,
            &second.managed_home,
        ] {
            let mut relays = fake_relays(directory.path());
            let reserved = relays.refresh_reserved(home).unwrap();
            assert_eq!(loopback_ports(&vms, &reserved), BTreeSet::from([2222]));
        }
    }

    #[test]
    fn new_reservations_close_conflicting_relays_even_when_the_api_is_unreachable() {
        for primary in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let instance =
                instance::test_instance(directory.path(), "aaaaaaaaaaaaaaaa", 60230, 60231);
            let home = if primary {
                &instance.primary_home
            } else {
                &instance.managed_home
            };
            let held = TcpListener::bind((Ipv4Addr::LOCALHOST, free_port())).unwrap();
            let blocked = held.local_addr().unwrap().port();
            let safe = free_port();
            drop(held);
            let mut relays = fake_relays(directory.path());
            let reserved = relays.refresh_reserved(home).unwrap();
            assert!(!reserved.contains(&blocked));
            assert!(!reserved.contains(&safe));
            let wanted = BTreeSet::from([blocked, safe]);
            sync_until_active(&mut relays, &wanted, blocked);
            sync_until_active(&mut relays, &wanted, safe);
            let blocked_pid = relays.active[&blocked].id();
            let safe_pid = relays.active[&safe].id();

            instance::test_instance(directory.path(), "bbbbbbbbbbbbbbbb", blocked, 61231);
            let reserved = relays.refresh_reserved(home).unwrap();
            let api = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let url = format!("http://{}/api/vms", api.local_addr().unwrap());
            drop(api);
            let client = reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(1))
                .build()
                .unwrap();
            assert!(wanted_ports(&client, &url, &reserved).is_err());
            assert!(!relays.active.contains_key(&blocked));
            assert!(!process_alive(blocked_pid));
            assert_eq!(relays.active[&safe].id(), safe_pid);
            assert!(process_alive(safe_pid));
        }
    }

    #[test]
    fn unreadable_reservations_close_all_existing_relays() {
        for primary in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let instance =
                instance::test_instance(directory.path(), "aaaaaaaaaaaaaaaa", 60230, 60231);
            let home = if primary {
                &instance.primary_home
            } else {
                &instance.managed_home
            };
            let mut relays = fake_relays(directory.path());
            relays.refresh_reserved(home).unwrap();
            let port = free_port();
            sync_until_active(&mut relays, &BTreeSet::from([port]), port);
            let pid = relays.active[&port].id();
            fs::write(instance.managed_home.join("dev-instance.json"), "invalid").unwrap();

            assert!(relays.refresh_reserved(home).is_err());
            assert!(relays.active.is_empty());
            assert!(relays.unavailable.is_empty());
            assert!(!process_alive(pid));
        }
    }

    #[test]
    fn port_inventory_uses_the_selected_api_endpoint() {
        use std::io::{Read, Write};

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let url = format!("http://{}/api/vms", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let server = thread::spawn(move || {
            let start = std::time::Instant::now();
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(start.elapsed() < Duration::from_secs(5));
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("temporary API listener failed: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") && request.len() < 8192 {
                let mut chunk = [0; 512];
                let read = stream.read(&mut chunk).unwrap();
                assert_ne!(read, 0, "request ended before its HTTP headers");
                request.extend_from_slice(&chunk[..read]);
            }
            let body = r#"[{"state":"running","portForwards":[{"hostPort":60230,"guestPort":80},{"hostPort":60231,"guestPort":80},{"hostPort":8080,"guestPort":80},{"hostPort":2222,"guestPort":22}]}]"#;
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            String::from_utf8_lossy(&request).into_owned()
        });
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let result = wanted_ports(
            &client,
            &url,
            &BTreeSet::from([5523, 5524, 8080, 60230, 60231]),
        );
        let request = server.join().unwrap();
        assert!(request.starts_with("GET /api/vms HTTP/1.1\r\n"));
        assert_eq!(result.unwrap(), BTreeSet::from([2222]));
    }

    #[test]
    fn each_forward_is_a_loopback_only_ssh_local_forward() {
        let relays = Relays::new(PathBuf::from("/usr/bin/ssh"), manager(), Ssh::default());
        let command = relays.forward(8080);
        let arguments: Vec<_> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        let joined = arguments.join(" ");
        assert_eq!(command.get_program(), "/usr/bin/ssh");
        assert!(joined.contains("-i /tmp/fc state/manager_ed25519"));
        assert!(joined.contains("UserKnownHostsFile=\"/tmp/fc state/known_hosts\""));
        assert!(joined.contains("ExitOnForwardFailure=yes"));
        assert!(
            joined.ends_with("-T -L 127.0.0.1:8080:127.0.0.1:8080 root@192.0.2.7 cat >/dev/null")
        );
    }

    #[test]
    fn sync_starts_skips_busy_respawns_and_closes_forwards() {
        let directory = tempfile::tempdir().unwrap();
        let busy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let busy_port = busy.local_addr().unwrap().port();
        let port = free_port();
        let mut relays = fake_relays(directory.path());
        let wanted = BTreeSet::from([busy_port, port]);

        sync_until_active(&mut relays, &wanted, port);
        assert_eq!(relays.active.keys().copied().collect::<Vec<_>>(), [port]);
        assert_eq!(relays.unavailable, BTreeSet::from([busy_port]));

        // A dropped SSH session is replaced on the next pass.
        let first = relays.active[&port].id();
        let _ = relays.active.get_mut(&port).unwrap().kill();
        let _ = relays.active.get_mut(&port).unwrap().wait();
        sync_until_active(&mut relays, &wanted, port);
        let second = relays.active[&port].id();
        assert_ne!(second, first);

        relays.sync(&BTreeSet::new());
        assert!(relays.active.is_empty());
        assert!(relays.unavailable.is_empty());
        assert!(!process_alive(second), "a closed forward's ssh is gone");
    }
}
