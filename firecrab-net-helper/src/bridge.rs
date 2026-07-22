//! Idempotent creation/repair of the single Firecrab-owned Linux bridge
//! (`fcbr0`) that every VM's TAP device attaches to.

use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr};

use futures_util::TryStreamExt;
use rtnetlink::packet_route::{
    AddressFamily,
    address::AddressAttribute,
    link::LinkMessage,
    route::{RouteAddress, RouteAttribute, RouteMessage},
};
use rtnetlink::{Handle, LinkBridge, LinkUnspec, new_connection};
use thiserror::Error;
use tokio::sync::Mutex;

/// Name of the single Firecrab-owned Linux bridge shared by every VM.
pub const BRIDGE_NAME: &str = "fcbr0";
/// MTU applied to the bridge and its link.
pub const BRIDGE_MTU: u32 = 1500;
/// Bridge's own address on the VPC subnet, also the VMs' default gateway.
pub const BRIDGE_GATEWAY: Ipv4Addr = Ipv4Addr::new(172, 30, 0, 1);
/// Network address of the Firecrab VPC subnet (172.30.0.0/24).
const BRIDGE_NETWORK: Ipv4Addr = Ipv4Addr::new(172, 30, 0, 0);
/// CIDR prefix length of the Firecrab VPC subnet.
const BRIDGE_PREFIX: u8 = 24;

/// Failure modes for [`ensure_bridge`].
#[derive(Debug, Error)]
pub enum BridgeError {
    /// Couldn't open the rtnetlink socket.
    #[error("failed to open rtnetlink connection")]
    Connection(#[source] io::Error),
    /// An rtnetlink request failed.
    #[error("rtnetlink operation failed")]
    Netlink(#[source] rtnetlink::Error),
    /// A pre-existing host address overlaps the Firecrab subnet.
    #[error("Firecrab subnet 172.30.0.0/24 overlaps host address {0}")]
    AddressConflict(Ipv4Addr),
    /// A pre-existing host route overlaps the Firecrab subnet.
    #[error("Firecrab subnet 172.30.0.0/24 overlaps host route {network}/{prefix}")]
    RouteConflict {
        /// The conflicting route's network address.
        network: Ipv4Addr,
        /// The conflicting route's prefix length.
        prefix: u8,
    },
    /// The bridge already has the gateway IP but at a different prefix.
    #[error("bridge gateway {BRIDGE_GATEWAY}/{BRIDGE_PREFIX} has a conflicting prefix")]
    GatewayPrefixConflict,
    /// The bridge vanished between being created and being looked up again.
    #[error("bridge {BRIDGE_NAME} disappeared while it was being configured")]
    MissingAfterCreate,
    /// Writing the per-interface IPv6-disable sysctl failed.
    #[error("failed to disable IPv6 on {BRIDGE_NAME}")]
    Ipv6Disable(#[source] io::Error),
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
pub async fn ensure_bridge(actor: &BridgeActor) -> Result<(), BridgeError> {
    let _guard = actor.lock.lock().await;

    let (connection, handle, _) = new_connection().map_err(BridgeError::Connection)?;
    tokio::spawn(connection);

    let bridge = match find_bridge(&handle).await? {
        Some(link) => {
            assert_subnet_available(&handle, Some(link.header.index)).await?;
            link
        }
        None => {
            assert_subnet_available(&handle, None).await?;
            handle
                .link()
                .add(LinkBridge::new(BRIDGE_NAME).mtu(BRIDGE_MTU).build())
                .execute()
                .await
                .map_err(BridgeError::Netlink)?;
            find_bridge(&handle)
                .await?
                .ok_or(BridgeError::MissingAfterCreate)?
        }
    };

    handle
        .link()
        .change(
            LinkUnspec::new_with_index(bridge.header.index)
                .mtu(BRIDGE_MTU)
                .up()
                .build(),
        )
        .execute()
        .await
        .map_err(BridgeError::Netlink)?;
    disable_ipv6()?;
    ensure_gateway(&handle, bridge.header.index).await
}

/// The bridge is IPv4-only for now. Writing the per-interface sysctl also
/// flushes any IPv6 addresses the kernel already auto-assigned.
fn disable_ipv6() -> Result<(), BridgeError> {
    let path = format!("/proc/sys/net/ipv6/conf/{BRIDGE_NAME}/disable_ipv6");
    match fs::write(&path, "1") {
        Ok(()) => Ok(()),
        // A kernel without IPv6 support has nothing to disable.
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(BridgeError::Ipv6Disable(source)),
    }
}

/// Looks up the Firecrab bridge by name, if it already exists.
async fn find_bridge(handle: &Handle) -> Result<Option<LinkMessage>, BridgeError> {
    let mut links = handle
        .link()
        .get()
        .match_name(BRIDGE_NAME.to_owned())
        .execute();
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
/// the Firecrab subnet.
async fn assert_subnet_available(
    handle: &Handle,
    own_bridge_index: Option<u32>,
) -> Result<(), BridgeError> {
    let mut addresses = handle.address().get().execute();
    while let Some(address) = addresses.try_next().await.map_err(BridgeError::Netlink)? {
        if Some(address.header.index) == own_bridge_index {
            continue;
        }
        if let Some(ipv4) = ipv4_address(&address)
            && subnet_contains(ipv4, BRIDGE_NETWORK, BRIDGE_PREFIX)
        {
            return Err(BridgeError::AddressConflict(ipv4));
        }
    }

    let mut routes = handle.route().get(RouteMessage::default()).execute();
    while let Some(route) = routes.try_next().await.map_err(BridgeError::Netlink)? {
        if route.header.address_family != AddressFamily::Inet {
            continue;
        }
        let prefix = route.header.destination_prefix_length;
        if prefix == 0 || prefix > 32 {
            continue;
        }
        if route_belongs_to_own_bridge(route_output_interface(&route), own_bridge_index) {
            continue;
        }
        if let Some(network) = route_ipv4_destination(&route)
            && cidrs_overlap(network, prefix, BRIDGE_NETWORK, BRIDGE_PREFIX)
        {
            return Err(BridgeError::RouteConflict { network, prefix });
        }
    }
    Ok(())
}

/// Adds [`BRIDGE_GATEWAY`] to the bridge if it isn't already assigned.
async fn ensure_gateway(handle: &Handle, bridge_index: u32) -> Result<(), BridgeError> {
    let mut addresses = handle
        .address()
        .get()
        .set_link_index_filter(bridge_index)
        .execute();
    while let Some(address) = addresses.try_next().await.map_err(BridgeError::Netlink)? {
        if ipv4_address(&address) == Some(BRIDGE_GATEWAY) {
            if address.header.prefix_len == BRIDGE_PREFIX {
                return Ok(());
            }
            return Err(BridgeError::GatewayPrefixConflict);
        }
    }

    handle
        .address()
        .add(bridge_index, IpAddr::V4(BRIDGE_GATEWAY), BRIDGE_PREFIX)
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
}
