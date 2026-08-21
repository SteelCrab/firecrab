//! NAT/uplink handling: detects the host's own default-route interface and
//! renders the postrouting/masquerade chain that lets VM traffic egress
//! through it. Split out of `firewall.rs`
//! (`public-docs/networking.md`) as an organizational
//! separation only — same `FirewallError` type, and `firewall.rs`'s
//! `render_apply_ruleset` still splices this module's output into the same
//! single atomic `nft -f -` transaction as before.

use std::path::Path;

use firecrab_helper_protocol::MAX_INTERFACE_NAME_LEN;
use futures_util::TryStreamExt;
use rtnetlink::Handle;
use rtnetlink::packet_route::link::LinkAttribute;
use rtnetlink::packet_route::route::RouteAttribute;
use rtnetlink::packet_route::{AddressFamily, route::RouteMessage};

use crate::firewall::FirewallError;

/// Whether `name` is safe to embed unescaped in an nftables ruleset string.
/// `1..=15` bytes, charset `[A-Za-z0-9._:-]` only (no `/`, `;`, `"`, `\`).
/// Rejects empty, loopback, and Firecrab-owned TAP/bridge prefixes.
pub(crate) fn validate_uplink(name: &str) -> Result<(), FirewallError> {
    let bytes = name.as_bytes();
    let charset_ok = (1..=MAX_INTERFACE_NAME_LEN).contains(&bytes.len())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'));
    if charset_ok && name != "lo" && !name.starts_with("fct") && !name.starts_with("mnb") {
        Ok(())
    } else {
        Err(FirewallError::InvalidUplinkName(name.to_owned()))
    }
}

/// Whether `/sys/class/net/<name>` is a host interface. Callers must run
/// [`validate_uplink`] first so this is not a path-join of untrusted text.
pub(crate) fn uplink_exists(name: &str) -> bool {
    Path::new("/sys/class/net").join(name).is_dir()
}

