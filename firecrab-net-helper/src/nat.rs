//! NAT/uplink handling: detects the host's own default-route interface and
//! renders the postrouting/masquerade chain that lets VM traffic egress
//! through it. Split out of `firewall.rs`
//! (`docs/task-network-configuration-dashboard.md`) as an organizational
//! separation only — same `FirewallError` type, and `firewall.rs`'s
//! `render_apply_ruleset` still splices this module's output into the same
//! single atomic `nft -f -` transaction as before.

use futures_util::TryStreamExt;
use rtnetlink::Handle;
use rtnetlink::packet_route::link::LinkAttribute;
use rtnetlink::packet_route::route::RouteAttribute;
use rtnetlink::packet_route::{AddressFamily, route::RouteMessage};

use crate::firewall::FirewallError;

/// The Firecrab VPC subnet, as an nftables-literal CIDR string.
pub(crate) const BRIDGE_SUBNET: &str = "172.30.0.0/24";

/// Whether `name` is safe to embed unescaped in an nftables ruleset string.
pub(crate) fn validate_uplink(name: &str) -> Result<(), FirewallError> {
    let is_valid = !name.is_empty()
        && name.len() < 16 // IFNAMSIZ
        && name.chars().all(|c| c.is_ascii_graphic() && c != '"' && c != '\\' && c != ';');
    if is_valid {
        Ok(())
    } else {
        Err(FirewallError::InvalidUplinkName(name.to_owned()))
    }
}

/// Renders the NAT postrouting chain fragment that `firewall.rs`'s
/// `render_apply_ruleset` splices into its single `table inet firecrab`
/// declaration.
pub(crate) fn render_postrouting_chain(uplink: &str) -> String {
    format!(
        "\tchain postrouting_dispatch {{\n\
         \t\ttype nat hook postrouting priority srcnat; policy accept;\n\
         \t\tip saddr {BRIDGE_SUBNET} oifname \"{uplink}\" jump firecrab_postrouting\n\
         \t}}\n\
         \tchain firecrab_postrouting {{\n\
         \t\tmasquerade\n\
         \t}}\n"
    )
}

/// Resolves the host's uplink by following its IPv4 default route to an
/// interface name.
pub(crate) async fn detect_uplink(handle: &Handle) -> Result<String, FirewallError> {
    let mut routes = handle.route().get(RouteMessage::default()).execute();
    let mut oif_index = None;
    while let Some(route) = routes.try_next().await.map_err(FirewallError::Netlink)? {
        if route.header.address_family == AddressFamily::Inet
            && route.header.destination_prefix_length == 0
        {
            oif_index = route
                .attributes
                .iter()
                .find_map(|attribute| match attribute {
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

#[cfg(test)]
mod tests {
    use rtnetlink::new_connection;

    use super::*;

    #[tokio::test]
    async fn detect_uplink_resolves_the_hosts_default_route_interface() {
        let (connection, handle, _) = new_connection().unwrap();
        tokio::spawn(connection);

        // Unprivileged read; requires this host to have an IPv4 default
        // route (true in the dev/CI sandbox this was written against).
        let uplink = detect_uplink(&handle).await.unwrap();
        assert!(!uplink.is_empty());
    }
}
