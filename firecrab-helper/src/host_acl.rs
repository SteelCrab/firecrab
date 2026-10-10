//! Distro-agnostic host firewall holes so guests can reach dnsmasq.
//!
//! Firecrab's own nft table only hooks forward/postrouting. `NF_ACCEPT`
//! there still walks later hooks, so a UFW / firewalld / `nftables.service`
//! drop on INPUT (DHCP/DNS) or FORWARD (NAT) wins unless we insert into
//! *that* backend. Detect what is enforcing and talk to it.
//!
//! Packet filter (always):
//! - `iptables` / `ip6tables` `-I INPUT` (legacy and iptables-nft)
//! - well-known nft filter tables, when they exist: Debian/Arch
//!   `inet filter`, iptables-nft `ip`/`ip6` `filter`, NixOS `nixos-fw`
//!
//! Frontend (at most one; UFW wins if both are present):
//! - Debian/Ubuntu/Arch UFW, via `/etc/ufw/ufw.conf` `ENABLED=yes`
//!   (locale-independent; `ufw status` is translated)
//! - RHEL/Fedora/Rocky/Alma/openSUSE firewalld: bind the bridge to the
//!   always-present `trusted` zone. Runtime + permanent, no `--reload`.
//!
//! Idempotent. Missing binaries and already-present rules are ignored.

use std::fs;
use std::process::Stdio;

use firecrab_helper_protocol::network::{MICRO_NETWORK_BRIDGE_PREFIX, MicroNetworkSpec};
use tokio::process::Command;

/// Which userspace frontend owns policy, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Frontend {
    Ufw,
    Firewalld,
    None,
}

/// UFW if its conf says enabled, otherwise firewalld if it is running.
fn select_frontend(ufw_enabled: bool, firewalld_running: bool) -> Frontend {
    if ufw_enabled {
        Frontend::Ufw
    } else if firewalld_running {
        Frontend::Firewalld
    } else {
        Frontend::None
    }
}

/// nft tables we are willing to insert into. Never `inet firecrab` (owned)
/// and never `firewalld` / `ufw` tables (those frontends have their own API).
const NFT_TABLES: &[NftTable] = &[
    NftTable {
        family: "inet",
        table: "filter",
    },
    NftTable {
        family: "ip",
        table: "filter",
    },
    NftTable {
        family: "ip6",
        table: "filter",
    },
    NftTable {
        family: "inet",
        table: "nixos-fw",
    },
];

#[derive(Clone, Copy)]
struct NftTable {
    family: &'static str,
    table: &'static str,
}

/// Opens DHCP (67/udp), DNS (53), and forward on every MicroNetwork bridge
/// for the host firewall that is actually enforcing policy.
pub async fn ensure_all(default_uplink: &str, micro_networks: &[MicroNetworkSpec]) {
    let frontend = select_frontend(ufw_is_enabled(), firewalld_is_running().await);
    let nft_listed = output_tool("nft", &["list", "tables"])
        .await
        .unwrap_or_default();
    for network in micro_networks {
        let bridge = network.bridge_name();
        if !is_safe_ifname(&bridge) {
            continue;
        }
        let uplink = network.uplink.as_deref().unwrap_or(default_uplink);
        ensure_iptables_input(&bridge).await;
        insert_nft_filter(&nft_listed, &bridge).await;
        match frontend {
            Frontend::Ufw => apply_ufw_bridge(&bridge, uplink).await,
            Frontend::Firewalld => apply_firewalld_bridge(&bridge).await,
            Frontend::None => {}
        }
    }
}

/// Drops the holes for one bridge that is going away.
pub async fn remove_bridge(bridge: &str) {
    if !is_safe_ifname(bridge) {
        return;
    }
    remove_iptables_input(bridge).await;
    let nft_listed = output_tool("nft", &["list", "tables"])
        .await
        .unwrap_or_default();
    delete_nft_filter(&nft_listed, bridge).await;
    if ufw_is_enabled() {
        let _ = run_tool(
            "ufw",
            &[
                "--force", "delete", "allow", "in", "on", bridge, "to", "any", "port", "67",
                "proto", "udp",
            ],
        )
        .await;
        let _ = run_tool(
            "ufw",
            &[
                "--force", "delete", "allow", "in", "on", bridge, "to", "any", "port", "53",
            ],
        )
        .await;
        // The route rule names the uplink too, so it cannot be deleted from
        // the bridge name alone; find it in the numbered listing instead.
        delete_ufw_rules_on(|interface| interface == bridge).await;
    }
    if firewalld_is_running().await {
        let _ = run_tool(
            "firewall-cmd",
            &["--zone=trusted", "--remove-interface", bridge],
        )
        .await;
        let _ = run_tool(
            "firewall-cmd",
            &[
                "--permanent",
                "--zone=trusted",
                "--remove-interface",
                bridge,
            ],
        )
        .await;
    }
}

