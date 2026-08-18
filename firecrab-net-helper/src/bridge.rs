//! Idempotent creation/repair of the single Firecrab-owned Linux bridge
//! (`fcbr0`) that every VM's TAP device attaches to.

use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr};

use firecrab_helper_protocol::network::{
    MICRO_NETWORK_BRIDGE_PREFIX, TAP_PREFIX, micro_network_bridge_name,
};
use futures_util::TryStreamExt;
use rtnetlink::packet_route::{
    AddressFamily,
    address::AddressAttribute,
    link::{LinkAttribute, LinkMessage},
    route::{RouteAddress, RouteAttribute, RouteMessage},
};
use rtnetlink::{Handle, LinkBridge, LinkUnspec, new_connection};
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;

/// Name of the single Firecrab-owned Linux bridge shared by every VM.
pub const BRIDGE_NAME: &str = "fcbr0";
/// MTU used when the host's uplink MTU cannot be determined.
pub const DEFAULT_BRIDGE_MTU: u32 = 1500;
/// Bridge's own address on the VPC subnet, also the VMs' default gateway.
pub const BRIDGE_GATEWAY: Ipv4Addr = Ipv4Addr::new(172, 30, 0, 1);
/// Network address of the Firecrab VPC subnet (172.30.0.0/24).
const BRIDGE_NETWORK: Ipv4Addr = Ipv4Addr::new(172, 30, 0, 0);
/// CIDR prefix length of the Firecrab VPC subnet.
const BRIDGE_PREFIX: u8 = 24;

/// One bridge's desired name/gateway/subnet — [`ensure_bridge`] is just
/// [`ensure_bridge_for`] called with the fixed default network's own values;
/// [`ensure_micro_network_bridge`] calls it with a MicroNetwork's own.
struct BridgeConfig<'a> {
    name: &'a str,
    gateway: Ipv4Addr,
    network: Ipv4Addr,
    prefix: u8,
    mtu: u32,
}

