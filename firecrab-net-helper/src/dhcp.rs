//! DHCP for guest VMs: a `dnsmasq` child process bound only to the Firecrab
//! bridge (`fcbr0`), handing out the exact IP each VM's IPAM lease already
//! reserved (MAC-keyed static reservations, no dynamic pool) and forwarding
//! DNS queries to the host's own resolver. Reservations are rewritten as one
//! full snapshot per `sync_dhcp_leases` call — write, validate, atomically
//! swap, reload — never edited in place.

use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use firecrab_helper_protocol::network::{DhcpLeaseEntry, MacAddr, guest_hostname};
use thiserror::Error;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::bridge::BRIDGE_NAME;

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
fn render_hosts_file(leases: &[DhcpLeaseEntry]) -> String {
    let mut rendered = String::new();
    for lease in leases {
        // No `dhcp-host=` prefix: unlike a plain `--conf-file`, dnsmasq's
        // `--dhcp-hostsfile` expects the bare `mac,ip,hostname` triplet per
        // line — the prefix (valid in a real conf file) makes it try to
        // parse the literal text "dhcp-host" as the leading MAC/hex field
        // and reject the whole file with "bad hex constant", silently
        // dropping every reservation.
        rendered.push_str(&format!(
            "{},{},{}\n",
            lease.mac,
            lease.ipv4,
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
async fn release_stale_leases(current: &[DhcpLeaseEntry]) {
    for (ip, mac) in stale_leases(active_leases().await, current) {
        release_lease(ip, mac).await;
    }
}

/// Reads dnsmasq's own lease database, returning every `(ip, mac)` pair it
/// currently considers actively leased. A missing or unreadable file is
/// treated as "no active leases" — the normal case right after a fresh
/// dnsmasq spawn that hasn't leased anything yet.
async fn active_leases() -> Vec<(Ipv4Addr, MacAddr)> {
    let Ok(text) = tokio::fs::read_to_string(LEASE_FILE).await else {
        return Vec::new();
    };
    // Format is `<expiry> <mac> <ip> <hostname-or-*> <client-id-or-*>`,
    // one lease per line.
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
async fn release_lease(ip: Ipv4Addr, mac: MacAddr) {
    let result = Command::new("dhcp_release")
        .arg(BRIDGE_NAME)
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
fn render_base_config(hosts_file: &Path) -> String {
    format!(
        "interface={BRIDGE_NAME}\n\
         bind-dynamic\n\
         dhcp-range=172.30.0.0,static\n\
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
) -> Result<(), DhcpError> {
    let mut state = actor.state.lock().await;
    if state
        .applied_revision
        .is_some_and(|applied| applied >= revision)
    {
        return Ok(());
    }

    let hosts_path = Path::new(HOSTS_FILE);
    let candidate_path = hosts_path.with_extension("tmp");
    write_atomic_candidate(&candidate_path, &render_hosts_file(leases)).await?;
    validate(&candidate_path).await?;
    tokio::fs::rename(&candidate_path, hosts_path)
        .await
        .map_err(|source| DhcpError::Swap {
            path: hosts_path.to_owned(),
            source,
        })?;

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
            None => state.child = Some(spawn_dnsmasq(hosts_path).await?),
        },
    }

    release_stale_leases(leases).await;
    state.applied_revision = Some(revision);
    Ok(())
}

/// Reads dnsmasq's own pid file and returns its pid if that process is
/// still alive — a `kill(pid, 0)` existence probe, not a real signal.
async fn running_orphan_pid() -> Option<u32> {
    let text = tokio::fs::read_to_string(PID_FILE).await.ok()?;
    let pid: u32 = text.trim().parse().ok()?;
    // SAFETY: signal 0 sends nothing; it only probes whether `pid` exists
    // and is signalable, per kill(2).
    let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
    alive.then_some(pid)
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
async fn validate(candidate_hosts_path: &Path) -> Result<(), DhcpError> {
    let config = render_base_config(candidate_hosts_path);
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
async fn spawn_dnsmasq(hosts_path: &Path) -> Result<Child, DhcpError> {
    let config_path = base_config_path(hosts_path);
    write_atomic_candidate(&config_path, &render_base_config(hosts_path)).await?;

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
        // Already exited; nothing to signal. The next sync_dhcp_leases call
        // will find `state.child` still `Some` but this reload a no-op —
        // acceptable since a crashed dnsmasq needs an operator/supervisor
        // restart regardless, same as any other unexpectedly-dead daemon.
        return Ok(());
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
    use uuid::Uuid;

    use super::*;

    fn lease(vm_id: u128, ipv4: &str, mac: &str) -> DhcpLeaseEntry {
        DhcpLeaseEntry {
            vm_id: Uuid::from_u128(vm_id),
            ipv4: ipv4.parse().unwrap(),
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

    #[test]
    fn base_config_binds_only_the_firecrab_bridge_and_is_static_only() {
        let config = render_base_config(Path::new("/run/firecrab/dnsmasq-hosts.conf"));
        assert!(config.contains("interface=fcbr0"));
        assert!(config.contains("bind-dynamic"));
        assert!(
            config.contains("dhcp-range=172.30.0.0,static"),
            "must not hand out addresses to unreserved MACs: {config}"
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

        validate(&candidate).await.expect("dnsmasq --test");
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
    async fn sync_ignores_a_stale_or_duplicate_revision() {
        let actor = DhcpActor::new();
        {
            let mut state = actor.state.lock().await;
            state.applied_revision = Some(5);
        }

        // Would fail trying to actually spawn/bind dnsmasq for real if it
        // got past the staleness check — reaching `Ok(())` here proves the
        // stale revision short-circuited before any of that.
        assert!(
            sync_dhcp_leases(&actor, 5, &[lease(1, "172.30.0.5", "02:fc:00:00:00:05")])
                .await
                .is_ok()
        );
        assert!(
            sync_dhcp_leases(&actor, 3, &[lease(1, "172.30.0.5", "02:fc:00:00:00:05")])
                .await
                .is_ok()
        );
    }
}
