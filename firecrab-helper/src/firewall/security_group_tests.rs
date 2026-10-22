use super::*;
use firecrab_api_types::{PortProtocol, SecurityGroupDirection};
use firecrab_helper_protocol::security_group::{SecurityGroupPolicySpec, SecurityGroupRuleSpec};

fn fixture() -> (Vec<MicroNetworkSpec>, Vec<VmPolicy>) {
    let networks: Vec<_> = ["172.30.0.1", "172.31.0.1"]
        .into_iter()
        .enumerate()
        .map(|(i, ip)| MicroNetworkSpec {
            micro_network_id: Uuid::from_u128(100 + i as u128),
            gateway: ip.parse().unwrap(),
            prefix: 24,
            internet_enabled: true,
            uplink: None,
            ipv6: None,
        })
        .collect();
    let mut policies: Vec<_> = ["172.30.0.2", "172.30.0.3", "172.30.0.4", "172.31.0.2"]
        .into_iter()
        .enumerate()
        .map(|(i, ip)| VmPolicy {
            vm_id: Uuid::from_u128(i as u128 + 1),
            ipv4: ip.parse().unwrap(),
            ipv6: None,
            mac: MacAddr([2, 0, 0, 0, 0, i as u8 + 2]),
            egress: EgressPolicy::Internet,
            security_group: None,
            allow_host_ssh: false,
            port_forwards: vec![],
        })
        .collect();
    let allow = |direction, protocol, port_from, port_to, peer: &str| SecurityGroupRuleSpec {
        direction,
        protocol,
        port_from,
        port_to,
        peer_address: peer.parse().unwrap(),
        peer_prefix: 32,
    };
    let rules = vec![
        allow(
            SecurityGroupDirection::Egress,
            PortProtocol::Tcp,
            5432,
            5433,
            "172.30.0.3",
        ),
        allow(
            SecurityGroupDirection::Egress,
            PortProtocol::Udp,
            5353,
            5353,
            "172.30.0.3",
        ),
        allow(
            SecurityGroupDirection::Egress,
            PortProtocol::Tcp,
            443,
            443,
            "192.0.2.2",
        ),
        allow(
            SecurityGroupDirection::Egress,
            PortProtocol::Tcp,
            443,
            443,
            "172.31.0.2",
        ),
    ];
    policies[0].security_group = Some(SecurityGroupPolicySpec {
        id: Uuid::from_u128(200),
        micro_network_id: networks[0].micro_network_id,
        rules,
    });
    policies[1].security_group = Some(SecurityGroupPolicySpec {
        id: Uuid::from_u128(201),
        micro_network_id: networks[0].micro_network_id,
        rules: vec![
            allow(
                SecurityGroupDirection::Ingress,
                PortProtocol::Tcp,
                5432,
                5433,
                "172.30.0.2",
            ),
            allow(
                SecurityGroupDirection::Ingress,
                PortProtocol::Udp,
                5353,
                5353,
                "172.30.0.2",
            ),
        ],
    });
    policies[1]
        .port_forwards
        .push(firecrab_helper_protocol::network::PortForwardSpec {
            host_port: 18080,
            guest_port: 5432,
            protocol: "tcp".into(),
        });
    (networks, policies)
}

#[test]
fn policy_validation_rejects_wrong_network_ipv6_and_noncanonical_peers() {
    let (mut networks, mut policies) = fixture();
    for policy in &policies {
        crate::security_groups::validate(policy, &networks).unwrap();
    }
    let group = policies[0].security_group.as_mut().unwrap();
    group.rules[0].peer_prefix = 24;
    assert!(crate::security_groups::validate(&policies[0], &networks).is_err());
    policies[0].security_group.as_mut().unwrap().rules[0].peer_prefix = 32;
    policies[0].ipv4 = "172.31.0.2".parse().unwrap();
    assert!(crate::security_groups::validate(&policies[0], &networks).is_err());
    policies[0].ipv4 = "172.30.0.2".parse().unwrap();
    networks[0].ipv6 = Some(firecrab_helper_protocol::network::MicroNetworkIpv6Spec {
        gateway: "fd00::1".parse().unwrap(),
        prefix: 64,
        address_mode: firecrab_helper_protocol::network::Ipv6AddressMode::Slaac,
    });
    assert!(crate::security_groups::validate(&policies[0], &networks).is_err());
}