/// Failure modes for [`ensure_bridge`].
#[derive(Debug, Error)]
pub enum BridgeError {
    /// Couldn't open the rtnetlink socket.
    #[error("failed to open rtnetlink connection")]
    Connection(#[source] io::Error),
    /// An rtnetlink request failed.
    #[error("rtnetlink operation failed")]
    Netlink(#[source] rtnetlink::Error),
    /// A pre-existing host address overlaps the target subnet.
    #[error("subnet {network}/{prefix} overlaps host address {address}")]
    AddressConflict {
        /// The subnet that was being configured.
        network: Ipv4Addr,
        /// Its prefix length.
        prefix: u8,
        /// The conflicting host address.
        address: Ipv4Addr,
    },
    /// A pre-existing host route overlaps the target subnet.
    #[error("subnet {network}/{prefix} overlaps host route {route_network}/{route_prefix}")]
    RouteConflict {
        /// The subnet that was being configured.
        network: Ipv4Addr,
        /// Its prefix length.
        prefix: u8,
        /// The conflicting route's network address.
        route_network: Ipv4Addr,
        /// The conflicting route's prefix length.
        route_prefix: u8,
    },
    /// The bridge already has the gateway IP but at a different prefix.
    #[error("bridge gateway {gateway}/{prefix} has a conflicting prefix")]
    GatewayPrefixConflict {
        /// The gateway address that was already assigned.
        gateway: Ipv4Addr,
        /// The prefix length that was requested instead.
        prefix: u8,
    },
    /// The bridge vanished between being created and being looked up again.
    #[error("bridge {name} disappeared while it was being configured")]
    MissingAfterCreate {
        /// The bridge's name.
        name: String,
    },
    /// Writing the per-interface IPv6-disable sysctl failed.
    #[error("failed to disable IPv6 on {name}")]
    Ipv6Disable {
        /// The bridge's name.
        name: String,
        #[source]
        source: io::Error,
    },
    /// Writing the per-interface route_localnet sysctl failed.
    #[error("failed to enable route_localnet on {name}")]
    RouteLocalnet {
        /// The bridge's name.
        name: String,
        #[source]
        source: io::Error,
    },
}

/// Single-writer guard: `main.rs` spawns one task per accepted connection,
/// so two concurrent `EnsureBridge` requests could otherwise both see "no
/// bridge yet" and both race to create `fcbr0`. Mirrors `FirewallActor` —
/// there's no state worth caching here (unlike the firewall's applied-uplink
/// short-circuit), just mutual exclusion over the whole check-then-act flow.
#[derive(Debug, Default)]
pub struct BridgeActor {
    /// Held for the duration of a whole check-then-act `ensure_bridge` call.
    lock: Mutex<()>,
}

impl BridgeActor {
    /// Creates an actor with no bridge-creation call in flight yet.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Ensure the single root-owned Firecrab bridge is present and usable.
///
/// This adds only the bridge, its gateway address and link state. It never
/// removes host routes or addresses, and it intentionally does not change the
/// global IPv4 forwarding sysctl.
pub async fn ensure_bridge(actor: &BridgeActor, mtu: u32) -> Result<(), BridgeError> {
    ensure_bridge_for(
        actor,
        &BridgeConfig {
            name: BRIDGE_NAME,
            gateway: BRIDGE_GATEWAY,
            network: BRIDGE_NETWORK,
            prefix: BRIDGE_PREFIX,
            mtu,
        },
    )
    .await
}

/// Ensure a MicroNetwork's own bridge is present and usable
/// (`public-docs/networking.md`) — same idempotent create-if-missing
/// behavior as [`ensure_bridge`], just for a bridge named/addressed after a
/// MicroNetwork instead of the fixed default network. The interface name is
/// derived from `micro_network_id`, never taken from a string the caller
/// supplies.
pub async fn ensure_micro_network_bridge(
    actor: &BridgeActor,
    micro_network_id: Uuid,
    gateway: Ipv4Addr,
    prefix: u8,
    mtu: u32,
) -> Result<(), BridgeError> {
    let name = micro_network_bridge_name(micro_network_id);
    let network = Ipv4Addr::from(u32::from(gateway) & prefix_mask(prefix));
    ensure_bridge_for(
        actor,
        &BridgeConfig {
            name: &name,
            gateway,
            network,
            prefix,
            mtu,
        },
    )
    .await
}

/// Removes a MicroNetwork's bridge; a no-op if it's already gone. Mirrors
/// `tap.rs::delete_tap`'s idempotent-delete shape.
pub async fn delete_micro_network_bridge(
    actor: &BridgeActor,
    micro_network_id: Uuid,
) -> Result<(), BridgeError> {
    let _guard = actor.lock.lock().await;
    let name = micro_network_bridge_name(micro_network_id);

    let (connection, handle, _) = new_connection().map_err(BridgeError::Connection)?;
    tokio::spawn(connection);

    let mut links = handle.link().get().match_name(name.clone()).execute();
    let link = match links.try_next().await {
        Ok(link) => link,
        Err(rtnetlink::Error::NetlinkError(message)) if message.raw_code() == -libc::ENODEV => None,
        Err(error) => return Err(BridgeError::Netlink(error)),
    };
    if let Some(link) = link {
        handle
            .link()
            .del(link.header.index)
            .execute()
            .await
            .map_err(BridgeError::Netlink)?;
    }
    crate::firewall::remove_iptables_forward_for_bridge(&name).await;
    Ok(())
}

/// Deletes every Firecrab-owned network interface still on the host: the
/// default bridge, every MicroNetwork bridge, and every VM TAP device.
/// Matched by name prefix rather than by id — this runs from `--teardown`
/// ahead of `install.sh --uninstall` removing the binaries, with no
/// `firecrab-api` database to read ids from. A plain `systemctl stop`
/// leaves all of these in place; only a host reboot clears them otherwise.
pub async fn teardown_all(actor: &BridgeActor) -> Result<(), BridgeError> {
    let _guard = actor.lock.lock().await;
    let (connection, handle, _) = new_connection().map_err(BridgeError::Connection)?;
    tokio::spawn(connection);

    let mut links = handle.link().get().execute();
    let mut owned = Vec::new();
    while let Some(link) = links.try_next().await.map_err(BridgeError::Netlink)? {
        let name = link
            .attributes
            .iter()
            .find_map(|attribute| match attribute {
                LinkAttribute::IfName(name) => Some(name.clone()),
                _ => None,
            });
        if let Some(name) = name
            && is_owned_interface(&name)
        {
            owned.push((link.header.index, name));
        }
    }

    for (index, name) in owned {
        match handle.link().del(index).execute().await {
            Ok(()) => {}
            Err(rtnetlink::Error::NetlinkError(message)) if message.raw_code() == -libc::ENODEV => {
            }
            Err(error) => return Err(BridgeError::Netlink(error)),
        }
        if name == BRIDGE_NAME || name.starts_with(MICRO_NETWORK_BRIDGE_PREFIX) {
            crate::firewall::remove_iptables_forward_for_bridge(&name).await;
        }
    }
    Ok(())
}

/// Whether `name` is an interface this crate creates: the default bridge, a
/// MicroNetwork bridge, or a VM TAP device.
fn is_owned_interface(name: &str) -> bool {
    name == BRIDGE_NAME
        || name.starts_with(MICRO_NETWORK_BRIDGE_PREFIX)
        || name.starts_with(TAP_PREFIX)
}

async fn ensure_bridge_for(
    actor: &BridgeActor,
    config: &BridgeConfig<'_>,
) -> Result<(), BridgeError> {
    let _guard = actor.lock.lock().await;

    let (connection, handle, _) = new_connection().map_err(BridgeError::Connection)?;
    tokio::spawn(connection);

    let bridge = match find_bridge(&handle, config.name).await? {
        Some(link) => {
            assert_subnet_available(&handle, config, Some(link.header.index)).await?;
            link
        }
        None => {
            assert_subnet_available(&handle, config, None).await?;
            handle
                .link()
                .add(LinkBridge::new(config.name).mtu(config.mtu).build())
                .execute()
                .await
                .map_err(BridgeError::Netlink)?;
            find_bridge(&handle, config.name).await?.ok_or_else(|| {
                BridgeError::MissingAfterCreate {
                    name: config.name.to_owned(),
                }
            })?
        }
    };

    handle
        .link()
        .change(
            LinkUnspec::new_with_index(bridge.header.index)
                .mtu(config.mtu)
                .up()
                .build(),
        )
        .execute()
        .await
        .map_err(BridgeError::Netlink)?;
    // Ports otherwise sit in STP listening/learning for the kernel's default
    // forward delay (15s each, ~30s total) before passing traffic, even with
    // STP itself off — a freshly attached TAP's DHCPDISCOVER gets dropped
    // because the guest requests it within its first few boot seconds, well
    // inside that window. No physical loop is possible here (every port is a
    // TAP to a VM we manage), so there's nothing for the delay to guard.
    handle
        .link()
        .change(
            LinkBridge::new(config.name)
                .index(bridge.header.index)
                .forward_delay(0)
                .build(),
        )
        .execute()
        .await
        .map_err(BridgeError::Netlink)?;
    disable_ipv6(config.name)?;
    enable_route_localnet(config.name)?;
    ensure_gateway(&handle, bridge.header.index, config).await
}

/// Enables IPv4 forwarding globally — required for NAT'd VM egress to work
/// at all. A global sysctl, not per-bridge, so this is deliberately not part
/// of [`ensure_bridge`] (its own doc comment disclaims touching this); the
/// caller runs it once at daemon startup instead.
pub fn enable_ip_forward() -> io::Result<()> {
    fs::write("/proc/sys/net/ipv4/ip_forward", "1")
}

/// The bridge is IPv4-only for now. Writing the per-interface sysctl also
/// flushes any IPv6 addresses the kernel already auto-assigned.
fn disable_ipv6(name: &str) -> Result<(), BridgeError> {
    let path = format!("/proc/sys/net/ipv6/conf/{name}/disable_ipv6");
    match fs::write(&path, "1") {
        Ok(()) => Ok(()),
        // A kernel without IPv6 support has nothing to disable.
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(BridgeError::Ipv6Disable {
            name: name.to_owned(),
            source,
        }),
    }
}

/// Lets the kernel route packets sourced from or destined to 127.0.0.0/8
/// through this bridge instead of dropping them as martian. Required for
/// port-forward DNAT to work when the connecting process is on the host
/// itself (e.g. `curl localhost:<host_port>`): after DNAT rewrites the
/// destination to a VM's address, the packet still carries its original
/// 127.0.0.1 source and must route out through the bridge to reach the VM.
fn enable_route_localnet(name: &str) -> Result<(), BridgeError> {
    let path = format!("/proc/sys/net/ipv4/conf/{name}/route_localnet");
    fs::write(&path, "1").map_err(|source| BridgeError::RouteLocalnet {
        name: name.to_owned(),
        source,
    })
}

/// Looks up a bridge by name, if it already exists.
async fn find_bridge(handle: &Handle, name: &str) -> Result<Option<LinkMessage>, BridgeError> {
    let mut links = handle.link().get().match_name(name.to_owned()).execute();
    match links.try_next().await {
        Ok(link) => Ok(link),
        // A get-by-name answers ENODEV when the link does not exist yet.
        Err(rtnetlink::Error::NetlinkError(message)) if message.raw_code() == -libc::ENODEV => {
            Ok(None)
        }
        Err(error) => Err(BridgeError::Netlink(error)),
    }
}

/// Fails if any host address/route outside our own bridge already overlaps
/// `config`'s subnet.
async fn assert_subnet_available(
    handle: &Handle,
    config: &BridgeConfig<'_>,
    own_bridge_index: Option<u32>,
) -> Result<(), BridgeError> {
    let mut addresses = handle.address().get().execute();
    while let Some(address) = addresses.try_next().await.map_err(BridgeError::Netlink)? {
        if Some(address.header.index) == own_bridge_index {
            continue;
        }
        if let Some(ipv4) = ipv4_address(&address)
            && subnet_contains(ipv4, config.network, config.prefix)
        {
            return Err(BridgeError::AddressConflict {
                network: config.network,
                prefix: config.prefix,
                address: ipv4,
            });
        }
    }

    let mut routes = handle.route().get(RouteMessage::default()).execute();
    while let Some(route) = routes.try_next().await.map_err(BridgeError::Netlink)? {
        if route.header.address_family != AddressFamily::Inet {
            continue;
        }
        let route_prefix = route.header.destination_prefix_length;
        if route_prefix == 0 || route_prefix > 32 {
            continue;
        }
        if route_belongs_to_own_bridge(route_output_interface(&route), own_bridge_index) {
            continue;
        }
        if let Some(route_network) = route_ipv4_destination(&route)
            && cidrs_overlap(route_network, route_prefix, config.network, config.prefix)
        {
            return Err(BridgeError::RouteConflict {
                network: config.network,
                prefix: config.prefix,
                route_network,
                route_prefix,
            });
        }
    }
    Ok(())
}

/// Adds `config`'s gateway to the bridge if it isn't already assigned.
async fn ensure_gateway(
    handle: &Handle,
    bridge_index: u32,
    config: &BridgeConfig<'_>,
) -> Result<(), BridgeError> {
    let mut addresses = handle
        .address()
        .get()
        .set_link_index_filter(bridge_index)
        .execute();
    while let Some(address) = addresses.try_next().await.map_err(BridgeError::Netlink)? {
        if ipv4_address(&address) == Some(config.gateway) {
            if address.header.prefix_len == config.prefix {
                return Ok(());
            }
            return Err(BridgeError::GatewayPrefixConflict {
                gateway: config.gateway,
                prefix: config.prefix,
            });
        }
    }

    handle
        .address()
        .add(bridge_index, IpAddr::V4(config.gateway), config.prefix)
        .execute()
        .await
        .map_err(BridgeError::Netlink)
}

/// Extracts the IPv4 address from an rtnetlink address attribute list, if any.
fn ipv4_address(address: &rtnetlink::packet_route::address::AddressMessage) -> Option<Ipv4Addr> {
    address
        .attributes
        .iter()
        .find_map(|attribute| match attribute {
            AddressAttribute::Address(IpAddr::V4(ipv4))
            | AddressAttribute::Local(IpAddr::V4(ipv4)) => Some(*ipv4),
            _ => None,
        })
}

/// Whether a route should be excluded from the conflict scan because it
/// belongs to the bridge we already own. `None == None` would wrongly match
/// a route with no `RTA_OIF` against the "no bridge exists yet" case, so
/// this only excludes on an explicit index match.
fn route_belongs_to_own_bridge(route_oif: Option<u32>, own_bridge_index: Option<u32>) -> bool {
    match own_bridge_index {
        Some(own_index) => route_oif == Some(own_index),
        None => false,
    }
}

/// Extracts a route's outgoing interface index, if it has one.
fn route_output_interface(route: &RouteMessage) -> Option<u32> {
    route
        .attributes
        .iter()
        .find_map(|attribute| match attribute {
            RouteAttribute::Oif(index) => Some(*index),
            _ => None,
        })
}

/// Extracts a route's IPv4 destination network, if it has one.
fn route_ipv4_destination(route: &RouteMessage) -> Option<Ipv4Addr> {
    route
        .attributes
        .iter()
        .find_map(|attribute| match attribute {
            RouteAttribute::Destination(RouteAddress::Inet(ipv4)) => Some(*ipv4),
            _ => None,
        })
}

/// Whether `address` falls within `network/prefix`.
fn subnet_contains(address: Ipv4Addr, network: Ipv4Addr, prefix: u8) -> bool {
    ipv4_to_u32(address) & prefix_mask(prefix) == ipv4_to_u32(network) & prefix_mask(prefix)
}

/// Whether two CIDR ranges share any address.
fn cidrs_overlap(a_network: Ipv4Addr, a_prefix: u8, b_network: Ipv4Addr, b_prefix: u8) -> bool {
    let shared_prefix = a_prefix.min(b_prefix);
    ipv4_to_u32(a_network) & prefix_mask(shared_prefix)
        == ipv4_to_u32(b_network) & prefix_mask(shared_prefix)
}

/// Detects the MTU of the host's default-route uplink interface.
///
/// Walks the IPv4 routing table to find the default route (destination
/// 0.0.0.0/0), then reads that interface's MTU. Falls back to
/// `DEFAULT_BRIDGE_MTU` if the route or its MTU cannot be determined —
/// for example on hosts with no default route, or when rtnetlink fails.
///
/// This is used to set bridge MTU to match the effective path MTU,
/// preventing TLS/PMTUD failures in guest VMs on hosts with overlay
/// networks (e.g. Calico VXLAN, WireGuard) that reduce the usable MTU.
pub async fn detect_uplink_mtu() -> u32 {
    let Ok((connection, handle, _)) = new_connection() else {
        return DEFAULT_BRIDGE_MTU;
    };
    tokio::spawn(connection);

    // Find the default route's outgoing interface index.
    let mut routes = handle.route().get(RouteMessage::default()).execute();
    let mut uplink_index: Option<u32> = None;
    while let Ok(Some(route)) = routes.try_next().await {
        if route.header.address_family != AddressFamily::Inet {
            continue;
        }
        // Default route: destination prefix length 0 (0.0.0.0/0).
        if route.header.destination_prefix_length == 0 {
            uplink_index = route_output_interface(&route);
            break;
        }
    }

    let Some(index) = uplink_index else {
        return DEFAULT_BRIDGE_MTU;
    };

    // Read the MTU of that interface.
    let mut links = handle.link().get().match_index(index).execute();
    if let Ok(Some(link)) = links.try_next().await {
        for attr in &link.attributes {
            if let rtnetlink::packet_route::link::LinkAttribute::Mtu(mtu) = attr {
                return *mtu;
            }
        }
    }

    DEFAULT_BRIDGE_MTU
}

/// Big-endian numeric form of an IPv4 address, for bitmask arithmetic.
fn ipv4_to_u32(address: Ipv4Addr) -> u32 {
    u32::from_be_bytes(address.octets())
}

/// Bitmask covering the top `prefix` bits of a 32-bit address.
fn prefix_mask(prefix: u8) -> u32 {
    match prefix {
        0 => 0,
        32.. => u32::MAX,
        _ => u32::MAX << (32 - prefix),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_host_route_overlap_in_both_directions() {
        assert!(cidrs_overlap(
            Ipv4Addr::new(172, 30, 0, 0),
            24,
            Ipv4Addr::new(172, 30, 0, 0),
            16
        ));
        assert!(cidrs_overlap(
            Ipv4Addr::new(172, 30, 0, 0),
            16,
            Ipv4Addr::new(172, 30, 0, 0),
            24
        ));
        assert!(!cidrs_overlap(
            Ipv4Addr::new(172, 31, 0, 0),
            16,
            Ipv4Addr::new(172, 30, 0, 0),
            24
        ));
    }

    #[test]
    fn subnet_contains_only_the_configured_range() {
        assert!(subnet_contains(
            Ipv4Addr::new(172, 30, 0, 254),
            BRIDGE_NETWORK,
            BRIDGE_PREFIX
        ));
        assert!(!subnet_contains(
            Ipv4Addr::new(172, 30, 1, 1),
            BRIDGE_NETWORK,
            BRIDGE_PREFIX
        ));
    }

    #[test]
    fn routes_without_an_oif_stay_eligible_when_no_bridge_exists_yet() {
        // The bug: None == None used to make this true, hiding a real
        // conflicting route from the scan during first-time bridge creation.
        assert!(!route_belongs_to_own_bridge(None, None));
    }

    #[test]
    fn routes_without_an_oif_stay_eligible_even_once_a_bridge_exists() {
        assert!(!route_belongs_to_own_bridge(None, Some(7)));
    }

    #[test]
    fn a_route_on_a_different_interface_is_not_excluded() {
        assert!(!route_belongs_to_own_bridge(Some(3), Some(7)));
    }

    #[test]
    fn a_route_on_the_owned_bridge_interface_is_excluded() {
        assert!(route_belongs_to_own_bridge(Some(7), Some(7)));
    }

    #[test]
    fn owned_interface_names_are_recognized_by_prefix() {
        assert!(is_owned_interface(BRIDGE_NAME));
        assert!(is_owned_interface("mnb1a2b3c4d5e6f7"));
        assert!(is_owned_interface("fct1a2b3c4d5e6f7"));
        assert!(!is_owned_interface("eth0"));
        assert!(!is_owned_interface("lo"));
        assert!(!is_owned_interface("docker0"));
    }

    #[tokio::test]
    async fn teardown_all_is_a_no_op_when_nothing_firecrab_owns_is_present() {
        // Read-only rtnetlink listing needs no special privilege; on a host
        // with no fcbr0/mnb*/fct* interfaces, nothing is ever deleted, so
        // this never reaches the point of needing CAP_NET_ADMIN (same
        // reasoning as tap.rs's deleting_a_tap_that_was_never_created_is_a_no_op).
        let actor = BridgeActor::new();
        assert!(teardown_all(&actor).await.is_ok());
    }
}
