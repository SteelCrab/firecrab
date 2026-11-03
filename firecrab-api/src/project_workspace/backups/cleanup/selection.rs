//! Age/count selection is a preview; deletion admission verifies a surviving complete archive.
use super::*;

/// A completed but tombstoned archive is history, not a retained checkpoint slot.
fn checkpoint(r: &Backup) -> bool {
    r.public.phase == "complete"
        && r.public.request.restore_checkpoint.is_none()
        && r.public.cleanup_id.is_none()
}
/// Combine count, age, protection and manual-backup rules without admitting deletion.
pub(super) fn preview(
    state: &AppState,
    project: Uuid,
    policy: &ProjectBackupRetention,
) -> Result<ProjectBackupCleanupPreview, Error> {
    // Registry admission order remains stable even when timestamps tie or the clock moves.
    let rows = state.store.backup_records(Some(project))?;
    let recent: Vec<_> = rows
        .iter()
        .filter(|r| checkpoint(r))
        .take(policy.keep_latest as usize)
        .map(|r| r.public.id)
        .collect();
    let count = rows.iter().filter(|r| checkpoint(r)).count();
    let busy = fenced(&state.workspaces, project)?
        || state.store.workspace_backup_active(project)?
        || state.store.workspace_cleanup_active(project)?;
    let mut candidates = vec![];
    let mut automatic_target_ids = vec![];
    for r in rows {
        if r.public.cleanup_id.is_some() {
            continue;
        }
        let kind = if r.public.phase == "aborted" {
            "partial"
        } else if r.public.request.restore_checkpoint.is_some() {
            "restore_originals"
        } else {
            "checkpoint"
        };
        let reason = if busy || r.active {
            Some("Finish or reconcile the active project maintenance operation first")
        } else if r.public.protected {
            Some("Unprotect this checkpoint before deleting it")
        } else if !matches!(r.public.phase.as_str(), "complete" | "aborted")
            || r.public.error.is_some()
        {
            Some("Finish or abort this operation before cleaning its artifacts")
        } else if kind == "checkpoint" && count <= 1 {
            Some("Keep at least one verified complete checkpoint")
        } else {
            None
        };
        let eligible = reason.is_none();
        if eligible
            && kind == "checkpoint"
            && !recent.contains(&r.public.id)
            && (r.scheduled || policy.include_manual)
            && policy.max_age_days.is_none_or(|days| {
                r.public.created_at <= ai_sandbox::now().saturating_sub(u64::from(days) * 86400)
            })
        {
            automatic_target_ids.push(r.public.id);
        }
        candidates.push(ProjectBackupCleanupCandidate {
            id: r.public.id,
            kind: kind.into(),
            created_at: r.public.created_at,
            eligible,
            protected: r.public.protected,
            reason: reason.map(str::to_owned),
        });
    }
    automatic_target_ids.truncate(64);
    Ok(ProjectBackupCleanupPreview {
        candidates,
        automatic_target_ids,
    })
}
/// Recheck the entire explicit selection against terminal state and retained resource identities.
pub(super) fn targets(state: &AppState, b: &Binding, ids: &[Uuid]) -> Result<Vec<Backup>, Error> {
    let mut rows = Vec::with_capacity(ids.len());
    for id in ids {
        let r = state
            .store
            .backup_record(*id)?
            .filter(|r| r.public.project_id == b.project_id)
            .ok_or(Error::Missing)?;
        if r.active
            || r.public.error.is_some()
            || r.public.protected
            || r.public.cleanup_id.is_some()
            || !matches!(r.public.phase.as_str(), "complete" | "aborted")
        {
            return Err(Error::Conflict(
                "Cleanup targets must be finished, unprotected and not already selected",
            ));
        }
        if r.binding.database_id != b.database_id
            || r.binding.volume_id != b.volume_id
            || r.binding.generation > b.generation
        {
            return Err(Error::Conflict("Cleanup target retained binding changed"));
        }
        rows.push(r);
    }
    Ok(rows)
}
/// Require a separate fully verified archive before deleting completed recovery artifacts.
pub(super) fn survivor(
    state: &AppState,
    b: &Binding,
    targets: &[Backup],
) -> Result<Option<Uuid>, Error> {
    if !targets.iter().any(|r| r.public.phase == "complete") {
        return Ok(None);
    }
    let root = root(&state.workspaces, b.project_id, false)?;
    for r in state.store.backup_records(Some(b.project_id))? {
        if checkpoint(&r)
            && !r.active
            && r.public.error.is_none()
            && !targets.iter().any(|t| t.public.id == r.public.id)
            && archive::completed(state, &root, b, r.public.id).is_ok()
        {
            return Ok(Some(r.public.id));
        }
    }
    Err(Error::Conflict(
        "Cleanup must retain at least one checksum-verified complete checkpoint",
    ))
}