/// Drops every UFW rule that still names a Firecrab bridge, including those of
/// bridges that no longer exist (a route rule outlives its network when the
/// network was deleted while UFW could not be written to). Run by `--teardown`.
pub async fn remove_stale_ufw_rules() {
    if ufw_is_enabled() {
        delete_ufw_rules_on(is_firecrab_bridge).await;
    }
}

/// The default bridge, or a MicroNetwork bridge (`mnb` + 12 hex digits).
fn is_firecrab_bridge(name: &str) -> bool {
    name == crate::bridge::BRIDGE_NAME
        || name
            .strip_prefix(MICRO_NETWORK_BRIDGE_PREFIX)
            .is_some_and(|hex| hex.len() == 12 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// Deletes every numbered UFW rule that names an interface `wanted` accepts.
async fn delete_ufw_rules_on(wanted: impl Fn(&str) -> bool) {
    // `ufw status` is translated, so ask for the C locale; the rule text we
    // match is an interface name, which is not translated either way.
    let Some(listing) = output_tool_env("ufw", &["status", "numbered"], &[("LC_ALL", "C")]).await
    else {
        return;
    };
    for number in ufw_rules_on(&listing, wanted) {
        let _ = run_tool("ufw", &["--force", "delete", &number.to_string()]).await;
    }
}

/// Numbers of the rules in a `ufw status numbered` listing that apply `on
/// <interface>` for an interface `wanted` accepts, highest first: deleting a
/// rule renumbers every rule after it, so the lower numbers stay valid.
fn ufw_rules_on(listing: &str, wanted: impl Fn(&str) -> bool) -> Vec<u32> {
    let mut numbers: Vec<u32> = listing
        .lines()
        .filter_map(|line| {
            let (number, rule) = line.trim_start().strip_prefix('[')?.split_once(']')?;
            let number: u32 = number.trim().parse().ok()?;
            let words: Vec<&str> = rule.split_whitespace().collect();
            words
                .windows(2)
                .any(|pair| pair[0] == "on" && wanted(pair[1]))
                .then_some(number)
        })
        .collect();
    numbers.sort_unstable_by(|a, b| b.cmp(a));
    numbers
}

fn ufw_is_enabled() -> bool {
    ufw_conf_is_enabled(&fs::read_to_string("/etc/ufw/ufw.conf").unwrap_or_default())
}

fn ufw_conf_is_enabled(text: &str) -> bool {
    text.lines().any(|line| line.trim() == "ENABLED=yes")
}

fn firewalld_state_is_running(stdout: &str) -> bool {
    stdout.trim() == "running"
}

async fn firewalld_is_running() -> bool {
    let Some(output) = output_tool("firewall-cmd", &["--state"]).await else {
        return false;
    };
    firewalld_state_is_running(&output)
}

async fn apply_ufw_bridge(bridge: &str, uplink: &str) {
    let _ = run_tool(
        "ufw",
        &[
            "allow", "in", "on", bridge, "to", "any", "port", "67", "proto", "udp",
        ],
    )
    .await;
    let _ = run_tool(
        "ufw",
        &["allow", "in", "on", bridge, "to", "any", "port", "53"],
    )
    .await;
    if is_safe_ifname(uplink) {
        let _ = run_tool(
            "ufw",
            &["route", "allow", "in", "on", bridge, "out", "on", uplink],
        )
        .await;
    }
}

/// Bind the bridge to firewalld's `trusted` zone (accepts DHCP/DNS/forward
/// on that iface). `--change-interface` steals it from the default zone
/// without a reload. Fall back to `--add-interface` on older firewalld.
async fn apply_firewalld_bridge(bridge: &str) {
    if !run_tool(
        "firewall-cmd",
        &["--zone=trusted", "--change-interface", bridge],
    )
    .await
    {
        let _ = run_tool(
            "firewall-cmd",
            &["--zone=trusted", "--add-interface", bridge],
        )
        .await;
    }
    if !run_tool(
        "firewall-cmd",
        &[
            "--permanent",
            "--zone=trusted",
            "--change-interface",
            bridge,
        ],
    )
    .await
    {
        let _ = run_tool(
            "firewall-cmd",
            &["--permanent", "--zone=trusted", "--add-interface", bridge],
        )
        .await;
    }
}

fn is_safe_ifname(name: &str) -> bool {
    !name.is_empty()
        && name.len() < 16
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn nft_tables_contain(listed: &str, family: &str, table: &str) -> bool {
    listed.lines().any(|line| {
        let mut parts = line.split_whitespace();
        parts.next() == Some("table") && parts.next() == Some(family) && parts.next() == Some(table)
    })
}

fn nft_rule_present(listed: &str, needle_quoted: &str, needle_bare: &str) -> bool {
    listed.contains(needle_quoted) || listed.contains(needle_bare)
}

fn nft_handles_for(listed_with_handles: &str, needle: &str) -> Vec<String> {
    listed_with_handles
        .lines()
        .filter(|line| line.contains(needle))
        .filter_map(|line| {
            let (_, rest) = line.rsplit_once("# handle ")?;
            let handle = rest.split_whitespace().next()?;
            if !handle.is_empty() && handle.bytes().all(|b| b.is_ascii_digit()) {
                Some(handle.to_owned())
            } else {
                None
            }
        })
        .collect()
}

fn iifname_needles(bridge: &str) -> (String, String) {
    (format!("iifname \"{bridge}\""), format!("iifname {bridge}"))
}

fn oifname_needles(bridge: &str) -> (String, String) {
    (format!("oifname \"{bridge}\""), format!("oifname {bridge}"))
}

async fn insert_nft_filter(tables_listed: &str, bridge: &str) {
    for spec in NFT_TABLES {
        if !nft_tables_contain(tables_listed, spec.family, spec.table) {
            continue;
        }
        insert_nft_input(*spec, bridge).await;
        insert_nft_forward(*spec, bridge).await;
    }
}

async fn insert_nft_input(spec: NftTable, bridge: &str) {
    for chain in input_chain_names() {
        let listed =
            match output_tool("nft", &["list", "chain", spec.family, spec.table, chain]).await {
                Some(text) => text,
                None => continue,
            };
        for (proto, port) in [("udp", "67"), ("udp", "53"), ("tcp", "53")] {
            let quoted = format!("iifname \"{bridge}\" {proto} dport {port}");
            let bare = format!("iifname {bridge} {proto} dport {port}");
            if nft_rule_present(&listed, &quoted, &bare) {
                continue;
            }
            let _ = run_tool(
                "nft",
                &[
                    "insert",
                    "rule",
                    spec.family,
                    spec.table,
                    chain,
                    "iifname",
                    bridge,
                    proto,
                    "dport",
                    port,
                    "accept",
                ],
            )
            .await;
        }
        return;
    }
}

async fn insert_nft_forward(spec: NftTable, bridge: &str) {
    for chain in forward_chain_names() {
        let listed =
            match output_tool("nft", &["list", "chain", spec.family, spec.table, chain]).await {
                Some(text) => text,
                None => continue,
            };
        for (key, needles) in [
            ("iifname", iifname_needles(bridge)),
            ("oifname", oifname_needles(bridge)),
        ] {
            if nft_rule_present(&listed, &needles.0, &needles.1) {
                continue;
            }
            let _ = run_tool(
                "nft",
                &[
                    "insert",
                    "rule",
                    spec.family,
                    spec.table,
                    chain,
                    key,
                    bridge,
                    "accept",
                ],
            )
            .await;
        }
        return;
    }
}

async fn delete_nft_filter(tables_listed: &str, bridge: &str) {
    let (iif_q, iif_b) = iifname_needles(bridge);
    let (oif_q, oif_b) = oifname_needles(bridge);
    for spec in NFT_TABLES {
        if !nft_tables_contain(tables_listed, spec.family, spec.table) {
            continue;
        }
        for chain in input_chain_names()
            .iter()
            .chain(forward_chain_names().iter())
        {
            let Some(listed) = output_tool(
                "nft",
                &["-a", "list", "chain", spec.family, spec.table, chain],
            )
            .await
            else {
                continue;
            };
            let mut handles = nft_handles_for(&listed, &iif_q);
            handles.extend(nft_handles_for(&listed, &iif_b));
            handles.extend(nft_handles_for(&listed, &oif_q));
            handles.extend(nft_handles_for(&listed, &oif_b));
            handles.sort();
            handles.dedup();
            for handle in handles {
                let _ = run_tool(
                    "nft",
                    &[
                        "delete",
                        "rule",
                        spec.family,
                        spec.table,
                        chain,
                        "handle",
                        &handle,
                    ],
                )
                .await;
            }
        }
    }
}

fn input_chain_names() -> &'static [&'static str] {
    // nftables.service uses `input`; iptables-nft uses `INPUT`.
    &["input", "INPUT"]
}

fn forward_chain_names() -> &'static [&'static str] {
    &["forward", "FORWARD"]
}

async fn ensure_iptables_input(bridge: &str) {
    for binary in ["iptables", "ip6tables"] {
        for args in [
            vec!["-i", bridge, "-p", "udp", "--dport", "67", "-j", "ACCEPT"],
            vec!["-i", bridge, "-p", "udp", "--dport", "53", "-j", "ACCEPT"],
            vec!["-i", bridge, "-p", "tcp", "--dport", "53", "-j", "ACCEPT"],
        ] {
            let already = run_tool(binary, &flatten_check("INPUT", &args)).await;
            if !already {
                let _ = run_tool(binary, &flatten_insert("INPUT", &args)).await;
            }
        }
    }
}

async fn remove_iptables_input(bridge: &str) {
    for binary in ["iptables", "ip6tables"] {
        for args in [
            vec!["-i", bridge, "-p", "udp", "--dport", "67", "-j", "ACCEPT"],
            vec!["-i", bridge, "-p", "udp", "--dport", "53", "-j", "ACCEPT"],
            vec!["-i", bridge, "-p", "tcp", "--dport", "53", "-j", "ACCEPT"],
        ] {
            let _ = run_tool(binary, &flatten_delete("INPUT", &args)).await;
        }
    }
}

fn flatten_check<'a>(chain: &'a str, args: &'a [&'a str]) -> Vec<&'a str> {
    let mut out = vec!["-C", chain];
    out.extend_from_slice(args);
    out
}

