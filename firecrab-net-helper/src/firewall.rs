//! Per-VM and global nftables rules: NAT/egress dispatch (`inet firecrab`)
//! and L2 anti-spoofing (`bridge firecrab_l2`), both idempotently rendered
//! and applied as single atomic `nft -f -` transactions.

use std::net::Ipv4Addr;
use std::process::Stdio;

use firecrab_helper_protocol::network::{MacAddr, MicroNetworkSpec, TAP_PREFIX, tap_name};
use rtnetlink::new_connection;
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::nat;

/// Name of the owned `inet` table (NAT/egress dispatch).
const TABLE_INET: &str = "firecrab";
/// Name of the owned `bridge` table (L2 anti-spoofing).
const TABLE_BRIDGE: &str = "firecrab_l2";

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
    /// Resolves an API-supplied policy ID, or `None` if it's not on the
    /// allowlist (the helper never accepts a raw CIDR from the API).
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmPolicy {
    /// The VM this policy applies to.
    pub vm_id: Uuid,
    /// The VM's leased IPv4 address.
    pub ipv4: Ipv4Addr,
    /// The VM's Firecracker guest MAC.
    pub mac: MacAddr,
    /// Outbound (egress) posture for this VM.
    pub egress: EgressPolicy,
    /// Open forwarded inbound TCP 22 to this VM. Note: host-*originated*
    /// traffic traverses the output hook, which this initial scope does not
    /// filter, so the admin's direct host->VM SSH already works; this flag
    /// governs SSH forwarded in from other networks (default-deny otherwise).
    pub allow_host_ssh: bool,
    /// Inbound port forwarding rules (DNAT).
    pub port_forwards: Vec<firecrab_helper_protocol::network::PortForwardSpec>,
}

