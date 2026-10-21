//! Build, installation paths, and live runtime information for the Host tab.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use firecrab_api_types::{FirecrabInfo, FirecrabInfoResponse};
use tokio::process::Command;
use tokio::time::timeout;

use crate::state::AppState;

const API_UNIT: &str = "firecrab-api.service";
const HELPER_UNIT: &str = "firecrab-helper.service";
const LEGACY_HELPER_UNIT: &str = "firecrab-net-helper.service";
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

fn info_for(bind: Option<String>) -> FirecrabInfo {
    FirecrabInfo::from_env(
        env!("CARGO_PKG_VERSION"),
        &format!("http://{}", crate::server::bind_addr_or_default(bind)),
    )
}

pub async fn get_info(State(state): State<AppState>) -> Json<FirecrabInfoResponse> {
    Json(
        collect(
            info_for(std::env::var("FIRECRAB_BIND_ADDR").ok()),
            &state.runtime.firecracker_binary,
            Path::new("systemctl"),
        )
        .await,
    )
}

async fn collect(info: FirecrabInfo, firecracker: &Path, systemctl: &Path) -> FirecrabInfoResponse {
    // A missing or stalled tool must not hide the other Host information.
    let (version, api, helper, legacy_helper) = tokio::join!(
        command_stdout(firecracker, &["--version"]),
        service_state(systemctl, API_UNIT),
        service_state(systemctl, HELPER_UNIT),
        service_state(systemctl, LEGACY_HELPER_UNIT),
    );
    let (helper_unit, helper_state) = match (helper, legacy_helper) {
        (Some(state), _) => (HELPER_UNIT, state),
        (None, Some(state)) => (LEGACY_HELPER_UNIT, state),
        (None, None) => (HELPER_UNIT, "unknown".to_owned()),
    };
    FirecrabInfoResponse {
        info,
        firecracker_version: version.as_deref().and_then(firecracker_version),
        api_service: api.unwrap_or_else(|| "unknown".to_owned()),
        net_helper_service: helper_state,
        helper_unit: helper_unit.to_owned(),
    }
}

async fn command_stdout(program: &Path, args: &[&str]) -> Option<String> {
    let output = timeout(
        PROBE_TIMEOUT,
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

async fn service_state(systemctl: &Path, unit: &str) -> Option<String> {
    let output = command_stdout(
        systemctl,
        &["show", "--property=LoadState,ActiveState", unit],
    )
    .await?;
    parse_service_state(&output)
}

fn parse_service_state(output: &str) -> Option<String> {
    let property = |name: &str| {
        output.lines().find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key == name).then_some(value)
        })
    };
    let load = property("LoadState")?;
    if load.is_empty() || load == "not-found" {
        return None;
    }
    let active = property("ActiveState")?;
    if active.is_empty() {
        return None;
    }
    Some(active.to_owned())
}

fn firecracker_version(output: &str) -> Option<String> {
    let version = output
        .trim()
        .strip_prefix("Firecracker ")?
        .split_whitespace()
        .next()?;
    let number = version.strip_prefix('v')?;
    if number.is_empty() {
        return None;
    }
    Some(version.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn executable(directory: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = directory.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn fixture_info() -> FirecrabInfo {
        FirecrabInfo::from_lookup("0.3.1", "http://127.0.0.1:5523", |_| None)
    }

    #[test]
    fn the_running_version_and_the_default_address_are_reported() {
        let info = info_for(None);
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.api_base, "http://127.0.0.1:5523");
    }

    #[test]
    fn the_address_the_api_listens_on_is_reported() {
        assert_eq!(
            info_for(Some("0.0.0.0:8443".to_owned())).api_base,
            "http://0.0.0.0:8443"
        );
    }

    #[test]
    fn service_states_include_failed_masked_and_transitional_units() {
        for state in ["active", "inactive", "failed", "activating", "deactivating"] {
            assert_eq!(
                parse_service_state(&format!("ActiveState={state}\nLoadState=loaded\n")),
                Some(state.to_owned())
            );
        }
        assert_eq!(
            parse_service_state("LoadState=masked\nActiveState=inactive\n"),
            Some("inactive".to_owned())
        );
        for output in [
            "",
            "LoadState=not-found\nActiveState=inactive\n",
            "LoadState=loaded\n",
        ] {
            assert_eq!(parse_service_state(output), None);
        }
    }

    #[test]
    fn the_firecracker_version_does_not_include_build_details() {
        assert_eq!(
            firecracker_version("Firecracker v1.17.0\nRevision: abc123\n"),
            Some("v1.17.0".to_owned())
        );
        for output in ["", "not Firecracker", "Firecracker v", "Firecracker"] {
            assert_eq!(firecracker_version(output), None);
        }
    }

    #[tokio::test]
    async fn the_configured_binary_and_legacy_helper_are_reported() {
        let directory = tempfile::tempdir().unwrap();
        let binary = executable(
            directory.path(),
            "custom-firecracker",
            "test \"$1\" = --version || exit 1\nprintf 'Firecracker v1.17.0\\n'",
        );
        let systemctl = executable(
            directory.path(),
            "systemctl",
            r#"
case "$3" in
  firecrab-helper.service) printf 'LoadState=not-found\nActiveState=inactive\n' ;;
  firecrab-net-helper.service) printf 'LoadState=loaded\nActiveState=failed\n' ;;
  firecrab-api.service) printf 'LoadState=loaded\nActiveState=active\n' ;;
  *) exit 1 ;;
esac
"#,
        );
        let response = collect(fixture_info(), &binary, &systemctl).await;
        assert_eq!(response.firecracker_version.as_deref(), Some("v1.17.0"));
        assert_eq!(response.api_service, "active");
        assert_eq!(response.net_helper_service, "failed");
        assert_eq!(response.helper_unit, LEGACY_HELPER_UNIT);
    }

    #[tokio::test]
    async fn a_failed_canonical_helper_is_not_hidden_by_the_legacy_unit() {
        let directory = tempfile::tempdir().unwrap();
        let systemctl = executable(
            directory.path(),
            "systemctl",
            r#"
case "$3" in
  firecrab-helper.service) printf 'LoadState=loaded\nActiveState=failed\n' ;;
  *) printf 'LoadState=loaded\nActiveState=active\n' ;;
esac
"#,
        );
        let response = collect(
            fixture_info(),
            &directory.path().join("missing"),
            &systemctl,
        )
        .await;
        assert_eq!(response.firecracker_version, None);
        assert_eq!(response.net_helper_service, "failed");
        assert_eq!(response.helper_unit, HELPER_UNIT);
        assert_eq!(response.api_service, "active");
    }

    #[tokio::test]
    async fn unavailable_tools_preserve_build_and_install_information() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing");
        let response = collect(fixture_info(), &missing, &missing).await;
        assert_eq!(response.info, fixture_info());
        assert_eq!(response.firecracker_version, None);
        assert_eq!(response.api_service, "unknown");
        assert_eq!(response.net_helper_service, "unknown");
    }

    #[tokio::test]
    async fn a_stalled_probe_is_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let binary = executable(directory.path(), "stalled", "exec sleep 30");
        assert!(command_stdout(&binary, &["--version"]).await.is_none());
    }
}
