//! Host-side update entry point for managed Debian on macOS and Windows.

#[cfg(any(target_os = "macos", target_os = "windows"))]
use super::{artifact, release};

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Release(#[from] release::Error),
    #[error(transparent)]
    Artifact(#[from] artifact::Error),
    #[error("could not update the managed guest: {0}")]
    Guest(String),
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn run(check: bool, apply: bool, json: bool) -> Result<i32, Error> {
    if check || !apply {
        let mut command = vec![
            "/usr/local/bin/firecrab".into(),
            "update".into(),
            "--check".into(),
        ];
        if json {
            command.push("--json".into());
        }
        return super::run_guest_command(&command, None)
            .map_err(|error| Error::Guest(error.to_string()));
    }

    // Do not call the old guest CLI's --apply: its helper protocol may differ
    // from the running development helper. The verified installer upgrades the
    // API, CLI, helper and units together before starting the release API.
    let latest = release::latest()?;
    let installer = latest.artifact("install.sh", format!("install-firecrab-{}.sh", latest.tag))?;
    let staging = tempfile::tempdir().map_err(|error| Error::Guest(error.to_string()))?;
    artifact::fetch_all(
        &[installer.spec("Firecrab release installer")],
        staging.path(),
        true,
    )?;
    let input = std::fs::File::open(staging.path().join(&installer.filename))
        .map_err(|error| Error::Guest(error.to_string()))?;
    let command = vec![
        "bash".into(),
        "-c".into(),
        include_str!("../../../scripts/micromanager/update-guest.sh").replace('\r', ""),
        "--".into(),
        latest.tag.clone(),
        installer.digest,
    ];
    let code = super::run_guest_command(&command, Some(input))
        .map_err(|error| Error::Guest(error.to_string()))?;
    if code == 0 && json {
        println!(
            "{}",
            serde_json::json!({"version": latest.tag.trim_start_matches('v'), "applied": true})
        );
    }
    Ok(code)
}