/// Failure modes shared by every firewall operation.
#[derive(Debug, Error)]
pub enum FirewallError {
    /// Couldn't open the rtnetlink socket.
    #[error("failed to open rtnetlink connection")]
    Connection(#[source] std::io::Error),
    /// An rtnetlink request failed.
    #[error("rtnetlink operation failed")]
    Netlink(#[source] rtnetlink::Error),
    /// No IPv4 default route exists, so the uplink can't be detected.
    #[error("host has no IPv4 default route to detect an uplink interface")]
    NoUplink,
    /// Detected uplink name isn't safe to embed in an nftables ruleset.
    #[error("uplink interface name {0:?} is not valid for an nftables rule")]
    InvalidUplinkName(String),
    /// Couldn't spawn the `nft` binary.
    #[error("failed to spawn nft")]
    Spawn(#[source] std::io::Error),
    /// Writing the ruleset to `nft`'s stdin failed.
    #[error("failed to write ruleset to nft stdin")]
    WriteStdin(#[source] std::io::Error),
    /// `nft` rejected the ruleset.
    #[error("nft rejected the ruleset: {stderr}")]
    NftFailed {
        /// `nft`'s stderr output.
        stderr: String,
    },
}

/// Single-writer actor: every `nft` write goes through one mutex, so
/// concurrent callers cannot race two transactions or act on a stale
/// "already applied" decision (lost update). The state it guards lets a
/// no-op apply short-circuit and lets `remove_vm_policy` recover the leased
/// IP it needs to delete this VM's IP-keyed map elements.
#[derive(Debug)]
pub struct FirewallActor {
    /// The one lock every `nft` write goes through.
    state: Mutex<FirewallState>,
}

/// The actor's cached view of what's currently applied to `nft`.
#[derive(Debug, Default)]
struct FirewallState {
    /// The global ruleset text last applied. Compared verbatim so both an
    /// uplink change and a MicroNetwork being added/removed re-apply, without
    /// this state needing to mirror every input that goes into rendering.
    applied_ruleset: Option<String>,
    /// vm_id -> complete policy of every VM whose policy is currently
    /// installed. Keeping the full value lets an identical re-apply be a
    /// true no-op, while a changed lease or egress setting can be replaced
    /// atomically.
    applied_vms: std::collections::HashMap<Uuid, VmPolicy>,
}

impl FirewallActor {
    /// Creates an actor with nothing recorded as applied yet.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(FirewallState::default()),
        }
    }
}

impl Default for FirewallActor {
    /// Same as [`FirewallActor::new`].
    fn default() -> Self {
        Self::new()
    }
}

/// Detect the uplink, and (re)apply the Firecrab tables only if the rendered
/// ruleset differs from what was last applied — i.e. on an uplink change or
/// when a MicroNetwork is created/removed. Never touches any table/chain this
/// helper does not own.
pub async fn ensure_firewall(
    actor: &FirewallActor,
    micro_networks: &[MicroNetworkSpec],
) -> Result<(), FirewallError> {
    let (connection, handle, _) = new_connection().map_err(FirewallError::Connection)?;
    tokio::spawn(connection);
    let uplink = nat::detect_uplink(&handle).await?;

    let mut state = actor.state.lock().await;
    let ruleset = render_apply_ruleset(&uplink, micro_networks)?;
    if state.applied_ruleset.as_deref() == Some(ruleset.as_str()) {
        return Ok(());
    }

    run_nft(&ruleset).await?;
    // Best-effort iptables compat: coexist with Docker's FORWARD DROP policy.
    ensure_iptables_compat(
        &bridge_names(micro_networks),
        &egress_subnet_cidrs(micro_networks),
        &uplink,
    )
    .await;
    // A global (re)apply flushes the tables, so no per-VM policy survives it.
    state.applied_vms.clear();
    state.applied_ruleset = Some(ruleset);
    Ok(())
}

/// Install (or atomically replace) one VM's isolation + egress policy.
/// Independent of every other VM: only this VM's named chains and map
/// elements are touched.
pub async fn apply_vm_policy(actor: &FirewallActor, policy: VmPolicy) -> Result<(), FirewallError> {
    let (connection, handle, _) = new_connection().map_err(FirewallError::Connection)?;
    tokio::spawn(connection);
    let uplink = nat::detect_uplink(&handle).await?;

    let mut state = actor.state.lock().await;
    let previous = state.applied_vms.get(&policy.vm_id).cloned();

    // `ensure_all_networks` defensively reapplies policies for running VMs.
    // An identical request needs no host mutation. Keeping this decision
    // inside the helper's single-writer lock also prevents two simultaneous
    // API requests from doing redundant nft work.
    if previous.as_ref() == Some(&policy) {
        return Ok(());
    }

    // A different policy for the same VM (for example a renewed lease) owns
    // the same chain names but may use different map keys. Delete the old
    // objects and build the new ones in one nft transaction so other VMs are
    // never affected and there is no unprotected intermediate state.
    let ruleset = match previous {
        Some(previous) => render_vm_policy_replacement(&uplink, &previous, &policy),
        None => render_vm_policy(&uplink, &policy),
    };
    run_nft(&ruleset).await?;
    state.applied_vms.insert(policy.vm_id, policy);
    Ok(())
}

/// Remove one VM's policy. Idempotent: a VM with no installed policy is a
/// no-op. VM stop/delete calls this; it never touches the shared tables.
pub async fn remove_vm_policy(actor: &FirewallActor, vm_id: Uuid) -> Result<(), FirewallError> {
    let mut state = actor.state.lock().await;
    let Some(policy) = state.applied_vms.get(&vm_id).cloned() else {
        return Ok(());
    };
    run_nft(&render_vm_policy_removal(vm_id, policy.ipv4)).await?;
    state.applied_vms.remove(&vm_id);
    Ok(())
}

/// Explicit uninstall: remove both Firecrab tables. VM stop/delete must
/// never call this — only an explicit teardown of the whole subsystem does.
/// Not wired to a caller yet (reserved for a future uninstall path); its
/// rendering is exercised directly by tests.
#[allow(dead_code)]
pub async fn remove_firewall(actor: &FirewallActor) -> Result<(), FirewallError> {
    let mut state = actor.state.lock().await;
    run_nft(&render_remove_ruleset()).await?;
    state.applied_vms.clear();
    state.applied_ruleset = None;
    Ok(())
}

/// Every Firecrab-owned bridge: one per MicroNetwork. Names are derived from
/// the network id (hex only), never taken as text from the API.
fn bridge_names(micro_networks: &[MicroNetworkSpec]) -> Vec<String> {
    micro_networks
        .iter()
        .map(MicroNetworkSpec::bridge_name)
        .collect()
}

/// Every Firecrab-owned subnet, in the same order as [`bridge_names`].
fn subnet_cidrs(micro_networks: &[MicroNetworkSpec]) -> Vec<String> {
    micro_networks
        .iter()
        .map(MicroNetworkSpec::subnet_cidr)
        .collect()
}

/// The subnets allowed out of the host — MicroNetworks with internet on
/// (`public-docs/networking.md`).
fn egress_subnet_cidrs(micro_networks: &[MicroNetworkSpec]) -> Vec<String> {
    micro_networks
        .iter()
        .filter(|network| network.internet_enabled)
        .map(MicroNetworkSpec::subnet_cidr)
        .collect()
}

/// The subnets whose internet is switched off — the complement of
/// [`egress_subnet_cidrs`] among the MicroNetworks.
fn offline_subnet_cidrs(micro_networks: &[MicroNetworkSpec]) -> Vec<String> {
    micro_networks
        .iter()
        .filter(|network| !network.internet_enabled)
        .map(MicroNetworkSpec::subnet_cidr)
        .collect()
}

/// Renders the whole VM-independent desired state for both owned tables as
/// one nft(8) script. `add table` + `flush table` before redeclaring keeps
/// this idempotent without ever touching a table this helper doesn't own.
///
/// Per-VM rules live in separate named chains + verdict-map elements (see
/// [`render_vm_policy`]) so replacing one VM's policy never disturbs another.
/// This global flush therefore only runs on an uplink change, when no per-VM
/// state is expected to be present yet.
fn render_apply_ruleset(
    uplink: &str,
    micro_networks: &[MicroNetworkSpec],
) -> Result<String, FirewallError> {
    nat::validate_uplink(uplink)?;
    let bridges = bridge_names(micro_networks);
    let subnets = subnet_cidrs(micro_networks);
    let postrouting =
        nat::render_postrouting_chain(uplink, &subnets, &egress_subnet_cidrs(micro_networks));

    // One dispatch pair per bridge: the per-VM verdict maps below are keyed
    // by leased IP (globally unique across networks, since their subnets
    // cannot overlap), so only the entry rules need to know about bridges.
    let forward_dispatch: String = bridges
        .iter()
        .map(|bridge| {
            format!(
                "\t\tiifname \"{bridge}\" jump firecrab_egress\n\
                 \t\toifname \"{bridge}\" jump firecrab_ingress\n"
            )
        })
        .collect();
    // Routed traffic aimed at any Firecrab subnet is denied before the
    // per-VM egress map is consulted: that is what keeps two MicroNetworks
    // from reaching each other now that the host routes all of them. Traffic
    // within one subnet is switched, not routed, and is denied separately by
    // the bridge table's tap-to-tap rule.
    // Empty MicroNetwork set: still block link-local/loopback as destinations
    // for any future per-VM policy; no trailing comma that would break nft.
    let internal_destinations = if subnets.is_empty() {
        "127.0.0.0/8, 169.254.0.0/16".to_owned()
    } else {
        format!("127.0.0.0/8, 169.254.0.0/16, {}", subnets.join(", "))
    };
    // A network with its internet switched off: every *new* forwarded flow
    // out of it is dropped, whatever the per-VM egress policy says. Placed
    // after the established/related accept so a VM that something else is
    // allowed to reach (forwarded inbound SSH) can still answer, and before
    // the per-VM map so the network-level switch wins over the VM-level one.
    // DHCP/DNS keep working — those terminate on the gateway itself and
    // never traverse the forward hook.
    let offline = offline_subnet_cidrs(micro_networks);
    let offline_drop = if offline.is_empty() {
        String::new()
    } else {
        format!("\t\tip saddr {{ {} }} drop\n", offline.join(", "))
    };
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
         {forward_dispatch}\
         \t}}\n\
         \tchain firecrab_egress {{\n\
         \t\tct state established,related accept\n\
         \t\tip daddr {{ {internal_destinations} }} drop\n\
         {offline_drop}\
         \t\tip saddr vmap @vm_egress\n\
         \t\tdrop\n\
         \t}}\n\
         \tchain firecrab_ingress {{\n\
         \t\tct state established,related accept\n\
         \t\tip daddr vmap @vm_ingress\n\
         \t\tdrop\n\
         \t}}\n\
         {postrouting}\
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

/// Renders one VM's isolation rules: L2 anti-spoofing tied to the lease, plus
/// L3 egress/ingress verdicts. Every per-VM object is named after the vm_id.
/// An identical policy is skipped by [`apply_vm_policy`]; a changed one is
/// rendered through [`render_vm_policy_replacement`] so it cannot disturb
/// any other VM's chains or map elements.
fn render_vm_policy(uplink: &str, policy: &VmPolicy) -> String {
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

    let mut dnat_prerouting_rules = String::new();
    let mut dnat_output_rules = String::new();
    for pf in &policy.port_forwards {
        let proto = if pf.protocol.eq_ignore_ascii_case("udp") { "udp" } else { "tcp" };
        let hp = pf.host_port;
        let gp = pf.guest_port;
        dnat_prerouting_rules.push_str(&format!(
            "add rule inet {TABLE_INET} vm_{tag}_dnat iifname \"{uplink}\" {proto} dport {hp} dnat ip to {ip}:{gp}\n"
        ));
        dnat_output_rules.push_str(&format!(
            "add rule inet {TABLE_INET} vm_{tag}_dnat_out {proto} dport {hp} dnat ip to {ip}:{gp}\n"
        ));
    }

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
         add element inet {TABLE_INET} vm_ingress {{ {ip} : jump vm_{tag}_in }}\n\
         add chain inet {TABLE_INET} vm_{tag}_dnat {{ type nat hook prerouting priority dstnat; policy accept; }}\n\
         flush chain inet {TABLE_INET} vm_{tag}_dnat\n\
         {dnat_prerouting_rules}\
         add chain inet {TABLE_INET} vm_{tag}_dnat_out {{ type nat hook output priority dstnat; policy accept; }}\n\
         flush chain inet {TABLE_INET} vm_{tag}_dnat_out\n\
         {dnat_output_rules}"
    )
}

