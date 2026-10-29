//! Real DNS on an owned high loopback port; no DHCP ports or host interfaces.
use super::*;
use std::net::SocketAddr;
use tokio::io::AsyncReadExt;
use uuid::Uuid;

fn network(id: u128, gateway: &str, mode: MicroNetworkDnsMode) -> MicroNetworkSpec {
    MicroNetworkSpec {
        micro_network_id: Uuid::from_u128(id),
        gateway: gateway.parse().unwrap(),
        prefix: 24,
        internet_enabled: false,
        uplink: None,
        ipv6: None,
        dns_mode: mode,
    }
}

fn reservation(id: u128, address: &str) -> DhcpLeaseEntry {
    DhcpLeaseEntry {
        vm_id: Uuid::from_u128(id),
        ipv4: address.parse().unwrap(),
        ipv6: None,
        mac: MacAddr([2, 0xfc, 0, 0, 0, id as u8]),
    }
}

async fn seed_pool(
    pool: &DhcpPool,
    revision: u64,
    leases: &[DhcpLeaseEntry],
    networks: &[MicroNetworkSpec],
    child: Child,
) {
    write_atomic_candidate(&pool.hosts_path, &render_hosts_file(leases))
        .await
        .unwrap();
    write_atomic_candidate(
        &base_config_path(&pool.hosts_path),
        &render_base_config(&pool.hosts_path, networks),
    )
    .await
    .unwrap();
    write_service_dns(pool).await.unwrap();
    let mut state = pool.state.lock().await;
    state.child = Some(child);
    state.applied_revision = Some(revision);
    state.applied_leases = leases.to_vec();
    state.served_networks = Some(networks.to_vec());
}

