//! Per-VM and global nftables rules: NAT/egress dispatch (`inet firecrab`)
//! and L2 anti-spoofing (`bridge firecrab_l2`), both idempotently rendered
//! and applied as single atomic `nft -f -` transactions.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::process::Stdio;

use firecrab_helper_protocol::network::{MacAddr, MicroNetworkSpec, tap_name};
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
    /// The VM's leased IPv6 address, when its MicroNetwork is dual-stack.
    /// `None` keeps the VM IPv4-only: every IPv6 frame it sends is dropped
    /// at L2, exactly as before this field existed.
    pub ipv6: Option<Ipv6Addr>,
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
    /// vm_id -> (uplink, complete policy) of every VM whose policy is
    /// currently installed. Keeping the full value lets an identical
    /// re-apply be a true no-op, while a changed lease, egress setting, or
    /// uplink can be replaced atomically. The uplink has to be part of this
    /// key too: `render_vm_policy_for_network` bakes it into the DNAT rules' `iifname`
    /// match, so an uplink change with an otherwise-identical `VmPolicy`
    /// still needs a real reapply, not a no-op.
    applied_vms: std::collections::HashMap<Uuid, (String, VmPolicy)>,
    /// Last `EnsureFirewall` network set, so `ApplyVmPolicy` can resolve
    /// the same per-network uplink without a protocol change.
    networks: Vec<MicroNetworkSpec>,
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

/// Detect the uplink and reconcile the complete Firecrab firewall snapshot.
/// Shared tables and every desired VM policy are sent to nft as one atomic
/// transaction, so policies absent from `vm_policies` disappear as orphans
/// while policies still belonging to active VMs never vanish between calls.
/// Never touches any table/chain this helper does not own.
pub async fn ensure_firewall(
    actor: &FirewallActor,
    micro_networks: &[MicroNetworkSpec],
    vm_policies: &[VmPolicy],
) -> Result<(), FirewallError> {
    let (connection, handle, _) = new_connection().map_err(FirewallError::Connection)?;
    tokio::spawn(connection);
    let default_uplink = nat::detect_uplink(&handle).await?;

    let mut state = actor.state.lock().await;
    state.networks = micro_networks.to_vec();
    let base_ruleset = render_apply_ruleset(&default_uplink, micro_networks)?;
    let desired_vms: std::collections::HashMap<Uuid, (String, VmPolicy)> = vm_policies
        .iter()
        .cloned()
        .map(|policy| {
            let uplink = resolved_uplink(&default_uplink, micro_networks, policy.ipv4);
            (policy.vm_id, (uplink, policy))
        })
        .collect();
    // Host INPUT/FORWARD DROP (UFW, firewalld, nftables.service, iptables)
    // swallows DHCP/DNS on a newly created bridge. The owned nft table only
    // hooks forward/postrouting, so each backend is punched here — even when
    // the Firecrab ruleset is unchanged, so a UFW reload or firewalld
    // restart is repaired on the next reconcile.
    crate::host_acl::ensure_all(&default_uplink, micro_networks).await;
    // Routing a dual-stack network's traffic needs host-wide IPv6
    // forwarding, which no IPv4-only deployment should have switched on for
    // it. Best-effort like the host ACL punches above: a host that refuses
    // the sysctl still gets its v4 ruleset applied.
    if micro_networks.iter().any(|network| network.ipv6.is_some()) {
        let mut uplinks: Vec<&str> = vec![default_uplink.as_str()];
        uplinks.extend(
            micro_networks
                .iter()
                .filter_map(|network| network.uplink.as_deref()),
        );
        if let Err(error) = crate::bridge::enable_ipv6_forward(&uplinks) {
            println!("[WARN] failed to enable IPv6 forwarding: {error}");
        }
    }
    if state.applied_ruleset.as_deref() == Some(base_ruleset.as_str())
        && state.applied_vms == desired_vms
    {
        return Ok(());
    }

    let ruleset = render_reconciled_ruleset(
        base_ruleset.as_str(),
        &default_uplink,
        micro_networks,
        vm_policies,
    );
    run_nft(&ruleset).await?;
    // Best-effort iptables compat: coexist with Docker's FORWARD DROP policy.
    ensure_iptables_compat(
        &bridge_names(micro_networks),
        &egress_pairs(&default_uplink, micro_networks),
    )
    .await;
    state.applied_vms = desired_vms;
    state.applied_ruleset = Some(base_ruleset);
    Ok(())
}

/// Appends a canonical, UUID-sorted VM policy snapshot to the base ruleset.
/// The base begins by flushing the two Firecrab-owned tables, so any old
/// policy not present in this output is removed in the same nft transaction
/// that restores policies which are still desired.
fn render_reconciled_ruleset(
    base_ruleset: &str,
    default_uplink: &str,
    micro_networks: &[MicroNetworkSpec],
    vm_policies: &[VmPolicy],
) -> String {
    let mut ruleset = base_ruleset.to_owned();
    let mut sorted_policies = vm_policies.iter().collect::<Vec<_>>();
    sorted_policies.sort_by_key(|policy| policy.vm_id);
    for policy in sorted_policies {
        let uplink = resolved_uplink(default_uplink, micro_networks, policy.ipv4);
        let internet = network_internet_enabled(micro_networks, policy.ipv4);
        ruleset.push_str(&render_vm_policy_for_network(&uplink, policy, internet));
        ruleset.push('\n');
    }
    ruleset
}

