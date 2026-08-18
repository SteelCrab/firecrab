use serde::Serialize;

use crate::api_client::{ApiClient, ApiError};
use crate::shell::CommandRunner;
use firecrab_api_types::HostStatusResponse;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusReport {
    /// `systemctl is-active` output for `firecrab-api.service` — "active",
    /// "inactive", "failed", or "unknown" if `systemctl` itself is missing.
    pub api_service: String,
    /// Same as `api_service`, for `firecrab-net-helper.service`.
    pub net_helper_service: String,
    /// `Some` only when the API answered; `host_error` explains a `None`.
    pub host: Option<HostStatusResponse>,
    pub host_error: Option<String>,
}

/// Wraps `systemctl is-active`; any runner error (missing binary, non-UTF8
/// output) collapses to `"unknown"` rather than failing the whole report.
fn systemd_is_active(runner: &dyn CommandRunner, unit: &str) -> String {
    match runner.run("systemctl", &["is-active", unit]) {
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        Err(_) => "unknown".to_owned(),
    }
}

/// Partial-failure tolerant: an unreachable API still lets the systemd
/// portion print (issue #138's requirement — a dead API must not hide
/// otherwise-useful status).
pub fn collect(runner: &dyn CommandRunner, client: &ApiClient) -> StatusReport {
    let api_service = systemd_is_active(runner, "firecrab-api.service");
    let net_helper_service = systemd_is_active(runner, "firecrab-net-helper.service");
    let (host, host_error) = match client.get_host_status() {
        Ok(h) => (Some(h), None),
        Err(ApiError::Unreachable(msg)) => (None, Some(format!("unreachable: {msg}"))),
        Err(ApiError::Http { status, body }) => (None, Some(format!("HTTP {status}: {body}"))),
    };
    StatusReport {
        api_service,
        net_helper_service,
        host,
        host_error,
    }
}

/// Plain-text rendering for a terminal (the default output mode).
pub fn print_human(report: &StatusReport) {
    println!("firecrab-api.service:        {}", report.api_service);
    println!("firecrab-net-helper.service: {}", report.net_helper_service);
    match &report.host {
        Some(h) => {
            println!("host:");
            println!("  load average (1m): {:.2}", h.load_average_1m);
            println!(
                "  memory: {} / {} MiB available",
                h.memory_available_mib, h.memory_total_mib
            );
            println!(
                "  disk:   {} / {} GiB available",
                h.disk_available_gib, h.disk_total_gib
            );
            println!("  uptime: {}s", h.uptime_seconds);
        }
        None => {
            println!(
                "host: {}",
                report.host_error.as_deref().unwrap_or("unreachable")
            );
        }
    }
}

/// `--json` output mode, for scripting.
pub fn print_json(report: &StatusReport) {
    println!("{}", serde_json::to_string_pretty(report).unwrap());
}

#[cfg(test)]
mod tests {
    use crate::api_client::ApiClient;
    use crate::shell::FakeCommandRunner;

    use super::*;

    #[test]
    fn collect_reads_systemd_state_via_runner() {
        let mut fake = FakeCommandRunner::new();
        fake.set(
            "systemctl",
            &["is-active", "firecrab-api.service"],
            0,
            "active\n",
            "",
        );
        fake.set(
            "systemctl",
            &["is-active", "firecrab-net-helper.service"],
            3,
            "inactive\n",
            "",
        );
        // Port 1 never listens — exercises the "API unreachable" branch so
        // this test does not depend on a live firecrab-api.
        let client = ApiClient::new("http://127.0.0.1:1".to_owned());
        let report = collect(&fake, &client);
        assert_eq!(report.api_service, "active");
        assert_eq!(report.net_helper_service, "inactive");
        assert!(report.host.is_none());
        assert!(report.host_error.is_some());
    }

    #[test]
    fn collect_reports_unknown_when_systemctl_missing() {
        let fake = FakeCommandRunner::new();
        let client = ApiClient::new("http://127.0.0.1:1".to_owned());
        let report = collect(&fake, &client);
        assert_eq!(report.api_service, "unknown");
    }
}
