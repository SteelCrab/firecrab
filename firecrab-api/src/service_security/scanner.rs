//! Bounded background scans of the exact registry digest used by Deploy.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use crate::{
    handlers::micro_services::require_image_auth,
    oci::{ImageReference, RegistryCredential, Sha256Digest},
    persistence::security_scans::Scan,
    state::AppState,
};

const MAX_REPORT: usize = 16 * 1024 * 1024;
const RESCAN_SECONDS: i64 = 24 * 60 * 60;
const RETRY_SECONDS: i64 = 60 * 60;

/// Start one worker. Scans are serialized and never delay deployment.
pub(crate) fn start(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            cleanup_expired_credentials(Path::new("data/security-scanner"));
            if scan_pass(&state).await.is_err() {
                tracing::warn!("service security scan scheduling failed");
            }
        }
    });
}

async fn scan_pass(state: &AppState) -> Result<(), ()> {
    let store = state.store.clone();
    let candidates = tokio::task::spawn_blocking(
        move || -> Result<Vec<_>, crate::persistence::PersistenceError> {
            let mut candidates = Vec::new();
            for service in store.list_micro_services()? {
                if store.service_is_controller_owned(service.id)? {
                    continue;
                }
                let Some(deployment) = store.service_deployment(service.id)? else {
                    continue;
                };
                let Some(digest) = deployment.digest.as_ref() else {
                    continue;
                };
                let existing = store.security_scan(service.id, deployment.id, digest)?;
                let due = existing.as_ref().is_none_or(|scan| {
                    if scan.status == "queued" {
                        return true;
                    }
                    let interval = if scan.status == "complete" {
                        RESCAN_SECONDS
                    } else {
                        RETRY_SECONDS
                    };
                    chrono::Utc::now()
                        .timestamp()
                        .saturating_sub(scan.scanned_at)
                        >= interval
                });
                if due {
                    candidates.push((service.id, deployment, existing));
                }
            }
            Ok(candidates)
        },
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    for (service, deployment, expected) in candidates {
        let digest = deployment.digest.as_deref().unwrap_or_default();
        let mut scan = Scan {
            deployment_id: deployment.id,
            request_id: expected.as_ref().and_then(|scan| scan.request_id),
            digest: digest.into(),
            replica_ids: deployment
                .instances
                .iter()
                .map(|instance| instance.vm_id)
                .collect(),
            scanned_at: chrono::Utc::now().timestamp(),
            status: "unavailable".into(),
            findings: vec![],
            limitations: vec![],
            database_updated_at: None,
        };
        let result = scan_deployment(state, service, &deployment.spec.image, digest).await;
        match result {
            Ok((findings, updated, limitations)) => {
                scan.status = "complete".into();
                scan.findings = findings;
                scan.limitations = limitations;
                scan.database_updated_at = Some(updated);
            }
            Err(status) => scan.status = status.into(),
        }
        scan.scanned_at = chrono::Utc::now().timestamp();
        let store = state.store.clone();
        tokio::task::spawn_blocking(move || {
            store.save_security_scan_if_unchanged(service, &scan, expected.as_ref())
        })
        .await
        .map_err(|_| ())?
        .map_err(|_| ())?;
    }
    Ok(())
}

async fn scan_deployment(
    state: &AppState,
    service: Uuid,
    image: &str,
    digest: &str,
) -> Result<(Vec<super::cve::Finding>, String, Vec<String>), &'static str> {
    let reference = ImageReference::parse(image).map_err(|_| "invalid_image")?;
    let digest = Sha256Digest::parse(digest).map_err(|_| "invalid_image")?;
    let target = format!("{}/{}@{}", reference.registry, reference.repository, digest);
    let auth = require_image_auth(state, image, service)
        .await
        .map_err(|_| "registry_auth_unavailable")?;
    let credential = auth
        .registry_credential()
        .await
        .map_err(|_| "registry_auth_unavailable")?;
    if credential.registry != reference.registry {
        return Err("registry_auth_unavailable");
    }
    let executable = std::env::var_os("FIRECRAB_SECURITY_TRIVY").unwrap_or_else(|| "trivy".into());
    run(
        Path::new(&executable),
        Path::new("data/security-scanner"),
        &target,
        &credential,
    )
    .await
}

