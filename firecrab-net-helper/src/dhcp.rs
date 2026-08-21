//! DHCP for guest VMs: a `dnsmasq` child process bound only to the Firecrab
//! bridge (`fcbr0`), handing out the exact IP each VM's IPAM lease already
//! reserved (MAC-keyed static reservations, no dynamic pool) and forwarding
//! DNS queries to the host's own resolver. Reservations are rewritten as one
//! full snapshot per `sync_dhcp_leases` call — write, validate, atomically
//! swap, reload — never edited in place.

use std::io;
use std::net::Ipv4Addr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use firecrab_helper_protocol::network::{
    DhcpLeaseEntry, Ipv6AddressMode, MacAddr, MicroNetworkSpec, guest_hostname,
};
use thiserror::Error;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

/// Where the live host-reservation file lives; `sync_dhcp_leases` only ever
/// replaces it via an atomic rename, never edits it in place.
const HOSTS_FILE: &str = "/run/firecrab/dnsmasq-hosts.conf";
/// PID file dnsmasq itself maintains, used to signal it for a reload.
const PID_FILE: &str = "/run/firecrab/dnsmasq.pid";
/// dnsmasq's own DHCP lease database — set explicitly (rather than letting
/// it default to the OS-wide `/var/lib/misc/dnsmasq.leases`) so
/// `release_stale_leases` can read it back to find stale leases, and so it
/// lives in our own runtime dir regardless of what user dnsmasq ends up
/// running as.
const LEASE_FILE: &str = "/run/firecrab/dnsmasq.leases";
/// dnsmasq starts as root but normally drops to an unprivileged account
/// before it handles SIGHUP.  The reservation snapshot has to remain
/// readable by that account when a later sync atomically replaces it.
/// It only contains MAC/IP/hostname mappings, not credentials.
const DNSMASQ_READABLE_MODE: u32 = 0o644;

