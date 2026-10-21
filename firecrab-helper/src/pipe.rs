//! The helper owns fixed pipe controls. The API never starts traffic processes.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use firecrab_helper_protocol::network::HelperFailure;
use firecrab_helper_protocol::pipe::{Operation, Snapshot};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Locate the matching root-owned CLI through protected sibling or install-prefix paths.
fn cli() -> Result<PathBuf, HelperFailure> {
    use std::os::unix::fs::MetadataExt;
    let executable = std::env::current_exe().map_err(failure)?;
    let sibling = executable
        .parent()
        .unwrap_or(Path::new("/usr/local/bin"))
        .join("firecrab");
    let prefix = std::env::var("PREFIX").unwrap_or_else(|_| "/usr/local".into());
    for path in [sibling, PathBuf::from(prefix).join("bin/firecrab")] {
        // Check the original path too: a mutable symlink must not select a root program.
        let protected = path.ancestors().all(|component| {
            std::fs::symlink_metadata(component).is_ok_and(|m| {
                m.uid() == 0
                    && (m.file_type().is_symlink()
                        || m.mode() & 0o022 == 0
                        || (m.is_dir() && m.mode() & 0o1000 != 0))
            })
        });
        if protected && let Ok(resolved) = crate::vm_unit::trusted_executable(&path, 0) {
            return Ok(resolved);
        }
    }
    Err(HelperFailure::Internal {
        detail: "install the matching root-owned Firecrab CLI beside the helper".into(),
    })
}

/// Preserve CLI and IO error context in the helper's structured failure response.
fn failure(error: impl std::fmt::Display) -> HelperFailure {
    HelperFailure::Internal {
        detail: error.to_string(),
    }
}

/// Run a fixed Pipe action with bounded execution time and optional secret stdin.
async fn execute(
    cli: &Path,
    action: &str,
    input: Option<Vec<u8>>,
) -> Result<Vec<u8>, HelperFailure> {
    let mut command = Command::new(cli);
    command
        .args(["pipe", action])
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Parallel tests can fork while another test writes its stub CLI. The child
    // briefly inherits that writer, so Linux rejects exec with ETXTBSY. Match
    // the API's bounded test-only retry; installed binaries keep failing fast.
    let mut attempt = 0;
    let mut child = loop {
        match command.spawn() {
            Err(error)
                if cfg!(test)
                    && error.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && attempt < 8 =>
            {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(10 * attempt)).await;
            }
            result => break result.map_err(failure)?,
        }
    };
    let task = async {
        if let Some(input) = input {
            child
                .stdin
                .take()
                .ok_or_else(|| failure("missing pipe stdin"))?
                .write_all(&input)
                .await
                .map_err(failure)?;
        }
        let output = child.wait_with_output().await.map_err(failure)?;
        if !output.status.success() {
            return Err(failure(String::from_utf8_lossy(&output.stderr)));
        }
        Ok(output.stdout)
    };
    let timeout = match action {
        "firecrab-manage" => 450,
        "quick-connect" | "quick-manage" => 450,
        "cloudflare-connect" => 420,
        _ => 220,
    };
    tokio::time::timeout(Duration::from_secs(timeout), task)
        .await
        .map_err(|_| failure("pipe command timed out"))?
}

/// Validate a helper operation, execute its fixed CLI action and inspect the result.
pub(super) async fn run(request: Operation) -> Result<Snapshot, HelperFailure> {
    run_with_cli(&cli()?, request).await
}