/// Install (or atomically replace) one VM's isolation + egress policy.
/// Independent of every other VM: only this VM's named chains and map
/// elements are touched.
pub async fn apply_vm_policy(actor: &FirewallActor, policy: VmPolicy) -> Result<(), FirewallError> {
    let (connection, handle, _) = new_connection().map_err(FirewallError::Connection)?;
    tokio::spawn(connection);
    let default_uplink = nat::detect_uplink(&handle).await?;

    let mut state = actor.state.lock().await;
    let uplink = resolved_uplink(&default_uplink, &state.networks, policy.ipv4);
    let internet = network_internet_enabled(&state.networks, policy.ipv4);
    let previous = state.applied_vms.get(&policy.vm_id).cloned();

    // `ensure_all_networks` now includes active policies in its atomic
    // snapshot, so setup's immediately-following per-VM apply is normally
    // identical. It needs no host mutation. Keeping this decision inside the
    // helper's single-writer lock also prevents two simultaneous API requests
    // from doing redundant nft work.
    if previous.as_ref() == Some(&(uplink.clone(), policy.clone())) {
        return Ok(());
    }

    // A different policy for the same VM (for example a renewed lease) owns
    // the same chain names but may use different map keys. Delete the old
    // objects and build the new ones in one nft transaction so other VMs are
    // never affected and there is no unprotected intermediate state.
    let ruleset = match previous {
        Some((_, previous)) => render_vm_policy_replacement(&uplink, &previous, &policy, internet),
        None => render_vm_policy_for_network(&uplink, &policy, internet),
    };
    run_nft(&ruleset).await?;
    state.applied_vms.insert(policy.vm_id, (uplink, policy));
    Ok(())
}

/// Remove one VM's policy. Idempotent: a VM with no installed policy is a
/// no-op. VM stop/delete calls this; it never touches the shared tables.
pub async fn remove_vm_policy(actor: &FirewallActor, vm_id: Uuid) -> Result<(), FirewallError> {
    let mut state = actor.state.lock().await;
    let Some((_, policy)) = state.applied_vms.get(&vm_id).cloned() else {
        return Ok(());
    };
    run_nft(&render_vm_policy_removal(vm_id, policy.ipv4, policy.ipv6)).await?;
    state.applied_vms.remove(&vm_id);
    Ok(())
}

/// Explicit uninstall: remove both Firecrab tables. VM stop/delete must
/// never call this — only `main.rs`'s `--teardown` mode does, ahead of
/// `install.sh --uninstall` removing the binaries.
pub async fn remove_firewall(actor: &FirewallActor) -> Result<(), FirewallError> {
    let mut state = actor.state.lock().await;
    run_nft(&render_remove_ruleset()).await?;
    state.applied_vms.clear();
    state.applied_ruleset = None;
    state.networks.clear();
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

/// Internet-enabled networks as `(subnet_cidr, oifname)` pairs. A missing
/// spec uplink uses `default_uplink` (`detect_uplink()` at apply time).
fn egress_pairs(
    default_uplink: &str,
    micro_networks: &[MicroNetworkSpec],
) -> Vec<(String, String)> {
    micro_networks
        .iter()
        .filter(|network| network.internet_enabled)
        .map(|network| {
            (
                network.subnet_cidr(),
                network
                    .uplink
                    .as_deref()
                    .unwrap_or(default_uplink)
                    .to_owned(),
            )
        })
        .collect()
}

/// The NIC a VM's port-forward DNAT should match: the uplink of the
/// MicroNetwork that owns its lease, or `default_uplink` when the spec
/// omitted one / the lease is not in any known subnet.
fn resolved_uplink(
    default_uplink: &str,
    micro_networks: &[MicroNetworkSpec],
    ipv4: Ipv4Addr,
) -> String {
    micro_networks
        .iter()
        .find(|network| network.contains(ipv4))
        .map(|network| {
            network
                .uplink
                .as_deref()
                .unwrap_or(default_uplink)
                .to_owned()
        })
        .unwrap_or_else(|| default_uplink.to_owned())
}

/// Whether the MicroNetwork that owns `ipv4` may accept inbound port
/// forwards. A missing lease keeps today's render (treat as online).
fn network_internet_enabled(micro_networks: &[MicroNetworkSpec], ipv4: Ipv4Addr) -> bool {
    micro_networks
        .iter()
        .find(|network| network.contains(ipv4))
        .map(|network| network.internet_enabled)
        .unwrap_or(true)
}

/// The subnets whose internet is switched off — the complement of
/// [`egress_pairs`] among the MicroNetworks.
fn offline_subnet_cidrs(micro_networks: &[MicroNetworkSpec]) -> Vec<String> {
    micro_networks
        .iter()
        .filter(|network| !network.internet_enabled)
        .map(MicroNetworkSpec::subnet_cidr)
        .collect()
}

/// Every Firecrab-owned IPv6 prefix, for the networks that have one.
fn subnet_cidrs6(micro_networks: &[MicroNetworkSpec]) -> Vec<String> {
    micro_networks
        .iter()
        .filter_map(|network| network.ipv6.as_ref())
        .map(|ipv6| ipv6.subnet_cidr())
        .collect()
}

/// The IPv6 prefixes that need NAT66, as `(prefix, oifname)` pairs. Only a
/// Unique Local prefix qualifies: a global prefix is routable as-is, so
/// translating it would hide the very addresses the network was given
/// (`public-docs/networking.md`). A network with the internet switched off
/// contributes no pair either, same as [`egress_pairs`].
fn egress_pairs6(
    default_uplink: &str,
    micro_networks: &[MicroNetworkSpec],
) -> Vec<(String, String)> {
    micro_networks
        .iter()
        .filter(|network| network.internet_enabled)
        .filter_map(|network| {
            let ipv6 = network
                .ipv6
                .as_ref()
                .filter(|ipv6| ipv6.is_unique_local())?;
            Some((
                ipv6.subnet_cidr(),
                network
                    .uplink
                    .as_deref()
                    .unwrap_or(default_uplink)
                    .to_owned(),
            ))
        })
        .collect()
}

/// The IPv6 prefixes whose internet is switched off — [`offline_subnet_cidrs`]
/// for the second family.
fn offline_subnet_cidrs6(micro_networks: &[MicroNetworkSpec]) -> Vec<String> {
    micro_networks
        .iter()
        .filter(|network| !network.internet_enabled)
        .filter_map(|network| network.ipv6.as_ref())
        .map(|ipv6| ipv6.subnet_cidr())
        .collect()
}

/// Renders the whole VM-independent desired state for both owned tables as
/// one nft(8) script. `add table` + `delete table` before recreating keeps
/// this idempotent without ever touching a table this helper doesn't own.
/// Deletion is required: nft's `flush table` removes rules but preserves
/// named map/set elements, which is exactly where stale VM IPv4 ownership
/// lives.
///
/// Per-VM rules live in separate named chains + verdict-map elements (see
/// [`render_vm_policy_for_network`]) so replacing one VM's policy never disturbs another.
/// The complete desired VM snapshot is appended to this recreation in the
/// same transaction by [`render_reconciled_ruleset`].
fn render_apply_ruleset(
    default_uplink: &str,
    micro_networks: &[MicroNetworkSpec],
) -> Result<String, FirewallError> {
    nat::validate_uplink(default_uplink)?;
    let egress = egress_pairs(default_uplink, micro_networks);
    for (_, oif) in &egress {
        nat::validate_uplink(oif)?;
    }
    let egress6 = egress_pairs6(default_uplink, micro_networks);
    for (_, oif) in &egress6 {
        nat::validate_uplink(oif)?;
    }
    let bridges = bridge_names(micro_networks);
    let subnets = subnet_cidrs(micro_networks);
    let subnets6 = subnet_cidrs6(micro_networks);
    let postrouting = nat::render_postrouting_chain(&subnets, &egress, &egress6);

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
    // within one subnet is switched, not routed, and is intentionally allowed
    // so VMs in the same MicroNetwork can communicate with each other.
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
    // The same two isolation rules for the second family. Rendered only when
    // a network actually has a prefix, so an IPv4-only host keeps exactly
    // the ruleset it had before dual-stack existed: with no v6 rule to match,
    // an IPv6 packet falls through to firecrab_egress's trailing drop.
    let cross_network_drop6 = if subnets6.is_empty() {
        String::new()
    } else {
        format!("\t\tip6 daddr {{ {} }} drop\n", subnets6.join(", "))
    };
    let offline6 = offline_subnet_cidrs6(micro_networks);
    let offline_drop6 = if offline6.is_empty() {
        String::new()
    } else {
        format!("\t\tip6 saddr {{ {} }} drop\n", offline6.join(", "))
    };
    Ok(format!(
        // L3: NAT + egress/ingress dispatch keyed by the VM's leased IP. The
        // L2 table below guarantees the source IP is genuine, so keying L3
        // policy on `ip saddr` is safe even though the routed packet's
        // iifname is the bridge, not the individual TAP.
        "add table inet {TABLE_INET}\n\
         delete table inet {TABLE_INET}\n\
         table inet {TABLE_INET} {{\n\
         \tmap vm_egress {{\n\
         \t\ttype ipv4_addr : verdict\n\
         \t}}\n\
         \tmap vm_ingress {{\n\
         \t\ttype ipv4_addr : verdict\n\
         \t}}\n\
         \tmap vm_egress6 {{\n\
         \t\ttype ipv6_addr : verdict\n\
         \t}}\n\
         \tmap vm_ingress6 {{\n\
         \t\ttype ipv6_addr : verdict\n\
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
         {cross_network_drop6}\
         {offline_drop6}\
         \t\tip6 saddr vmap @vm_egress6\n\
         \t\tdrop\n\
         \t}}\n\
         \tchain firecrab_ingress {{\n\
         \t\tct state established,related accept\n\
         \t\tip daddr vmap @vm_ingress\n\
         \t\tip6 daddr vmap @vm_ingress6\n\
         \t\tdrop\n\
         \t}}\n\
         {postrouting}\
         }}\n\
         add table bridge {TABLE_BRIDGE}\n\
         delete table bridge {TABLE_BRIDGE}\n\
         table bridge {TABLE_BRIDGE} {{\n\
         \tmap l2_ingress {{\n\
         \t\ttype ifname : verdict\n\
         \t}}\n\
         \tchain prerouting {{\n\
         \t\ttype filter hook prerouting priority -300; policy accept;\n\
         \t\tiifname vmap @l2_ingress\n\
         \t}}\n\
         }}\n"
    ))
}