/// Replaces a previously installed policy for the same VM in one nft
/// transaction. The old map elements must be deleted before their chains;
/// then the new chain and map entries can be installed without conflicting
/// with the old names or (when the lease changed) its old IPv4 map keys.
fn render_vm_policy_replacement(uplink: &str, previous: &VmPolicy, policy: &VmPolicy) -> String {
    debug_assert_eq!(previous.vm_id, policy.vm_id);
    format!(
        "{}{}",
        render_vm_policy_removal(previous.vm_id, previous.ipv4),
        render_vm_policy(uplink, policy)
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
         delete chain inet {TABLE_INET} vm_{tag}_in\n\
         add chain inet {TABLE_INET} vm_{tag}_dnat {{ type nat hook prerouting priority dstnat; policy accept; }}\n\
         delete chain inet {TABLE_INET} vm_{tag}_dnat\n\
         add chain inet {TABLE_INET} vm_{tag}_dnat_out {{ type nat hook output priority dstnat; policy accept; }}\n\
         delete chain inet {TABLE_INET} vm_{tag}_dnat_out\n"
    )
}

/// `add table` before `delete table` makes removal idempotent even if the
/// table was never installed, without depending on nft's newer `destroy`.
#[allow(dead_code)]
fn render_remove_ruleset() -> String {
    format!(
        "add table inet {TABLE_INET}\n\
         delete table inet {TABLE_INET}\n\
         add table bridge {TABLE_BRIDGE}\n\
         delete table bridge {TABLE_BRIDGE}\n"
    )
}

