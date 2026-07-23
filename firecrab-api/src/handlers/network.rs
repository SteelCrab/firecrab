//! Read-only host/network info for the dashboard's status panel (see
//! `docs/task-network-configuration-dashboard.md`). The subnet/bridge are
//! fixed constants for now — making them genuinely editable needs a larger
//! IPAM/bridge refactor, deferred per that task doc's own scope note. Host
//! status is a live snapshot read straight from `/proc` and `df`.

use std::fs;
use std::path::Path;
use std::process::Command;

use axum::Json;
use axum::extract::State;
use firecrab_api_types::{HostStatusResponse, NetworkInfoResponse};

use crate::ipam::{GATEWAY, NETWORK, PREFIX_LEN};
use crate::state::AppState;

/// Name of the shared Linux bridge every VM's TAP attaches to. Mirrors
/// `firecrab-net-helper/src/bridge.rs::BRIDGE_NAME` — not shared directly,
/// since `firecrab-api` deliberately has no dependency on
/// `firecrab-net-helper` (privilege separation).
const BRIDGE_NAME: &str = "fcbr0";

/// `GET /api/network`: the fixed network configuration firecrab has set up.
pub async fn get_network_info() -> Json<NetworkInfoResponse> {
    Json(NetworkInfoResponse {
        bridge_name: BRIDGE_NAME.to_owned(),
        subnet_cidr: format!("{NETWORK}/{PREFIX_LEN}"),
        gateway: GATEWAY.to_string(),
    })
}

/// `GET /api/host`: a point-in-time snapshot of host resource usage.
pub async fn get_host_status(State(state): State<AppState>) -> Json<HostStatusResponse> {
    let vms_dir = state.runtime.vms_dir.clone();
    let status = tokio::task::spawn_blocking(move || read_host_status(&vms_dir))
        .await
        .unwrap_or_default();
    Json(status)
}

fn read_host_status(vms_dir: &Path) -> HostStatusResponse {
    let (disk_total_gib, disk_available_gib) = read_disk_usage_gib(vms_dir).unwrap_or((0, 0));
    HostStatusResponse {
        load_average_1m: read_load_average().unwrap_or(0.0),
        memory_total_mib: read_meminfo_kib("MemTotal").map(kib_to_mib).unwrap_or(0),
        memory_available_mib: read_meminfo_kib("MemAvailable")
            .map(kib_to_mib)
            .unwrap_or(0),
        disk_total_gib,
        disk_available_gib,
        uptime_seconds: read_uptime().unwrap_or(0),
    }
}

fn kib_to_mib(kib: u64) -> u64 {
    kib / 1024
}

/// Parses `/proc/loadavg`'s first (1-minute) field.
fn read_load_average() -> Option<f64> {
    fs::read_to_string("/proc/loadavg")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Parses `/proc/uptime`'s first field (seconds since boot, as a float —
/// truncated to whole seconds since sub-second precision isn't useful here).
fn read_uptime() -> Option<u64> {
    let seconds: f64 = fs::read_to_string("/proc/uptime")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some(seconds as u64)
}

/// Parses one `/proc/meminfo` field (e.g. `MemTotal`, `MemAvailable`),
/// whose value is always in KiB regardless of the trailing unit label.
fn read_meminfo_kib(field: &str) -> Option<u64> {
    fs::read_to_string("/proc/meminfo")
        .ok()?
        .lines()
        .find_map(|line| {
            let (name, rest) = line.split_once(':')?;
            (name == field)
                .then(|| rest.split_whitespace().next())
                .flatten()?
                .parse()
                .ok()
        })
}

/// `df -kP` (1024-byte blocks, POSIX single-line output — plain `df -k` can
/// wrap onto two lines for a long device name) for the filesystem backing
/// `path`. `None` if `path` doesn't exist yet (a fresh install with no VM
/// ever created) or `df` isn't available.
fn read_disk_usage_gib(path: &Path) -> Option<(u64, u64)> {
    let output = Command::new("df").arg("-kP").arg(path).output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let fields: Vec<&str> = text.lines().nth(1)?.split_whitespace().collect();
    let total_kib: u64 = fields.get(1)?.parse().ok()?;
    let available_kib: u64 = fields.get(3)?.parse().ok()?;
    const KIB_PER_GIB: u64 = 1024 * 1024;
    Some((total_kib / KIB_PER_GIB, available_kib / KIB_PER_GIB))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::templates::TemplateRegistry;

    #[tokio::test]
    async fn network_info_reports_the_fixed_subnet() {
        let Json(info) = get_network_info().await;
        assert_eq!(info.bridge_name, "fcbr0");
        assert_eq!(info.subnet_cidr, "172.30.0.0/24");
        assert_eq!(info.gateway, "172.30.0.1");
    }

    #[test]
    fn read_host_status_aggregates_real_proc_and_disk_values() {
        let status = read_host_status(Path::new("/"));
        assert!(status.load_average_1m >= 0.0);
        assert!(status.memory_total_mib > 0);
        assert!(status.memory_available_mib > 0);
        assert!(status.disk_total_gib > 0);
        assert!(status.uptime_seconds > 0);
    }

    #[test]
    fn read_host_status_falls_back_to_zero_disk_usage_for_a_missing_path() {
        let status = read_host_status(Path::new("/no/such/path/at/all"));
        assert_eq!(status.disk_total_gib, 0);
        assert_eq!(status.disk_available_gib, 0);
        // Unrelated to disk lookup, so still real values.
        assert!(status.memory_total_mib > 0);
    }

    #[tokio::test]
    async fn get_host_status_serves_real_values_through_the_handler() {
        let directory = tempdir().unwrap();
        let templates = TemplateRegistry::from_specs(directory.path(), std::iter::empty())
            .expect("empty template spec list should always verify");
        let state = AppState::with_db_file(templates, directory.path().join("state.db"))
            .await
            .expect("fresh temp db should open cleanly");

        let Json(status) = get_host_status(State(state)).await;

        assert!(status.memory_total_mib > 0);
        assert!(status.uptime_seconds > 0);
    }

    #[test]
    fn read_load_average_parses_a_real_proc_file() {
        // Read-only, always present on Linux — safe without any privilege.
        assert!(read_load_average().unwrap() >= 0.0);
    }

    #[test]
    fn read_uptime_parses_a_real_proc_file() {
        assert!(read_uptime().unwrap() > 0);
    }

    #[test]
    fn read_meminfo_kib_finds_known_fields_and_rejects_unknown_ones() {
        assert!(read_meminfo_kib("MemTotal").unwrap() > 0);
        assert!(read_meminfo_kib("NotARealField").is_none());
    }

    #[test]
    fn read_disk_usage_gib_reports_a_real_filesystem() {
        let (total, available) = read_disk_usage_gib(Path::new("/")).unwrap();
        assert!(total > 0);
        assert!(available <= total);
    }

    #[test]
    fn read_disk_usage_gib_is_none_for_a_path_that_does_not_exist() {
        assert!(read_disk_usage_gib(Path::new("/no/such/path/at/all")).is_none());
    }
}
