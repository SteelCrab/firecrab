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
///
/// `pub(crate)` so `service::units::is_active` can delegate to the same rule
/// instead of duplicating it.
pub(crate) fn systemd_is_active(runner: &dyn CommandRunner, unit: &str) -> String {
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

/// Builds the plain-text rendering as a `String` — split out from
/// [`print_human`] so tests can assert on the formatted content (both the
/// `Some(host)` and `None` branches) without capturing real stdout.
fn format_human(report: &StatusReport) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    writeln!(out, "firecrab-api.service:        {}", report.api_service).unwrap();
    writeln!(
        out,
        "firecrab-net-helper.service: {}",
        report.net_helper_service
    )
    .unwrap();
    match &report.host {
        Some(h) => {
            writeln!(out, "host:").unwrap();
            writeln!(out, "  load average (1m): {:.2}", h.load_average_1m).unwrap();
            writeln!(
                out,
                "  memory: {} / {} MiB available",
                h.memory_available_mib, h.memory_total_mib
            )
            .unwrap();
            writeln!(
                out,
                "  disk:   {} / {} GiB available",
                h.disk_available_gib, h.disk_total_gib
            )
            .unwrap();
            writeln!(out, "  uptime: {}s", h.uptime_seconds).unwrap();
        }
        None => {
            writeln!(
                out,
                "host: {}",
                report.host_error.as_deref().unwrap_or("unreachable")
            )
            .unwrap();
        }
    }
    out
}

/// Plain-text rendering for a terminal (the default output mode).
pub fn print_human(report: &StatusReport) {
    print!("{}", format_human(report));
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

    #[test]
    fn format_human_none_host_shows_host_error() {
        let report = StatusReport {
            api_service: "active".to_owned(),
            net_helper_service: "inactive".to_owned(),
            host: None,
            host_error: Some("unreachable: connection refused".to_owned()),
        };
        let text = format_human(&report);
        assert!(text.contains("firecrab-api.service:        active"));
        assert!(text.contains("firecrab-net-helper.service: inactive"));
        assert!(text.contains("host: unreachable: connection refused"));
    }

    #[test]
    fn format_human_none_host_falls_back_to_unreachable_when_no_error_text() {
        let report = StatusReport {
            api_service: "unknown".to_owned(),
            net_helper_service: "unknown".to_owned(),
            host: None,
            host_error: None,
        };
        let text = format_human(&report);
        assert!(text.contains("host: unreachable"));
    }

    #[test]
    fn format_human_some_host_prints_host_metrics() {
        let report = StatusReport {
            api_service: "active".to_owned(),
            net_helper_service: "active".to_owned(),
            host: Some(HostStatusResponse {
                load_average_1m: 0.42,
                memory_available_mib: 512,
                memory_total_mib: 2048,
                disk_available_gib: 10,
                disk_total_gib: 40,
                uptime_seconds: 3600,
            }),
            host_error: None,
        };
        let text = format_human(&report);
        assert!(text.contains("host:"));
        assert!(text.contains("load average (1m): 0.42"));
        assert!(text.contains("memory: 512 / 2048 MiB available"));
        assert!(text.contains("disk:   10 / 40 GiB available"));
        assert!(text.contains("uptime: 3600s"));
    }

    #[test]
    fn print_human_and_print_json_do_not_panic() {
        let report = StatusReport {
            api_service: "active".to_owned(),
            net_helper_service: "active".to_owned(),
            host: None,
            host_error: Some("unreachable: test".to_owned()),
        };
        print_human(&report);
        print_json(&report);
    }
}