/// Keep command execution separate from trusted executable discovery for isolated tests.
async fn run_with_cli(cli: &Path, request: Operation) -> Result<Snapshot, HelperFailure> {
    match request {
        Operation::ManageFirecrabTunnel { request } => {
            request
                .validate()
                .map_err(|detail| HelperFailure::InvalidRequest { detail })?;
            execute(
                cli,
                "firecrab-manage",
                Some(serde_json::to_vec(&request).map_err(failure)?),
            )
            .await?;
        }
        Operation::Inspect => {}
        Operation::DisconnectCloudflare => {
            execute(cli, "cloudflare-disconnect", None).await?;
        }
        Operation::Disable => {
            execute(cli, "disable", None).await?;
        }
        Operation::Control { request } => {
            execute(
                cli,
                "control",
                Some(serde_json::to_vec(&request).map_err(failure)?),
            )
            .await?;
        }
        Operation::ManageQuickTunnel { request } => {
            request
                .validate()
                .map_err(|error| HelperFailure::InvalidRequest {
                    detail: error.to_string(),
                })?;
            execute(
                cli,
                "quick-manage",
                Some(serde_json::to_vec(&request).map_err(failure)?),
            )
            .await?;
        }
        Operation::ConnectQuickTunnel { request } => {
            request
                .validate()
                .map_err(|error| HelperFailure::InvalidRequest {
                    detail: error.to_string(),
                })?;
            execute(
                cli,
                "quick-connect",
                Some(serde_json::to_vec(&request).map_err(failure)?),
            )
            .await?;
        }
        Operation::ConnectCloudflare { request } => {
            request
                .validate()
                .map_err(|detail| HelperFailure::InvalidRequest { detail })?;
            execute(
                cli,
                "cloudflare-connect",
                Some(serde_json::to_vec(&request).map_err(failure)?),
            )
            .await?;
        }
        Operation::Apply { request } => {
            request
                .config
                .validate()
                .map_err(|error| HelperFailure::InvalidRequest {
                    detail: error.to_string(),
                })?;
            if let Some(credentials) = &request.credentials {
                request
                    .config
                    .validate_credentials(credentials)
                    .map_err(|error| HelperFailure::InvalidRequest {
                        detail: error.to_string(),
                    })?;
            }
            let bytes = serde_json::to_vec(&request).map_err(failure)?;
            if bytes.len() > 48 * 1024 {
                return Err(HelperFailure::InvalidRequest {
                    detail: "pipe request exceeds 48 KiB".into(),
                });
            }
            execute(cli, "apply-managed", Some(bytes)).await?;
        }
    }
    serde_json::from_slice(&execute(cli, "inspect", None).await?).map_err(failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use firecrab_helper_protocol::pipe::{Service, ServiceAction, ServiceRequest};
    use std::os::unix::fs::PermissionsExt;

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_busy_cli_is_retried_without_losing_stdin_or_running_twice() {
        let directory = tempfile::tempdir().unwrap();
        let cli = directory.path().join("firecrab");
        std::fs::write(
            &cli,
            "#!/bin/sh\nprintf 'run\\n' >> \"${0%/*}/runs\"\ncat\n",
        )
        .unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        let writer = std::fs::OpenOptions::new().write(true).open(&cli).unwrap();
        assert_eq!(
            std::process::Command::new(&cli)
                .status()
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::ExecutableFileBusy
        );
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            drop(writer);
        });

        let input = b"private request".to_vec();
        let output = execute(&cli, "quick-manage", Some(input.clone())).await;
        releaser.join().unwrap();

        assert_eq!(output.unwrap(), input);
        assert_eq!(
            std::fs::read(directory.path().join("runs")).unwrap(),
            b"run\n"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_cli_that_stays_busy_returns_the_spawn_error() {
        let directory = tempfile::tempdir().unwrap();
        let cli = directory.path().join("firecrab");
        std::fs::write(&cli, "#!/bin/sh\nprintf run > \"${0%/*}/runs\"\n").unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _writer = std::fs::OpenOptions::new().write(true).open(&cli).unwrap();

        let error = tokio::time::timeout(Duration::from_secs(2), execute(&cli, "inspect", None))
            .await
            .expect("busy retries must be bounded")
            .unwrap_err();

        let expected = std::io::Error::from_raw_os_error(libc::ETXTBSY).to_string();
        assert!(matches!(error, HelperFailure::Internal { detail } if detail == expected));
        assert!(!directory.path().join("runs").exists());
    }

    #[tokio::test]
    async fn controls_only_the_fixed_cli_action_and_returns_a_fresh_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let cli = directory.path().join("firecrab");
        std::fs::write(
            &cli,
            r#"#!/bin/sh
location="${0%/*}"
case "$1:$2" in
  pipe:control) cat > "$location/control.json" ;;
  pipe:inspect) cat "$location/snapshot.json" ;;
  *) exit 23 ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        let expected: Snapshot = serde_json::from_value(serde_json::json!({
            "config": null,
            "units": [{ "unit": "firecrab-ingress.service", "loadState": "loaded", "activeState": "deactivating", "subState": "stop-sigterm", "mainPid": 42 }],
            "nginxAvailable": true,
            "cloudflaredAvailable": false
        })).unwrap();
        std::fs::write(
            directory.path().join("snapshot.json"),
            serde_json::to_vec(&expected).unwrap(),
        )
        .unwrap();
        let request = ServiceRequest {
            service: Service::Ingress,
            action: ServiceAction::Stop,
        };
        let actual = run_with_cli(
            &cli,
            Operation::Control {
                request: request.clone(),
            },
        )
        .await
        .unwrap();
        let received: ServiceRequest =
            serde_json::from_slice(&std::fs::read(directory.path().join("control.json")).unwrap())
                .unwrap();
        assert_eq!(received, request);
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn rejected_service_jobs_preserve_the_command_failure() {
        let directory = tempfile::tempdir().unwrap();
        let cli = directory.path().join("firecrab");
        std::fs::write(
            &cli,
            "#!/bin/sh\ncat >/dev/null\nprintf 'Cloudflare Tunnel is not configured' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        let failure = run_with_cli(
            &cli,
            Operation::Control {
                request: ServiceRequest {
                    service: Service::Tunnel,
                    action: ServiceAction::Start,
                },
            },
        )
        .await
        .unwrap_err();
        assert!(
            matches!(failure, HelperFailure::Internal { detail } if detail.contains("Cloudflare Tunnel is not configured"))
        );
    }

    #[tokio::test]
    async fn quick_connections_use_the_fixed_action_and_return_only_runtime_urls() {
        let directory = tempfile::tempdir().unwrap();
        let cli = directory.path().join("firecrab");
        std::fs::write(
            &cli,
            r#"#!/bin/sh
location="${0%/*}"
case "$1:$2" in
  pipe:quick-connect) cat > "$location/quick.json" ;;
  pipe:inspect) cat "$location/snapshot.json" ;;
  *) exit 23 ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        let request: firecrab_helper_protocol::pipe::QuickConnectRequest = serde_json::from_value(serde_json::json!({
            "config": { "routes": [], "tunnel": null, "quickTunnel": { "upstreams": ["127.0.0.1:9000"] } }
        })).unwrap();
        let expected: Snapshot = serde_json::from_value(serde_json::json!({
            "config": request.config.clone(), "units": [], "nginxAvailable": true,
            "cloudflaredAvailable": true, "quickTunnel": { "publicUrl": "https://new-crab.trycloudflare.com" }
        })).unwrap();
        std::fs::write(
            directory.path().join("snapshot.json"),
            serde_json::to_vec(&expected).unwrap(),
        )
        .unwrap();
        let actual = run_with_cli(
            &cli,
            Operation::ConnectQuickTunnel {
                request: request.clone(),
            },
        )
        .await
        .unwrap();
        let bytes = std::fs::read(directory.path().join("quick.json")).unwrap();
        assert_eq!(
            serde_json::from_slice::<firecrab_helper_protocol::pipe::QuickConnectRequest>(&bytes)
                .unwrap(),
            request
        );
        assert!(
            !String::from_utf8(bytes)
                .unwrap()
                .contains("trycloudflare.com")
        );
        assert_eq!(actual, expected);
    }
    #[tokio::test]
    async fn list_intents_and_named_disconnect_use_fixed_cli_actions() {
        let directory = tempfile::tempdir().unwrap();
        let cli = directory.path().join("firecrab");
        std::fs::write(
            &cli,
            r#"#!/bin/sh
location="${0%/*}"
case "$1:$2" in
pipe:quick-manage) test "$#" -eq 2 || exit 24; cat > "$location/intent.json" ;;
pipe:cloudflare-disconnect) printf done > "$location/disconnected" ;;
pipe:inspect) cat "$location/snapshot.json" ;;
*) exit 23 ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        let expected: Snapshot = serde_json::from_value(serde_json::json!({"config":null,"units":[],"nginxAvailable":true,"cloudflaredAvailable":true})).unwrap();
        std::fs::write(
            directory.path().join("snapshot.json"),
            serde_json::to_vec(&expected).unwrap(),
        )
        .unwrap();
        let request: firecrab_helper_protocol::pipe::QuickTunnelRequest = serde_json::from_value(serde_json::json!({"action":"save","name":"Web","target":{"type":"apps","upstreams":["127.0.0.1:9000"]}})).unwrap();
        assert_eq!(
            run_with_cli(
                &cli,
                Operation::ManageQuickTunnel {
                    request: request.clone()
                }
            )
            .await
            .unwrap(),
            expected
        );
        assert_eq!(
            serde_json::from_slice::<firecrab_helper_protocol::pipe::QuickTunnelRequest>(
                &std::fs::read(directory.path().join("intent.json")).unwrap()
            )
            .unwrap(),
            request
        );
        assert_eq!(
            run_with_cli(&cli, Operation::DisconnectCloudflare)
                .await
                .unwrap(),
            expected
        );
        assert!(directory.path().join("disconnected").exists());
    }
}
