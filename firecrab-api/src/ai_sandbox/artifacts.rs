//! Exported artifact bytes are owned by an immutable task/attempt and survive guest cleanup.
use super::{SavedTask, files};
use firecrab_api_types::{SandboxArtifact, SandboxTestResult};
use sha2::{Digest, Sha256};
use std::{fs, io, os::unix::fs::PermissionsExt};
use uuid::Uuid;

/// Declare a test only after the associated guest command has an actual exit status.
pub fn record_test(saved: &mut SavedTask, name: &str, sequence: usize) -> Result<(), String> {
    if name.is_empty() || name.len() > 200 || name.chars().any(char::is_control) {
        return Err("Use a test name up to 200 bytes without control characters".into());
    }
    let event = saved
        .task
        .events
        .get(sequence)
        .ok_or("Unknown command event")?;
    if !matches!(event.name.as_str(), "run_command" | "run_argv") || event.exit_code.is_none() {
        return Err("Tests must refer to a completed guest command event".into());
    }
    if saved
        .task
        .tests
        .iter()
        .any(|test| test.name == name && test.event_sequence == sequence)
    {
        return Ok(());
    }
    if saved.task.tests.len() >= 64 {
        return Err("Task reached its 64-test record limit".into());
    }
    saved.task.tests.push(SandboxTestResult {
        name: name.into(),
        event_sequence: sequence,
        passed: event.exit_code == Some(0),
    });
    Ok(())
}

/// Export a bounded regular file. Provider credentials never become downloadable artifacts.
pub fn retain(
    saved: &mut SavedTask,
    path: &str,
    credentials: &[String],
) -> io::Result<SandboxArtifact> {
    let attempt = saved
        .workspace
        .as_ref()
        .ok_or_else(|| io::Error::other("Task workspace is unavailable"))?;
    let bytes = files::Root::open(&attempt.join("work"))?.read(path)?;
    if credentials
        .iter()
        .filter(|value| !value.is_empty())
        .any(|value| {
            bytes
                .windows(value.len())
                .any(|part| part == value.as_bytes())
        })
    {
        return Err(io::Error::other(
            "Artifact contains provider credentials and cannot be exported",
        ));
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if let Some(artifact) = saved
        .task
        .artifacts
        .iter()
        .find(|artifact| artifact.path == path && artifact.sha256 == digest)
    {
        read(saved, artifact.id)?;
        return Ok(artifact.clone());
    }
    if saved.task.artifacts.len() >= 16
        || saved
            .task
            .artifacts
            .iter()
            .map(|artifact| artifact.size)
            .sum::<usize>()
            + bytes.len()
            > 8 * files::FILE_LIMIT
    {
        return Err(io::Error::other(
            "Task artifacts exceed sixteen files or 8 MiB",
        ));
    }
    let directory = attempt.join("artifacts");
    fs::create_dir_all(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    let artifact = SandboxArtifact {
        id: Uuid::new_v4(),
        task_id: saved.task.id,
        attempt_id: saved.task.attempt_id,
        path: path.into(),
        size: bytes.len(),
        sha256: digest,
    };
    files::Root::open(&directory)?.write(&artifact.id.to_string(), &bytes)?;
    saved.task.artifacts.push(artifact.clone());
    Ok(artifact)
}

/// Verify ownership, size and digest before returning private exported bytes.
pub fn read(saved: &SavedTask, id: Uuid) -> io::Result<Vec<u8>> {
    let artifact = saved
        .task
        .artifacts
        .iter()
        .find(|artifact| artifact.id == id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Artifact is unavailable"))?;
    if artifact.task_id != saved.task.id || artifact.attempt_id != saved.task.attempt_id {
        return Err(io::Error::other("Artifact attempt identity does not match"));
    }
    let attempt = saved
        .workspace
        .as_ref()
        .ok_or_else(|| io::Error::other("Artifact workspace is unavailable"))?;
    let bytes = files::Root::open(&attempt.join("artifacts"))?.read(&id.to_string())?;
    if bytes.len() != artifact.size || format!("{:x}", Sha256::digest(&bytes)) != artifact.sha256 {
        return Err(io::Error::other(
            "Artifact export is missing or changed; cleanup is blocked",
        ));
    }
    Ok(bytes)
}

/// Environment management calls this before deleting any owned temporary resources.
pub fn verify_export(saved: &SavedTask) -> Result<(), String> {
    if !saved.results_exported {
        return Err("Task result export is not complete".into());
    }
    for artifact in &saved.task.artifacts {
        read(saved, artifact.id).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "artifacts_tests.rs"]
mod tests;
