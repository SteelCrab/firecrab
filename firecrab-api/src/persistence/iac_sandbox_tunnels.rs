//! Exact preview binding ownership and native mutation fences share the SQLite transaction.

use super::PersistenceError as Error;
use firecrab_api_types::{SandboxTunnelBinding, SandboxTunnelState};
use firecrab_stack::ManagedResource;
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

/// Stable binding UUID; its project, attempt, runtime and address remain immutable.
pub(super) fn resource(id: Uuid) -> ManagedResource {
    ManagedResource {
        kind: "sandbox_tunnel".into(),
        id: id.to_string(),
    }
}

/// Public and legacy native writers cannot stand in for the declared workflow executor.
pub(super) fn ensure_native_write(conn: &Connection, id: Uuid) -> Result<(), Error> {
    let resource = resource(id);
    super::iac_owners::ensure_unowned(conn, &resource)?;
    if !super::iac_owners::public_write_allowed(conn, &resource)? {
        return Err(Error::IacOwnershipConflict);
    }
    Ok(())
}

/// Admit only stable original task/runtime bindings, before changing owner metadata.
pub(super) fn validate(
    conn: &Connection,
    resource: &ManagedResource,
    adopting: bool,
) -> Result<(), Error> {
    let id = Uuid::parse_str(&resource.id)
        .ok()
        .filter(|id| !id.is_nil())
        .ok_or(Error::InvalidIacOwner)?;
    let saved: Option<String> = conn
        .query_row(
            "SELECT spec FROM ai_sandbox_tunnels WHERE id=?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    let Some(saved) = saved else {
        return if adopting {
            Err(Error::InvalidStackResource)
        } else {
            Ok(())
        };
    };
    let binding: SandboxTunnelBinding =
        serde_json::from_str(&saved).map_err(|_| Error::InvalidStackResource)?;
    if binding.id != id || binding.task_id != binding.spec.task_id {
        return Err(Error::InvalidStackResource);
    }
    if matches!(
        binding.state,
        SandboxTunnelState::Connecting
            | SandboxTunnelState::Stopping
            | SandboxTunnelState::Deleting
            | SandboxTunnelState::NeedsReconciliation
    ) || conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM ai_sandbox_tunnels WHERE
            (id=?1 OR json_extract(spec,'$.successorBindingId')=?1)
            AND json_extract(spec,'$.successorBindingId') IS NOT NULL
            AND json_extract(spec,'$.handoffConfirmedAt') IS NULL)",
        [id.to_string()],
        |row| row.get::<_, bool>(0),
    )? {
        return Err(Error::IacResourceBusy);
    }
    if !adopting {
        return Ok(());
    }
    if matches!(
        binding.state,
        SandboxTunnelState::Deleted | SandboxTunnelState::Superseded
    ) {
        return Err(Error::InvalidStackResource);
    }
    super::iac_native::ensure_project_available(conn, binding.project_id)?;
    let task: Option<String> = conn
        .query_row(
            "SELECT spec FROM ai_sandbox_tasks WHERE id=?1",
            [binding.task_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    let task: crate::ai_sandbox::SavedTask =
        serde_json::from_str(&task.ok_or(Error::InvalidStackResource)?)
            .map_err(|_| Error::InvalidStackResource)?;
    if task.task.id != binding.task_id
        || task.task.project_id != binding.project_id
        || task.task.attempt_id != binding.attempt_id
        || task.task.runtime != Some(binding.runtime)
        || task.caller.principal != binding.owner
        || !conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM ai_sandbox_projects WHERE id=?1)",
            [binding.project_id.to_string()],
            |row| row.get::<_, bool>(0),
        )?
    {
        return Err(Error::InvalidStackResource);
    }
    if task.task.state.active() || task.guest_pending || task.guest_staged {
        return Err(Error::IacResourceBusy);
    }
    Ok(())
}
