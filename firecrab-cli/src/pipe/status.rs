use std::fmt::Write;

use firecrab_helper_protocol::pipe::{Snapshot, UnitStatus};

/// Keep optional, stopped and failed services distinct from executable availability.
fn state(unit: &UnitStatus) -> &str {
    match unit.load_state.as_str() {
        "not-found" => "not configured",
        "loaded" => match unit.active_state.as_str() {
            "active" => match unit.sub_state.as_str() {
                "" | "unknown" => "active",
                state => state,
            },
            "inactive" => "stopped",
            state => state,
        },
        state => state,
    }
}

/// Format an API-independent snapshot as a service status table.
pub(super) fn format(snapshot: &Snapshot) -> String {
    let mut output = format!(
        "{:<19} {:<18} {:>7}  {}\n",
        "SERVICE", "STATE", "PID", "BINARY"
    );
    let units = snapshot
        .units
        .iter()
        .map(|unit| {
            let (name, available) = match unit.unit.as_str() {
                "firecrab-ingress.service" => ("Nginx", Some(snapshot.nginx_available)),
                "firecrab-tunnel.service" => {
                    ("Cloudflare Tunnel", Some(snapshot.cloudflared_available))
                }
                name => (name, None),
            };
            (name, unit, available)
        })
        .chain(
            snapshot
                .quick_tunnels
                .iter()
                .filter(|entry| entry.unit.unit != "firecrab-tunnel.service")
                .map(|entry| {
                    (
                        entry.name.as_str(),
                        &entry.unit,
                        Some(snapshot.cloudflared_available),
                    )
                }),
        );
    for (name, unit, available) in units {
        let pid = if unit.main_pid == 0 {
            "-".into()
        } else {
            unit.main_pid.to_string()
        };
        let binary = match available {
            Some(true) => "available",
            Some(false) => "missing",
            None => "unknown",
        };
        writeln!(output, "{name:<19} {:<18} {pid:>7}  {binary}", state(unit)).unwrap();
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> Snapshot {
        serde_json::from_value(serde_json::json!({
            "config": null,
            "units": [
                { "unit": "firecrab-ingress.service", "loadState": "loaded", "activeState": "active", "subState": "running", "mainPid": 6808 },
                { "unit": "firecrab-tunnel.service", "loadState": "not-found", "activeState": "inactive", "subState": "dead", "mainPid": 0 }
            ],
            "nginxAvailable": true,
            "cloudflaredAvailable": true
        })).unwrap()
    }

    #[test]
    fn an_available_connector_is_not_reported_as_a_configured_tunnel() {
        let output = format(&snapshot());
        let tunnel = output
            .lines()
            .find(|line| line.starts_with("Cloudflare Tunnel"))
            .unwrap();
        assert!(tunnel.contains("not configured"));
        assert!(tunnel.contains("available"));
        assert!(tunnel.contains('-'));
        assert!(!output.contains("null"));
    }

    #[test]
    fn failed_services_and_missing_executables_remain_visible() {
        let mut snapshot = snapshot();
        snapshot.units[0].active_state = "failed".into();
        snapshot.units[0].sub_state = "failed".into();
        snapshot.units[0].main_pid = 0;
        snapshot.nginx_available = false;
        let output = format(&snapshot);
        let ingress = output
            .lines()
            .find(|line| line.starts_with("Nginx"))
            .unwrap();
        assert!(ingress.contains("failed"));
        assert!(ingress.contains("missing"));
    }
}