fn render_vm_policy_for_network(uplink: &str, policy: &VmPolicy, internet_enabled: bool) -> String {
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

    // Dual-stack VM: IPv6 stops being a blanket-dropped ethertype, and its
    // leased address is pinned the way `ip saddr` is. Neighbor discovery,
    // DAD, MLD, and DHCPv6 are the exceptions — they run before the guest
    // owns the address, from the unspecified (`::`) or link-local source —
    // and they stay behind the `ether saddr` pin, so a VM still cannot
    // speak for another's MAC. Guest-originated Router Advertisements are
    // not in the set. Without an IPv6 lease none of this is rendered and
    // the VM keeps the IPv4-only posture it had before.
    let (l2_v6_exceptions, l2_ethertypes, l2_v6_pin, v6_map_elements) = match policy.ipv6 {
        Some(ipv6) => (
            format!(
                "{l2} ether saddr {mac} ip6 saddr :: icmpv6 type nd-neighbor-solicit accept\n\
                 {l2} ether saddr {mac} ip6 saddr fe80::/10 icmpv6 type {{ nd-neighbor-solicit, nd-neighbor-advert, nd-router-solicit, mld-listener-report, mld2-listener-report }} accept\n\
                 {l2} ether saddr {mac} ip6 saddr fe80::/10 udp dport 547 accept\n"
            ),
            format!("{l2} ether type != {{ ip, arp, ip6 }} drop\n"),
            format!("{l2} ether type ip6 ip6 saddr != {ipv6} drop\n"),
            format!(
                "add element inet {TABLE_INET} vm_egress6 {{ {ipv6} : jump vm_{tag}_eg }}\n\
                 add element inet {TABLE_INET} vm_ingress6 {{ {ipv6} : jump vm_{tag}_in }}\n"
            ),
        ),
        None => (
            String::new(),
            format!("{l2} ether type != {{ ip, arp }} drop\n"),
            String::new(),
            String::new(),
        ),
    };

    let mut dnat_prerouting_rules = String::new();
    let mut dnat_output_rules = String::new();
    let mut dnat_forward_accept_rules = String::new();
    for pf in policy.port_forwards.iter().filter(|_| internet_enabled) {
        let proto = if pf.protocol.eq_ignore_ascii_case("udp") {
            "udp"
        } else {
            "tcp"
        };
        let hp = pf.host_port;
        let gp = pf.guest_port;
        dnat_prerouting_rules.push_str(&format!(
            "add rule inet {TABLE_INET} vm_{tag}_dnat iifname \"{uplink}\" {proto} dport {hp} dnat ip to {ip}:{gp}\n"
        ));
        // `fib daddr type local` restricts this to packets the host itself
        // originated *to one of its own addresses* (127.0.0.1, its LAN/uplink
        // IP, ...). Without it, this output-hook rule matches every locally
        // generated packet on this port, including the host's own outbound
        // connections to unrelated remote hosts on the same port — hijacking
        // them to this VM instead of letting them leave normally.
        dnat_output_rules.push_str(&format!(
            "add rule inet {TABLE_INET} vm_{tag}_dnat_out fib daddr type local {proto} dport {hp} dnat ip to {ip}:{gp}\n"
        ));
        // The prerouting DNAT above only rewrites the destination; it does
        // not itself authorize forwarding. Without an explicit accept here,
        // an externally-initiated forwarded connection's first (NEW, not yet
        // established) packet falls through to firecrab_ingress's trailing
        // drop, so external port forwarding would never actually work.
        // `ct status dnat` keeps this scoped to traffic the rules above
        // actually redirected, not just anything hitting the guest port.
        dnat_forward_accept_rules
            .push_str(&format!("{in_} {proto} dport {gp} ct status dnat accept\n"));
    }

    format!(
        "add chain bridge {TABLE_BRIDGE} vm_{tag}_l2\n\
         flush chain bridge {TABLE_BRIDGE} vm_{tag}_l2\n\
         {l2} ether saddr {mac} ip saddr 0.0.0.0 udp sport 68 udp dport 67 accept\n\
         {l2} ether type arp arp operation request arp saddr ip 0.0.0.0 arp saddr ether {mac} accept\n\
         {l2_v6_exceptions}\
         {l2_ethertypes}\
         {l2} ether saddr != {mac} drop\n\
         {l2} ether type arp arp saddr ether != {mac} drop\n\
         {l2} ether type arp arp saddr ip != {ip} drop\n\
         {l2} ether type ip ip saddr != {ip} drop\n\
         {l2_v6_pin}\
         {l2} accept\n\
         add element bridge {TABLE_BRIDGE} l2_ingress {{ \"{tap}\" : jump vm_{tag}_l2 }}\n\
         add chain inet {TABLE_INET} vm_{tag}_eg\n\
         flush chain inet {TABLE_INET} vm_{tag}_eg\n\
         {egress_rule}\
         add element inet {TABLE_INET} vm_egress {{ {ip} : jump vm_{tag}_eg }}\n\
         add chain inet {TABLE_INET} vm_{tag}_in\n\
         flush chain inet {TABLE_INET} vm_{tag}_in\n\
         {ingress_rule}\
         {dnat_forward_accept_rules}\
         add element inet {TABLE_INET} vm_ingress {{ {ip} : jump vm_{tag}_in }}\n\
         {v6_map_elements}\
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
fn render_vm_policy_replacement(
    uplink: &str,
    previous: &VmPolicy,
    policy: &VmPolicy,
    internet_enabled: bool,
) -> String {
    debug_assert_eq!(previous.vm_id, policy.vm_id);
    format!(
        "{}{}",
        render_vm_policy_removal(previous.vm_id, previous.ipv4, previous.ipv6),
        render_vm_policy_for_network(uplink, policy, internet_enabled)
    )
}

/// Removes every object [`render_vm_policy_for_network`] created for `vm_id`, and nothing
/// else. Each map element is deleted before the chain it jumps to, so nft
/// never rejects a still-referenced chain.
fn render_vm_policy_removal(vm_id: Uuid, ipv4: Ipv4Addr, ipv6: Option<Ipv6Addr>) -> String {
    let tap = tap_name(vm_id);
    let tag = vm_id.simple();
    // Only a policy that installed v6 elements has any to delete; asking nft
    // to remove an element that was never added fails the whole transaction.
    let v6_elements = match ipv6 {
        Some(ipv6) => format!(
            "delete element inet {TABLE_INET} vm_egress6 {{ {ipv6} }}\n\
             delete element inet {TABLE_INET} vm_ingress6 {{ {ipv6} }}\n"
        ),
        None => String::new(),
    };
    format!(
        "delete element bridge {TABLE_BRIDGE} l2_ingress {{ \"{tap}\" }}\n\
         delete chain bridge {TABLE_BRIDGE} vm_{tag}_l2\n\
         delete element inet {TABLE_INET} vm_egress {{ {ipv4} }}\n\
         {v6_elements}\
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
async fn ensure_iptables_compat(bridges: &[String], egress: &[(String, String)]) {
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
    for (subnet, oif) in egress {
        let already = Command::new("iptables")
            .args([
                "-t",
                "nat",
                "-C",
                "POSTROUTING",
                "-s",
                subnet,
                "-o",
                oif,
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
                    oif,
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
/// available). Also drops host INPUT holes for that bridge.
pub async fn remove_iptables_forward_for_bridge(bridge: &str) {
    for dir in ["-i", "-o"] {
        let _ = Command::new("iptables")
            .args(["-D", "FORWARD", dir, bridge, "-j", "ACCEPT"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
    crate::host_acl::remove_bridge(bridge).await;
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
    use firecrab_helper_protocol::network::{Ipv6AddressMode, MicroNetworkIpv6Spec};

    use super::*;
    use core::assert_matches;

    fn sample_network(id: u128, gateway: &str, prefix: u8) -> MicroNetworkSpec {
        MicroNetworkSpec {
            micro_network_id: Uuid::from_u128(id),
            gateway: gateway.parse().unwrap(),
            prefix,
            internet_enabled: true,
            uplink: None,
            ipv6: None,
        }
    }

    fn dual_stack_network(
        id: u128,
        gateway: &str,
        prefix: u8,
        v6_gateway: &str,
        address_mode: Ipv6AddressMode,
    ) -> MicroNetworkSpec {
        MicroNetworkSpec {
            ipv6: Some(MicroNetworkIpv6Spec {
                gateway: v6_gateway.parse().unwrap(),
                prefix: 64,
                address_mode,
            }),
            ..sample_network(id, gateway, prefix)
        }
    }

    #[test]
    fn a_dual_stack_network_gets_ip6_maps_and_nat66() {
        let network = dual_stack_network(
            0x1234,
            "172.31.0.1",
            24,
            "fd00:1234:5678:9abc::1",
            Ipv6AddressMode::Slaac,
        );
        let ruleset = render_apply_ruleset("eth0", &[network]).unwrap();

        // The v6 policy maps mirror their v4 counterparts.
        assert!(ruleset.contains("map vm_egress6"));
        assert!(ruleset.contains("map vm_ingress6"));
        assert!(ruleset.contains("type ipv6_addr : verdict"));
        assert!(ruleset.contains("ip6 saddr vmap @vm_egress6"));
        assert!(ruleset.contains("ip6 daddr vmap @vm_ingress6"));
        // A ULA prefix is not routable off-host, so it is masqueraded.
        assert!(ruleset.contains(
            "ip6 saddr fd00:1234:5678:9abc::/64 oifname \"eth0\" jump firecrab_postrouting"
        ));
    }

    #[test]
    fn a_global_prefix_egresses_without_nat66() {
        let network = dual_stack_network(
            0x1234,
            "172.31.0.1",
            24,
            "2001:db8:1::1",
            Ipv6AddressMode::Slaac,
        );
        let ruleset = render_apply_ruleset("eth0", &[network]).unwrap();

        // Publicly routable: forwarded with the VM's own source address.
        assert!(!ruleset.contains("ip6 saddr 2001:db8:1::/64 oifname"));
        // The policy path is still wired up — only the translation is absent.
        assert!(ruleset.contains("ip6 saddr vmap @vm_egress6"));
    }

    #[test]
    fn an_ipv4_only_network_renders_no_v6_nat_or_isolation_rules() {
        let ruleset =
            render_apply_ruleset("eth0", &[sample_network(0x1234, "172.31.0.1", 24)]).unwrap();
        assert!(!ruleset.contains("ip6 saddr fd"));
        assert!(!ruleset.contains("ip6 daddr {"));
    }

    #[test]
    fn traffic_routed_between_dual_stack_micro_networks_is_dropped_for_v6_too() {
        let networks = [
            dual_stack_network(
                0x1234,
                "172.31.0.1",
                24,
                "fd00:1::1",
                Ipv6AddressMode::Slaac,
            ),
            dual_stack_network(
                0x5678,
                "172.32.0.1",
                24,
                "fd00:2::1",
                Ipv6AddressMode::Dhcpv6,
            ),
        ];
        let ruleset = render_apply_ruleset("eth0", &networks).unwrap();
        assert!(ruleset.contains("ip6 daddr { fd00:1::/64, fd00:2::/64 } drop"));
    }

    #[test]
    fn a_dual_stack_network_with_the_internet_off_drops_v6_egress_too() {
        let offline = MicroNetworkSpec {
            internet_enabled: false,
            ..dual_stack_network(
                0x1234,
                "172.31.0.1",
                24,
                "fd00:1::1",
                Ipv6AddressMode::Slaac,
            )
        };
        let ruleset = render_apply_ruleset("eth0", &[offline]).unwrap();
        assert!(ruleset.contains("ip6 saddr { fd00:1::/64 } drop"));
        assert!(!ruleset.contains("ip6 saddr fd00:1::/64 oifname"));
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
    fn two_internet_networks_render_distinct_oifname_rules() {
        let eth0_net = MicroNetworkSpec {
            uplink: Some("eth0".to_owned()),
            ..sample_network(0x1234, "172.31.0.1", 24)
        };
        let eth1_net = MicroNetworkSpec {
            uplink: Some("eth1".to_owned()),
            ..sample_network(0x5678, "172.32.0.1", 24)
        };
        let ruleset = render_apply_ruleset("wlan0", &[eth0_net, eth1_net]).unwrap();
        assert!(
            ruleset.contains("ip saddr 172.31.0.0/24 oifname \"eth0\" jump firecrab_postrouting")
        );
        assert!(
            ruleset.contains("ip saddr 172.32.0.0/24 oifname \"eth1\" jump firecrab_postrouting")
        );
        assert!(!ruleset.contains("oifname \"wlan0\" jump firecrab_postrouting"));
        // Shared masquerade chain plus the loopback-hairpin rule.
        assert_eq!(ruleset.matches("masquerade").count(), 2);
        assert_eq!(ruleset.matches("chain firecrab_postrouting").count(), 1);
    }

    #[test]
    fn omitted_uplink_uses_the_default_detect_uplink_value() {
        let auto = sample_network(0x1234, "172.31.0.1", 24);
        let chosen = MicroNetworkSpec {
            uplink: Some("eth1".to_owned()),
            ..sample_network(0x5678, "172.32.0.1", 24)
        };
        let ruleset = render_apply_ruleset("wlan0", &[auto, chosen]).unwrap();
        assert!(
            ruleset.contains("ip saddr 172.31.0.0/24 oifname \"wlan0\" jump firecrab_postrouting")
        );
        assert!(
            ruleset.contains("ip saddr 172.32.0.0/24 oifname \"eth1\" jump firecrab_postrouting")
        );
    }

    #[test]
    fn a_network_with_the_internet_off_is_neither_masqueraded_nor_forwarded() {
        let offline = MicroNetworkSpec {
            internet_enabled: false,
            uplink: Some("eth1".to_owned()),
            ..sample_network(0x1234, "172.31.0.1", 24)
        };
        let online = sample_network(0x5678, "172.32.0.1", 24);
        let ruleset = render_apply_ruleset("eth0", &[offline.clone(), online]).unwrap();

        // No NAT for it: its addresses are never translated, even if a
        // per-network uplink was stored.
        assert!(!ruleset.contains("ip saddr 172.31.0.0/24 oifname"));
        assert!(!ruleset.contains("oifname \"eth1\" jump firecrab_postrouting"));
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
            ..online.clone()
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
        let ruleset = render_apply_ruleset("eth0", std::slice::from_ref(&network)).unwrap();
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
    fn global_ruleset_allows_east_west_within_a_micro_network() {
        let ruleset = render_apply_ruleset("eth0", &[]).unwrap();
        // TAPs in the same MicroNetwork share one Linux bridge, so leaving
        // bridge forwarding at its accept default permits their L2 traffic.
        // Different MicroNetworks use different bridges and their routed
        // traffic is denied by firecrab_egress above.
        assert!(!ruleset.contains("iifname \"fct*\" oifname \"fct*\" drop"));
        assert!(!ruleset.contains("type filter hook forward priority -200"));
    }

    #[test]
    fn global_ruleset_recreates_only_the_two_owned_tables() {
        let ruleset = render_apply_ruleset("eth0", &[]).unwrap();
        assert!(ruleset.contains("add table inet firecrab\ndelete table inet firecrab"));
        assert!(ruleset.contains("add table bridge firecrab_l2\ndelete table bridge firecrab_l2"));
        assert!(!ruleset.contains("flush ruleset"));
    }

    #[test]
    fn malformed_uplink_names_are_rejected_before_touching_nft() {
        for bad in [
            "",
            "eth0\"; flush ruleset #",
            "way-too-long-interface-name",
            "eth0/foo",
            "lo",
            "fct0",
            "mnb0",
        ] {
            let result = render_apply_ruleset(bad, &[]);
            assert_matches!(result, Err(FirewallError::InvalidUplinkName(_)));
        }
        let bad_spec = MicroNetworkSpec {
            uplink: Some("eth0;id".to_owned()),
            ..sample_network(0x1234, "172.31.0.1", 24)
        };
        let result = render_apply_ruleset("eth0", &[bad_spec]);
        assert_matches!(result, Err(FirewallError::InvalidUplinkName(_)));
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
            ipv6: None,
            mac: "02:fc:aa:bb:cc:dd".parse().unwrap(),
            egress,
            allow_host_ssh,
            port_forwards: Vec::new(),
        }
    }

    #[test]
    fn vm_policy_pins_l2_source_to_the_lease_and_blocks_ipv6_vlan() {
        let policy = sample_policy(EgressPolicy::Internet, false);
        let ruleset = render_vm_policy_for_network("eth0", &policy, true);
        let mac = "02:fc:aa:bb:cc:dd";
        // Spoofed source MAC / ARP sender / IPv4 source are all dropped.
        assert!(ruleset.contains(&format!("ether saddr != {mac} drop")));
        assert!(ruleset.contains(&format!("ether type arp arp saddr ether != {mac} drop")));
        assert!(ruleset.contains("ether type arp arp saddr ip != 172.30.0.42 drop"));
        assert!(ruleset.contains("ether type ip ip saddr != 172.30.0.42 drop"));
        // Non-IPv4/ARP ethertypes (IPv6, VLAN) are dropped.
        assert!(ruleset.contains("ether type != { ip, arp } drop"));
    }

    fn dual_stack_policy() -> VmPolicy {
        VmPolicy {
            ipv6: Some("fd00:1::5".parse().unwrap()),
            ..sample_policy(EgressPolicy::Internet, false)
        }
    }

    #[test]
    fn a_dual_stack_vm_policy_pins_its_v6_source_the_way_it_pins_v4() {
        let policy = dual_stack_policy();
        let tag = policy.vm_id.simple();
        let ruleset = render_vm_policy_for_network("eth0", &policy, true);

        // IPv6 frames are no longer dropped wholesale for this VM...
        assert!(ruleset.contains("ether type != { ip, arp, ip6 } drop"));
        // ...but only its own leased address may source them.
        assert!(ruleset.contains("ether type ip6 ip6 saddr != fd00:1::5 drop"));
        // Neighbor discovery, DAD, MLD, and DHCPv6 run from the
        // link-local/unspecified address, which no lease can cover; the MAC
        // pin above still applies. Guest-originated RAs stay dropped.
        assert!(ruleset.contains("ip6 saddr :: icmpv6 type nd-neighbor-solicit accept"));
        assert!(ruleset.contains("ip6 saddr fe80::/10 icmpv6 type"));
        assert!(ruleset.contains("udp dport 547 accept"));
        assert!(!ruleset.contains("nd-router-advert"));
        // L3 verdicts are reachable through the v6 maps.
        assert!(ruleset.contains(&format!(
            "add element inet firecrab vm_egress6 {{ fd00:1::5 : jump vm_{tag}_eg }}"
        )));
        assert!(ruleset.contains(&format!(
            "add element inet firecrab vm_ingress6 {{ fd00:1::5 : jump vm_{tag}_in }}"
        )));
    }

    #[test]
    fn an_ipv4_only_vm_policy_still_drops_every_v6_frame() {
        let ruleset = render_vm_policy_for_network(
            "eth0",
            &sample_policy(EgressPolicy::Internet, false),
            true,
        );
        assert!(ruleset.contains("ether type != { ip, arp } drop"));
        assert!(!ruleset.contains("vm_egress6"));
    }

    #[test]
    fn removing_a_dual_stack_policy_deletes_its_v6_map_elements() {
        let policy = dual_stack_policy();
        let ruleset = render_vm_policy_removal(policy.vm_id, policy.ipv4, policy.ipv6);
        assert!(ruleset.contains("delete element inet firecrab vm_egress6 { fd00:1::5 }"));
        assert!(ruleset.contains("delete element inet firecrab vm_ingress6 { fd00:1::5 }"));
    }

    #[test]
    fn removing_an_ipv4_only_policy_touches_no_v6_map() {
        let policy = sample_policy(EgressPolicy::Internet, false);
        let ruleset = render_vm_policy_removal(policy.vm_id, policy.ipv4, None);
        assert!(!ruleset.contains("vm_egress6"));
        assert!(!ruleset.contains("vm_ingress6"));
    }

    #[test]
    fn vm_policy_allows_only_the_two_dhcp_exceptions() {
        let ruleset = render_vm_policy_for_network(
            "eth0",
            &sample_policy(EgressPolicy::Internet, false),
            true,
        );
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
        let internet = render_vm_policy_for_network(
            "eth0",
            &sample_policy(EgressPolicy::Internet, false),
            true,
        );
        let tag = Uuid::from_u128(0x1234).simple();
        assert!(internet.contains(&format!("add rule inet firecrab vm_{tag}_eg accept")));

        let isolated = render_vm_policy_for_network(
            "eth0",
            &sample_policy(EgressPolicy::Isolated, false),
            true,
        );
        // Isolated leaves the egress chain empty (no accept rule for it).
        assert!(!isolated.contains(&format!("add rule inet firecrab vm_{tag}_eg accept")));
        // But the chain and its dispatch element still exist.
        assert!(isolated.contains(&format!("add chain inet firecrab vm_{tag}_eg")));
        assert!(isolated.contains("add element inet firecrab vm_egress { 172.30.0.42 : jump"));
    }

    #[test]
    fn host_ssh_is_allowed_only_when_requested() {
        let tag = Uuid::from_u128(0x1234).simple();
        let with_ssh = render_vm_policy_for_network(
            "eth0",
            &sample_policy(EgressPolicy::Internet, true),
            true,
        );
        assert!(with_ssh.contains(&format!(
            "add rule inet firecrab vm_{tag}_in tcp dport 22 ct state new,established accept"
        )));

        let without = render_vm_policy_for_network(
            "eth0",
            &sample_policy(EgressPolicy::Internet, false),
            true,
        );
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
        let ruleset = render_vm_policy_for_network("eth0", &policy, true);
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
            "add rule inet firecrab vm_{tag}_dnat_out fib daddr type local tcp dport 8080 dnat ip to 172.30.0.42:80"
        )));
        assert!(ruleset.contains(&format!(
            "add rule inet firecrab vm_{tag}_dnat iifname \"eth0\" udp dport 5353 dnat ip to 172.30.0.42:53"
        )));
        assert!(ruleset.contains(&format!(
            "add rule inet firecrab vm_{tag}_dnat_out fib daddr type local udp dport 5353 dnat ip to 172.30.0.42:53"
        )));
        // Forwarded traffic is only useful once it's actually allowed through
        // the per-VM ingress chain; without this, external clients hitting
        // the forwarded port would be dropped by firecrab_ingress.
        assert!(ruleset.contains(&format!(
            "add rule inet firecrab vm_{tag}_in tcp dport 80 ct status dnat accept"
        )));
        assert!(ruleset.contains(&format!(
            "add rule inet firecrab vm_{tag}_in udp dport 53 ct status dnat accept"
        )));
    }

    #[test]
    fn dnat_output_rule_only_matches_the_hosts_own_addresses() {
        // Without `fib daddr type local`, this output-hook rule would
        // redirect *every* locally generated packet on the host port,
        // including the host's own unrelated outbound connections to a
        // remote host that happens to use the same destination port.
        let mut policy = sample_policy(EgressPolicy::Internet, false);
        policy.port_forwards = vec![firecrab_helper_protocol::network::PortForwardSpec {
            host_port: 8080,
            guest_port: 80,
            protocol: "tcp".to_owned(),
        }];
        let ruleset = render_vm_policy_for_network("eth0", &policy, true);
        assert!(
            ruleset.contains("vm_")
                && ruleset.contains("_dnat_out fib daddr type local tcp dport 8080")
        );
    }

    #[test]
    fn port_forward_dnat_follows_the_vms_network_uplink() {
        let network = MicroNetworkSpec {
            uplink: Some("eth1".to_owned()),
            ..sample_network(0x1234, "172.31.0.1", 24)
        };
        let mut policy = sample_policy(EgressPolicy::Internet, false);
        policy.ipv4 = Ipv4Addr::new(172, 31, 0, 42);
        policy.port_forwards = vec![firecrab_helper_protocol::network::PortForwardSpec {
            host_port: 8080,
            guest_port: 80,
            protocol: "tcp".to_owned(),
        }];
        let base = render_apply_ruleset("eth0", std::slice::from_ref(&network)).unwrap();
        let ruleset = render_reconciled_ruleset(
            &base,
            "eth0",
            std::slice::from_ref(&network),
            std::slice::from_ref(&policy),
        );
        assert!(ruleset.contains("iifname \"eth1\" tcp dport 8080"));
        assert!(!ruleset.contains("iifname \"eth0\" tcp dport 8080"));
    }

    #[test]
    fn port_forwards_are_omitted_when_the_network_internet_is_off() {
        let offline = MicroNetworkSpec {
            internet_enabled: false,
            uplink: Some("eth1".to_owned()),
            ..sample_network(0x1234, "172.31.0.1", 24)
        };
        let mut policy = sample_policy(EgressPolicy::Internet, false);
        policy.ipv4 = Ipv4Addr::new(172, 31, 0, 42);
        policy.port_forwards = vec![firecrab_helper_protocol::network::PortForwardSpec {
            host_port: 8080,
            guest_port: 80,
            protocol: "tcp".to_owned(),
        }];
        let base = render_apply_ruleset("eth0", std::slice::from_ref(&offline)).unwrap();
        let ruleset = render_reconciled_ruleset(
            &base,
            "eth0",
            std::slice::from_ref(&offline),
            std::slice::from_ref(&policy),
        );
        assert!(!ruleset.contains("dnat ip to 172.31.0.42:80"), "{ruleset}");
        assert!(!ruleset.contains("ct status dnat accept"), "{ruleset}");
        assert!(
            ruleset.contains("ip saddr { 172.31.0.0/24 } drop"),
            "{ruleset}"
        );
    }

    #[test]
    fn port_forward_prerouting_dnat_is_scoped_to_the_uplink() {
        let mut policy = sample_policy(EgressPolicy::Internet, false);
        policy.port_forwards = vec![firecrab_helper_protocol::network::PortForwardSpec {
            host_port: 8080,
            guest_port: 80,
            protocol: "tcp".to_owned(),
        }];
        let ruleset = render_vm_policy_for_network("wan0", &policy, true);
        // Prerouting DNAT only matches traffic arriving on the uplink, so
        // routed traffic between VMs (or any other host-forwarded traffic)
        // hitting the same destination port cannot be hijacked to this VM.
        assert!(
            ruleset.contains("vm_") && ruleset.contains("_dnat iifname \"wan0\" tcp dport 8080")
        );
    }

    #[test]
    fn vm_policy_objects_are_namespaced_so_replacing_one_vm_cannot_touch_another() {
        let a = render_vm_policy_for_network(
            "eth0",
            &sample_policy(EgressPolicy::Internet, false),
            true,
        );
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
    fn reconciled_ruleset_keeps_desired_vms_and_drops_orphans() {
        let desired = sample_policy(EgressPolicy::Internet, false);
        let orphan = VmPolicy {
            vm_id: Uuid::from_u128(0x9999),
            ipv4: Ipv4Addr::new(172, 30, 0, 40),
            ..desired.clone()
        };
        let base = render_apply_ruleset("eth0", &[]).unwrap();
        let ruleset = render_reconciled_ruleset(&base, "eth0", &[], std::slice::from_ref(&desired));

        assert!(ruleset.contains("delete table inet firecrab"));
        assert!(ruleset.contains(&format!("vm_{}", desired.vm_id.simple())));
        assert!(ruleset.contains(&desired.ipv4.to_string()));
        assert!(!ruleset.contains(&format!("vm_{}", orphan.vm_id.simple())));
        assert!(!ruleset.contains(&orphan.ipv4.to_string()));
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
        let ruleset = render_vm_policy_replacement("eth0", &previous, &replacement, true);

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
        let ruleset = render_vm_policy_removal(vm, Ipv4Addr::new(172, 30, 0, 42), None);
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

    #[test]
    fn resolved_uplink_uses_the_matching_network_or_falls_back() {
        let auto = sample_network(0x1234, "172.31.0.1", 24);
        let pinned = MicroNetworkSpec {
            uplink: Some("eth1".to_owned()),
            ..sample_network(0x5678, "172.32.0.1", 24)
        };
        let networks = [auto, pinned];
        assert_eq!(
            resolved_uplink("wlan0", &networks, Ipv4Addr::new(172, 31, 0, 42)),
            "wlan0"
        );
        assert_eq!(
            resolved_uplink("wlan0", &networks, Ipv4Addr::new(172, 32, 0, 9)),
            "eth1"
        );
        assert_eq!(
            resolved_uplink("wlan0", &networks, Ipv4Addr::new(10, 0, 0, 1)),
            "wlan0"
        );
    }

    #[tokio::test]
    async fn ensure_firewall_renders_per_vm_uplink_before_nft() {
        let network = MicroNetworkSpec {
            uplink: Some("eth1".to_owned()),
            ..sample_network(0x1234, "172.30.0.1", 24)
        };
        let policy = sample_policy(EgressPolicy::Internet, false);
        let actor = FirewallActor::new();
        let _ = ensure_firewall(&actor, std::slice::from_ref(&network), &[policy]).await;
        assert_eq!(
            actor.state.lock().await.networks[0].uplink.as_deref(),
            Some("eth1")
        );
    }

    #[tokio::test]
    async fn iptables_compat_walks_each_egress_oif() {
        // Best-effort shim: exercise the per-oif loop even when iptables
        // is missing or the process cannot change the host tables.
        ensure_iptables_compat(
            &["mnbtst0".to_owned()],
            &[
                ("172.31.0.0/24".to_owned(), "eth0".to_owned()),
                ("172.32.0.0/24".to_owned(), "eth1".to_owned()),
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn remove_firewall_clears_cached_networks() {
        let actor = FirewallActor::new();
        actor.state.lock().await.networks = vec![sample_network(0x1234, "172.31.0.1", 24)];
        if remove_firewall(&actor).await.is_ok() {
            assert!(actor.state.lock().await.networks.is_empty());
        }
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

        assert!(ensure_firewall(&actor, &[], &[]).await.is_ok());
        assert_eq!(
            actor.state.lock().await.applied_ruleset.as_deref(),
            Some(applied.as_str())
        );
    }

    #[tokio::test]
    async fn applying_an_identical_vm_policy_skips_nft_entirely() {
        let (connection, handle, _) = new_connection().unwrap();
        tokio::spawn(connection);
        let real_uplink = nat::detect_uplink(&handle).await.unwrap();

        let actor = FirewallActor::new();
        let policy = sample_policy(EgressPolicy::Internet, false);
        actor
            .state
            .lock()
            .await
            .applied_vms
            .insert(policy.vm_id, (real_uplink.clone(), policy.clone()));

        // If the identical request spawned nft, this unprivileged unit test
        // would fail on a host without NET_ADMIN. Returning Ok proves an
        // idempotent reapply causes no unnecessary host-side mutation.
        assert!(apply_vm_policy(&actor, policy.clone()).await.is_ok());
        assert_eq!(
            actor.state.lock().await.applied_vms.get(&policy.vm_id),
            Some(&(real_uplink, policy))
        );
    }

    #[tokio::test]
    async fn applying_the_same_policy_under_a_different_uplink_is_not_a_no_op() {
        // `render_vm_policy_for_network` bakes the uplink into the DNAT rules' `iifname`
        // match, so a changed uplink with an otherwise byte-identical
        // `VmPolicy` must still be treated as a real change, not skipped.
        let actor = FirewallActor::new();
        let policy = sample_policy(EgressPolicy::Internet, false);
        actor.state.lock().await.applied_vms.insert(
            policy.vm_id,
            (
                "stale-uplink-that-is-not-the-real-one".to_owned(),
                policy.clone(),
            ),
        );

        // Reaching real `nft`/rtnetlink here (rather than short-circuiting)
        // is exactly what this test wants to prove happens; on a host
        // without NET_ADMIN this call fails for privilege reasons, not
        // because the no-op check wrongly skipped it.
        let result = apply_vm_policy(&actor, policy).await;
        assert!(
            !matches!(result, Ok(())),
            "expected a real nft attempt, not a skipped no-op"
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
