use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr};

use futures_util::TryStreamExt;
use rtnetlink::packet_route::{
    AddressFamily,
    address::AddressAttribute,
    link::{InfoKind, LinkAttribute, LinkInfo, LinkMessage},
    route::{RouteAddress, RouteAttribute, RouteMessage},
};
use rtnetlink::{Handle, LinkBridge, LinkUnspec, new_connection};
use thiserror::Error;

pub const BRIDGE_NAME: &str = "fcbr0";
pub const BRIDGE_ALIAS: &str = "firecrab:bridge:v1";
pub const BRIDGE_MTU: u32 = 1500;
pub const BRIDGE_GATEWAY: Ipv4Addr = Ipv4Addr::new(172, 30, 0, 1);
const BRIDGE_NETWORK: Ipv4Addr = Ipv4Addr::new(172, 30, 0, 0);
const BRIDGE_PREFIX: u8 = 24;

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("failed to open rtnetlink connection")]
    Connection(#[source] io::Error),
    #[error("rtnetlink operation failed")]
    Netlink(#[source] rtnetlink::Error),
    #[error("interface {BRIDGE_NAME} exists but is not a Firecrab-owned bridge")]
    NotOwned,
    #[error("Firecrab subnet 172.30.0.0/24 overlaps host address {0}")]
    AddressConflict(Ipv4Addr),
    #[error("Firecrab subnet 172.30.0.0/24 overlaps host route {network}/{prefix}")]
    RouteConflict { network: Ipv4Addr, prefix: u8 },
    #[error("bridge gateway {BRIDGE_GATEWAY}/{BRIDGE_PREFIX} has a conflicting prefix")]
    GatewayPrefixConflict,
    #[error("bridge {BRIDGE_NAME} disappeared while it was being configured")]
    MissingAfterCreate,
    #[error("failed to disable IPv6 on {BRIDGE_NAME}")]
    Ipv6Disable(#[source] io::Error),
}

/// Ensure the single root-owned Firecrab bridge is present and usable.
///
/// This adds only the bridge, its gateway address and link state. It never
/// removes host routes or addresses, and it intentionally does not change the
/// global IPv4 forwarding sysctl.
pub async fn ensure_bridge() -> Result<(), BridgeError> {
    let (connection, handle, _) = new_connection().map_err(BridgeError::Connection)?;
    tokio::spawn(connection);

    let bridge = match find_bridge(&handle).await? {
        Some(link) => {
            validate_owned_bridge(&link)?;
            assert_subnet_available(&handle, Some(link.header.index)).await?;
            link
        }
        None => {
            assert_subnet_available(&handle, None).await?;
            handle
                .link()
                .add(
                    LinkBridge::new(BRIDGE_NAME)
                        .mtu(BRIDGE_MTU)
                        .append_extra_attribute(LinkAttribute::IfAlias(BRIDGE_ALIAS.to_owned()))
                        .build(),
                )
                .execute()
                .await
                .map_err(BridgeError::Netlink)?;
            find_bridge(&handle)
                .await?
                .ok_or(BridgeError::MissingAfterCreate)?
        }
    };

    // A Firecrab-owned bridge is the only link this helper may repair.
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

async fn find_bridge(handle: &Handle) -> Result<Option<LinkMessage>, BridgeError> {
    let mut links = handle
        .link()
        .get()
        .match_name(BRIDGE_NAME.to_owned())
        .execute();
    match links.try_next().await {
        Ok(link) => Ok(link),
        // A get-by-name answers ENODEV when the link does not exist yet.
        Err(rtnetlink::Error::NetlinkError(message))
            if message.raw_code() == -libc::ENODEV =>
        {
            Ok(None)
        }
        Err(error) => Err(BridgeError::Netlink(error)),
    }
}

fn validate_owned_bridge(link: &LinkMessage) -> Result<(), BridgeError> {
    let is_bridge = link.attributes.iter().any(|attribute| {
        matches!(
            attribute,
            LinkAttribute::LinkInfo(info)
                if info.iter().any(|item| matches!(item, LinkInfo::Kind(InfoKind::Bridge)))
        )
    });
    let has_owner_alias = link
        .attributes
        .iter()
        .any(|attribute| matches!(attribute, LinkAttribute::IfAlias(alias) if alias == BRIDGE_ALIAS));

    if is_bridge && has_owner_alias {
        Ok(())
    } else {
        Err(BridgeError::NotOwned)
    }
}

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
        if prefix == 0 || prefix > 32 || route_output_interface(&route) == own_bridge_index {
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
        .add(
            bridge_index,
            IpAddr::V4(BRIDGE_GATEWAY),
            BRIDGE_PREFIX,
        )
        .execute()
        .await
        .map_err(BridgeError::Netlink)
}

fn ipv4_address(address: &rtnetlink::packet_route::address::AddressMessage) -> Option<Ipv4Addr> {
    address.attributes.iter().find_map(|attribute| match attribute {
        AddressAttribute::Address(IpAddr::V4(ipv4))
        | AddressAttribute::Local(IpAddr::V4(ipv4)) => Some(*ipv4),
        _ => None,
    })
}

fn route_output_interface(route: &RouteMessage) -> Option<u32> {
    route.attributes.iter().find_map(|attribute| match attribute {
        RouteAttribute::Oif(index) => Some(*index),
        _ => None,
    })
}

fn route_ipv4_destination(route: &RouteMessage) -> Option<Ipv4Addr> {
    route.attributes.iter().find_map(|attribute| match attribute {
        RouteAttribute::Destination(RouteAddress::Inet(ipv4)) => Some(*ipv4),
        _ => None,
    })
}

fn subnet_contains(address: Ipv4Addr, network: Ipv4Addr, prefix: u8) -> bool {
    ipv4_to_u32(address) & prefix_mask(prefix) == ipv4_to_u32(network) & prefix_mask(prefix)
}

fn cidrs_overlap(a_network: Ipv4Addr, a_prefix: u8, b_network: Ipv4Addr, b_prefix: u8) -> bool {
    let shared_prefix = a_prefix.min(b_prefix);
    ipv4_to_u32(a_network) & prefix_mask(shared_prefix)
        == ipv4_to_u32(b_network) & prefix_mask(shared_prefix)
}

fn ipv4_to_u32(address: Ipv4Addr) -> u32 {
    u32::from_be_bytes(address.octets())
}

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
    fn only_a_tagged_linux_bridge_is_firecrab_owned() {
        let mut bridge = LinkMessage::default();
        bridge.attributes = vec![
            LinkAttribute::LinkInfo(vec![LinkInfo::Kind(InfoKind::Bridge)]),
            LinkAttribute::IfAlias(BRIDGE_ALIAS.to_owned()),
        ];
        assert!(validate_owned_bridge(&bridge).is_ok());

        bridge.attributes.retain(|attribute| !matches!(attribute, LinkAttribute::IfAlias(_)));
        assert!(matches!(validate_owned_bridge(&bridge), Err(BridgeError::NotOwned)));
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
}
