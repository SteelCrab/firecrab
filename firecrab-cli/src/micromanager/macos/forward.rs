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
use std::path::PathBuf;
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::thread;
use std::time::Duration;

use firecrab_api_types::{PortForward, PortProtocol, VmState};
use serde::Deserialize;

const VMS_URL: &str = "http://127.0.0.1:5523/api/vms";
/// The API tunnel already owns this loopback port.
const API_PORT: u16 = 5523;
const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum Error {
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
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(Error::Client)?;
    let mut relays = Relays::new(PathBuf::from("/usr/bin/ssh"), manager);
    let mut last_error = None;
    loop {
        match wanted_ports(&client) {
            Ok(wanted) => {
                last_error = None;
                relays.sync(&wanted);
            }
            // Existing forwards stay up while the API is briefly unreachable.
            Err(error) => {
                let error = error.to_string();
                if last_error.as_ref() != Some(&error) {
                    eprintln!("port-forward: GET {VMS_URL}: {error}");
                }
                last_error = Some(error);
            }
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn wanted_ports(client: &reqwest::blocking::Client) -> Result<BTreeSet<u16>, reqwest::Error> {
    let vms: Vec<Vm> = client.get(VMS_URL).send()?.error_for_status()?.json()?;
    Ok(loopback_ports(&vms))
}

/// The DNAT rules exist only while a VM runs, and `ssh -L` carries TCP only.
fn loopback_ports(vms: &[Vm]) -> BTreeSet<u16> {
    vms.iter()
        .filter(|vm| vm.state == VmState::Running)
        .flat_map(|vm| &vm.port_forwards)
        .filter(|forward| forward.protocol == PortProtocol::Tcp && forward.host_port != API_PORT)
        .map(|forward| forward.host_port)
        .collect()
}

struct Relays {
    ssh: PathBuf,
    manager: Manager,
    active: HashMap<u16, Child>,
    /// Ports another Mac process holds; remembered so the failure is logged
    /// once rather than on every poll.
    unavailable: BTreeSet<u16>,
}

impl Relays {
    fn new(ssh: PathBuf, manager: Manager) -> Self {
        Self {
            ssh,
            manager,
            active: HashMap::new(),
            unavailable: BTreeSet::new(),
        }
    }

    fn sync(&mut self, wanted: &BTreeSet<u16>) {
        self.active.retain(|port, ssh| {
            if !wanted.contains(port) {
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
        self.unavailable.retain(|port| wanted.contains(port));
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
            .arg("-i")
            .arg(&self.manager.key)
            .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"])
            .args(["-o", "StrictHostKeyChecking=accept-new"])
            .arg("-o")
            .arg(format!(
                "UserKnownHostsFile={}",
                self.manager.known_hosts.display()
            ))
            .args(["-o", "ExitOnForwardFailure=yes", "-o", "LogLevel=ERROR"])
            .args([
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
            ])
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
        assert_eq!(loopback_ports(&vms), BTreeSet::from([2222, 8080]));
    }

    #[test]
    fn each_forward_is_a_loopback_only_ssh_local_forward() {
        let relays = Relays::new(PathBuf::from("/usr/bin/ssh"), manager());
        let command = relays.forward(8080);
        let arguments: Vec<_> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        let joined = arguments.join(" ");
        assert_eq!(command.get_program(), "/usr/bin/ssh");
        assert!(joined.contains("-i /tmp/fc state/manager_ed25519"));
        assert!(joined.contains("UserKnownHostsFile=/tmp/fc state/known_hosts"));
        assert!(joined.contains("ExitOnForwardFailure=yes"));
        assert!(
            joined.ends_with("-T -L 127.0.0.1:8080:127.0.0.1:8080 root@192.0.2.7 cat >/dev/null")
        );
    }

    #[test]
    fn sync_starts_skips_busy_respawns_and_closes_forwards() {
        let directory = tempfile::tempdir().unwrap();
        let ssh = directory.path().join("ssh");
        fs::write(&ssh, "#!/bin/sh\nexec sleep 600\n").unwrap();
        fs::set_permissions(&ssh, Permissions::from_mode(0o755)).unwrap();
        let busy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let busy_port = busy.local_addr().unwrap().port();
        let port = free_port();
        let mut relays = Relays::new(ssh, manager());
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
        let alive = ProcessCommand::new("/bin/kill")
            .args(["-0", &second.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(!alive, "a closed forward's ssh is gone");
    }
}
