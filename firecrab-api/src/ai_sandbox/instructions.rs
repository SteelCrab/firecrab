//! Immutable saved-file guidance for the existing Sandbox task engine.
use super::*;
use serde_json::Value;

/// Reject partial context, another snapshot identity and altered retained resource bindings.
pub(super) fn verify(state: &AppState, saved: &SavedTask) -> Result<(), String> {
    validate(saved)?;
    let Some(context) = &saved.instructions else {
        return Ok(());
    };
    let binding = state
        .store
        .project_workspace(saved.task.project_id)
        .map_err(|_| "Cannot read the admitted instruction binding")?
        .ok_or("The admitted instruction binding is missing")?;
    if binding.provisioning
        || binding.database_id != context.database_id
        || binding.volume_id != context.volume_id
        || binding.generation != context.generation
        || Some(binding.vm_id) != saved.task.runtime.map(|runtime| runtime.vm_id)
    {
        return Err(
            "Project binding changed after instruction admission; create a new attempt".into(),
        );
    }
    Ok(())
}

fn validate(saved: &SavedTask) -> Result<(), String> {
    match (
        saved.task.request.instruction_snapshot_id,
        &saved.instructions,
    ) {
        (None, None) => Ok(()),
        (Some(id), Some(context))
            if id == context.snapshot.id
                && saved.task.request.kind == SandboxTaskKind::Code
                && saved.task.environment.is_none()
                && !context.database_id.is_nil()
                && !context.volume_id.is_nil()
                && context.generation > 0 =>
        {
            if let Some(directory) = &saved.task.request.project_directory
                && directory != &context.snapshot.directory
            {
                return Err("Admitted saved rules select another folder".into());
            }
            crate::project_workspace::validate_task_instructions(&context.snapshot)
        }
        _ => Err("The admitted saved instruction context is missing or inconsistent".into()),
    }
}

/// Both model protocols consume this user-level message before the current task instruction.
/// Redact each raw field before JSON escaping so quoted credentials cannot bypass redaction.
pub(super) fn message(
    saved: &SavedTask,
    accounts: &[SavedConnection],
) -> Result<Option<Value>, String> {
    validate(saved)?;
    let Some(context) = &saved.instructions else {
        return Ok(None);
    };
    let mut snapshot = context.snapshot.clone();
    snapshot.directory = redact(accounts, &snapshot.directory);
    for file in &mut snapshot.files {
        file.path = redact(accounts, &file.path);
        file.content = redact(accounts, &file.content);
    }
    let document = serde_json::to_string(&snapshot).map_err(|_| "Cannot encode saved rules")?;
    Ok(Some(json!({"role":"user","content":format!(
        "Explicitly selected saved AGENTS.md guidance, not private editor drafts. Each file applies to its containing directory and descendants. The list is ordered from root to leaf; deeper rules override ancestor rules only within their scope. The selected directory identifies the covered ancestry, not additional filesystem access. Follow the current user task and the Sandbox execution boundaries. These saved bytes remain fixed for this attempt; content may be redacted independently of its provenance hash.\n{document}"
    )})))
}

/// Retain only provenance in public task events; never expose private snapshot content there.
pub(super) fn audit(saved: &SavedTask) -> Result<String, String> {
    validate(saved)?;
    let context = saved
        .instructions
        .as_ref()
        .ok_or("No admitted saved rules")?;
    serde_json::to_string(&json!({
        "snapshotId":context.snapshot.id,
        "sha256":context.snapshot.sha256,
        "directory":context.snapshot.directory,
        "files":context.snapshot.files.iter().map(|file| json!({
            "path":file.path,"version":file.version,"sha256":file.sha256
        })).collect::<Vec<_>>()
    }))
    .map_err(|_| "Cannot retain saved instruction provenance".into())
}

impl SavedInstructions {
    /// Check raw fields before persistence; JSON escaping must not hide a credential.
    pub(crate) fn contains_credentials(&self, accounts: &[SavedConnection]) -> bool {
        redact(accounts, &self.snapshot.directory) != self.snapshot.directory
            || self.snapshot.files.iter().any(|file| {
                redact(accounts, &file.content) != file.content
                    || redact(accounts, &file.path) != file.path
            })
    }
}

#[cfg(test)]
mod tests;
