use std::net::Ipv4Addr;
use std::process::Stdio;

use firecrab_helper_protocol::network::MacAddr;
use futures_util::TryStreamExt;
use rtnetlink::packet_route::link::LinkAttribute;
use rtnetlink::packet_route::route::RouteAttribute;
use rtnetlink::packet_route::{AddressFamily, route::RouteMessage};
use rtnetlink::{Handle, new_connection};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::bridge::BRIDGE_NAME;

const TABLE_INET: &str = "firecrab";
const TABLE_BRIDGE: &str = "firecrab_l2";
const BRIDGE_SUBNET: &str = "172.30.0.0/24";
/// TAP interface names are bounded by IFNAMSIZ (16 incl. NUL). `fct` + 12 hex
/// of sha256(vm_id) = 15 chars. The prefix is distinct from `fcbr0` so an
/// east-west wildcard (`fct*`) never matches the bridge itself. The
/// tap-automation helper derives the same name from the same vm_id, so policy
/// rules and the real device agree.
const TAP_PREFIX: &str = "fct";

/// The egress posture the helper resolves an API-supplied policy ID into.
/// The API selects the ID; the helper is the trust boundary and owns the
/// mapping from ID to concrete rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressPolicy {
    /// Outbound to non-reserved destinations is permitted.
    Internet,
    /// No outbound egress; only gateway-local services reach the VM.
    Isolated,
}

impl EgressPolicy {
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "internet" => Some(EgressPolicy::Internet),
            "isolated" => Some(EgressPolicy::Isolated),
            _ => None,
        }
    }
}

/// Everything the helper needs to render one VM's isolation + egress rules.
/// The IPv4/MAC come from the VM's active lease; the helper never trusts a
/// source address that does not match them.
#[derive(Debug, Clone, Copy)]
pub struct VmPolicy {
    pub vm_id: Uuid,
    pub ipv4: Ipv4Addr,
    pub mac: MacAddr,
    pub egress: EgressPolicy,
    /// Open forwarded inbound TCP 22 to this VM. Note: host-*originated*
    /// traffic traverses the output hook, which this initial scope does not
    /// filter, so the admin's direct host->VM SSH already works; this flag
    /// governs SSH forwarded in from other networks (default-deny otherwise).
    pub allow_host_ssh: bool,
}