/// Best-effort: insert iptables FORWARD ACCEPT rules for every Firecrab bridge
/// and iptables NAT MASQUERADE rules for every egress subnet. This coexists
/// with Docker and other tools that configure `iptables -P FORWARD DROP`: our
/// FORWARD ACCEPT rules are checked with `-C` first (idempotent) and appended
/// only if absent. All failures are silently ignored — the nftables path is
/// canonical; this is purely a compatibility shim for iptables-managed hosts.
async fn ensure_iptables_compat(bridges: &[String], egress_subnets: &[String], uplink: &str) {
    for bridge in bridges {
        for dir in ["-i", "-o"] {
            let already = Command::new("iptables")
                .args(["-C", "FORWARD", dir, bridge, "-j", "ACCEPT"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false);
            if !already {
                let _ = Command::new("iptables")
                    .args(["-A", "FORWARD", dir, bridge, "-j", "ACCEPT"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await;
            }
        }
    }
    for subnet in egress_subnets {
        let already = Command::new("iptables")
            .args([
                "-t",
                "nat",
                "-C",
                "POSTROUTING",
                "-s",
                subnet,
                "-o",
                uplink,
                "-j",
                "MASQUERADE",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);
        if !already {
            let _ = Command::new("iptables")
                .args([
                    "-t",
                    "nat",
                    "-A",
                    "POSTROUTING",
                    "-s",
                    subnet,
                    "-o",
                    uplink,
                    "-j",
                    "MASQUERADE",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
    }
}

/// Removes iptables FORWARD ACCEPT rules for a bridge that is being torn down.
/// Best-effort; silently ignores errors (rule already absent, iptables not
/// available).
pub async fn remove_iptables_forward_for_bridge(bridge: &str) {
    for dir in ["-i", "-o"] {
        let _ = Command::new("iptables")
            .args(["-D", "FORWARD", dir, bridge, "-j", "ACCEPT"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
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

    let output = child
        .wait_with_output()
        .await
        .map_err(FirewallError::Spawn)?;
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

    fn sample_network(id: u128, gateway: &str, prefix: u8) -> MicroNetworkSpec {
        MicroNetworkSpec {
            micro_network_id: Uuid::from_u128(id),
            gateway: gateway.parse().unwrap(),
            prefix,
            internet_enabled: true,
        }
    }

    #[test]
    fn every_micro_network_gets_its_own_dispatch_and_nat_rules() {
        let networks = [
            sample_network(0x1234, "172.31.0.1", 24),
            sample_network(0x5678, "172.32.0.1", 24),
        ];
        let ruleset = render_apply_ruleset("eth0", &networks).unwrap();

        assert!(!ruleset.contains("iifname \"fcbr0\""));

        for network in networks {
            let bridge = network.bridge_name();
            assert!(ruleset.contains(&format!("iifname \"{bridge}\" jump firecrab_egress")));
            assert!(ruleset.contains(&format!("oifname \"{bridge}\" jump firecrab_ingress")));
            assert!(ruleset.contains(&format!(
                "ip saddr {} oifname \"eth0\" jump firecrab_postrouting",
                network.subnet_cidr()
            )));
        }
        // All of them masquerade through the one shared chain, plus the
        // loopback-source rule for host-local port forwards.
        assert_eq!(ruleset.matches("masquerade").count(), 2);
    }

    #[test]
    fn loopback_masquerade_is_scoped_to_vm_subnets_only() {
        // A prior version of this rule masqueraded every 127.0.0.0/8-sourced
        // packet unconditionally, which also caught ordinary host loopback
        // traffic (e.g. systemd-resolved's 127.0.0.53 stub) and broke host
        // DNS resolution. It must stay scoped to `ip daddr <vm subnets>` so
        // it only ever touches traffic actually headed into a Firecrab VM.
        let networks = [sample_network(0x1234, "172.31.0.1", 24)];
        let ruleset = render_apply_ruleset("eth0", &networks).unwrap();
        assert!(ruleset.contains("ip saddr 127.0.0.0/8 ip daddr { 172.31.0.0/24 } masquerade"));

        let ruleset_empty = render_apply_ruleset("eth0", &[]).unwrap();
        assert!(!ruleset_empty.contains("ip saddr 127.0.0.0/8"));
    }

    #[test]
    fn traffic_routed_between_micro_networks_is_dropped() {
        let networks = [
            sample_network(0x1234, "172.31.0.1", 24),
            sample_network(0x5678, "172.32.0.1", 24),
        ];
        let ruleset = render_apply_ruleset("eth0", &networks).unwrap();
        // Every Firecrab subnet is a denied destination for routed traffic,
        // so one MicroNetwork cannot reach another even though the host has
        // a connected route to both and ip_forward is on.
        assert!(ruleset.contains(
            "ip daddr { 127.0.0.0/8, 169.254.0.0/16, 172.31.0.0/24, 172.32.0.0/24 } drop"
        ));
    }

    #[test]
    fn a_network_with_the_internet_off_is_neither_masqueraded_nor_forwarded() {
        let offline = MicroNetworkSpec {
            internet_enabled: false,
            ..sample_network(0x1234, "172.31.0.1", 24)
        };
        let online = sample_network(0x5678, "172.32.0.1", 24);
        let ruleset = render_apply_ruleset("eth0", &[offline, online]).unwrap();

        // No NAT for it: its addresses are never translated.
        assert!(!ruleset.contains("ip saddr 172.31.0.0/24 oifname"));
        // And nothing new leaves it at L3 regardless of per-VM egress policy.
        assert!(ruleset.contains("ip saddr { 172.31.0.0/24 } drop"));
        // The drop lands after the established/related accept, so a VM that
        // is reachable from outside can still answer.
        let accept = ruleset.find("ct state established,related accept").unwrap();
        assert!(accept < ruleset.find("ip saddr { 172.31.0.0/24 } drop").unwrap());

        // Its bridge and DHCP range are untouched — the network still exists,
        // it just has no way out.
        assert!(ruleset.contains(&format!(
            "iifname \"{}\" jump firecrab_egress",
            offline.bridge_name()
        )));
        // The other network is unaffected.
        assert!(
            ruleset.contains("ip saddr 172.32.0.0/24 oifname \"eth0\" jump firecrab_postrouting")
        );
    }

    #[test]
    fn switching_the_internet_off_changes_the_ruleset_so_it_gets_re_applied() {
        // FirewallState compares the rendered text verbatim, so a toggle that
        // rendered identically would silently never reach nft.
        let online = sample_network(0x1234, "172.31.0.1", 24);
        let offline = MicroNetworkSpec {
            internet_enabled: false,
            ..online
        };
        assert_ne!(
            render_apply_ruleset("eth0", &[online]).unwrap(),
            render_apply_ruleset("eth0", &[offline]).unwrap()
        );
    }

    #[test]
    fn ruleset_only_declares_the_two_owned_tables() {
        let ruleset = render_apply_ruleset("eth0", &[]).unwrap();
        assert!(ruleset.contains("table inet firecrab"));
        assert!(ruleset.contains("table bridge firecrab_l2"));
        // Never a blanket flush of the whole host ruleset.
        assert!(!ruleset.contains("flush ruleset"));
    }

    #[test]
    fn global_ruleset_dispatches_bridge_traffic_from_accept_policy_base_chains() {
        let network = sample_network(0x1234, "172.31.0.1", 24);
        let ruleset = render_apply_ruleset("eth0", &[network]).unwrap();
        assert!(ruleset.contains("policy accept"));
        let bridge = network.bridge_name();
        assert!(ruleset.contains(&format!("iifname \"{bridge}\" jump firecrab_egress")));
        assert!(ruleset.contains(&format!("oifname \"{bridge}\" jump firecrab_ingress")));
        assert!(
            ruleset.contains("ip saddr 172.31.0.0/24 oifname \"eth0\" jump firecrab_postrouting")
        );
        assert!(ruleset.contains("masquerade"));
    }

    #[test]
    fn global_ruleset_default_denies_egress_and_ingress_and_reserved_dests() {
        let ruleset = render_apply_ruleset("eth0", &[]).unwrap();
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
        let ruleset = render_apply_ruleset("eth0", &[]).unwrap();
        assert!(ruleset.contains("iifname \"fct*\" oifname \"fct*\" drop"));
        // The wildcard must not be able to match the bridge itself.
        assert!(!"fcbr0".starts_with("fct"));
    }

    #[test]
    fn global_ruleset_is_idempotent_via_add_then_flush_and_owns_only_two_tables() {
        let ruleset = render_apply_ruleset("eth0", &[]).unwrap();
        assert!(ruleset.contains("add table inet firecrab\nflush table inet firecrab"));
        assert!(ruleset.contains("add table bridge firecrab_l2\nflush table bridge firecrab_l2"));
        assert!(!ruleset.contains("flush ruleset"));
    }

    #[test]
    fn malformed_uplink_names_are_rejected_before_touching_nft() {
        for bad in ["", "eth0\"; flush ruleset #", "way-too-long-interface-name"] {
            assert!(matches!(
                render_apply_ruleset(bad, &[]),
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
            port_forwards: Vec::new(),
        }
    }

    #[test]
    fn vm_policy_pins_l2_source_to_the_lease_and_blocks_ipv6_vlan() {
        let policy = sample_policy(EgressPolicy::Internet, false);
        let ruleset = render_vm_policy("eth0", &policy);
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
        let ruleset = render_vm_policy("eth0", &sample_policy(EgressPolicy::Internet, false));
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
        let internet = render_vm_policy("eth0", &sample_policy(EgressPolicy::Internet, false));
        let tag = Uuid::from_u128(0x1234).simple();
        assert!(internet.contains(&format!("add rule inet firecrab vm_{tag}_eg accept")));

        let isolated = render_vm_policy("eth0", &sample_policy(EgressPolicy::Isolated, false));
        // Isolated leaves the egress chain empty (no accept rule for it).
        assert!(!isolated.contains(&format!("add rule inet firecrab vm_{tag}_eg accept")));
        // But the chain and its dispatch element still exist.
        assert!(isolated.contains(&format!("add chain inet firecrab vm_{tag}_eg")));
        assert!(isolated.contains("add element inet firecrab vm_egress { 172.30.0.42 : jump"));
    }

    #[test]
    fn host_ssh_is_allowed_only_when_requested() {
        let tag = Uuid::from_u128(0x1234).simple();
        let with_ssh = render_vm_policy("eth0", &sample_policy(EgressPolicy::Internet, true));
        assert!(with_ssh.contains(&format!(
            "add rule inet firecrab vm_{tag}_in tcp dport 22 ct state new,established accept"
        )));

        let without = render_vm_policy("eth0", &sample_policy(EgressPolicy::Internet, false));
        assert!(!without.contains("tcp dport 22"));
    }

    #[test]
    fn port_forwarding_renders_dnat_rules() {
        let tag = Uuid::from_u128(0x1234).simple();
        let mut policy = sample_policy(EgressPolicy::Internet, false);
        policy.port_forwards = vec![
            firecrab_helper_protocol::network::PortForwardSpec {
                host_port: 8080,
                guest_port: 80,
                protocol: "tcp".to_owned(),
            },
            firecrab_helper_protocol::network::PortForwardSpec {
                host_port: 5353,
                guest_port: 53,
                protocol: "udp".to_owned(),
            },
        ];
        let ruleset = render_vm_policy("eth0", &policy);
        assert!(ruleset.contains(&format!(
            "add chain inet firecrab vm_{tag}_dnat {{ type nat hook prerouting priority dstnat; policy accept; }}"
        )));
        assert!(ruleset.contains(&format!(
            "add chain inet firecrab vm_{tag}_dnat_out {{ type nat hook output priority dstnat; policy accept; }}"
        )));
        assert!(ruleset.contains(&format!(
            "add rule inet firecrab vm_{tag}_dnat iifname \"eth0\" tcp dport 8080 dnat ip to 172.30.0.42:80"
        )));
        assert!(ruleset.contains(&format!(
            "add rule inet firecrab vm_{tag}_dnat_out tcp dport 8080 dnat ip to 172.30.0.42:80"
        )));
        assert!(ruleset.contains(&format!(
            "add rule inet firecrab vm_{tag}_dnat iifname \"eth0\" udp dport 5353 dnat ip to 172.30.0.42:53"
        )));
        assert!(ruleset.contains(&format!(
            "add rule inet firecrab vm_{tag}_dnat_out udp dport 5353 dnat ip to 172.30.0.42:53"
        )));
    }

    #[test]
    fn port_forward_prerouting_dnat_is_scoped_to_the_uplink() {
        let mut policy = sample_policy(EgressPolicy::Internet, false);
        policy.port_forwards = vec![firecrab_helper_protocol::network::PortForwardSpec {
            host_port: 8080,
            guest_port: 80,
            protocol: "tcp".to_owned(),
        }];
        let ruleset = render_vm_policy("wan0", &policy);
        // Prerouting DNAT only matches traffic arriving on the uplink, so
        // routed traffic between VMs (or any other host-forwarded traffic)
        // hitting the same destination port cannot be hijacked to this VM.
        assert!(ruleset.contains("vm_") && ruleset.contains("_dnat iifname \"wan0\" tcp dport 8080"));
    }

    #[test]
    fn vm_policy_objects_are_namespaced_so_replacing_one_vm_cannot_touch_another() {
        let a = render_vm_policy("eth0", &sample_policy(EgressPolicy::Internet, false));
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
    fn replacement_removes_the_old_policy_before_adding_the_new_one() {
        let previous = sample_policy(EgressPolicy::Internet, false);
        let replacement = VmPolicy {
            ipv4: Ipv4Addr::new(172, 30, 0, 43),
            egress: EgressPolicy::Isolated,
            allow_host_ssh: true,
            ..previous.clone()
        };
        let ruleset = render_vm_policy_replacement("eth0", &previous, &replacement);

        let old_element = ruleset
            .find("delete element inet firecrab vm_egress { 172.30.0.42 }")
            .expect("the old IP map key is removed");
        let new_chain = ruleset
            .find("add chain bridge firecrab_l2")
            .expect("the replacement starts by adding new chains");
        let new_element = ruleset
            .find("add element inet firecrab vm_egress { 172.30.0.43 : jump")
            .expect("the new IP map key is installed");

        assert!(old_element < new_chain);
        assert!(new_chain < new_element);
    }

    #[test]
    fn removal_deletes_map_elements_before_their_chains() {
        let vm = Uuid::from_u128(0x1234);
        let ruleset = render_vm_policy_removal(vm, Ipv4Addr::new(172, 30, 0, 42));
        let tag = vm.simple();
        let l2_elem = ruleset
            .find("delete element bridge firecrab_l2 l2_ingress")
            .unwrap();
        let l2_chain = ruleset
            .find(&format!("delete chain bridge firecrab_l2 vm_{tag}_l2"))
            .unwrap();
        assert!(
            l2_elem < l2_chain,
            "map element must be deleted before its chain"
        );

        let eg_elem = ruleset
            .find("delete element inet firecrab vm_egress")
            .unwrap();
        let eg_chain = ruleset
            .find(&format!("delete chain inet firecrab vm_{tag}_eg"))
            .unwrap();
        assert!(
            eg_elem < eg_chain,
            "egress element must be deleted before its chain"
        );
    }

    #[test]
    fn unknown_egress_policy_id_is_rejected() {
        assert_eq!(
            EgressPolicy::from_id("internet"),
            Some(EgressPolicy::Internet)
        );
        assert_eq!(
            EgressPolicy::from_id("isolated"),
            Some(EgressPolicy::Isolated)
        );
        assert_eq!(EgressPolicy::from_id("0.0.0.0/0"), None);
        assert_eq!(EgressPolicy::from_id("wide-open"), None);
    }

    #[tokio::test]
    async fn ensure_firewall_skips_nft_entirely_when_the_uplink_is_unchanged() {
        let (connection, handle, _) = new_connection().unwrap();
        tokio::spawn(connection);
        let real_uplink = nat::detect_uplink(&handle).await.unwrap();

        // Pre-seed the actor as if this uplink was already applied. No `nft`
        // binary needs to exist or succeed for this call to return Ok, since
        // it must short-circuit before ever calling run_nft/spawning nft.
        let applied = render_apply_ruleset(&real_uplink, &[]).unwrap();
        let actor = FirewallActor::new();
        actor.state.lock().await.applied_ruleset = Some(applied.clone());

        assert!(ensure_firewall(&actor, &[]).await.is_ok());
        assert_eq!(
            actor.state.lock().await.applied_ruleset.as_deref(),
            Some(applied.as_str())
        );
    }

    #[tokio::test]
    async fn applying_an_identical_vm_policy_skips_nft_entirely() {
        let actor = FirewallActor::new();
        let policy = sample_policy(EgressPolicy::Internet, false);
        actor
            .state
            .lock()
            .await
            .applied_vms
            .insert(policy.vm_id, policy.clone());

        // If the identical request spawned nft, this unprivileged unit test
        // would fail on a host without NET_ADMIN. Returning Ok proves an
        // idempotent reapply causes no unnecessary host-side mutation.
        assert!(apply_vm_policy(&actor, policy.clone()).await.is_ok());
        assert_eq!(
            actor.state.lock().await.applied_vms.get(&policy.vm_id),
            Some(&policy)
        );
    }

    #[test]
    fn adding_a_micro_network_changes_the_rendered_ruleset() {
        // What `ensure_firewall`'s short-circuit compares is this text, so a
        // network set that renders differently is exactly what makes it
        // re-apply rather than skip.
        let without = render_apply_ruleset("eth0", &[]).unwrap();
        let with =
            render_apply_ruleset("eth0", &[sample_network(0x1234, "172.31.0.1", 24)]).unwrap();
        assert_ne!(without, with);
    }

    #[tokio::test]
    async fn remove_vm_policy_is_a_no_op_when_nothing_was_applied() {
        // No applied_vms entry -> returns Ok without ever invoking nft.
        let actor = FirewallActor::new();
        assert!(
            remove_vm_policy(&actor, Uuid::from_u128(0x1234))
                .await
                .is_ok()
        );
    }
}
