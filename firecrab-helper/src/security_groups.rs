//! Host-enforced policies for bridged, routed, and host-local VM traffic.

use crate::firewall::{FirewallError, VmPolicy};
use firecrab_api_types::SecurityGroupDirection;
use firecrab_helper_protocol::network::{MicroNetworkSpec, tap_name};
use std::net::Ipv4Addr;
use tokio::process::Command;

pub const TABLE: &str = "firecrab_sg";

pub fn validate(policy: &VmPolicy, networks: &[MicroNetworkSpec]) -> Result<(), FirewallError> {
    let Some(group) = &policy.security_group else {
        return Ok(());
    };
    group.validate().map_err(FirewallError::InvalidPolicy)?;
    let network = networks
        .iter()
        .find(|network| network.micro_network_id == group.micro_network_id)
        .ok_or_else(|| FirewallError::InvalidPolicy("security group network is missing".into()))?;
    let mask = u32::MAX
        .checked_shl(u32::from(32 - network.prefix))
        .unwrap_or(0);
    if policy.ipv6.is_some()
        || network.ipv6.is_some()
        || u32::from(policy.ipv4) & mask != u32::from(network.gateway) & mask
    {
        return Err(FirewallError::InvalidPolicy(
            "security group requires its own IPv4-only MicroNetwork".into(),
        ));
    }
    Ok(())
}

/// Render both families into the caller's single nft transaction.
pub fn render(policies: &[VmPolicy], networks: &[MicroNetworkSpec], listing: &str) -> String {
    let mut grouped: Vec<_> = policies
        .iter()
        .filter(|policy| policy.security_group.is_some())
        .collect();
    grouped.sort_by_key(|policy| policy.vm_id);
    let mut output = String::new();
    for family in ["inet", "bridge"] {
        let exists = listing
            .lines()
            .any(|line| line.trim() == format!("table {family} {TABLE}"));
        if grouped.is_empty() {
            if exists {
                output.push_str(&format!("delete table {family} {TABLE}\n"));
            }
            continue;
        }
        output.push_str(&format!("add table {family} {TABLE}\ndelete table {family} {TABLE}\nadd table {family} {TABLE}\n"));
        let priority = if family == "inet" { -10 } else { 0 };
        output.push_str(&format!("add chain {family} {TABLE} forward {{ type filter hook forward priority {priority}; policy accept; }}\n"));
        if family == "inet" {
            output.push_str(&format!("add chain inet {TABLE} input {{ type filter hook input priority -10; policy accept; }}\n"));
        }
        for policy in &grouped {
            let group = policy.security_group.as_ref().unwrap();
            let tag = policy.vm_id.simple();
            let tap = tap_name(policy.vm_id);
            let network = networks
                .iter()
                .find(|network| network.micro_network_id == group.micro_network_id)
                .unwrap();
            let bridge = network.bridge_name();
            for (direction, suffix) in [
                (SecurityGroupDirection::Egress, "eg"),
                (SecurityGroupDirection::Ingress, "in"),
            ] {
                let chain = format!("vm_{tag}_{suffix}");
                output.push_str(&format!("add chain {family} {TABLE} {chain}\n"));
                let rule = format!("add rule {family} {TABLE} {chain}");
                if family == "bridge" {
                    output.push_str(&format!("{rule} ether type arp return\n"));
                }
                output.push_str(&format!(
                    "{rule} ct state invalid drop\n{rule} ct state established,related return\n"
                ));
                let peer = if direction == SecurityGroupDirection::Ingress {
                    "saddr"
                } else {
                    "daddr"
                };
                for allow in group
                    .rules
                    .iter()
                    .filter(|rule| rule.direction == direction)
                {
                    let ports = if allow.port_from == allow.port_to {
                        allow.port_from.to_string()
                    } else {
                        format!("{}-{}", allow.port_from, allow.port_to)
                    };
                    output.push_str(&format!(
                        "{rule} ip {peer} {}/{} {} dport {ports} return\n",
                        allow.peer_address, allow.peer_prefix, allow.protocol
                    ));
                }
                output.push_str(&format!("{rule} drop\n"));
                let selector = if family == "bridge" {
                    let interface = if direction == SecurityGroupDirection::Ingress {
                        "oifname"
                    } else {
                        "iifname"
                    };
                    format!("{interface} \"{tap}\"")
                } else if direction == SecurityGroupDirection::Ingress {
                    format!("oifname \"{bridge}\" ip daddr {}", policy.ipv4)
                } else {
                    format!("iifname \"{bridge}\" ip saddr {}", policy.ipv4)
                };
                // Each allowed side returns so the other side is still checked.
                output.push_str(&format!(
                    "add rule {family} {TABLE} forward {selector} jump {chain}\n"
                ));
            }
            if family == "inet" {
                let chain = format!("host_{tag}");
                let rule = format!("add rule inet {TABLE} {chain}");
                output.push_str(&format!("add chain inet {TABLE} {chain}\n{rule} ct state invalid drop\n{rule} ct state established,related return\n"));
                output.push_str(&format!(
                    "{rule} ip daddr {{ {}, 255.255.255.255 }} udp sport 68 udp dport 67 return\n",
                    network.gateway
                ));
                for protocol in ["tcp", "udp"] {
                    output.push_str(&format!(
                        "{rule} ip daddr {} {protocol} dport 53 return\n",
                        network.gateway
                    ));
                }
                // Internal HTTP is host-local. Require source egress permission
                // before Nginx checks the destination service's ingress rules.
                let internal_http = group.rules.iter().any(|allow| {
                    let mask = u32::MAX
                        .checked_shl(u32::from(32 - allow.peer_prefix))
                        .unwrap_or(0);
                    allow.direction == SecurityGroupDirection::Egress
                        && allow.protocol == firecrab_api_types::PortProtocol::Tcp
                        && (allow.port_from..=allow.port_to).contains(&80)
                        && (u32::from(network.gateway) & mask) == u32::from(allow.peer_address)
                });
                if internal_http {
                    output.push_str(&format!(
                        "{rule} ip daddr {} tcp dport 80 return\n",
                        network.gateway
                    ));
                }
                output.push_str(&format!("{rule} drop\nadd rule inet {TABLE} input iifname \"{bridge}\" ip saddr {} jump {chain}\n", policy.ipv4));
            }
        }
    }
    output
}

