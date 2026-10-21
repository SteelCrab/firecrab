//! Anonymous connectors keep their temporary address in the current unit journal.

use crate::shell::CommandRunner;
use firecrab_helper_protocol::pipe::{
    Config, LEGACY_QUICK_TUNNEL_ID, QuickTunnelEntry, QuickTunnelRequest, TUNNEL_UNIT, UnitStatus,
    quick_tunnel_unit,
};

const URL_FILTER: &str = "--grep=https://[A-Za-z0-9-]+[.]trycloudflare[.]com";
const ERROR_FILTER: &str = "--grep=quick tunnel provisioning failed|failed to request quick Tunnel|failed to unmarshal quick Tunnel";

/// Only fixed messages cross the helper boundary; raw logs can contain secrets.
pub(super) fn error(
    runner: &dyn CommandRunner,
    unit: &UnitStatus,
    invocation: &str,
) -> Option<String> {
    if !managed_unit(&unit.unit)
        || !["active", "activating", "failed"].contains(&unit.active_state.as_str())
        || invocation.len() != 32
        || !invocation.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    let output = runner.run(
        "journalctl",
        &[
            "--unit",
            &unit.unit,
            "--no-pager",
            "--output=json",
            "--lines=20",
            ERROR_FILTER,
            &format!("_SYSTEMD_INVOCATION_ID={invocation}"),
        ],
    );
    if let Ok(output) = output
        && output.status.success()
    {
        for line in String::from_utf8_lossy(&output.stdout).lines().rev() {
            let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if record["_SYSTEMD_INVOCATION_ID"].as_str() != Some(invocation)
                || record["_SYSTEMD_UNIT"].as_str() != Some(unit.unit.as_str())
            {
                continue;
            }
            let Some(message) = record["MESSAGE"].as_str() else {
                continue;
            };
            if message.contains("quick tunnel provisioning failed") {
                return Some(if message.contains("status 429") {
                    "Cloudflare rate limited URL creation (HTTP 429). Automatic retries use increasing delays; wait before restarting."
                } else {
                    "Cloudflare could not create a public URL. The connector will retry automatically."
                }.into());
            }
            if message.contains("failed to request quick Tunnel") {
                return Some("Cannot reach the Cloudflare URL service. Check the host network and DNS; the connector will retry automatically.".into());
            }
            if message.contains("failed to unmarshal quick Tunnel") {
                return Some("Cloudflare returned an invalid URL response. The connector will retry automatically.".into());
            }
        }
    }
    (unit.active_state == "failed" || unit.sub_state == "auto-restart").then(|| {
        "The Cloudflare connector failed. Check its service logs or restart the tunnel.".into()
    })
}

