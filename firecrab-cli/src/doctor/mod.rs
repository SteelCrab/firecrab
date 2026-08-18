pub mod checks;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pass,
    Fail,
    Skip,
}

/// One line of doctor output — mirrors the bash script's flat `REPORT`
/// array entries (`fail "title" "detail" "fix"` / `skip ...` / `pass`).
/// A single check function may return more than one of these (e.g. `ufw`
/// reports one failure per firecrab bridge).
#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub title: String,
    pub status: Status,
    pub detail: Option<String>,
    pub fix: Option<String>,
}

impl CheckResult {
    pub fn pass(title: impl Into<String>) -> CheckResult {
        CheckResult { title: title.into(), status: Status::Pass, detail: None, fix: None }
    }

    pub fn fail(title: impl Into<String>, detail: Option<&str>, fix: Option<&str>) -> CheckResult {
        CheckResult {
            title: title.into(),
            status: Status::Fail,
            detail: detail.map(str::to_owned),
            fix: fix.map(str::to_owned),
        }
    }

    pub fn skip(title: impl Into<String>, detail: Option<&str>, fix: Option<&str>) -> CheckResult {
        CheckResult {
            title: title.into(),
            status: Status::Skip,
            detail: detail.map(str::to_owned),
            fix: fix.map(str::to_owned),
        }
    }
}

/// Resolved copy of the env vars the bash script reads once at the top
/// (`DATADIR`, `FIRECRAB_API_USER`, ...). Explicit injection instead of
/// each check reading `std::env` directly keeps checks pure and lets tests
/// set values without mutating process-global env vars (which would race
/// under `cargo test`'s parallel test runner).
pub struct DoctorEnv {
    pub datadir: String,
    pub api_user: Option<String>,
    pub helper_sock: String,
    pub dnsmasq_conf: String,
    pub dnsmasq_pid: String,
    pub libdir: String,
    pub image_root: Option<String>,
    pub image_base_url: Option<String>,
    pub storage_roots: Option<String>,
    pub firecracker_bin: Option<String>,
    pub confdir: String,
}

impl DoctorEnv {
    pub fn from_process_env() -> Self {
        Self {
            datadir: env_or("DATADIR", "/var/lib/firecrab"),
            api_user: std::env::var("FIRECRAB_API_USER").ok().or_else(|| std::env::var("FIRECRAB_USER").ok()),
            helper_sock: env_or("FIRECRAB_NET_HELPER_SOCK", "/run/firecrab/net-helper.sock"),
            dnsmasq_conf: env_or("FIRECRAB_DNSMASQ_CONF", "/run/firecrab/dnsmasq.conf"),
            dnsmasq_pid: env_or("FIRECRAB_DNSMASQ_PID", "/run/firecrab/dnsmasq.pid"),
            libdir: env_or("FIRECRAB_LIBDIR", "/usr/local/lib/firecrab"),
            image_root: std::env::var("FIRECRAB_IMAGE_ROOT").ok(),
            image_base_url: std::env::var("FIRECRAB_IMAGE_BASE_URL").ok(),
            storage_roots: std::env::var("FIRECRAB_STORAGE_ROOTS").ok(),
            firecracker_bin: std::env::var("FIRECRAB_FIRECRACKER_BIN").ok(),
            confdir: env_or("CONFDIR", "/etc/firecrab"),
        }
    }
}

impl Default for DoctorEnv {
    fn default() -> Self {
        Self {
            datadir: "/var/lib/firecrab".to_owned(),
            api_user: None,
            helper_sock: "/run/firecrab/net-helper.sock".to_owned(),
            dnsmasq_conf: "/run/firecrab/dnsmasq.conf".to_owned(),
            dnsmasq_pid: "/run/firecrab/dnsmasq.pid".to_owned(),
            libdir: "/usr/local/lib/firecrab".to_owned(),
            image_root: None,
            image_base_url: None,
            storage_roots: None,
            firecracker_bin: None,
            confdir: "/etc/firecrab".to_owned(),
        }
    }
}

fn env_or(var: &str, default: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| default.to_owned())
}