/// A recycled lease must not inherit another VM's tracked permissions.
pub async fn forget_connections(ipv4: Ipv4Addr) -> Result<(), FirewallError> {
    for selector in ["--orig-src", "--orig-dst", "--reply-src", "--reply-dst"] {
        let mut command = Command::new("conntrack");
        command
            .args(["--delete", "--family", "ipv4", selector, &ipv4.to_string()])
            .env("LC_ALL", "C")
            .kill_on_drop(true);
        let output = tokio::time::timeout(std::time::Duration::from_secs(10), command.output())
            .await
            .map_err(|_| FirewallError::Conntrack("conntrack deletion timed out".into()))?
            .map_err(|error| FirewallError::Conntrack(error.to_string()))?;
        let detail = String::from_utf8_lossy(&output.stderr);
        if !output.status.success()
            && !(output.status.code() == Some(1) && detail.contains("0 flow entries"))
        {
            return Err(FirewallError::Conntrack(detail.into_owned()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    #[ignore = "requires the isolated TAP namespace created by ci-qa-security-groups.py"]
    async fn clear_packet_qa_connections() {
        let expected =
            std::env::var_os("FIRECRAB_SG_QA_NETNS").expect("packet QA namespace marker");
        assert_eq!(
            std::fs::read_link("/proc/self/ns/net").unwrap().as_os_str(),
            expected
        );
        assert!(std::path::Path::new("/sys/class/net/sgwan/tun_flags").exists());
        for link in std::fs::read_dir("/sys/class/net").unwrap() {
            let name = link.unwrap().file_name().to_string_lossy().into_owned();
            assert!(
                name == "lo"
                    || name == "sgwan"
                    || name.starts_with("mnb")
                    || name.starts_with("fct"),
                "refusing non-QA interface {name}"
            );
        }
        super::forget_connections("172.30.0.2".parse().unwrap())
            .await
            .unwrap();
    }
}