#[derive(Debug, Error)]
pub enum FirewallError {
    #[error("failed to open rtnetlink connection")]
    Connection(#[source] std::io::Error),
    #[error("rtnetlink operation failed")]
    Netlink(#[source] rtnetlink::Error),
    #[error("host has no IPv4 default route to detect an uplink interface")]
    NoUplink,
    #[error("uplink interface name {0:?} is not valid for an nftables rule")]
    InvalidUplinkName(String),
    #[error("failed to spawn nft")]
    Spawn(#[source] std::io::Error),
    #[error("failed to write ruleset to nft stdin")]
    WriteStdin(#[source] std::io::Error),
    #[error("nft rejected the ruleset: {stderr}")]
    NftFailed { stderr: String },
}

/// Single-writer actor: every `nft` write goes through one mutex, so
/// concurrent callers cannot race two transactions or act on a stale
/// "already applied" decision (lost update). The state it guards lets a
/// no-op apply short-circuit and lets `remove_vm_policy` recover the leased
/// IP it needs to delete this VM's IP-keyed map elements.
#[derive(Debug)]
pub struct FirewallActor {
    state: Mutex<FirewallState>,
}

#[derive(Debug, Default)]
struct FirewallState {
    applied_uplink: Option<String>,
    /// vm_id -> leased IPv4 of every VM whose policy is currently installed.
    applied_vms: std::collections::HashMap<Uuid, Ipv4Addr>,
}

impl FirewallActor {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(FirewallState::default()),
        }
    }
}

impl Default for FirewallActor {
    fn default() -> Self {
        Self::new()
    }
}

/// Detect the uplink, and (re)apply the Firecrab tables only if the uplink
/// differs from what was last applied. Never touches any table/chain this
/// helper does not own.
pub async fn ensure_firewall(actor: &FirewallActor) -> Result<(), FirewallError> {
    let (connection, handle, _) = new_connection().map_err(FirewallError::Connection)?;
    tokio::spawn(connection);
    let uplink = detect_uplink(&handle).await?;

    let mut state = actor.state.lock().await;
    if state.applied_uplink.as_deref() == Some(uplink.as_str()) {
        return Ok(());
    }

    let ruleset = render_apply_ruleset(&uplink)?;
    run_nft(&ruleset).await?;
    // A global (re)apply flushes the tables, so no per-VM policy survives it.
    state.applied_vms.clear();
    state.applied_uplink = Some(uplink);
    Ok(())
}

/// Install (or atomically replace) one VM's isolation + egress policy.
/// Independent of every other VM: only this VM's named chains and map
/// elements are touched.
pub async fn apply_vm_policy(actor: &FirewallActor, policy: VmPolicy) -> Result<(), FirewallError> {
    let ruleset = render_vm_policy(&policy);
    let mut state = actor.state.lock().await;
    run_nft(&ruleset).await?;
    state.applied_vms.insert(policy.vm_id, policy.ipv4);
    Ok(())
}

/// Remove one VM's policy. Idempotent: a VM with no installed policy is a
/// no-op. VM stop/delete calls this; it never touches the shared tables.
pub async fn remove_vm_policy(actor: &FirewallActor, vm_id: Uuid) -> Result<(), FirewallError> {
    let mut state = actor.state.lock().await;
    let Some(ipv4) = state.applied_vms.get(&vm_id).copied() else {
        return Ok(());
    };
    run_nft(&render_vm_policy_removal(vm_id, ipv4)).await?;
    state.applied_vms.remove(&vm_id);
    Ok(())
}

/// Explicit uninstall: remove both Firecrab tables. VM stop/delete must
/// never call this — only an explicit teardown of the whole subsystem does.
pub async fn remove_firewall(actor: &FirewallActor) -> Result<(), FirewallError> {
    let mut state = actor.state.lock().await;
    run_nft(&render_remove_ruleset()).await?;
    state.applied_vms.clear();
    state.applied_uplink = None;
    Ok(())
}

fn validate_uplink(name: &str) -> Result<(), FirewallError> {
    let is_valid = !name.is_empty()
        && name.len() < 16 // IFNAMSIZ
        && name.chars().all(|c| c.is_ascii_graphic() && c != '"' && c != '\\' && c != ';');
    if is_valid {
        Ok(())
    } else {
        Err(FirewallError::InvalidUplinkName(name.to_owned()))
    }
}

/// Renders the whole VM-independent desired state for both owned tables as
/// one nft(8) script. `add table` + `flush table` before redeclaring keeps
/// this idempotent without ever touching a table this helper doesn't own.
///
/// Per-VM rules live in separate named chains + verdict-map elements (see
/// [`render_vm_policy`]) so replacing one VM's policy never disturbs another.
/// This global flush therefore only runs on an uplink change, when no per-VM
/// state is expected to be present yet.
fn render_apply_ruleset(uplink: &str) -> Result<String, FirewallError> {
    validate_uplink(uplink)?;
    Ok(format!(
        // L3: NAT + egress/ingress dispatch keyed by the VM's leased IP. The
        // L2 table below guarantees the source IP is genuine, so keying L3
        // policy on `ip saddr` is safe even though the routed packet's
        // iifname is the bridge, not the individual TAP.
        "add table inet {TABLE_INET}\n\
         flush table inet {TABLE_INET}\n\
         table inet {TABLE_INET} {{\n\
         \tmap vm_egress {{\n\
         \t\ttype ipv4_addr : verdict\n\
         \t}}\n\
         \tmap vm_ingress {{\n\
         \t\ttype ipv4_addr : verdict\n\
         \t}}\n\
         \tchain forward_dispatch {{\n\
         \t\ttype filter hook forward priority filter; policy accept;\n\
         \t\tiifname \"{BRIDGE_NAME}\" jump firecrab_egress\n\
         \t\toifname \"{BRIDGE_NAME}\" jump firecrab_ingress\n\
         \t}}\n\
         \tchain firecrab_egress {{\n\
         \t\tct state established,related accept\n\
         \t\tip daddr {{ 127.0.0.0/8, 169.254.0.0/16 }} drop\n\
         \t\tip saddr vmap @vm_egress\n\
         \t\tdrop\n\
         \t}}\n\
         \tchain firecrab_ingress {{\n\
         \t\tct state established,related accept\n\
         \t\tip daddr vmap @vm_ingress\n\
         \t\tdrop\n\
         \t}}\n\
         \tchain postrouting_dispatch {{\n\
         \t\ttype nat hook postrouting priority srcnat; policy accept;\n\
         \t\tip saddr {BRIDGE_SUBNET} oifname \"{uplink}\" jump firecrab_postrouting\n\
         \t}}\n\
         \tchain firecrab_postrouting {{\n\
         \t\tmasquerade\n\
         \t}}\n\
         }}\n\
         add table bridge {TABLE_BRIDGE}\n\
         flush table bridge {TABLE_BRIDGE}\n\
         table bridge {TABLE_BRIDGE} {{\n\
         \tmap l2_ingress {{\n\
         \t\ttype ifname : verdict\n\
         \t}}\n\
         \tchain prerouting {{\n\
         \t\ttype filter hook prerouting priority -300; policy accept;\n\
         \t\tiifname vmap @l2_ingress\n\
         \t}}\n\
         \tchain forward {{\n\
         \t\ttype filter hook forward priority -200; policy accept;\n\
         \t\tiifname \"{TAP_PREFIX}*\" oifname \"{TAP_PREFIX}*\" drop\n\
         \t}}\n\
         }}\n"
    ))
}

/// The deterministic TAP name for a VM. Both this module and the
/// tap-automation helper derive it from the same vm_id.
pub fn tap_name(vm_id: Uuid) -> String {
    let digest = Sha256::digest(vm_id.as_bytes());
    let mut name = String::from(TAP_PREFIX);
    for byte in &digest[..6] {
        name.push_str(&format!("{byte:02x}"));
    }
    name
}

/// Renders one VM's isolation rules: L2 anti-spoofing tied to the lease, plus
/// L3 egress/ingress verdicts. Every per-VM object is named after the vm_id
/// and (re)built with add+flush, so re-applying or replacing this VM's policy
/// is atomic and cannot disturb any other VM's chains or map elements.
fn render_vm_policy(policy: &VmPolicy) -> String {
    let tap = tap_name(policy.vm_id);
    let tag = policy.vm_id.simple();
    let ip = policy.ipv4;
    let mac = policy.mac;
    let l2 = format!("add rule bridge {TABLE_BRIDGE} vm_{tag}_l2");
    let eg = format!("add rule inet {TABLE_INET} vm_{tag}_eg");
    let in_ = format!("add rule inet {TABLE_INET} vm_{tag}_in");

    // Internet: a bare accept (reserved-dest drops live upstream in the
    // shared firecrab_egress chain). Isolated: no rule, so control returns
    // to firecrab_egress and its trailing drop denies; gateway-local DHCP/DNS
    // still works because that is the host input hook, not our forward chain.
    let egress_rule = match policy.egress {
        EgressPolicy::Internet => format!("{eg} accept\n"),
        EgressPolicy::Isolated => String::new(),
    };
    let ingress_rule = if policy.allow_host_ssh {
        format!("{in_} tcp dport 22 ct state new,established accept\n")
    } else {
        String::new()
    };

    format!(
        "add chain bridge {TABLE_BRIDGE} vm_{tag}_l2\n\
         flush chain bridge {TABLE_BRIDGE} vm_{tag}_l2\n\
         {l2} ether saddr {mac} ip saddr 0.0.0.0 udp sport 68 udp dport 67 accept\n\
         {l2} ether type arp arp operation request arp saddr ip 0.0.0.0 arp saddr ether {mac} accept\n\
         {l2} ether type != {{ ip, arp }} drop\n\
         {l2} ether saddr != {mac} drop\n\
         {l2} ether type arp arp saddr ether != {mac} drop\n\
         {l2} ether type arp arp saddr ip != {ip} drop\n\
         {l2} ether type ip ip saddr != {ip} drop\n\
         {l2} accept\n\
         add element bridge {TABLE_BRIDGE} l2_ingress {{ \"{tap}\" : jump vm_{tag}_l2 }}\n\
         add chain inet {TABLE_INET} vm_{tag}_eg\n\
         flush chain inet {TABLE_INET} vm_{tag}_eg\n\
         {egress_rule}\
         add element inet {TABLE_INET} vm_egress {{ {ip} : jump vm_{tag}_eg }}\n\
         add chain inet {TABLE_INET} vm_{tag}_in\n\
         flush chain inet {TABLE_INET} vm_{tag}_in\n\
         {ingress_rule}\
         add element inet {TABLE_INET} vm_ingress {{ {ip} : jump vm_{tag}_in }}\n"
    )
}

/// Removes every object [`render_vm_policy`] created for `vm_id`, and nothing
/// else. Each map element is deleted before the chain it jumps to, so nft
/// never rejects a still-referenced chain.
fn render_vm_policy_removal(vm_id: Uuid, ipv4: Ipv4Addr) -> String {
    let tap = tap_name(vm_id);
    let tag = vm_id.simple();
    format!(
        "delete element bridge {TABLE_BRIDGE} l2_ingress {{ \"{tap}\" }}\n\
         delete chain bridge {TABLE_BRIDGE} vm_{tag}_l2\n\
         delete element inet {TABLE_INET} vm_egress {{ {ipv4} }}\n\
         delete chain inet {TABLE_INET} vm_{tag}_eg\n\
         delete element inet {TABLE_INET} vm_ingress {{ {ipv4} }}\n\
         delete chain inet {TABLE_INET} vm_{tag}_in\n"
    )
}

/// `add table` before `delete table` makes removal idempotent even if the
/// table was never installed, without depending on nft's newer `destroy`.
fn render_remove_ruleset() -> String {
    format!(
        "add table inet {TABLE_INET}\n\
         delete table inet {TABLE_INET}\n\
         add table bridge {TABLE_BRIDGE}\n\
         delete table bridge {TABLE_BRIDGE}\n"
    )
}

async fn detect_uplink(handle: &Handle) -> Result<String, FirewallError> {
    let mut routes = handle.route().get(RouteMessage::default()).execute();
    let mut oif_index = None;
    while let Some(route) = routes.try_next().await.map_err(FirewallError::Netlink)? {
        if route.header.address_family == AddressFamily::Inet
            && route.header.destination_prefix_length == 0
        {
            oif_index = route.attributes.iter().find_map(|attribute| match attribute {
                RouteAttribute::Oif(index) => Some(*index),
                _ => None,
            });
            if oif_index.is_some() {
                break;
            }
        }
    }
    let index = oif_index.ok_or(FirewallError::NoUplink)?;

    let mut links = handle.link().get().match_index(index).execute();
    let link = links
        .try_next()
        .await
        .map_err(FirewallError::Netlink)?
        .ok_or(FirewallError::NoUplink)?;
    link.attributes
        .iter()
        .find_map(|attribute| match attribute {
            LinkAttribute::IfName(name) => Some(name.clone()),
            _ => None,
        })
        .ok_or(FirewallError::NoUplink)
}

/// Applies `ruleset` as a single atomic transaction: `nft -f -` accepts the
/// whole script as one netlink batch, so a mid-script failure leaves the
/// previous ruleset untouched instead of partially applying.
async fn run_nft(ruleset: &str) -> Result<(), FirewallError> {
    let mut child = Command::new("nft")
        .arg("-f")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(FirewallError::Spawn)?;

    let mut stdin = child.stdin.take().expect("stdin was piped");
    stdin
        .write_all(ruleset.as_bytes())
        .await
        .map_err(FirewallError::WriteStdin)?;
    drop(stdin);

    let output = child.wait_with_output().await.map_err(FirewallError::Spawn)?;
    if !output.status.success() {
        return Err(FirewallError::NftFailed {
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruleset_only_declares_the_two_owned_tables() {
        let ruleset = render_apply_ruleset("eth0").unwrap();
        assert!(ruleset.contains("table inet firecrab"));
        assert!(ruleset.contains("table bridge firecrab_l2"));
        // Never a blanket flush of the whole host ruleset.
        assert!(!ruleset.contains("flush ruleset"));
    }

    #[test]
    fn global_ruleset_dispatches_bridge_traffic_from_accept_policy_base_chains() {
        let ruleset = render_apply_ruleset("eth0").unwrap();
        assert!(ruleset.contains("policy accept"));
        assert!(ruleset.contains("iifname \"fcbr0\" jump firecrab_egress"));
        assert!(ruleset.contains("oifname \"fcbr0\" jump firecrab_ingress"));
        assert!(ruleset.contains("ip saddr 172.30.0.0/24 oifname \"eth0\" jump firecrab_postrouting"));
        assert!(ruleset.contains("masquerade"));
    }

    #[test]
    fn global_ruleset_default_denies_egress_and_ingress_and_reserved_dests() {
        let ruleset = render_apply_ruleset("eth0").unwrap();
        // firecrab_egress: reserved destinations dropped, then per-VM map,
        // then a trailing drop (default deny for anything not accepted).
        assert!(ruleset.contains("ip daddr { 127.0.0.0/8, 169.254.0.0/16 } drop"));
        assert!(ruleset.contains("ip saddr vmap @vm_egress"));
        assert!(ruleset.contains("ip daddr vmap @vm_ingress"));
        // Both dispatch chains must end in drop.
        assert_eq!(ruleset.matches("\t\tdrop\n").count(), 2);
    }

    #[test]
    fn global_ruleset_denies_east_west_between_firecrab_taps() {
        let ruleset = render_apply_ruleset("eth0").unwrap();
        assert!(ruleset.contains("iifname \"fct*\" oifname \"fct*\" drop"));
        // The wildcard must not be able to match the bridge itself.
        assert!(!"fcbr0".starts_with("fct"));
    }

    #[test]
    fn global_ruleset_is_idempotent_via_add_then_flush_and_owns_only_two_tables() {
        let ruleset = render_apply_ruleset("eth0").unwrap();
        assert!(ruleset.contains("add table inet firecrab\nflush table inet firecrab"));
        assert!(ruleset.contains("add table bridge firecrab_l2\nflush table bridge firecrab_l2"));
        assert!(!ruleset.contains("flush ruleset"));
    }

    #[test]
    fn malformed_uplink_names_are_rejected_before_touching_nft() {
        for bad in ["", "eth0\"; flush ruleset #", "way-too-long-interface-name"] {
            assert!(matches!(
                render_apply_ruleset(bad),
                Err(FirewallError::InvalidUplinkName(_))
            ));
        }
    }

    #[test]
    fn remove_ruleset_deletes_only_the_owned_tables_idempotently() {
        let ruleset = render_remove_ruleset();
        assert!(ruleset.contains("add table inet firecrab\ndelete table inet firecrab"));
        assert!(ruleset.contains("add table bridge firecrab_l2\ndelete table bridge firecrab_l2"));
    }

    fn sample_policy(egress: EgressPolicy, allow_host_ssh: bool) -> VmPolicy {
        VmPolicy {
            vm_id: Uuid::from_u128(0x1234),
            ipv4: Ipv4Addr::new(172, 30, 0, 42),
            mac: "02:fc:aa:bb:cc:dd".parse().unwrap(),
            egress,
            allow_host_ssh,
        }
    }

    #[test]
    fn tap_name_is_deterministic_and_within_ifnamsiz() {
        let vm = Uuid::from_u128(0x1234);
        assert_eq!(tap_name(vm), tap_name(vm));
        assert!(tap_name(vm).len() <= 15, "{}", tap_name(vm));
        assert!(tap_name(vm).starts_with("fct"));
        assert_ne!(tap_name(vm), tap_name(Uuid::from_u128(0x1235)));
    }

    #[test]
    fn vm_policy_pins_l2_source_to_the_lease_and_blocks_ipv6_vlan() {
        let policy = sample_policy(EgressPolicy::Internet, false);
        let ruleset = render_vm_policy(&policy);
        let mac = "02:fc:aa:bb:cc:dd";
        // Spoofed source MAC / ARP sender / IPv4 source are all dropped.
        assert!(ruleset.contains(&format!("ether saddr != {mac} drop")));
        assert!(ruleset.contains(&format!("ether type arp arp saddr ether != {mac} drop")));
        assert!(ruleset.contains("ether type arp arp saddr ip != 172.30.0.42 drop"));
        assert!(ruleset.contains("ether type ip ip saddr != 172.30.0.42 drop"));
        // Non-IPv4/ARP ethertypes (IPv6, VLAN) are dropped.
        assert!(ruleset.contains("ether type != { ip, arp } drop"));
    }

    #[test]
    fn vm_policy_allows_only_the_two_dhcp_exceptions() {
        let ruleset = render_vm_policy(&sample_policy(EgressPolicy::Internet, false));
        // DHCP discover/request from an unconfigured client (src 0.0.0.0).
        assert!(ruleset.contains(
            "ether saddr 02:fc:aa:bb:cc:dd ip saddr 0.0.0.0 udp sport 68 udp dport 67 accept"
        ));
        // ARP address-conflict probe (sender ip 0.0.0.0), sender mac still checked.
        assert!(ruleset.contains(
            "ether type arp arp operation request arp saddr ip 0.0.0.0 arp saddr ether 02:fc:aa:bb:cc:dd accept"
        ));
    }

    #[test]
    fn internet_egress_accepts_but_isolated_falls_through_to_default_drop() {
        let internet = render_vm_policy(&sample_policy(EgressPolicy::Internet, false));
        let tag = Uuid::from_u128(0x1234).simple();
        assert!(internet.contains(&format!("add rule inet firecrab vm_{tag}_eg accept")));

        let isolated = render_vm_policy(&sample_policy(EgressPolicy::Isolated, false));
        // Isolated leaves the egress chain empty (no accept rule for it).
        assert!(!isolated.contains(&format!("add rule inet firecrab vm_{tag}_eg accept")));
        // But the chain and its dispatch element still exist.
        assert!(isolated.contains(&format!("add chain inet firecrab vm_{tag}_eg")));
        assert!(isolated.contains("add element inet firecrab vm_egress { 172.30.0.42 : jump"));
    }

    #[test]
    fn host_ssh_is_allowed_only_when_requested() {
        let tag = Uuid::from_u128(0x1234).simple();
        let with_ssh = render_vm_policy(&sample_policy(EgressPolicy::Internet, true));
        assert!(with_ssh.contains(&format!(
            "add rule inet firecrab vm_{tag}_in tcp dport 22 ct state new,established accept"
        )));

        let without = render_vm_policy(&sample_policy(EgressPolicy::Internet, false));
        assert!(!without.contains("tcp dport 22"));
    }

    #[test]
    fn vm_policy_objects_are_namespaced_so_replacing_one_vm_cannot_touch_another() {
        let a = render_vm_policy(&sample_policy(EgressPolicy::Internet, false));
        let tag_a = Uuid::from_u128(0x1234).simple();
        let tag_b = Uuid::from_u128(0x9999).simple();
        // A's rendered ruleset only ever names A's objects.
        assert!(a.contains(&format!("vm_{tag_a}_l2")));
        assert!(!a.contains(&format!("vm_{tag_b}")));
        // Per-VM apply uses add+flush on named chains, never a table flush.
        assert!(!a.contains("flush table"));
        assert!(a.contains(&format!("flush chain bridge firecrab_l2 vm_{tag_a}_l2")));
    }

    #[test]
    fn removal_deletes_map_elements_before_their_chains() {
        let vm = Uuid::from_u128(0x1234);
        let ruleset = render_vm_policy_removal(vm, Ipv4Addr::new(172, 30, 0, 42));
        let tag = vm.simple();
        let l2_elem = ruleset.find("delete element bridge firecrab_l2 l2_ingress").unwrap();
        let l2_chain = ruleset.find(&format!("delete chain bridge firecrab_l2 vm_{tag}_l2")).unwrap();
        assert!(l2_elem < l2_chain, "map element must be deleted before its chain");

        let eg_elem = ruleset.find("delete element inet firecrab vm_egress").unwrap();
        let eg_chain = ruleset.find(&format!("delete chain inet firecrab vm_{tag}_eg")).unwrap();
        assert!(eg_elem < eg_chain, "egress element must be deleted before its chain");
    }

    #[test]
    fn unknown_egress_policy_id_is_rejected() {
        assert_eq!(EgressPolicy::from_id("internet"), Some(EgressPolicy::Internet));
        assert_eq!(EgressPolicy::from_id("isolated"), Some(EgressPolicy::Isolated));
        assert_eq!(EgressPolicy::from_id("0.0.0.0/0"), None);
        assert_eq!(EgressPolicy::from_id("wide-open"), None);
    }

    #[tokio::test]
    async fn detect_uplink_resolves_the_hosts_default_route_interface() {
        let (connection, handle, _) = new_connection().unwrap();
        tokio::spawn(connection);

        // Unprivileged read; requires this host to have an IPv4 default
        // route (true in the dev/CI sandbox this was written against).
        let uplink = detect_uplink(&handle).await.unwrap();
        assert!(!uplink.is_empty());
    }

    #[tokio::test]
    async fn ensure_firewall_skips_nft_entirely_when_the_uplink_is_unchanged() {
        let (connection, handle, _) = new_connection().unwrap();
        tokio::spawn(connection);
        let real_uplink = detect_uplink(&handle).await.unwrap();

        // Pre-seed the actor as if this uplink was already applied. No `nft`
        // binary needs to exist or succeed for this call to return Ok, since
        // it must short-circuit before ever calling run_nft/spawning nft.
        let actor = FirewallActor::new();
        actor.state.lock().await.applied_uplink = Some(real_uplink.clone());

        assert!(ensure_firewall(&actor).await.is_ok());
        assert_eq!(
            actor.state.lock().await.applied_uplink.as_deref(),
            Some(real_uplink.as_str())
        );
    }

    #[tokio::test]
    async fn remove_vm_policy_is_a_no_op_when_nothing_was_applied() {
        // No applied_vms entry -> returns Ok without ever invoking nft.
        let actor = FirewallActor::new();
        assert!(remove_vm_policy(&actor, Uuid::from_u128(0x1234)).await.is_ok());
    }
}