/// Install a verified connector if needed, then apply its independent unit.
#[cfg(target_os = "linux")]
pub(super) fn connect() -> Result<(), Box<dyn std::error::Error>> {
    use firecrab_helper_protocol::pipe::QuickConnectRequest;
    use std::io::Read;
    super::host::require_root()?;
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(48 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 48 * 1024 {
        return Err("Quick Tunnel request exceeds 48 KiB".into());
    }
    let request: QuickConnectRequest = serde_json::from_slice(&bytes)?;
    request.validate()?;
    let _lock = super::host::lock()?;
    // The installer validates both the pinned digest and an existing binary's marker.
    super::engines::install()?;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("pipe.json");
    std::fs::write(&path, serde_json::to_vec(&request.config)?)?;
    super::host::apply_locked(&path, None, None, || Ok(()))
}

/// Apply only the requested connector change to the helper's latest saved manifest.
#[cfg(target_os = "linux")]
pub(super) fn manage() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Read;
    super::host::require_root()?;
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(48 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 48 * 1024 {
        return Err("Quick Tunnel request exceeds 48 KiB".into());
    }
    let request: QuickTunnelRequest = serde_json::from_slice(&bytes)?;
    request.validate()?;
    let _lock = super::host::lock()?;
    if let QuickTunnelRequest::Delete { id } = &request {
        super::firecrab::require_unreferenced(*id)?;
    }
    if let QuickTunnelRequest::Save { id: Some(id), .. } = &request {
        super::firecrab::require_editable(*id)?;
    }
    let manifest = crate::service::env::ServiceEnv::from_process_env()
        .confdir
        .join("pipe/manifest.json");
    let config = if manifest.is_file() {
        Config::read(&manifest)?
    } else {
        serde_json::from_value(serde_json::json!({ "routes": [], "tunnel": null }))?
    };
    if let QuickTunnelRequest::Control { id, service_action } = request {
        return super::host::control_quick_locked(&config, id, service_action);
    }
    let config = updated_config(&config, &request)?;
    if matches!(request, QuickTunnelRequest::Save { .. }) {
        super::engines::install()?;
    }
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("pipe.json");
    std::fs::write(&path, serde_json::to_vec(&config)?)?;
    super::host::apply_quick_locked(&path)
}

fn updated_config(
    config: &Config,
    request: &QuickTunnelRequest,
) -> Result<Config, Box<dyn std::error::Error>> {
    request.validate()?;
    let mut next = config.clone();
    match request {
        QuickTunnelRequest::Save { id, name, target } => {
            let id = match id {
                Some(id) => *id,
                None => uuid::Uuid::new_v4(),
            };
            let entry = QuickTunnelEntry {
                id,
                name: name.clone(),
                target: target.clone(),
            };
            if let Some(existing) = next.quick_tunnels.iter_mut().find(|entry| entry.id == id) {
                *existing = entry;
            } else if id == LEGACY_QUICK_TUNNEL_ID && next.quick_tunnel.is_some() {
                next.quick_tunnel = None;
                next.quick_tunnels.push(entry);
            } else if matches!(request, QuickTunnelRequest::Save { id: None, .. }) {
                next.quick_tunnels.push(entry);
            } else {
                return Err("Quick Tunnel was not found".into());
            }
        }
        QuickTunnelRequest::Delete { id } => {
            if *id == LEGACY_QUICK_TUNNEL_ID && next.quick_tunnel.is_some() {
                next.quick_tunnel = None;
            } else {
                let index = next
                    .quick_tunnels
                    .iter()
                    .position(|entry| entry.id == *id)
                    .ok_or("Quick Tunnel was not found")?;
                next.quick_tunnels.remove(index);
            }
        }
        QuickTunnelRequest::Control { .. } => {
            return Err("service actions do not change Pipe configuration".into());
        }
    }
    next.validate()?;
    Ok(next)
}

fn managed_unit(unit: &str) -> bool {
    if unit == TUNNEL_UNIT {
        return true;
    }
    let Some(id) = unit
        .strip_prefix("firecrab-quick-tunnel-")
        .and_then(|value| value.strip_suffix(".service"))
    else {
        return false;
    };
    uuid::Uuid::parse_str(id).is_ok_and(|id| quick_tunnel_unit(id) == unit)
}

/// Accept one HTTPS DNS label beneath trycloudflare.com, without paths or credentials.
fn temporary_url(value: &str) -> Option<String> {
    let hostname = value.strip_prefix("https://")?;
    let label = hostname.strip_suffix(".trycloudflare.com")?;
    if label.is_empty()
        || label.len() > 63
        || label.starts_with('-')
        || label.ends_with('-')
        || !label
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return None;
    }
    Some(value.to_owned())
}