/// Renders the NAT postrouting chain fragment that `firewall.rs`'s
/// `render_apply_ruleset` splices into its single `table inet firecrab`
/// declaration. `egress` is one `(subnet_cidr, oifname)` pair per
/// internet-enabled MicroNetwork — each jumps to the same shared
/// `firecrab_postrouting { masquerade }` chain, but the dispatch `oifname`
/// is that network's own uplink (or the host default-route iface when the
/// spec omitted one). A network with the internet switched off contributes
/// no pair, so its addresses are never translated (the forward-path drop in
/// `firewall.rs` is what actually stops the traffic; this keeps the NAT
/// table from claiming otherwise).
///
/// Also masquerades a 127.0.0.0/8-sourced packet destined to one of
/// `vm_subnets`: a host-local port-forward DNAT (`curl
/// localhost:<host_port>`, rewritten by `firewall.rs`'s `vm_*_dnat_out`
/// chain) leaves the original loopback source untouched, and a VM has no
/// route back to 127.0.0.1, so it must be rewritten to the bridge's own
/// address before the packet leaves the host. Scoped to `ip daddr
/// vm_subnets` specifically — an earlier, unscoped version of this rule
/// masqueraded *every* loopback-sourced packet crossing the postrouting
/// hook, which includes ordinary intra-host loopback traffic (e.g.
/// systemd-resolved's 127.0.0.53 stub resolver), and broke host DNS
/// resolution.
///
/// `egress6` is the IPv6 counterpart of `egress`, but carries only the
/// prefixes that actually need translating: a Unique Local prefix is not
/// routable off-host, so it is masqueraded through the same shared chain,
/// while a global prefix contributes no pair at all and reaches the wire
/// with the VM's own address intact (`public-docs/networking.md`).
pub(crate) fn render_postrouting_chain(
    vm_subnets: &[String],
    egress: &[(String, String)],
    egress6: &[(String, String)],
) -> String {
    let dispatch: String = egress
        .iter()
        .map(|(subnet, oif)| {
            format!("\t\tip saddr {subnet} oifname \"{oif}\" jump firecrab_postrouting\n")
        })
        .chain(egress6.iter().map(|(prefix, oif)| {
            format!("\t\tip6 saddr {prefix} oifname \"{oif}\" jump firecrab_postrouting\n")
        }))
        .collect();
    let loopback_hairpin = if vm_subnets.is_empty() {
        String::new()
    } else {
        format!(
            "\t\tip saddr 127.0.0.0/8 ip daddr {{ {} }} masquerade\n",
            vm_subnets.join(", ")
        )
    };
    format!(
        "\tchain postrouting_dispatch {{\n\
         \t\ttype nat hook postrouting priority srcnat; policy accept;\n\
         {loopback_hairpin}\
         {dispatch}\
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
    use core::assert_matches;

    #[tokio::test]
    async fn detect_uplink_resolves_the_hosts_default_route_interface() {
        let (connection, handle, _) = new_connection().unwrap();
        tokio::spawn(connection);

        // Unprivileged read; requires this host to have an IPv4 default
        // route (true in the dev/CI sandbox this was written against).
        let uplink = detect_uplink(&handle).await.unwrap();
        assert!(!uplink.is_empty());
    }

    #[test]
    fn validate_uplink_rejects_empty_unsafe_and_owned_names() {
        for bad in [
            "",
            "eth0/foo",
            "eth0;id",
            "eth0\"x",
            "eth0\\x",
            "eth0 x",
            "lo",
            "fct0",
            "fct0123456789a",
            "mnb0",
            "mnb0123456789a",
            "way-too-long-interface-name",
            "1234567890123456",
        ] {
            let result = validate_uplink(bad);
            assert_matches!(result, Err(FirewallError::InvalidUplinkName(_)), "{bad:?}");
        }
    }

    #[test]
    fn validate_uplink_accepts_host_style_names() {
        for good in ["eth0", "enp0s3", "wlan0", "eth0.100", "eth0:1", "br-ex"] {
            assert!(validate_uplink(good).is_ok(), "{good}");
        }
    }

    #[test]
    fn uplink_exists_is_false_for_a_missing_interface() {
        assert!(validate_uplink("nosuchiface0").is_ok());
        assert!(!uplink_exists("nosuchiface0"));
    }

    #[test]
    fn uplink_exists_is_true_for_a_real_sysfs_interface() {
        let name = std::fs::read_dir("/sys/class/net")
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .find(|name| validate_uplink(name).is_ok())
            .expect("test host has a usable interface");
        assert!(uplink_exists(&name), "{name}");
    }

    #[test]
    fn postrouting_emits_one_oifname_per_egress_pair() {
        let ruleset = render_postrouting_chain(
            &["172.31.0.0/24".to_owned(), "172.32.0.0/24".to_owned()],
            &[
                ("172.31.0.0/24".to_owned(), "eth0".to_owned()),
                ("172.32.0.0/24".to_owned(), "eth1".to_owned()),
            ],
            &[],
        );
        assert!(
            ruleset.contains("ip saddr 172.31.0.0/24 oifname \"eth0\" jump firecrab_postrouting")
        );
        assert!(
            ruleset.contains("ip saddr 172.32.0.0/24 oifname \"eth1\" jump firecrab_postrouting")
        );
        // Shared masquerade chain, plus the loopback-hairpin rule.
        assert_eq!(ruleset.matches("masquerade").count(), 2);
        assert_eq!(ruleset.matches("chain firecrab_postrouting").count(), 1);
    }

    #[test]
    fn postrouting_masquerades_a_ula_prefix_through_its_own_uplink() {
        let ruleset = render_postrouting_chain(
            &["172.31.0.0/24".to_owned()],
            &[("172.31.0.0/24".to_owned(), "eth0".to_owned())],
            &[("fd00:1234:5678:9abc::/64".to_owned(), "eth0".to_owned())],
        );
        assert!(ruleset.contains(
            "ip6 saddr fd00:1234:5678:9abc::/64 oifname \"eth0\" jump firecrab_postrouting"
        ));
        // One shared masquerade chain still serves both families.
        assert_eq!(ruleset.matches("chain firecrab_postrouting").count(), 1);
    }

    #[test]
    fn postrouting_leaves_a_network_with_no_v6_egress_untranslated() {
        // A GUA network contributes no v6 pair: its addresses are publicly
        // routable and must reach the wire unchanged.
        let ruleset = render_postrouting_chain(
            &["172.31.0.0/24".to_owned()],
            &[("172.31.0.0/24".to_owned(), "eth0".to_owned())],
            &[],
        );
        assert!(!ruleset.contains("ip6 saddr"));
    }

    #[test]
    fn postrouting_omits_dispatch_when_no_network_is_allowed_out() {
        let ruleset = render_postrouting_chain(&["172.31.0.0/24".to_owned()], &[], &[]);
        assert!(!ruleset.contains("oifname"));
        assert!(ruleset.contains("chain firecrab_postrouting"));
        assert!(ruleset.contains("ip saddr 127.0.0.0/8 ip daddr { 172.31.0.0/24 } masquerade"));
    }
}