async fn resolver(pool: &DhcpPool, networks: &[MicroNetworkSpec]) -> (Child, SocketAddr) {
    // Retain the production DNS directives and paths, but never configure DHCP,
    // real MicroNetwork interfaces, port 53, upstreams or a host resolver.
    let dns_lines = render_base_config(&pool.hosts_path, networks)
        .lines()
        .filter(|line| {
            line.starts_with("addn-hosts=")
                || line.starts_with("local=")
                || line.starts_with("domain=")
                || matches!(*line, "no-resolv" | "no-poll" | "no-hosts")
        })
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let udp = std::net::UdpSocket::bind(address).unwrap();
    let config = pool.hosts_path.with_extension("dns-fixture.conf");
    write_atomic_candidate(
        &config,
        &format!(
            "port={}\nlisten-address=127.0.0.1\nbind-interfaces\npid-file={}\n{dns_lines}",
            address.port(),
            pool.hosts_path.with_extension("dns-fixture.pid").display()
        ),
    )
    .await
    .unwrap();
    drop(udp);
    drop(listener);
    let child = Command::new("dnsmasq")
        .arg("--keep-in-foreground")
        .arg("--user=root")
        .arg(format!("--conf-file={}", config.display()))
        .kill_on_drop(true)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..30 {
        if query(address, "fixture.invalid", false).await.is_some() {
            return (child, address);
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("isolated DNS-only fixture did not start");
}

async fn query(address: SocketAddr, name: &str, tcp: bool) -> Option<Vec<u8>> {
    use tokio::net::{TcpStream, UdpSocket};
    let mut packet = vec![0x41, 0x41, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in name.split('.') {
        packet.push(label.len() as u8);
        packet.extend_from_slice(label.as_bytes());
    }
    packet.extend_from_slice(&[0, 0, 1, 0, 1]);
    tokio::time::timeout(std::time::Duration::from_millis(200), async {
        if tcp {
            let mut stream = TcpStream::connect(address).await.ok()?;
            stream.write_u16(packet.len() as u16).await.ok()?;
            stream.write_all(&packet).await.ok()?;
            let length = stream.read_u16().await.ok()?;
            let mut response = vec![0; length as usize];
            stream.read_exact(&mut response).await.ok()?;
            Some(response)
        } else {
            let socket = UdpSocket::bind("127.0.0.1:0").await.ok()?;
            socket.connect(address).await.ok()?;
            socket.send(&packet).await.ok()?;
            let mut response = vec![0; 4096];
            let size = socket.recv(&mut response).await.ok()?;
            response.truncate(size);
            Some(response)
        }
    })
    .await
    .ok()
    .flatten()
}

async fn answer(address: SocketAddr, name: &str, expected: Option<Ipv4Addr>, tcp: bool) {
    let mut last = None;
    for _ in 0..30 {
        if let Some(bytes) = query(address, name, tcp).await {
            let valid = bytes.len() >= 12 && bytes[..2] == [0x41, 0x41];
            let matches = if let Some(ip) = expected {
                valid
                    && bytes[3] & 15 == 0
                    && u16::from_be_bytes([bytes[6], bytes[7]]) == 1
                    && bytes.ends_with(&ip.octets())
            } else {
                valid && bytes[3] & 15 == 3 && bytes[6..8] == [0, 0]
            };
            if matches {
                return;
            }
            last = Some(bytes);
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("{name} tcp={tcp} expected={expected:?}, actual DNS={last:?}");
}

async fn all_names(address: SocketAddr, leases: &[DhcpLeaseEntry]) {
    for lease in leases {
        let name = guest_hostname(lease.vm_id);
        for alias in [name.clone(), format!("{name}.internal")] {
            for tcp in [false, true] {
                answer(address, &alias, Some(lease.ipv4), tcp).await;
            }
        }
    }
}

#[tokio::test]
async fn private_vm_names_survive_missing_dynamic_leases_network_addition_and_dns_restart() {
    let dir = tempfile::tempdir().unwrap();
    let actor = DhcpActor::at(
        dir.path().join("dnsmasq-hosts.conf"),
        dir.path().join("manifest.json"),
    );
    let legacy = network(1, "172.30.218.1", MicroNetworkDnsMode::LegacyForward);
    let a = network(2, "172.30.219.1", MicroNetworkDnsMode::PrivateOnly);
    let b = network(3, "172.30.225.1", MicroNetworkDnsMode::PrivateOnly);
    let old = reservation(1, "172.30.218.2");
    let a_leases: Vec<_> = (0..4)
        .map(|index| reservation(10 + index, &format!("172.30.219.{}", index + 2)))
        .collect();
    let b_leases: Vec<_> = (0..4)
        .map(|index| reservation(20 + index, &format!("172.30.225.{}", index + 2)))
        .collect();
    write_service_dns(&actor.private).await.unwrap();
    let (child, address) = resolver(&actor.private, std::slice::from_ref(&a)).await;
    seed_pool(
        &actor.private,
        1,
        &a_leases,
        std::slice::from_ref(&a),
        child,
    )
    .await;
    let standby = Command::new("sleep")
        .arg("60")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let legacy_pid = standby.id();
    seed_pool(
        &actor.legacy,
        1,
        &[old],
        std::slice::from_ref(&legacy),
        standby,
    )
    .await;
    let legacy_hosts = tokio::fs::read(&actor.legacy.hosts_path).await.unwrap();
    let legacy_config = tokio::fs::read(base_config_path(&actor.legacy.hosts_path))
        .await
        .unwrap();
    // No dynamic lease file exists. IPAM still owns all four A reservations.
    assert!(!runtime_path(&actor.private.hosts_path, "leases").exists());
    let mut leases = vec![old];
    leases.extend_from_slice(&a_leases);
    sync_dhcp_leases(&actor, 1, &leases, &[legacy.clone(), a.clone()])
        .await
        .unwrap();
    all_names(address, &a_leases).await;

    // The DNS-only fixture acknowledges the new interface set without touching
    // interfaces. The production sync still publishes the full lease snapshot.
    let private_networks = vec![a.clone(), b.clone()];
    write_atomic_candidate(
        &base_config_path(&actor.private.hosts_path),
        &render_base_config(&actor.private.hosts_path, &private_networks),
    )
    .await
    .unwrap();
    actor.private.state.lock().await.served_networks = Some(private_networks.clone());
    leases.extend_from_slice(&b_leases);
    sync_dhcp_leases(&actor, 2, &leases, &[legacy.clone(), a.clone(), b.clone()])
        .await
        .unwrap();
    all_names(address, &a_leases).await;
    all_names(address, &b_leases).await;
    {
        let mut state = actor.private.state.lock().await;
        state.child.take().unwrap().kill().await.unwrap();
    }
    let (child, address) = resolver(&actor.private, &private_networks).await;
    actor.private.state.lock().await.child = Some(child);
    all_names(address, &a_leases).await;
    all_names(address, &b_leases).await;

    let vm_names = tokio::fs::read(vm_hosts_path(&actor.private.hosts_path))
        .await
        .unwrap();
    let route = serde_json::json!({"routes":[{
        "host":"api.internal","upstreams":["172.30.219.2:8080"],"network":a,
        "owner":{"serviceId":Uuid::from_u128(50),"deploymentId":Uuid::from_u128(51)}
    }],"tunnel":null});
    tokio::fs::write(
        &actor.private.manifest_path,
        serde_json::to_vec(&route).unwrap(),
    )
    .await
    .unwrap();
    refresh_service_dns(&actor).await.unwrap();
    for tcp in [false, true] {
        answer(address, "api.internal", Some(a.gateway), tcp).await;
    }
    assert_eq!(
        tokio::fs::read(vm_hosts_path(&actor.private.hosts_path))
            .await
            .unwrap(),
        vm_names
    );
    all_names(address, &a_leases).await;
    tokio::fs::write(
        &actor.private.manifest_path,
        b"{\"routes\":[],\"tunnel\":null}",
    )
    .await
    .unwrap();
    refresh_service_dns(&actor).await.unwrap();
    for tcp in [false, true] {
        answer(address, "api.internal", None, tcp).await;
    }
    all_names(address, &b_leases).await;

    let mut stale = vec![old];
    stale.extend_from_slice(&a_leases);
    sync_dhcp_leases(&actor, 1, &stale, &[legacy.clone(), a.clone(), b.clone()])
        .await
        .unwrap();
    all_names(address, &b_leases).await;
    let mut retained = vec![old];
    retained.extend_from_slice(&b_leases);
    sync_dhcp_leases(&actor, 3, &retained, &[legacy, a, b])
        .await
        .unwrap();
    for lease in &a_leases {
        let name = guest_hostname(lease.vm_id);
        for alias in [name.clone(), format!("{name}.internal")] {
            for tcp in [false, true] {
                answer(address, &alias, None, tcp).await;
            }
        }
    }
    all_names(address, &b_leases).await;
    let mut state = actor.legacy.state.lock().await;
    assert_eq!(state.child.as_ref().unwrap().id(), legacy_pid);
    assert!(
        state.child.as_mut().unwrap().try_wait().unwrap().is_none(),
        "unrelated private sync must not signal legacy"
    );
    assert_eq!(
        tokio::fs::read(&actor.legacy.hosts_path).await.unwrap(),
        legacy_hosts
    );
    assert_eq!(
        tokio::fs::read(base_config_path(&actor.legacy.hosts_path))
            .await
            .unwrap(),
        legacy_config
    );
    state.child.take().unwrap().kill().await.unwrap();
    actor
        .private
        .state
        .lock()
        .await
        .child
        .take()
        .unwrap()
        .kill()
        .await
        .unwrap();
}

#[tokio::test]
async fn removed_or_legacy_networks_cannot_retain_private_vm_names_from_a_newer_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let actor = DhcpActor::at(
        dir.path().join("dnsmasq-hosts.conf"),
        dir.path().join("manifest.json"),
    );
    let a = network(1, "172.30.219.1", MicroNetworkDnsMode::PrivateOnly);
    let mut b = network(2, "172.30.225.1", MicroNetworkDnsMode::PrivateOnly);
    let first = reservation(10, "172.30.219.2");
    let retained = reservation(20, "172.30.225.2");
    let leases = [first, retained];
    write_vm_dns(&actor.private, &leases, &[a, b.clone()])
        .await
        .unwrap();
    write_vm_dns(&actor.private, &leases, std::slice::from_ref(&b))
        .await
        .unwrap();
    let path = vm_hosts_path(&actor.private.hosts_path);
    let name = guest_hostname(retained.vm_id);
    assert_eq!(
        tokio::fs::read_to_string(&path).await.unwrap(),
        format!("172.30.225.2 {name} {name}.internal\n")
    );
    b.dns_mode = MicroNetworkDnsMode::LegacyForward;
    write_vm_dns(&actor.private, &leases, &[b]).await.unwrap();
    assert_eq!(tokio::fs::read(&path).await.unwrap(), b"");
}

#[test]
fn newer_ipam_reservations_survive_stale_revision_with_an_added_network() {
    let a = network(1, "172.30.219.1", MicroNetworkDnsMode::PrivateOnly);
    let b = network(2, "172.30.225.1", MicroNetworkDnsMode::PrivateOnly);
    let c = network(3, "172.30.226.1", MicroNetworkDnsMode::PrivateOnly);
    let old = reservation(10, "172.30.219.2");
    let newest = reservation(20, "172.30.225.2");
    let state = DhcpState {
        applied_revision: Some(8),
        applied_leases: vec![old, newest],
        served_networks: Some(vec![a.clone(), b.clone()]),
        ..Default::default()
    };
    let (revision, leases) = snapshot_reservations(&state, 7, &[old], &[a.clone(), b.clone(), c]);
    assert_eq!(revision, 8);
    assert_eq!(leases, vec![old, newest]);
    // Keep the latest DHCP checkpoint even when no interface remains. DNS
    // publication separately filters that checkpoint to the served networks.
    let (revision, leases) = snapshot_reservations(&state, 7, &[], &[b]);
    assert_eq!(revision, 8);
    assert_eq!(leases, vec![old, newest]);
    assert!(snapshot_reservations(&state, 9, &[], &[a]).1.is_empty());
    assert_eq!(
        snapshot_reservations(&state, 7, &[], &[]).1,
        vec![old, newest]
    );
}

#[tokio::test]
async fn private_vm_dns_is_readable_idempotent_repairable_and_separate_from_legacy() {
    let dir = tempfile::tempdir().unwrap();
    let actor = DhcpActor::at(
        dir.path().join("dnsmasq-hosts.conf"),
        dir.path().join("manifest.json"),
    );
    let mut lease = reservation(10, "172.30.219.2");
    lease.ipv6 = Some("fd00:414::2".parse().unwrap());
    let name = guest_hostname(lease.vm_id);
    let networks = [network(1, "172.30.219.1", MicroNetworkDnsMode::PrivateOnly)];
    assert!(
        write_vm_dns(&actor.private, &[lease], &networks)
            .await
            .unwrap()
    );
    let path = vm_hosts_path(&actor.private.hosts_path);
    assert_eq!(path.file_name().unwrap(), "dnsmasq-private-vms.hosts");
    assert_eq!(
        tokio::fs::read_to_string(&path).await.unwrap(),
        format!("172.30.219.2 {name} {name}.internal\nfd00:414::2 {name} {name}.internal\n")
    );
    assert_eq!(
        tokio::fs::metadata(&path)
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
    assert!(
        !write_vm_dns(&actor.private, &[lease], &networks)
            .await
            .unwrap()
    );
    assert!(
        !write_vm_dns(&actor.legacy, &[lease], &networks)
            .await
            .unwrap()
    );
    assert!(!vm_hosts_path(&actor.legacy.hosts_path).exists());
    tokio::fs::remove_file(&path).await.unwrap();
    assert!(
        write_vm_dns(&actor.private, &[lease], &networks)
            .await
            .unwrap()
    );
    assert!(write_vm_dns(&actor.private, &[lease], &[]).await.unwrap());
    assert_eq!(tokio::fs::read(&path).await.unwrap(), b"");
    assert!(
        write_vm_dns(&actor.private, &[lease], &networks)
            .await
            .unwrap()
    );
    assert!(write_vm_dns(&actor.private, &[], &networks).await.unwrap());
    assert_eq!(tokio::fs::read(&path).await.unwrap(), b"");
    let legacy = render_base_config(
        &actor.legacy.hosts_path,
        &[network(
            1,
            "172.30.218.1",
            MicroNetworkDnsMode::LegacyForward,
        )],
    );
    assert!(!legacy.contains("vms.hosts"));
}