/// Never use records from an earlier unit invocation or expose a stopped URL.
pub(super) fn public_url(
    runner: &dyn CommandRunner,
    unit: &UnitStatus,
    invocation: &str,
) -> Option<String> {
    if !managed_unit(&unit.unit)
        || unit.active_state != "active"
        || unit.main_pid == 0
        || invocation.len() != 32
        || !invocation.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    let invocation_filter = format!("_SYSTEMD_INVOCATION_ID={invocation}");
    let output = runner
        .run(
            "journalctl",
            &[
                "--unit",
                &unit.unit,
                "--no-pager",
                "--output=json",
                "--lines=20",
                URL_FILTER,
                &invocation_filter,
            ],
        )
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let url = text.lines().rev().find_map(|line| {
        let record: serde_json::Value = serde_json::from_str(line).ok()?;
        if record["_SYSTEMD_INVOCATION_ID"].as_str() != Some(invocation)
            || record["_SYSTEMD_UNIT"].as_str() != Some(unit.unit.as_str())
        {
            return None;
        }
        let message = record["MESSAGE"].as_str()?;
        let (_, banner) = message.split_once('|')?;
        let (address, _) = banner.split_once('|')?;
        temporary_url(address.trim())
    })?;
    // A stop/restart can race journal IO. Confirm the same invocation is still active.
    let current = runner
        .run(
            "systemctl",
            &["show", "--property=ActiveState,InvocationID", &unit.unit],
        )
        .ok()?;
    if !current.status.success() {
        return None;
    }
    let state = String::from_utf8_lossy(&current.stdout);
    if !state.lines().any(|line| line == "ActiveState=active")
        || !state
            .lines()
            .any(|line| line == format!("InvocationID={invocation}"))
    {
        return None;
    }
    Some(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::FakeCommandRunner;

    const INVOCATION: &str = "11111111111111111111111111111111";
    const OTHER: &str = "22222222222222222222222222222222";

    fn unit() -> UnitStatus {
        UnitStatus {
            unit: TUNNEL_UNIT.into(),
            load_state: "loaded".into(),
            active_state: "active".into(),
            sub_state: "running".into(),
            main_pid: 42,
        }
    }

    fn journal(invocation: &str, message: &str) -> String {
        serde_json::json!({"_SYSTEMD_UNIT": TUNNEL_UNIT, "_SYSTEMD_INVOCATION_ID": invocation, "MESSAGE": message}).to_string()
    }

    fn runner(records: &str, current_state: &str, current_invocation: &str) -> FakeCommandRunner {
        let mut runner = FakeCommandRunner::new();
        runner.set(
            "journalctl",
            &[
                "--unit",
                TUNNEL_UNIT,
                "--no-pager",
                "--output=json",
                "--lines=20",
                URL_FILTER,
                &format!("_SYSTEMD_INVOCATION_ID={INVOCATION}"),
            ],
            0,
            records,
            "",
        );
        runner.set(
            "systemctl",
            &["show", "--property=ActiveState,InvocationID", TUNNEL_UNIT],
            0,
            &format!("ActiveState={current_state}\nInvocationID={current_invocation}\n"),
            "",
        );
        runner
    }

    #[test]
    fn extracts_only_an_https_address_with_one_ascii_host_label() {
        assert_eq!(
            temporary_url("https://blue-crab-123.trycloudflare.com"),
            Some("https://blue-crab-123.trycloudflare.com".into())
        );
        for invalid in [
            "http://blue-crab.trycloudflare.com",
            "https://trycloudflare.com",
            "https://a.b.trycloudflare.com",
            "https://crab.trycloudflare.com.evil.test",
            "https://user@crab.trycloudflare.com",
            "https://crab.trycloudflare.com:443",
            "https://crab.trycloudflare.com/path",
            "https://crab.trycloudflare.com?query=1",
            "https://-crab.trycloudflare.com",
            "https://crab-.trycloudflare.com",
            "https://크랩.trycloudflare.com",
            "https://crab_try.trycloudflare.com",
        ] {
            assert_eq!(temporary_url(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn recovers_the_current_url_from_json_journal_messages() {
        let records = journal(INVOCATION, "|  https://blue-crab.trycloudflare.com  |");
        let runner = runner(&records, "active", INVOCATION);
        assert_eq!(
            public_url(&runner, &unit(), INVOCATION),
            Some("https://blue-crab.trycloudflare.com".into())
        );
        assert_eq!(runner.calls().len(), 2);
    }

    #[test]
    fn ignores_old_invocations_even_when_the_journal_returns_their_entries() {
        let records = journal(OTHER, "| https://old-crab.trycloudflare.com |");
        let runner = runner(&records, "active", INVOCATION);
        assert_eq!(public_url(&runner, &unit(), INVOCATION), None);
        assert_eq!(runner.calls().len(), 1);
    }

    #[test]
    fn inactive_failed_and_starting_units_never_read_or_return_old_urls() {
        for state in [
            "inactive",
            "failed",
            "activating",
            "deactivating",
            "reloading",
        ] {
            let mut unit = unit();
            unit.active_state = state.into();
            let runner = FakeCommandRunner::new();
            assert_eq!(public_url(&runner, &unit, INVOCATION), None);
            assert!(runner.calls().is_empty());
        }
        let runner = FakeCommandRunner::new();
        for invocation in [
            "",
            "unknown",
            "000;reboot",
            "1111111111111111111111111111111z",
        ] {
            assert_eq!(public_url(&runner, &unit(), invocation), None);
        }
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn a_restart_or_stop_during_journal_read_invalidates_the_url() {
        let records = journal(INVOCATION, "| https://old-crab.trycloudflare.com |");
        for (state, invocation) in [
            ("active", OTHER),
            ("inactive", INVOCATION),
            ("deactivating", INVOCATION),
        ] {
            let runner = runner(&records, state, invocation);
            assert_eq!(public_url(&runner, &unit(), INVOCATION), None);
        }
    }

    #[test]
    fn missing_and_unreadable_journals_leave_the_address_pending() {
        let empty = runner("", "active", INVOCATION);
        assert_eq!(public_url(&empty, &unit(), INVOCATION), None);
        let unavailable = FakeCommandRunner::new();
        assert_eq!(public_url(&unavailable, &unit(), INVOCATION), None);
    }

    #[test]
    fn unrelated_log_urls_are_not_treated_as_an_issued_address() {
        for message in [
            "Documentation: https://developers.cloudflare.com/trycloudflare/",
            "https://trycloudflare.com",
            "Request failed for https://other-crab.trycloudflare.com",
        ] {
            let records = journal(INVOCATION, message);
            let runner = runner(&records, "active", INVOCATION);
            assert_eq!(public_url(&runner, &unit(), INVOCATION), None);
        }
    }

    fn error_runner(records: &str) -> FakeCommandRunner {
        let mut runner = FakeCommandRunner::new();
        runner.set(
            "journalctl",
            &[
                "--unit",
                TUNNEL_UNIT,
                "--no-pager",
                "--output=json",
                "--lines=20",
                ERROR_FILTER,
                &format!("_SYSTEMD_INVOCATION_ID={INVOCATION}"),
            ],
            0,
            records,
            "",
        );
        runner
    }

    #[test]
    fn reports_rate_limiting_during_the_restart_delay_without_exposing_logs() {
        let mut retrying = unit();
        retrying.active_state = "activating".into();
        retrying.sub_state = "auto-restart".into();
        retrying.main_pid = 0;
        let records = journal(
            INVOCATION,
            "quick tunnel provisioning failed with status 429: private upstream response",
        );
        let runner = error_runner(&records);
        let failure = error(&runner, &retrying, INVOCATION).unwrap();
        assert!(failure.contains("HTTP 429"));
        assert!(failure.contains("Automatic retries"));
        assert!(!failure.contains("private upstream response"));
        assert_eq!(public_url(&runner, &retrying, INVOCATION), None);
    }

    #[test]
    fn connector_errors_only_use_the_selected_units_current_invocation() {
        for record in [
            journal(OTHER, "quick tunnel provisioning failed with status 429"),
            serde_json::json!({"_SYSTEMD_UNIT":"unrelated.service", "_SYSTEMD_INVOCATION_ID":INVOCATION, "MESSAGE":"quick tunnel provisioning failed with status 429"}).to_string(),
        ] {
            assert_eq!(error(&error_runner(&record), &unit(), INVOCATION), None);
        }
        let mut stopped = unit();
        stopped.active_state = "inactive".into();
        let runner = FakeCommandRunner::new();
        assert_eq!(error(&runner, &stopped, INVOCATION), None);
        assert_eq!(error(&runner, &unit(), "invalid"), None);
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn reports_network_and_provisioning_failures_with_fixed_messages() {
        for (message, expected) in [
            (
                "failed to request quick Tunnel: private network details",
                "Check the host network and DNS",
            ),
            (
                "quick tunnel provisioning failed with status 503",
                "Cloudflare could not create a public URL",
            ),
            (
                "failed to unmarshal quick Tunnel: private response",
                "Cloudflare returned an invalid URL response",
            ),
        ] {
            let runner = error_runner(&journal(INVOCATION, message));
            let failure = error(&runner, &unit(), INVOCATION).unwrap();
            assert!(failure.contains(expected));
            assert!(!failure.contains("private"));
        }
        assert_eq!(error(&error_runner(""), &unit(), INVOCATION), None);
    }

    fn saved_config() -> Config {
        serde_json::from_value(serde_json::json!({
            "listen": "127.0.0.1:8080", "routes": [{ "host": "pipe.test", "upstreams": ["127.0.0.1:9000"] }],
            "quickTunnels": [{ "id": "11111111-1111-4111-8111-111111111111", "name": "first", "target": { "type": "apps", "upstreams": ["127.0.0.1:9000"] } }]
        })).unwrap()
    }

    fn apps() -> firecrab_helper_protocol::pipe::QuickTunnelTarget {
        firecrab_helper_protocol::pipe::QuickTunnelTarget::Apps {
            upstreams: vec!["127.0.0.1:9010".parse().unwrap()],
        }
    }

    #[test]
    fn connector_creation_preserves_saved_routes_listener_and_existing_entries() {
        let saved = saved_config();
        let next = updated_config(
            &saved,
            &QuickTunnelRequest::Save {
                id: None,
                name: "second".into(),
                target: apps(),
            },
        )
        .unwrap();
        assert_eq!(next.listen, saved.listen);
        assert_eq!(next.routes, saved.routes);
        assert_eq!(next.quick_tunnels[0], saved.quick_tunnels[0]);
        assert_eq!(next.quick_tunnels.len(), 2);
        assert_ne!(next.quick_tunnels[1].id, saved.quick_tunnels[0].id);
        assert_eq!(next.quick_tunnels[1].id.get_version_num(), 4);
        assert_eq!(saved.quick_tunnels.len(), 1);
    }

    #[test]
    fn updates_and_deletes_only_the_selected_connector() {
        let saved = saved_config();
        let added = updated_config(
            &saved,
            &QuickTunnelRequest::Save {
                id: None,
                name: "second".into(),
                target: apps(),
            },
        )
        .unwrap();
        let id = added.quick_tunnels[0].id;
        let edited = updated_config(
            &added,
            &QuickTunnelRequest::Save {
                id: Some(id),
                name: "paths".into(),
                target: firecrab_helper_protocol::pipe::QuickTunnelTarget::Routes {
                    host: "pipe.test".into(),
                },
            },
        )
        .unwrap();
        assert_eq!(edited.quick_tunnels[0].id, id);
        assert_eq!(edited.quick_tunnels[0].name, "paths");
        assert_eq!(edited.quick_tunnels[1], added.quick_tunnels[1]);
        let deleted = updated_config(&edited, &QuickTunnelRequest::Delete { id }).unwrap();
        assert_eq!(deleted.quick_tunnels, vec![added.quick_tunnels[1].clone()]);
        assert_eq!(deleted.routes, saved.routes);
    }

    #[test]
    fn legacy_edit_migrates_to_a_stable_list_identity_and_delete_is_local() {
        let mut saved = saved_config();
        saved.quick_tunnel = Some(firecrab_helper_protocol::pipe::QuickTunnel {
            upstreams: vec!["127.0.0.1:9020".parse().unwrap()],
        });
        let edited = updated_config(
            &saved,
            &QuickTunnelRequest::Save {
                id: Some(LEGACY_QUICK_TUNNEL_ID),
                name: "legacy".into(),
                target: apps(),
            },
        )
        .unwrap();
        assert!(edited.quick_tunnel.is_none());
        assert_eq!(edited.quick_tunnels[0], saved.quick_tunnels[0]);
        assert_eq!(edited.quick_tunnels[1].id, LEGACY_QUICK_TUNNEL_ID);
        let deleted = updated_config(
            &saved,
            &QuickTunnelRequest::Delete {
                id: LEGACY_QUICK_TUNNEL_ID,
            },
        )
        .unwrap();
        assert!(deleted.quick_tunnel.is_none());
        assert_eq!(deleted.quick_tunnels, saved.quick_tunnels);
        assert!(saved.quick_tunnel.is_some());
    }

    #[test]
    fn invalid_intents_do_not_replace_the_saved_manifest() {
        let saved = saved_config();
        let before = saved.clone();
        let unknown = uuid::Uuid::from_u128(999);
        for request in [
            QuickTunnelRequest::Save {
                id: Some(unknown),
                name: "unknown".into(),
                target: apps(),
            },
            QuickTunnelRequest::Delete { id: unknown },
            QuickTunnelRequest::Save {
                id: None,
                name: "bad host".into(),
                target: firecrab_helper_protocol::pipe::QuickTunnelTarget::Routes {
                    host: "missing.test".into(),
                },
            },
        ] {
            assert!(updated_config(&saved, &request).is_err());
        }
        assert_eq!(saved, before);
    }

    #[test]
    fn accepts_only_the_reserved_legacy_or_uuid_based_unit_names() {
        assert!(managed_unit(TUNNEL_UNIT));
        assert!(managed_unit(&quick_tunnel_unit(uuid::Uuid::from_u128(42))));
        for unit in [
            "sshd.service",
            "firecrab-quick-tunnel-../sshd.service",
            "firecrab-quick-tunnel-abc.service",
            "firecrab-quick-tunnel-11111111-1111-4111-8111-111111111111.service",
        ] {
            assert!(!managed_unit(unit));
        }
    }

    #[test]
    fn independent_units_never_reuse_another_units_address() {
        let mut active = unit();
        active.unit = quick_tunnel_unit(uuid::Uuid::from_u128(42));
        let other = quick_tunnel_unit(uuid::Uuid::from_u128(43));
        for record_unit in [&active.unit, &other] {
            let mut runner = FakeCommandRunner::new();
            let record = serde_json::json!({ "_SYSTEMD_UNIT": record_unit, "_SYSTEMD_INVOCATION_ID": INVOCATION, "MESSAGE": "| https://individual-crab.trycloudflare.com |" }).to_string();
            runner.set(
                "journalctl",
                &[
                    "--unit",
                    &active.unit,
                    "--no-pager",
                    "--output=json",
                    "--lines=20",
                    URL_FILTER,
                    &format!("_SYSTEMD_INVOCATION_ID={INVOCATION}"),
                ],
                0,
                &record,
                "",
            );
            runner.set(
                "systemctl",
                &["show", "--property=ActiveState,InvocationID", &active.unit],
                0,
                &format!("ActiveState=active\nInvocationID={INVOCATION}\n"),
                "",
            );
            let actual = public_url(&runner, &active, INVOCATION);
            assert_eq!(actual.is_some(), record_unit == &active.unit);
        }
    }
}