fn flatten_insert<'a>(chain: &'a str, args: &'a [&'a str]) -> Vec<&'a str> {
    let mut out = vec!["-I", chain];
    out.extend_from_slice(args);
    out
}

fn flatten_delete<'a>(chain: &'a str, args: &'a [&'a str]) -> Vec<&'a str> {
    let mut out = vec!["-D", chain];
    out.extend_from_slice(args);
    out
}

/// PATH first, then the sbin locations used on Debian, RHEL, and SUSE
/// when the helper's unit has a slim `PATH`.
fn tool_search_order(bin: &str) -> Vec<String> {
    vec![
        bin.to_owned(),
        format!("/usr/sbin/{bin}"),
        format!("/usr/bin/{bin}"),
        format!("/sbin/{bin}"),
    ]
}

async fn run_tool(bin: &str, args: &[&str]) -> bool {
    for path in tool_search_order(bin) {
        let status = Command::new(&path)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        if let Ok(status) = status {
            return status.success();
        }
    }
    false
}

async fn output_tool(bin: &str, args: &[&str]) -> Option<String> {
    output_tool_env(bin, args, &[]).await
}

async fn output_tool_env(bin: &str, args: &[&str], envs: &[(&str, &str)]) -> Option<String> {
    for path in tool_search_order(bin) {
        let Ok(output) = Command::new(&path)
            .args(args)
            .envs(envs.iter().copied())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .await
        else {
            continue;
        };
        if output.status.success() {
            return Some(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        // Binary existed; a non-zero exit is a real failure, not a
        // missing-path miss. Do not try /usr/sbin after /usr/bin already ran.
        return None;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const UFW_LISTING: &str = "Status: active\n\n\
         \x20    To                         Action      From\n\
         \x20    --                         ------      ----\n\
        [ 1] 22/tcp                     ALLOW IN    Anywhere\n\
        [ 2] 67/udp on mnb0f8450ea2318  ALLOW IN    Anywhere\n\
        [ 3] 53 on mnb0f8450ea2318      ALLOW IN    Anywhere\n\
        [ 4] Anywhere on eth0           ALLOW FWD   Anywhere on mnb0f8450ea2318\n\
        [ 5] 53 (v6) on mnb0f8450ea2318 ALLOW IN    Anywhere (v6)\n\
        [ 6] Anywhere on eth0           ALLOW FWD   Anywhere on mnb34a9c59a5f1b\n\
        [ 7] 80/tcp                     ALLOW IN    Anywhere\n\
        [10] Anywhere on docker0        ALLOW FWD   Anywhere on eth0\n";

    #[test]
    fn ufw_rules_naming_a_bridge_are_found_highest_number_first() {
        // The allow rules, their IPv6 twin, and the route rule that carries the uplink.
        assert_eq!(
            ufw_rules_on(UFW_LISTING, |name| name == "mnb0f8450ea2318"),
            vec![5, 4, 3, 2]
        );
    }

    #[test]
    fn ufw_rules_of_other_interfaces_and_ports_are_left_alone() {
        let found = ufw_rules_on(UFW_LISTING, is_firecrab_bridge);
        assert_eq!(found, vec![6, 5, 4, 3, 2]);
        // 1 and 7 name no interface; 10 is Docker's.
        assert!(![1, 7, 10].iter().any(|number| found.contains(number)));
        assert!(ufw_rules_on("Status: inactive\n", is_firecrab_bridge).is_empty());
        assert!(ufw_rules_on("", is_firecrab_bridge).is_empty());
    }

    #[test]
    fn only_firecrab_bridge_names_count_as_firecrab_bridges() {
        assert!(is_firecrab_bridge("fcbr0"));
        assert!(is_firecrab_bridge("mnb0f8450ea2318"));
        // Not ours: too short, too long, not hex, another prefix, a TAP.
        for name in [
            "mnb",
            "mnb0f8450ea231",
            "mnb0f8450ea23188",
            "mnbzzzzzzzzzzzz",
            "br-0f8450ea2318",
            "fct0f8450ea2318",
            "eth0",
            "docker0",
        ] {
            assert!(!is_firecrab_bridge(name), "{name}");
        }
    }

    #[test]
    fn ufw_conf_is_enabled_reads_the_enabled_line() {
        assert!(ufw_conf_is_enabled("# comment\nENABLED=yes\n"));
        assert!(ufw_conf_is_enabled("ENABLED=yes\r\n"));
        assert!(!ufw_conf_is_enabled("ENABLED=no\n"));
        assert!(!ufw_conf_is_enabled(""));
        assert!(!ufw_conf_is_enabled("ENABLED=yes please\n"));
    }

    #[test]
    fn firewalld_state_is_running_is_exact() {
        assert!(firewalld_state_is_running("running\n"));
        assert!(!firewalld_state_is_running("not running\n"));
        assert!(!firewalld_state_is_running(""));
    }

    #[test]
    fn select_frontend_prefers_ufw_over_firewalld() {
        assert_eq!(select_frontend(true, true), Frontend::Ufw);
        assert_eq!(select_frontend(false, true), Frontend::Firewalld);
        assert_eq!(select_frontend(false, false), Frontend::None);
    }

    #[test]
    fn nft_tables_contain_matches_whole_tokens() {
        let listed = "table inet firecrab\ntable inet filter\ntable ip filter\n";
        assert!(nft_tables_contain(listed, "inet", "filter"));
        assert!(nft_tables_contain(listed, "ip", "filter"));
        assert!(!nft_tables_contain(listed, "inet", "filterfoo"));
        assert!(!nft_tables_contain(listed, "inet", "firewalld"));
    }

    #[test]
    fn nft_rule_present_accepts_quoted_or_bare_ifname() {
        assert!(nft_rule_present(
            "iifname \"mnbad86a20811d6\" udp dport 67 accept",
            "iifname \"mnbad86a20811d6\" udp dport 67",
            "iifname mnbad86a20811d6 udp dport 67",
        ));
        assert!(nft_rule_present(
            "iifname mnbad86a20811d6 udp dport 67 accept",
            "iifname \"mnbad86a20811d6\" udp dport 67",
            "iifname mnbad86a20811d6 udp dport 67",
        ));
        assert!(!nft_rule_present(
            "iifname \"other\" udp dport 67 accept",
            "iifname \"mnbad86a20811d6\" udp dport 67",
            "iifname mnbad86a20811d6 udp dport 67",
        ));
    }

    #[test]
    fn nft_handles_for_reads_nft_a_comments() {
        let listed = "\t\tiifname \"mnbad86a20811d6\" udp dport 67 accept # handle 12\n\
                      \t\tiifname \"other\" udp dport 67 accept # handle 13\n";
        assert_eq!(
            nft_handles_for(listed, "iifname \"mnbad86a20811d6\""),
            vec!["12".to_owned()]
        );
    }

    #[test]
    fn is_safe_ifname_accepts_mnb_and_rejects_shell() {
        assert!(is_safe_ifname("mnbad86a20811d6"));
        assert!(is_safe_ifname("eth0"));
        assert!(!is_safe_ifname(""));
        assert!(!is_safe_ifname("br;rm -rf /"));
        assert!(!is_safe_ifname("thisnameiswaytoolong"));
    }

    #[test]
    fn tool_search_order_covers_debian_and_rhel_sbin() {
        assert_eq!(
            tool_search_order("nft"),
            vec!["nft", "/usr/sbin/nft", "/usr/bin/nft", "/sbin/nft"]
        );
    }
}
