//! Stop waiting for responses from a dead VM without changing healthy timeouts.

use crate::firewall::VmPolicy;
use firecrab_helper_protocol::network::MicroNetworkSpec;
use std::io;
use std::net::IpAddr;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// Only root-tracked gateway/guest pairs may select host sockets for removal.
pub fn targets(policy: &VmPolicy, networks: &[MicroNetworkSpec]) -> Vec<(IpAddr, IpAddr)> {
    let Some(network) = networks
        .iter()
        .find(|network| network.contains(policy.ipv4) && network.gateway != policy.ipv4)
    else {
        return Vec::new();
    };
    let mut targets = vec![(network.gateway.into(), policy.ipv4.into())];
    if let (Some(network), Some(address)) = (&network.ipv6, policy.ipv6)
        && network.contains(address)
        && network.gateway != address
    {
        targets.push((network.gateway.into(), address.into()));
    }
    targets
}

/// `ss -K` uses SOCK_DESTROY with the helper's existing CAP_NET_ADMIN.
/// The source filter excludes other host interfaces, even for the same guest IP.
pub async fn close(source: IpAddr, destination: IpAddr) -> io::Result<()> {
    if source.is_ipv4() != destination.is_ipv4() || source == destination {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid VM socket pair",
        ));
    }
    let host = |address| match address {
        IpAddr::V4(address) => address.to_string(),
        IpAddr::V6(address) => format!("[{address}]"),
    };
    let mut command = Command::new("ss");
    command
        .args([
            "-K",
            "-H",
            "-n",
            "-t",
            if source.is_ipv4() { "-4" } else { "-6" },
        ])
        .args([
            "state",
            "connected",
            "src",
            &host(source),
            "dst",
            &host(destination),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(2), command.output())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "VM socket cleanup timed out"))??;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err(io::Error::other(format!(
            "VM socket cleanup failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