#[test]
fn recycled_leases_changed_rules_and_helper_restarts_revoke_tracked_permissions() {
    let (_, policies) = fixture();
    let previous = policies
        .iter()
        .map(|vm| (vm.vm_id, ("sgwan".into(), vm.clone())))
        .collect();
    assert!(changed_connection_addresses(&previous, &policies).is_empty());
    assert_eq!(
        changed_connection_addresses(&Default::default(), &policies),
        policies.iter().map(|vm| vm.ipv4).collect()
    );
    let mut changed = policies.clone();
    changed[0].ipv4 = "172.30.0.10".parse().unwrap();
    assert_eq!(
        changed_connection_addresses(&previous, &changed),
        [policies[0].ipv4, changed[0].ipv4].into_iter().collect()
    );
    changed = policies.clone();
    changed[0].security_group.as_mut().unwrap().rules.clear();
    assert_eq!(
        changed_connection_addresses(&previous, &changed),
        [policies[0].ipv4].into_iter().collect()
    );
    changed = policies.clone();
    changed[2].vm_id = Uuid::new_v4();
    assert_eq!(
        changed_connection_addresses(&previous, &changed),
        [policies[2].ipv4].into_iter().collect()
    );
    let legacy: Vec<_> = policies
        .into_iter()
        .map(|mut vm| {
            vm.security_group = None;
            vm
        })
        .collect();
    assert!(changed_connection_addresses(&Default::default(), &legacy).is_empty());
}

/// Export production-rendered rules only; this test never changes networking.
/// scripts/ci-qa-security-groups.py consumes them in an isolated network namespace.
#[test]
#[ignore = "exports fixtures for the isolated TAP packet QA script"]
fn export_security_group_packet_fixtures() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("FIRECRAB_SG_QA_FIXTURES").expect("set FIRECRAB_SG_QA_FIXTURES"),
    );
    std::fs::create_dir_all(&directory).unwrap();
    let (networks, policies) = fixture();
    std::fs::write(directory.join("topology.json"),serde_json::to_vec_pretty(&serde_json::json!({
        "bridges":networks.iter().map(|network|serde_json::json!({"name":network.bridge_name(),"gateway":network.gateway})).collect::<Vec<_>>(),
        "vms":policies.iter().enumerate().map(|(index,vm)|serde_json::json!({"tap":tap_name(vm.vm_id),"ip":vm.ipv4,"mac":vm.mac.to_string(),"bridge":networks[usize::from(index==3)].bridge_name()})).collect::<Vec<_>>()
    })).unwrap()).unwrap();
    for mode in ["normal", "external", "isolated", "offline", "empty"] {
        let mut networks = networks.clone();
        let mut policies = policies.clone();
        match mode {
            "external" => {
                policies[1]
                    .security_group
                    .as_mut()
                    .unwrap()
                    .rules
                    .push(SecurityGroupRuleSpec {
                        direction: SecurityGroupDirection::Ingress,
                        protocol: PortProtocol::Tcp,
                        port_from: 5432,
                        port_to: 5432,
                        peer_address: "192.0.2.2".parse().unwrap(),
                        peer_prefix: 32,
                    })
            }
            "isolated" => policies[0].egress = EgressPolicy::Isolated,
            "offline" => networks[0].internet_enabled = false,
            "empty" => policies[0].security_group.as_mut().unwrap().rules.clear(),
            _ => {}
        }
        let base = render_apply_ruleset("sgwan", &networks).unwrap();
        let mut output = render_reconciled_ruleset(&base, "sgwan", &networks, &policies);
        output.push_str(&crate::security_groups::render(&policies, &networks, ""));
        std::fs::write(directory.join(format!("{mode}.nft")), output).unwrap();
    }
}
