//! Read-only host/network info for the dashboard's status panel (see
//! `public-docs/networking.md`). The subnet/bridge
//! come from the first MicroNetwork when any exist — prefer
//! `GET /api/micro-networks` for the full set. Host status is a live
//! snapshot read straight from `/proc` and `df`.

use std::fs;
use std::path::Path;
use std::process::Command;

use axum::Json;
use axum::extract::State;
use firecrab_api_types::{HostStatusResponse, NetworkInfoResponse};
use firecrab_helper_protocol::network::micro_network_bridge_name;

use crate::ipam::SubnetSpec;
use crate::state::AppState;

/// `GET /api/network`: first MicroNetwork summary, or empty strings when none.
pub async fn get_network_info(State(state): State<AppState>) -> Json<NetworkInfoResponse> {
    let uplink = read_uplink().unwrap_or_default();
    let store = state.store.clone();
    let networks = tokio::task::spawn_blocking(move || store.list_micro_networks())
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();

    let interfaces = read_host_interfaces();
    let Some(network) = networks.into_iter().next() else {
        return Json(NetworkInfoResponse {
            bridge_name: String::new(),
            subnet_cidr: String::new(),
            gateway: String::new(),
            uplink,
            interfaces,
        });
    };

    let gateway = SubnetSpec::parse(network.id, &network.subnet_cidr)
        .map(|subnet| subnet.gateway().to_string())
        .unwrap_or(network.gateway);

    Json(NetworkInfoResponse {
        bridge_name: micro_network_bridge_name(network.id),
        subnet_cidr: network.subnet_cidr,
        gateway,
        uplink,
        interfaces,
    })
}

/// Default route's outbound interface, read from `/proc/net/route` (no
/// privilege needed). This is the same value `firecrab-net-helper`'s own
/// `nat::detect_uplink` resolves via rtnetlink, just read a different way —
/// a read-only value isn't worth a new IPC round trip across the privilege
/// boundary for.
pub(crate) fn read_uplink() -> Option<String> {
    let text = fs::read_to_string("/proc/net/route").ok()?;
    text.lines().skip(1).find_map(|line| {
        let mut fields = line.split_whitespace();
        let iface = fields.next()?;
        let destination = fields.next()?;
        (destination == "00000000").then(|| iface.to_owned())
    })
}

/// Host NIC names from `/sys/class/net`, excluding loopback and Firecrab-owned
/// TAP/bridge prefixes. Picker-only; the helper still re-checks existence.
pub(crate) fn read_host_interfaces() -> Vec<String> {
    read_host_interfaces_from(Path::new("/sys/class/net"))
}

fn read_host_interfaces_from(sysfs: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = fs::read_dir(sysfs) else {
        return names;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == "lo" || name.starts_with("fct") || name.starts_with("mnb") {
            continue;
        }
        if sysfs.join(name).is_dir() {
            names.push(name.to_owned());
        }
    }
    names.sort();
    names
}

/// Whether `/sys/class/net/<name>` is a directory. Callers must charset-check
/// `name` first so this is not a path-join of user text.
pub(crate) fn host_interface_exists(name: &str) -> bool {
    Path::new("/sys/class/net").join(name).is_dir()
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
    async fn network_info_is_empty_when_no_micro_networks_exist() {
        let directory = tempdir().unwrap();
        let templates = TemplateRegistry::from_specs(directory.path(), std::iter::empty())
            .expect("empty template spec list should always verify");
        let state = AppState::with_db_file(templates, directory.path().join("state.db"))
            .await
            .expect("fresh temp db should open cleanly");

        let Json(info) = get_network_info(State(state)).await;
        assert!(info.bridge_name.is_empty());
        assert!(info.subnet_cidr.is_empty());
        assert!(info.gateway.is_empty());
    }

    /// With MicroNetworks present, the panel reports the first one's bridge
    /// and a gateway recomputed from its CIDR (not whatever was stored).
    #[tokio::test]
    async fn network_info_summarises_the_first_micro_network() {
        let directory = tempdir().unwrap();
        let state = crate::handlers::vms::test_support::test_state(directory.path()).await;
        let (_, Json(network)) = crate::handlers::micro_networks::create_micro_network(
            State(state.clone()),
            axum::extract::Extension(crate::server::RequestId(uuid::Uuid::new_v4())),
            crate::extract::ValidatedJson(firecrab_api_types::CreateMicroNetworkRequest {
                name: "panel-net".to_owned(),
                subnet_cidr: "172.29.0.0/24".to_owned(),
                internet_enabled: true,
                uplink: None,
                ipv6_cidr: None,
                ipv6_address_mode: Default::default(),
            }),
        )
        .await
        .expect("create micro network");

        let Json(info) = get_network_info(State(state)).await;
        assert_eq!(info.subnet_cidr, "172.29.0.0/24");
        assert_eq!(info.gateway, "172.29.0.1");
        assert_eq!(info.bridge_name, micro_network_bridge_name(network.id));
    }

    #[test]
    fn read_uplink_resolves_a_real_default_route_interface() {
        // Unprivileged read; requires this host to have an IPv4 default
        // route (true in the dev/CI sandbox this was written against).
        assert!(!read_uplink().unwrap().is_empty());
    }

    #[test]
    fn read_host_interfaces_from_a_missing_dir_is_empty() {
        assert!(read_host_interfaces_from(Path::new("/no/such/sysfs/net")).is_empty());
    }

    #[test]
    fn read_host_interfaces_from_skips_owned_prefixes_files_and_non_utf8() {
        use std::os::unix::ffi::OsStringExt;

        let directory = tempdir().unwrap();
        let sysfs = directory.path();
        for name in ["lo", "fct0", "mnb66d3df7f8547", "eth1", "wlan0"] {
            std::fs::create_dir(sysfs.join(name)).unwrap();
        }
        std::fs::write(sysfs.join("notadir"), b"").unwrap();
        std::fs::create_dir(sysfs.join(std::ffi::OsString::from_vec(vec![0xff]))).unwrap();

        assert_eq!(
            read_host_interfaces_from(sysfs),
            vec!["eth1".to_owned(), "wlan0".to_owned()]
        );
    }

    #[test]
    fn host_interface_exists_is_true_only_for_a_sysfs_directory() {
        assert!(host_interface_exists("lo"));
        assert!(!host_interface_exists("nosuchiface0"));
    }

    #[test]
    fn read_host_interfaces_lists_sysfs_and_skips_owned_prefixes() {
        let ifaces = read_host_interfaces();
        assert!(
            ifaces
                .iter()
                .any(|name| Path::new("/sys/class/net").join(name).is_dir()),
            "{ifaces:?}"
        );
        assert!(
            !ifaces
                .iter()
                .any(|name| name == "lo" || name.starts_with("fct") || name.starts_with("mnb")),
            "{ifaces:?}"
        );
    }

    #[tokio::test]
    async fn network_info_includes_host_interfaces() {
        let directory = tempdir().unwrap();
        let templates = TemplateRegistry::from_specs(directory.path(), std::iter::empty())
            .expect("empty template spec list should always verify");
        let state = AppState::with_db_file(templates, directory.path().join("state.db"))
            .await
            .expect("fresh temp db should open cleanly");

        let Json(info) = get_network_info(State(state)).await;
        assert!(
            info.interfaces
                .iter()
                .any(|name| Path::new("/sys/class/net").join(name).is_dir()),
            "{:?}",
            info.interfaces
        );
        assert!(
            !info
                .interfaces
                .iter()
                .any(|name| name == "lo" || name.starts_with("fct") || name.starts_with("mnb")),
            "{:?}",
            info.interfaces
        );
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
