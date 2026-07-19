use std::process::Stdio;

use futures_util::TryStreamExt;
use rtnetlink::packet_route::link::LinkAttribute;
use rtnetlink::packet_route::route::RouteAttribute;
use rtnetlink::packet_route::{AddressFamily, route::RouteMessage};
use rtnetlink::{Handle, new_connection};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::bridge::BRIDGE_NAME;

const TABLE_INET: &str = "firecrab";
const TABLE_BRIDGE: &str = "firecrab_l2";
const BRIDGE_SUBNET: &str = "172.30.0.0/24";

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

/// Serializes apply/remove/reconcile so concurrent callers cannot race two
/// `nft` transactions or act on a stale "already applied" decision (lost
/// update). Remembers the uplink the ruleset was last rendered for, so a
/// repeat call with nothing changed is a no-op instead of unnecessary churn.
#[derive(Debug)]
pub struct FirewallActor {
    applied_for_uplink: Mutex<Option<String>>,
}

impl FirewallActor {
    pub fn new() -> Self {
        Self {
            applied_for_uplink: Mutex::new(None),
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

    let mut applied = actor.applied_for_uplink.lock().await;
    if applied.as_deref() == Some(uplink.as_str()) {
        return Ok(());
    }

    let ruleset = render_apply_ruleset(&uplink)?;
    run_nft(&ruleset).await?;
    *applied = Some(uplink);
    Ok(())
}

/// Explicit uninstall: remove both Firecrab tables. VM stop/delete must
/// never call this — only an explicit teardown of the whole subsystem does.
pub async fn remove_firewall(actor: &FirewallActor) -> Result<(), FirewallError> {
    let mut applied = actor.applied_for_uplink.lock().await;
    run_nft(&render_remove_ruleset()).await?;
    *applied = None;
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

/// Renders the whole desired state for both owned tables as one nft(8)
/// script. `add table` + `flush table` before redeclaring keeps this
/// idempotent without ever touching a table this helper doesn't own.
fn render_apply_ruleset(uplink: &str) -> Result<String, FirewallError> {
    validate_uplink(uplink)?;
    Ok(format!(
        "add table inet {TABLE_INET}\n\
         flush table inet {TABLE_INET}\n\
         table inet {TABLE_INET} {{\n\
         \tchain forward_dispatch {{\n\
         \t\ttype filter hook forward priority filter; policy accept;\n\
         \t\tiifname \"{BRIDGE_NAME}\" jump firecrab_forward\n\
         \t\toifname \"{BRIDGE_NAME}\" jump firecrab_forward\n\
         \t}}\n\
         \tchain firecrab_forward {{\n\
         \t\tct state established,related accept\n\
         \t\tiifname \"{BRIDGE_NAME}\" oifname \"{BRIDGE_NAME}\" accept\n\
         \t\tiifname \"{BRIDGE_NAME}\" accept\n\
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
         \tchain forward_dispatch {{\n\
         \t\ttype filter hook forward priority filter; policy accept;\n\
         \t}}\n\
         }}\n"
    ))
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
    fn ruleset_dispatches_bridge_traffic_from_accept_policy_base_chains() {
        let ruleset = render_apply_ruleset("eth0").unwrap();
        assert!(ruleset.contains("policy accept"));
        assert!(ruleset.contains("iifname \"fcbr0\" jump firecrab_forward"));
        assert!(ruleset.contains("ip saddr 172.30.0.0/24 oifname \"eth0\" jump firecrab_postrouting"));
        assert!(ruleset.contains("masquerade"));
        assert!(ruleset.contains("ct state established,related accept"));
    }

    #[test]
    fn ruleset_is_idempotent_via_add_then_flush() {
        let ruleset = render_apply_ruleset("eth0").unwrap();
        assert!(ruleset.contains("add table inet firecrab\nflush table inet firecrab"));
        assert!(ruleset.contains("add table bridge firecrab_l2\nflush table bridge firecrab_l2"));
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
        *actor.applied_for_uplink.lock().await = Some(real_uplink.clone());

        assert!(ensure_firewall(&actor).await.is_ok());
        assert_eq!(actor.applied_for_uplink.lock().await.as_deref(), Some(real_uplink.as_str()));
    }
}