/// Failure modes for syncing DHCP reservations or (re)starting dnsmasq.
#[derive(Debug, Error)]
pub enum DhcpError {
    /// Couldn't write the candidate hosts file.
    #[error("failed to write DHCP hosts file {path}")]
    Write {
        /// The path that couldn't be written.
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Couldn't rename the validated candidate onto the live path.
    #[error("failed to swap in the new DHCP hosts file at {path}")]
    Swap {
        /// The live path the candidate couldn't be renamed onto.
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Couldn't spawn `dnsmasq` (or, during validation, itself).
    #[error("failed to spawn dnsmasq")]
    Spawn(#[source] io::Error),
    /// `dnsmasq --test` rejected the generated config.
    #[error("dnsmasq rejected the generated config: {stderr}")]
    ConfigInvalid {
        /// dnsmasq's stderr output.
        stderr: String,
    },
    /// Couldn't signal the running dnsmasq process to reload.
    #[error("failed to reload the running dnsmasq process")]
    Reload(#[source] io::Error),
    /// Couldn't inspect the supervised dnsmasq process.
    #[error("failed to inspect the running dnsmasq process")]
    Inspect(#[source] io::Error),
}

/// Single-writer actor: every reservation-file rewrite goes through one
/// mutex, and the last-applied revision it caches is what lets a
/// duplicate/out-of-order snapshot be recognized as stale and ignored (see
/// `NetworkRequest::SyncDhcpLeases`'s doc comment).
#[derive(Debug, Default)]
pub struct DhcpActor {
    state: Mutex<DhcpState>,
}

#[derive(Debug, Default)]
struct DhcpState {
    /// The supervised dnsmasq child, once first spawned.
    child: Option<Child>,
    /// Lease generation of the snapshot currently applied, if any.
    applied_revision: Option<u64>,
    /// The MicroNetwork set the running dnsmasq's base config was rendered
    /// for. dnsmasq only reads its main config file at startup — SIGHUP
    /// re-reads the `dhcp-hostsfile` and nothing else — so a network being
    /// created or deleted has to restart the process, not just reload it.
    served_networks: Option<Vec<MicroNetworkSpec>>,
}

impl DhcpActor {
    /// Creates an actor with no dnsmasq process running yet.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Renders `leases` into dnsmasq's `dhcp-host=` reservation file format,
/// tagging each with its deterministic guest hostname (see
/// [`guest_hostname`]) so the guest picks it up from DHCP option 12.
/// Drops a v6 reservation unless that lease sits in a DHCPv6 network.
/// SLAAC guests derive the address from the RA; putting it in
/// `dhcp-hostsfile` would describe an assignment dnsmasq never makes.
fn dhcpv6_reservations_only(
    leases: &[DhcpLeaseEntry],
    micro_networks: &[MicroNetworkSpec],
) -> Vec<DhcpLeaseEntry> {
    leases
        .iter()
        .copied()
        .map(|mut lease| {
            let dhcpv6 = micro_networks
                .iter()
                .find(|network| network.contains(lease.ipv4))
                .and_then(|network| network.ipv6.as_ref())
                .is_some_and(|ipv6| ipv6.address_mode == Ipv6AddressMode::Dhcpv6);
            if !dhcpv6 {
                lease.ipv6 = None;
            }
            lease
        })
        .collect()
}

fn render_hosts_file(leases: &[DhcpLeaseEntry]) -> String {
    let mut rendered = String::new();
    for lease in leases {
        // No `dhcp-host=` prefix: unlike a plain `--conf-file`, dnsmasq's
        // `--dhcp-hostsfile` expects the bare `mac,ip,hostname` triplet per
        // line — the prefix (valid in a real conf file) makes it try to
        // parse the literal text "dhcp-host" as the leading MAC/hex field
        // and reject the whole file with "bad hex constant", silently
        // dropping every reservation.
        // A dual-stack reservation carries the v6 address in the brackets
        // dnsmasq uses to tell the two families apart on one line; a
        // v4-only lease renders exactly the line it always did.
        let ipv6 = match lease.ipv6 {
            Some(ipv6) => format!("[{ipv6}],"),
            None => String::new(),
        };
        rendered.push_str(&format!(
            "{},{},{}{}\n",
            lease.mac,
            lease.ipv4,
            ipv6,
            guest_hostname(lease.vm_id)
        ));
    }
    rendered
}

/// Force-releases dnsmasq's active lease for any `(ip, mac)` pair its own
/// lease database (`active_leases`) has that `current` doesn't — i.e. an IP
/// whose active lease belongs to a MAC that no longer holds the static
/// reservation for it (typically: freed by a deleted VM and immediately
/// handed to a new one, since this project's IPAM reuses addresses right
/// away). Needed because the static `dhcp-hostsfile` reservations this
/// module manages only take effect for *new* DHCP negotiations — reloading
/// a changed reservation via SIGHUP does not invalidate an already-active
/// lease. Reading the lease file back (rather than diffing against
/// whatever snapshot this process last applied) also catches leases still
/// active from a *previous* net-helper/dnsmasq lifetime — the lease
/// database is a file on disk, read back in on every dnsmasq spawn,
/// unaffected by our own process restarting. Without this, a stale lease
/// blocks the new MAC ("no address available") until the old one's full
/// `dhcp-range` lease time (an hour) naturally expires.
async fn release_stale_leases(current: &[DhcpLeaseEntry], micro_networks: &[MicroNetworkSpec]) {
    for (ip, mac) in stale_leases(active_leases().await, current) {
        release_lease(ip, mac, serving_interface(ip, micro_networks)).await;
    }
}

/// Which bridge serves `ip` — the MicroNetwork whose subnet contains it.
/// Falls back to empty when none match (release becomes a best-effort no-op).
fn serving_interface(ip: Ipv4Addr, micro_networks: &[MicroNetworkSpec]) -> String {
    micro_networks
        .iter()
        .find(|network| {
            let mask = u32::MAX
                .checked_shl(32 - u32::from(network.prefix))
                .unwrap_or(0);
            u32::from(ip) & mask == u32::from(network.network_address()) & mask
        })
        .map(MicroNetworkSpec::bridge_name)
        .unwrap_or_default()
}

/// Reads dnsmasq's own lease database, returning every `(ip, mac)` pair it
/// currently considers actively leased. A missing or unreadable file is
/// treated as "no active leases" — the normal case right after a fresh
/// dnsmasq spawn that hasn't leased anything yet.
async fn active_leases() -> Vec<(Ipv4Addr, MacAddr)> {
    let Ok(text) = tokio::fs::read_to_string(LEASE_FILE).await else {
        return Vec::new();
    };
    parse_lease_file(&text)
}

/// Parses dnsmasq's own lease database format — split out from
/// [`active_leases`] so the parsing itself is testable without touching the
/// real lease file. Format is `<expiry> <mac> <ip> <hostname-or-*>
/// <client-id-or-*>`, one lease per line; a line that doesn't match (blank,
/// truncated, or holding an unparseable field) is skipped rather than
/// failing the whole read.
fn parse_lease_file(text: &str) -> Vec<(Ipv4Addr, MacAddr)> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            fields.next()?; // expiry epoch, unused
            let mac: MacAddr = fields.next()?.parse().ok()?;
            let ip: Ipv4Addr = fields.next()?.parse().ok()?;
            Some((ip, mac))
        })
        .collect()
}

/// Active `(ip, mac)` pairs not matched by the same pair in `current` —
/// split out from [`release_stale_leases`] so the diff itself is testable
/// without touching the real lease file or running `dhcp_release`.
fn stale_leases(
    active: Vec<(Ipv4Addr, MacAddr)>,
    current: &[DhcpLeaseEntry],
) -> Vec<(Ipv4Addr, MacAddr)> {
    active
        .into_iter()
        .filter(|(ip, mac)| {
            !current
                .iter()
                .any(|lease| lease.ipv4 == *ip && lease.mac == *mac)
        })
        .collect()
}

/// Sends a DHCPRELEASE for `ip`/`mac` via the `dhcp_release` helper
/// (`dnsmasq-utils`) so dnsmasq drops the lease immediately instead of
/// waiting out its lease time. Best-effort: a failure here just means the
/// old lease lingers until it expires, not a fatal sync error.
async fn release_lease(ip: Ipv4Addr, mac: MacAddr, interface: String) {
    let result = Command::new("dhcp_release")
        .arg(&interface)
        .arg(ip.to_string())
        .arg(mac.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    match result {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!("[ERROR] dhcp_release {ip} {mac} exited with {status}"),
        Err(error) => eprintln!("[ERROR] failed to run dhcp_release {ip} {mac}: {error}"),
    }
}

/// The fixed (lease-independent) part of dnsmasq's config: bound only to
/// the Firecrab bridge, static-reservations-only (no dynamic pool — an
/// unreserved MAC gets nothing), DNS forwarding left at dnsmasq's default
/// (reads the host's own `/etc/resolv.conf`). `bind-dynamic` rather than
/// `bind-interfaces`: on a host with several other interfaces (docker0,
/// virbr0, the uplink...), `bind-interfaces`' wildcard socket can't reliably
/// tell which interface a broadcast-flag-0 DHCPDISCOVER arrived on, so the
/// unicast DHCPOFFER a client without a broadcast flag expects never goes
/// out — `bind-dynamic` binds directly to `fcbr0` instead.
fn render_base_config(hosts_file: &Path, micro_networks: &[MicroNetworkSpec]) -> String {
    // One `interface=`/`dhcp-range=` pair per served MicroNetwork. dnsmasq
    // picks the range matching the interface a request arrived on, so a VM
    // on a given bridge is offered that network's reservation only.
    // Zero networks: no interface= lines (dnsmasq will not serve Firecrab).
    let served: String = micro_networks
        .iter()
        .map(|network| {
            // A dual-stack network gets a second range on the same
            // interface: addresses come from the guest itself under SLAAC
            // (`ra-stateless` advertises the prefix and hands out
            // configuration only) or from this file's reservations under
            // DHCPv6 (`static`, exactly like the v4 range above).
            let range6 = match &network.ipv6 {
                Some(ipv6) => {
                    let mode = match ipv6.address_mode {
                        Ipv6AddressMode::Slaac => "ra-stateless",
                        Ipv6AddressMode::Dhcpv6 => "static",
                    };
                    format!(
                        "dhcp-range={},{mode},{}\n",
                        ipv6.network_address(),
                        ipv6.prefix
                    )
                }
                None => String::new(),
            };
            format!(
                "interface={}\ndhcp-range={},static\n{range6}",
                network.bridge_name(),
                network.network_address()
            )
        })
        .collect();
    // Router advertisements are what makes a guest configure a v6 address
    // and a default route at all; an IPv4-only host never enables them.
    let enable_ra = if micro_networks.iter().any(|network| network.ipv6.is_some()) {
        "enable-ra\n"
    } else {
        ""
    };
    format!(
        "{served}\
         {enable_ra}\
         bind-dynamic\n\
         dhcp-hostsfile={}\n\
         dhcp-leasefile={LEASE_FILE}\n\
         pid-file={PID_FILE}\n\
         log-dhcp\n",
        hosts_file.display()
    )
}

/// Applies `leases` as the complete set of DHCP reservations, starting
/// dnsmasq if it isn't already running or reloading it otherwise. A
/// `revision` at or behind the last one actually applied is a stale/
/// out-of-order snapshot and is silently ignored rather than clobbering
/// newer state.
pub async fn sync_dhcp_leases(
    actor: &DhcpActor,
    revision: u64,
    leases: &[DhcpLeaseEntry],
    micro_networks: &[MicroNetworkSpec],
) -> Result<(), DhcpError> {
    let mut state = actor.state.lock().await;
    let networks_changed = state.served_networks.as_deref() != Some(micro_networks);
    let dnsmasq_running = owned_child_running(&mut state)?
        || (state.child.is_none() && running_orphan_pid().await.is_some());
    if can_skip_snapshot(
        state.applied_revision,
        revision,
        networks_changed,
        dnsmasq_running,
    ) {
        return Ok(());
    }

    let hosts_path = Path::new(HOSTS_FILE);
    let candidate_path = hosts_path.with_extension("tmp");
    let leases = dhcpv6_reservations_only(leases, micro_networks);
    write_atomic_candidate(&candidate_path, &render_hosts_file(&leases)).await?;
    validate(&candidate_path, micro_networks).await?;
    tokio::fs::rename(&candidate_path, hosts_path)
        .await
        .map_err(|source| DhcpError::Swap {
            path: hosts_path.to_owned(),
            source,
        })?;

    if networks_changed {
        // Restart rather than reload, and tear down an orphan from a prior
        // net-helper lifetime too: whatever is running was configured for a
        // different set of interfaces, so reusing it would leave the new
        // network's bridge unserved (or keep answering on a deleted one).
        stop_running_dnsmasq(&mut state).await;
        state.child = Some(spawn_dnsmasq(hosts_path, micro_networks).await?);
        state.served_networks = Some(micro_networks.to_vec());
    } else {
        match state.child.as_mut() {
            Some(child) => reload(child)?,
            // No Child handle in *this* process's memory doesn't mean no
            // dnsmasq is running — a prior net-helper instance (before a
            // restart) may have spawned one that's still alive, orphaned but
            // otherwise healthy, tracked only by its own pid file. Reusing it
            // is what makes a restart not silently strand every VM started
            // afterwards without a working DHCP reload target.
            None => match running_orphan_pid().await {
                Some(pid) => reload_pid(pid)?,
                None => state.child = Some(spawn_dnsmasq(hosts_path, micro_networks).await?),
            },
        }
    }

    release_stale_leases(&leases, micro_networks).await;
    state.applied_revision = Some(revision);
    Ok(())
}

/// Whether this snapshot can be skipped as stale/duplicate. A changed
/// network set never counts as stale, however old its revision: the set of
/// interfaces dnsmasq serves is not part of the lease snapshot, so a
/// MicroNetwork created before it holds any VM bumps no lease revision at
/// all and would otherwise never reach dnsmasq's config.
fn is_stale_snapshot(applied_revision: Option<u64>, revision: u64, networks_changed: bool) -> bool {
    !networks_changed && applied_revision.is_some_and(|applied| applied >= revision)
}

/// A duplicate lease snapshot is safe to skip only while the process serving
/// it is still alive. VM starts resend the same revision, so this health gate
/// is also how a crashed dnsmasq is recovered without operator intervention.
fn can_skip_snapshot(
    applied_revision: Option<u64>,
    revision: u64,
    networks_changed: bool,
    dnsmasq_running: bool,
) -> bool {
    dnsmasq_running && is_stale_snapshot(applied_revision, revision, networks_changed)
}

/// Reaps an exited supervised child and reports whether it is still running.
/// Merely retaining a [`Child`] handle is not a liveness check: an exited,
/// unreaped child still has a PID and accepts `kill(pid, 0)` as a zombie.
fn owned_child_running(state: &mut DhcpState) -> Result<bool, DhcpError> {
    let Some(child) = state.child.as_mut() else {
        return Ok(false);
    };
    match child.try_wait().map_err(DhcpError::Inspect)? {
        None => Ok(true),
        Some(status) => {
            eprintln!("[WARN] dnsmasq exited with {status}; restarting it on this DHCP sync");
            state.child = None;
            Ok(false)
        }
    }
}

/// Terminates whatever dnsmasq is currently running — this process's own
/// child if it has one, otherwise an orphan left by a previous net-helper
/// lifetime (found via its pid file). Best-effort: if nothing is running, or
/// the signal fails because it already exited, there is nothing to clean up
/// and the caller can go straight to spawning a replacement.
async fn stop_running_dnsmasq(state: &mut DhcpState) {
    if let Some(mut child) = state.child.take() {
        let _ = child.kill().await;
        return;
    }
    if let Some(pid) = running_orphan_pid().await {
        // SAFETY: sending a signal is memory-safe; a stale/reused pid only
        // risks misdelivering the signal, not memory unsafety.
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    }
}

/// Reads dnsmasq's own pid file and returns its pid if that process is
/// still alive and is not a zombie. `kill(pid, 0)` alone is insufficient:
/// Linux reports success for an unreaped zombie, which cannot serve DHCP or
/// react to SIGHUP.
async fn running_orphan_pid() -> Option<u32> {
    let text = tokio::fs::read_to_string(PID_FILE).await.ok()?;
    let pid: u32 = text.trim().parse().ok()?;
    // SAFETY: signal 0 sends nothing; it only probes whether `pid` exists
    // and is signalable, per kill(2).
    if unsafe { libc::kill(pid as i32, 0) } != 0 {
        return None;
    }
    let stat = tokio::fs::read_to_string(format!("/proc/{pid}/stat"))
        .await
        .ok()?;
    (proc_state(&stat) != Some('Z')).then_some(pid)
}

/// Extracts the one-character process state after `/proc/<pid>/stat`'s
/// parenthesized command name. Use the last delimiter because a command name
/// itself may contain `)` characters.
fn proc_state(stat: &str) -> Option<char> {
    stat.rsplit_once(") ")?.1.chars().next()
}

/// Tells `pid` (not necessarily a process this instance spawned) to
/// re-read its hosts file.
fn reload_pid(pid: u32) -> Result<(), DhcpError> {
    // SAFETY: as in `reload` — sending a signal is memory-safe.
    let result = unsafe { libc::kill(pid as i32, libc::SIGHUP) };
    if result < 0 {
        return Err(DhcpError::Reload(io::Error::last_os_error()));
    }
    Ok(())
}
/// Writes `content` to `path` and fsyncs it before returning, so a crash
/// right after this call can never leave a half-written candidate file.
async fn write_atomic_candidate(path: &Path, content: &str) -> Result<(), DhcpError> {
    // Doesn't rely on the net-helper socket happening to share the same
    // parent directory (today `/run/firecrab/` for both, but that's a
    // coincidence of the default paths, not something this module should
    // depend on) — ensured directly instead.
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|source| DhcpError::Write {
                path: path.to_owned(),
                source,
            })?;
    }
    let mut file = File::create(path)
        .await
        .map_err(|source| DhcpError::Write {
            path: path.to_owned(),
            source,
        })?;
    // `dnsmasq` initially opens its config as root, then drops privileges.
    // A later SIGHUP reopens `dhcp-hostsfile` as that unprivileged account,
    // so relying on the helper's umask (which can produce 0600) would leave
    // a newly atomically-renamed reservation snapshot unreadable and make
    // every newly allocated IP appear unavailable.
    file.set_permissions(std::fs::Permissions::from_mode(DNSMASQ_READABLE_MODE))
        .await
        .map_err(|source| DhcpError::Write {
            path: path.to_owned(),
            source,
        })?;
    file.write_all(content.as_bytes())
        .await
        .map_err(|source| DhcpError::Write {
            path: path.to_owned(),
            source,
        })?;
    file.sync_all().await.map_err(|source| DhcpError::Write {
        path: path.to_owned(),
        source,
    })
}

/// Validates the base config (interface/range/hostsfile-path directives)
/// via `dnsmasq --test`, without starting a real dnsmasq. A rejected
/// candidate leaves the live hosts file untouched, since the caller only
/// renames it in on success. Note `--test` does not deeply validate the
/// *content* of the referenced hosts file — only its own directives — but
/// that content only ever comes from already-typed `MacAddr`/`Ipv4Addr`
/// values (see [`render_hosts_file`]), so there is no path that could hand
/// it malformed lines to catch in the first place.
async fn validate(
    candidate_hosts_path: &Path,
    micro_networks: &[MicroNetworkSpec],
) -> Result<(), DhcpError> {
    let config = render_base_config(candidate_hosts_path, micro_networks);
    let config_path = candidate_hosts_path.with_extension("test-conf");
    write_atomic_candidate(&config_path, &config).await?;

    // dnsmasq's getopt parsing rejects `--conf-file <path>` as two argv
    // entries ("junk found in command line") — it must be one `--conf-file=`
    // argument.
    let output = Command::new("dnsmasq")
        .arg("--test")
        .arg(format!("--conf-file={}", config_path.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(DhcpError::Spawn)?;
    let _ = tokio::fs::remove_file(&config_path).await;

    if output.status.success() {
        Ok(())
    } else {
        Err(DhcpError::ConfigInvalid {
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Where the base config (bridge/range/hostsfile directives) is written —
/// must differ from `hosts_path` itself. `hosts_path` already ends in
/// `.conf`, so deriving this via `.with_extension("conf")` would be a no-op
/// and collide the two files into one. Every `sync_dhcp_leases` call then
/// overwrites that shared path with nothing but bare lease lines, wiping out
/// the base config's own `interface=`/`dhcp-range=`/`dhcp-hostsfile=`
/// directives — and dnsmasq's SIGHUP-triggered hostsfile reload rejects the
/// `dhcp-host=`-prefixed lines it left behind ("bad hex constant"), silently
/// breaking every reservation for the process's whole lifetime.
fn base_config_path(hosts_path: &Path) -> PathBuf {
    hosts_path.with_file_name("dnsmasq.conf")
}

/// Starts dnsmasq bound to the live hosts file, in the foreground so this
/// process supervises it directly (matching how Firecracker's own child
/// processes are supervised, rather than a separately-managed systemd
/// unit — every privileged host process this project runs is owned by
/// `firecrab-net-helper` alone).
async fn spawn_dnsmasq(
    hosts_path: &Path,
    micro_networks: &[MicroNetworkSpec],
) -> Result<Child, DhcpError> {
    let config_path = base_config_path(hosts_path);
    write_atomic_candidate(
        &config_path,
        &render_base_config(hosts_path, micro_networks),
    )
    .await?;

    Command::new("dnsmasq")
        .arg("--keep-in-foreground")
        .arg(format!("--conf-file={}", config_path.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(DhcpError::Spawn)
}

/// Tells a running dnsmasq to re-read its hosts file.
fn reload(child: &mut Child) -> Result<(), DhcpError> {
    let Some(pid) = child.id() else {
        return Err(DhcpError::Reload(io::Error::new(
            io::ErrorKind::NotFound,
            "dnsmasq has already exited",
        )));
    };
    // SAFETY: sending a signal is memory-safe; a stale/reused pid only
    // risks misdelivering the signal, not memory unsafety.
    let result = unsafe { libc::kill(pid as i32, libc::SIGHUP) };
    if result < 0 {
        return Err(DhcpError::Reload(io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use firecrab_helper_protocol::network::MicroNetworkIpv6Spec;
    use uuid::Uuid;

    use super::*;

    fn lease(vm_id: u128, ipv4: &str, mac: &str) -> DhcpLeaseEntry {
        DhcpLeaseEntry {
            vm_id: Uuid::from_u128(vm_id),
            ipv4: ipv4.parse().unwrap(),
            ipv6: None,
            mac: mac.parse().unwrap(),
        }
    }

    #[test]
    fn render_hosts_file_emits_one_line_per_lease_with_its_hostname() {
        let leases = [
            lease(1, "172.30.0.5", "02:fc:00:00:00:05"),
            lease(2, "172.30.0.6", "02:fc:00:00:00:06"),
        ];
        let rendered = render_hosts_file(&leases);
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            format!(
                "02:fc:00:00:00:05,172.30.0.5,{}",
                guest_hostname(Uuid::from_u128(1))
            )
        );
    }

    #[test]
    fn render_hosts_file_of_an_empty_snapshot_is_empty() {
        assert_eq!(render_hosts_file(&[]), "");
    }

    fn active(ipv4: &str, mac: &str) -> (Ipv4Addr, MacAddr) {
        (ipv4.parse().unwrap(), mac.parse().unwrap())
    }

    #[test]
    fn stale_leases_flags_an_ip_reused_by_a_different_mac() {
        // Regression: a deleted VM's IP handed straight to a new VM's new
        // MAC (this project's IPAM reuses addresses immediately) must be
        // force-released, or dnsmasq refuses it as still actively leased
        // to the old MAC until the lease's full hour expires.
        let old = active("172.30.0.5", "02:fc:00:00:00:01");
        let current = [lease(2, "172.30.0.5", "02:fc:00:00:00:02")];
        assert_eq!(stale_leases(vec![old], &current), vec![old]);
    }

    #[test]
    fn stale_leases_flags_a_deleted_vms_ip_even_with_no_replacement() {
        let old = active("172.30.0.5", "02:fc:00:00:00:01");
        assert_eq!(stale_leases(vec![old], &[]), vec![old]);
    }

    #[test]
    fn stale_leases_ignores_an_unchanged_reservation() {
        let old = active("172.30.0.5", "02:fc:00:00:00:01");
        let current = [lease(1, "172.30.0.5", "02:fc:00:00:00:01")];
        assert!(stale_leases(vec![old], &current).is_empty());
    }

    #[test]
    fn parse_lease_file_reads_one_ip_mac_pair_per_line() {
        let text = "\
            1784810338 02:fc:00:00:00:01 172.30.0.5 fc-abc123456789 *\n\
            1784810400 02:fc:00:00:00:02 172.30.0.6 * 01:02:fc:00:00:00:02\n";
        assert_eq!(
            parse_lease_file(text),
            vec![
                active("172.30.0.5", "02:fc:00:00:00:01"),
                active("172.30.0.6", "02:fc:00:00:00:02"),
            ]
        );
    }

    #[test]
    fn parse_lease_file_skips_blank_and_malformed_lines_instead_of_failing() {
        let text = "\n \
            not-enough-fields\n\
            1784810338 not-a-mac 172.30.0.5 fc-abc123456789 *\n\
            1784810338 02:fc:00:00:00:01 not-an-ip fc-abc123456789 *\n\
            1784810338 02:fc:00:00:00:03 172.30.0.7 fc-abc123456789 *\n";
        assert_eq!(
            parse_lease_file(text),
            vec![active("172.30.0.7", "02:fc:00:00:00:03")]
        );
    }

    #[test]
    fn parse_lease_file_of_an_empty_string_is_empty() {
        assert!(parse_lease_file("").is_empty());
    }

    #[test]
    fn base_config_path_never_collides_with_the_hosts_file_it_describes() {
        // Regression: `base_config_path` used to be derived via
        // `hosts_path.with_extension("conf")`, a no-op for a path that
        // already ends in `.conf` — it silently returned `hosts_path`
        // itself, so `spawn_dnsmasq` wrote the base config on top of the
        // live lease-reservation file (see its doc comment for the fallout).
        let hosts_path = Path::new(HOSTS_FILE);
        let config_path = base_config_path(hosts_path);
        assert_ne!(config_path, hosts_path);
    }

    fn sample_network(id: u128, gateway: &str, prefix: u8) -> MicroNetworkSpec {
        MicroNetworkSpec {
            micro_network_id: Uuid::from_u128(id),
            gateway: gateway.parse().unwrap(),
            prefix,
            // DHCP is served on the network's own bridge either way — the
            // internet toggle only governs what leaves it.
            internet_enabled: true,
            uplink: None,
            ipv6: None,
        }
    }

    #[test]
    fn base_config_binds_only_the_firecrab_bridge_and_is_static_only() {
        let config = render_base_config(Path::new("/run/firecrab/dnsmasq-hosts.conf"), &[]);
        assert!(config.contains("bind-dynamic"));
        // No MicroNetworks: no interface= lines (empty explicit set).
        assert!(!config.contains("interface="));
    }

    #[test]
    fn base_config_serves_every_micro_network_bridge_with_its_own_range() {
        let networks = [
            sample_network(0x1234, "172.31.0.1", 24),
            sample_network(0x5678, "172.32.0.1", 24),
        ];
        let config = render_base_config(Path::new("/run/firecrab/dnsmasq-hosts.conf"), &networks);

        assert!(!config.contains("interface=fcbr0"));
        for network in networks {
            assert!(config.contains(&format!("interface={}", network.bridge_name())));
            assert!(config.contains(&format!("dhcp-range={},static", network.network_address())));
        }
    }

    fn dual_stack_network(
        id: u128,
        gateway: &str,
        v6_gateway: &str,
        address_mode: Ipv6AddressMode,
    ) -> MicroNetworkSpec {
        MicroNetworkSpec {
            ipv6: Some(MicroNetworkIpv6Spec {
                gateway: v6_gateway.parse().unwrap(),
                prefix: 64,
                address_mode,
            }),
            ..sample_network(id, gateway, 24)
        }
    }

    #[test]
    fn a_slaac_network_is_served_router_advertisements_and_no_addresses() {
        let network = dual_stack_network(
            0x1234,
            "172.31.0.1",
            "fd00:1234:5678:9abc::1",
            Ipv6AddressMode::Slaac,
        );
        let config = render_base_config(Path::new("/run/hosts"), &[network]);
        assert!(config.contains("enable-ra"));
        // ra-stateless: the guest builds its own address from the prefix, so
        // dnsmasq hands out configuration only, never an address.
        assert!(config.contains("dhcp-range=fd00:1234:5678:9abc::,ra-stateless,64"));
    }

    #[test]
    fn a_dhcpv6_network_is_served_a_static_range_like_its_v4_one() {
        let network = dual_stack_network(
            0x1234,
            "172.31.0.1",
            "fd00:1234:5678:9abc::1",
            Ipv6AddressMode::Dhcpv6,
        );
        let config = render_base_config(Path::new("/run/hosts"), &[network]);
        assert!(config.contains("enable-ra"));
        assert!(config.contains("dhcp-range=fd00:1234:5678:9abc::,static,64"));
        assert!(!config.contains("ra-stateless"));
    }

    #[test]
    fn an_ipv4_only_network_is_served_no_router_advertisements() {
        let config = render_base_config(
            Path::new("/run/hosts"),
            &[sample_network(0x1234, "172.31.0.1", 24)],
        );
        assert!(!config.contains("enable-ra"));
        assert!(!config.contains("dhcp-range=fd"));
    }

    #[test]
    fn a_dual_stack_reservation_carries_both_addresses_on_one_line() {
        let mut lease = lease(1, "172.31.0.5", "02:fc:00:00:00:05");
        lease.ipv6 = Some("fd00:1234:5678:9abc::5".parse().unwrap());
        let rendered = render_hosts_file(&[lease]);
        // dnsmasq wants the v6 address bracketed, alongside the v4 one.
        assert_eq!(
            rendered,
            format!(
                "02:fc:00:00:00:05,172.31.0.5,[fd00:1234:5678:9abc::5],{}\n",
                guest_hostname(Uuid::from_u128(1))
            )
        );
    }

    #[test]
    fn an_ipv4_only_reservation_is_unchanged_by_dual_stack_support() {
        let rendered = render_hosts_file(&[lease(1, "172.31.0.5", "02:fc:00:00:00:05")]);
        assert!(!rendered.contains('['));
    }

    #[test]
    fn slaac_leases_drop_the_bracketed_ipv6_reservation() {
        let mut lease = lease(1, "172.31.0.5", "02:fc:00:00:00:05");
        lease.ipv6 = Some("fd00:1234:5678:9abc::5".parse().unwrap());
        let slaac = dual_stack_network(
            0x1234,
            "172.31.0.1",
            "fd00:1234:5678:9abc::1",
            Ipv6AddressMode::Slaac,
        );
        let dhcpv6 = dual_stack_network(
            0x1234,
            "172.31.0.1",
            "fd00:1234:5678:9abc::1",
            Ipv6AddressMode::Dhcpv6,
        );
        let slaac_only = dhcpv6_reservations_only(&[lease], &[slaac]);
        assert_eq!(slaac_only[0].ipv6, None);
        assert_eq!(
            dhcpv6_reservations_only(&[lease], &[dhcpv6])[0].ipv6,
            lease.ipv6
        );
        assert!(!render_hosts_file(&slaac_only).contains('['));
    }

    #[test]
    fn a_lease_is_released_through_the_bridge_that_actually_serves_it() {
        let networks = [sample_network(0x1234, "172.31.0.1", 24)];
        // Inside the MicroNetwork's subnet -> its own bridge.
        assert_eq!(
            serving_interface("172.31.0.7".parse().unwrap(), &networks),
            networks[0].bridge_name()
        );
        // Outside every MicroNetwork -> empty (no implicit default bridge).
        assert_eq!(
            serving_interface("172.30.0.7".parse().unwrap(), &networks),
            ""
        );
    }

    #[tokio::test]
    async fn write_atomic_candidate_creates_a_missing_parent_directory() {
        // Regression: this must not depend on the net-helper socket's own
        // bind happening to have already created the same directory — a
        // custom FIRECRAB_NET_HELPER_SOCK elsewhere would leave nothing to
        // implicitly create /run/firecrab (or wherever HOSTS_FILE lives).
        let dir = tempfile::Builder::new()
            .prefix("fc-dhcp")
            .tempdir_in("/tmp")
            .expect("create tempdir");
        let nested = dir.path().join("does/not/exist/yet/hosts.tmp");

        write_atomic_candidate(&nested, "dhcp-host=02:fc:00:00:00:05,172.30.0.5\n")
            .await
            .expect("write candidate");

        assert!(nested.exists());
    }

    #[tokio::test]
    async fn write_atomic_candidate_is_readable_after_dnsmasq_drops_privileges() {
        // Regression: the helper's restrictive umask used to leave the
        // atomic replacement at 0600 root-owned. dnsmasq had already dropped
        // privileges by the time SIGHUP asked it to reload this file, so it
        // kept the previous reservations and reported new MACs as having no
        // address available.
        let dir = tempfile::Builder::new()
            .prefix("fc-dhcp")
            .tempdir_in("/tmp")
            .expect("create tempdir");
        let candidate = dir.path().join("hosts.tmp");

        write_atomic_candidate(&candidate, "02:fc:00:00:00:05,172.30.0.5,fc-test\n")
            .await
            .expect("write candidate");

        let mode = std::fs::metadata(&candidate)
            .expect("read candidate metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, DNSMASQ_READABLE_MODE);
    }

    #[tokio::test]
    async fn a_valid_snapshot_passes_dnsmasq_test_validation() {
        let dir = tempfile::Builder::new()
            .prefix("fc-dhcp")
            .tempdir_in("/tmp")
            .expect("create tempdir");
        let candidate = dir.path().join("hosts.tmp");
        let leases = [lease(1, "172.30.0.5", "02:fc:00:00:00:05")];
        write_atomic_candidate(&candidate, &render_hosts_file(&leases))
            .await
            .expect("write candidate");

        validate(&candidate, &[]).await.expect("dnsmasq --test");
    }

    #[tokio::test]
    async fn a_malformed_base_directive_fails_dnsmasq_test_validation() {
        // dnsmasq's --test only deeply parses the main config file's own
        // directives, not the *content* of a file a directive points at
        // (confirmed manually: a garbage dhcp-hostsfile line still passes
        // --test, since dnsmasq only checks it can open that path). That's
        // fine here because dhcp-host lines are rendered from already
        // strongly-typed `MacAddr`/`Ipv4Addr` — there's no code path that
        // could hand render_hosts_file malformed content in the first
        // place. So the meaningful thing to prove --test actually catches
        // is a broken *base* config directive.
        let dir = tempfile::Builder::new()
            .prefix("fc-dhcp")
            .tempdir_in("/tmp")
            .expect("create tempdir");
        let config_path = dir.path().join("bad.conf");
        write_atomic_candidate(&config_path, "dhcp-range=not,valid,at,all\n")
            .await
            .expect("write candidate");

        let output = Command::new("dnsmasq")
            .arg("--test")
            .arg(format!("--conf-file={}", config_path.display()))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await
            .expect("run dnsmasq --test");
        assert!(!output.status.success());
    }

    #[tokio::test]
    async fn sync_ignores_a_stale_or_duplicate_revision_while_dnsmasq_is_alive() {
        let actor = DhcpActor::new();
        {
            let mut state = actor.state.lock().await;
            state.applied_revision = Some(5);
            state.served_networks = Some(Vec::new());
            state.child = Some(
                Command::new("sleep")
                    .arg("60")
                    .spawn()
                    .expect("spawn stand-in dnsmasq"),
            );
        }

        // Would fail trying to actually spawn/bind dnsmasq for real if it
        // got past the staleness check — reaching `Ok(())` here proves the
        // stale revision short-circuited before any of that.
        assert!(
            sync_dhcp_leases(
                &actor,
                5,
                &[lease(1, "172.30.0.5", "02:fc:00:00:00:05")],
                &[]
            )
            .await
            .is_ok()
        );
        assert!(
            sync_dhcp_leases(
                &actor,
                3,
                &[lease(1, "172.30.0.5", "02:fc:00:00:00:05")],
                &[]
            )
            .await
            .is_ok()
        );

        let mut child = actor
            .state
            .lock()
            .await
            .child
            .take()
            .expect("stand-in child remains tracked");
        child.kill().await.expect("stop stand-in dnsmasq");
    }

    #[test]
    fn a_changed_network_set_is_never_stale_however_old_its_revision() {
        // Same revision, same networks: nothing to do.
        assert!(is_stale_snapshot(Some(5), 5, false));
        // Older revision, same networks: an out-of-order snapshot to ignore.
        assert!(is_stale_snapshot(Some(5), 3, false));
        // Creating a MicroNetwork bumps no lease revision (it holds no VMs
        // yet), so the revision alone would skip it and dnsmasq would never
        // learn to serve the new bridge.
        assert!(!is_stale_snapshot(Some(5), 5, true));
        assert!(!is_stale_snapshot(Some(5), 3, true));
        // Nothing applied yet is never stale.
        assert!(!is_stale_snapshot(None, 0, false));
    }

    #[test]
    fn a_stale_snapshot_is_reapplied_when_dnsmasq_is_dead() {
        assert!(can_skip_snapshot(Some(5), 5, false, true));
        assert!(!can_skip_snapshot(Some(5), 5, false, false));
    }

    #[test]
    fn proc_state_distinguishes_a_zombie_even_with_parentheses_in_comm() {
        assert_eq!(proc_state("42 (dnsmasq) S 1 2 3"), Some('S'));
        assert_eq!(proc_state("42 (dns)masq) Z 1 2 3"), Some('Z'));
        assert_eq!(proc_state("malformed"), None);
    }
}