/// Remove abandoned private credentials after the maximum scan lifetime.
fn cleanup_expired_credentials(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with("auth-") {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() || kind.is_symlink() {
            continue;
        }
        let old = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|time| time.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(600));
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Use a private Docker config so credentials are scoped to one registry, never argv or logs.
async fn run(
    executable: &Path,
    root: &Path,
    target: &str,
    credential: &RegistryCredential,
) -> Result<(Vec<super::cve::Finding>, String, Vec<String>), &'static str> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    std::fs::create_dir_all(root).map_err(|_| "scanner_error")?;
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| "scanner_error")?;
    let root = std::fs::canonicalize(root).map_err(|_| "scanner_error")?;
    cleanup_expired_credentials(&root);
    let private = tempfile::Builder::new()
        .prefix("auth-")
        .tempdir_in(&root)
        .map_err(|_| "scanner_error")?;
    let config = private.path().join("config.json");
    let auth = STANDARD.encode(format!("{}:{}", credential.username, credential.secret));
    let document = serde_json::json!({"auths":{credential.registry.clone():{"auth":auth}}});
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(config)
        .map_err(|_| "scanner_error")?;
    use std::io::Write as _;
    file.write_all(document.to_string().as_bytes())
        .map_err(|_| "scanner_error")?;
    drop(file);
    let cache = root.join("cache");
    let platform = match std::env::consts::ARCH {
        "x86_64" => "linux/amd64",
        "aarch64" => "linux/arm64",
        _ => return Err("unsupported_platform"),
    };
    let mut cmd = tokio::process::Command::new(executable);
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", private.path())
        .env("DOCKER_CONFIG", private.path())
        .current_dir(private.path())
        .args([
            "image",
            "--config",
            "",
            "--ignorefile",
            "",
            "--quiet",
            "--no-progress",
            "--format",
            "json",
            "--scanners",
            "vuln",
            "--list-all-pkgs",
            "--image-src",
            "remote",
            "--platform",
            platform,
            "--timeout",
            "4m",
            "--cache-dir",
        ])
        .arg(&cache)
        .arg("--")
        .arg(target)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "scanner_missing"
        } else {
            "scanner_error"
        }
    })?;
    let report = output(child, MAX_REPORT).await?;
    let findings = super::cve::parse_report(&report, target).map_err(|_| "report_invalid")?;
    let limitations = super::cve::limitations(&report).map_err(|_| "report_invalid")?;
    let mut cmd = tokio::process::Command::new(executable);
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", private.path())
        .current_dir(private.path())
        .args(["version", "--config", "", "--format", "json", "--cache-dir"])
        .arg(cache)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(|_| "scanner_error")?;
    let version = output(child, 1024 * 1024).await?;
    let version: serde_json::Value =
        serde_json::from_slice(&version).map_err(|_| "database_unavailable")?;
    let updated = version["VulnerabilityDB"]["UpdatedAt"]
        .as_str()
        .ok_or("database_unavailable")?;
    let time = chrono::DateTime::parse_from_rfc3339(updated).map_err(|_| "database_unavailable")?;
    let age = chrono::Utc::now().signed_duration_since(time.with_timezone(&chrono::Utc));
    if age > chrono::Duration::hours(48) || age < -chrono::Duration::minutes(5) {
        return Err("database_stale");
    }
    Ok((findings, updated.into(), limitations))
}

async fn output(mut child: tokio::process::Child, limit: usize) -> Result<Vec<u8>, &'static str> {
    let stdout = child.stdout.take().ok_or("scanner_error")?;
    let work = async move {
        let mut bytes = Vec::new();
        stdout
            .take((limit + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "scanner_error")?;
        if bytes.len() > limit {
            return Err("report_too_large");
        }
        let status = child.wait().await.map_err(|_| "scanner_error")?;
        if !status.success() {
            return Err("scanner_error");
        }
        Ok(bytes)
    };
    tokio::time::timeout(Duration::from_secs(300), work)
        .await
        .map_err(|_| "scanner_timeout")?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn credential() -> RegistryCredential {
        RegistryCredential {
            registry: "ghcr.io".into(),
            username: "private-user".into(),
            secret: "private-token".into(),
        }
    }

    #[tokio::test]
    async fn missing_scanner_is_not_a_clean_result_and_auth_is_removed() {
        let root = tempfile::tempdir().unwrap();
        let outcome = run(
            Path::new("/definitely/missing/trivy"),
            root.path(),
            "ghcr.io/a/b@sha256:abc",
            &credential(),
        )
        .await;
        assert_eq!(outcome.unwrap_err(), "scanner_missing");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn runs_scanner_with_scoped_private_credentials_and_bounded_report() {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("fake-trivy");
        std::fs::write(&script, r##"#!/bin/sh
set -eu
if [ "$1" = version ]; then
  printf '{"VulnerabilityDB":{"UpdatedAt":"%s"}}' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  exit 0
fi
[ -f "$DOCKER_CONFIG/config.json" ]
case "$*" in *private-token*) exit 2;; esac
for arg do target="$arg"; done
printf '{"SchemaVersion":2,"ArtifactName":"%s","ArtifactType":"container_image","Metadata":{"RepoDigests":["%s"]},"Results":[{"Target":"alpine","Packages":[{"Name":"openssl"}],"Vulnerabilities":[{"VulnerabilityID":"CVE-2024-12345","PkgName":"openssl","InstalledVersion":"1","FixedVersion":"2","Severity":"HIGH"}]}]}' "$target" "$target"
"##).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let work = root.path().join("work");
        let (findings, updated, _) = run(&script, &work, "ghcr.io/a/b@sha256:abc", &credential())
            .await
            .unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].cve, "CVE-2024-12345");
        assert_eq!(findings[0].fixed_version.as_deref(), Some("2"));
        assert!(!updated.is_empty());
        assert_eq!(std::fs::read_dir(work).unwrap().count(), 0);
    }

    #[tokio::test]
    #[ignore = "requires a real Trivy executable, cached database and public registry access"]
    async fn real_pinned_image_reports_known_cves() {
        let executable = std::env::var("FIRECRAB_TEST_TRIVY").unwrap();
        let root = std::env::var("FIRECRAB_TEST_TRIVY_ROOT").unwrap();
        let target = "registry-1.docker.io/library/alpine@sha256:8fc3dacfb6d69da8d44e42390de777e48577085db99aa4e4af35f483eb08b989";
        let (findings, _, limitations) = run(
            Path::new(&executable),
            Path::new(&root),
            target,
            &credential(),
        )
        .await
        .unwrap();
        assert!(limitations.iter().any(|limit| limit == "os_end_of_support"));
        assert!(
            findings
                .iter()
                .any(|finding| finding.cve == "CVE-2025-15467" && finding.package == "libcrypto3")
        );
    }
}
