//! Destructive-step ownership and prospective replacement UUID reservations.

use std::collections::BTreeSet;

use firecrab_stack::{
    Action, ApplyState, ApplyStatus, Binding, ResourceKind, StackRecord, StepState,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use super::{StoredApply, encode, read};
use crate::persistence::{PersistenceError, Store};

pub(super) fn validate(
    conn: &Connection,
    record: &StackRecord,
    status: &ApplyStatus,
) -> Result<(), PersistenceError> {
    let mut creates = BTreeSet::new();
    let mut deletes = BTreeSet::new();
    for step in &status.changes {
        let change = &step.change;
        if (step.replacement_for.is_some() && change.action != Action::Create)
            || (step.vm_disk.is_some()
                && (change.kind != ResourceKind::Vm || change.action != Action::Delete))
            || matches!(change.action, Action::Replace | Action::Blocked)
        {
            return Err(PersistenceError::StackApplyIdentity);
        }
        let Some(resource) = change.resource_id else {
            return Err(PersistenceError::StackApplyIdentity);
        };
        if change.action == Action::Create {
            if resource.is_nil()
                || !creates.insert((change.kind, resource))
                || exists(conn, change.kind, resource)?
                || conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM stack_bindings WHERE kind=?1 AND resource_id=?2)",
                    params![change.kind.as_str(), resource.to_string()],
                    |r| r.get::<_, bool>(0),
                )?
                || conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM stack_apply_reservations WHERE kind=?1 AND resource_id=?2)",
                    params![change.kind.as_str(), resource.to_string()],
                    |r| r.get::<_, bool>(0),
                )?
            {
                return Err(PersistenceError::StackApplyIdentity);
            }
            if let Some(old) = step.replacement_for
                && (!status.lifecycle.replace
                    || old == resource
                    || !status.changes.iter().any(|s| {
                        s.change.kind == change.kind
                            && s.change.key == change.key
                            && s.change.action == Action::Delete
                            && s.change.resource_id == Some(old)
                    }))
            {
                return Err(PersistenceError::StackApplyIdentity);
            }
        }
        if change.action == Action::Delete {
            if !deletes.insert((change.kind, change.key.clone())) {
                return Err(PersistenceError::StackApplyIdentity);
            }
            if !record
                .bindings
                .iter()
                .any(|b| b.kind == change.kind && b.key == change.key && b.resource_id == resource)
            {
                return Err(PersistenceError::StaleStackPlan);
            }
            let replacing = status.changes.iter().any(|s| {
                s.change.kind == change.kind
                    && s.change.key == change.key
                    && s.replacement_for == Some(resource)
            });
            if !(if replacing {
                status.lifecycle.replace
            } else {
                status.lifecycle.prune
            }) {
                return Err(PersistenceError::StackApplyIdentity);
            }
            verify_disk(conn, step)?;
            if change.kind == ResourceKind::Vm && exists(conn, change.kind, resource)? {
                crate::persistence::stacks::validate_resource(
                    conn,
                    &Binding {
                        kind: change.kind,
                        key: change.key.clone(),
                        resource_id: resource,
                    },
                )?;
            }
        }
    }
    Ok(())
}

pub(super) fn reserve(conn: &Connection, status: &ApplyStatus) -> Result<(), PersistenceError> {
    for step in status
        .changes
        .iter()
        .filter(|s| s.change.action == Action::Create)
    {
        conn.execute("INSERT INTO stack_apply_reservations (kind,resource_id,operation_id) VALUES (?1,?2,?3)",
            params![step.change.kind.as_str(), step.change.resource_id.ok_or(PersistenceError::StackApplyIdentity)?.to_string(), status.operation_id.to_string()])?;
    }
    Ok(())
}

impl Store {
    /// All active lease owners, including leases whose VM record is missing.
    pub(crate) fn stack_lifecycle_leases(
        &self,
    ) -> Result<Vec<(Uuid, Option<Uuid>)>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT vm_id,micro_network_id FROM network_leases WHERE released_at IS NULL",
        )?;
        let rows = statement.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        rows.map(|row| {
            let (vm, network) = row?;
            Ok((
                Uuid::parse_str(&vm).map_err(|_| PersistenceError::StackApplyIdentity)?,
                network
                    .map(|v| Uuid::parse_str(&v).map_err(|_| PersistenceError::StackApplyIdentity))
                    .transpose()?,
            ))
        })
        .collect()
    }

    /// Verify the running step and exact binding immediately before backend deletion.
    pub(crate) fn guard_stack_delete(
        &self,
        operation: Uuid,
        index: usize,
        binding: &Binding,
    ) -> Result<(), PersistenceError> {
        let conn = self.lock();
        let job = read(&conn, operation)?.ok_or(PersistenceError::MissingStackApply)?;
        guard(&conn, &job, index, Some(binding))?;
        verify_disk(&conn, &job.status.changes[index])
    }

    /// Deletion calls this while holding its resource lock, before touching artifacts.
    pub(crate) fn guard_stack_delete_resource(
        &self,
        operation: Uuid,
        kind: ResourceKind,
        resource: Uuid,
    ) -> Result<(), PersistenceError> {
        let conn = self.lock();
        let job = read(&conn, operation)?.ok_or(PersistenceError::MissingStackApply)?;
        let index = job
            .status
            .changes
            .iter()
            .position(|s| {
                s.change.kind == kind
                    && s.change.resource_id == Some(resource)
                    && s.change.action == Action::Delete
            })
            .ok_or(PersistenceError::StackApplyIdentity)?;
        guard(&conn, &job, index, None)?;
        if exists(&conn, kind, resource)? {
            crate::persistence::stacks::validate_resource(
                &conn,
                &Binding {
                    kind,
                    key: job.status.changes[index].change.key.clone(),
                    resource_id: resource,
                },
            )?;
        }
        verify_disk(&conn, &job.status.changes[index])
    }

    /// Recheck all external network references under the network mutation lock.
    pub(crate) fn stack_network_has_dependents(&self, id: Uuid) -> Result<bool, PersistenceError> {
        let conn = self.lock();
        let used: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM vms WHERE micro_network_id=?1)
             OR EXISTS(SELECT 1 FROM network_leases WHERE released_at IS NULL
                       AND (micro_network_id=?1 OR micro_network_id IS NULL))",
            [id.to_string()],
            |r| r.get(0),
        )?;
        if used {
            return Ok(true);
        }
        let mut statement = conn.prepare("SELECT deployment FROM micro_service_deployments")?;
        let rows = statement.query_map([], |r| r.get::<_, String>(0))?;
        for row in rows {
            let deployment: firecrab_api_types::MicroServiceDeployment =
                serde_json::from_str(&row?).map_err(|_| PersistenceError::StackApplyIdentity)?;
            if deployment.request.micro_network_id == Some(id) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Commit deletion progress and release/transfer ownership together. A crash
    /// before this transaction leaves the old binding and both journaled UUIDs.
    pub(crate) fn finish_stack_delete(
        &self,
        operation: Uuid,
        index: usize,
    ) -> Result<ApplyStatus, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut job = read(&tx, operation)?.ok_or(PersistenceError::MissingStackApply)?;
        guard(&tx, &job, index, None)?;
        let change = &job.status.changes[index].change;
        let old = change
            .resource_id
            .ok_or(PersistenceError::StackApplyIdentity)?;
        if exists(&tx, change.kind, old)? {
            return Err(PersistenceError::StaleStackPlan);
        }
        let replacement = job.status.changes.iter().find(|s| {
            s.change.kind == change.kind
                && s.change.key == change.key
                && s.replacement_for == Some(old)
        });
        crate::persistence::iac_owners::native_binding_changed(&tx, change.kind.as_str(), old)?;
        let changed = if let Some(replacement) = replacement {
            crate::persistence::iac_owners::native_binding_changed(
                &tx,
                change.kind.as_str(),
                replacement
                    .change
                    .resource_id
                    .ok_or(PersistenceError::StackApplyIdentity)?,
            )?;
            tx.execute("UPDATE stack_bindings SET resource_id=?1 WHERE stack_id=?2 AND kind=?3 AND logical_key=?4 AND resource_id=?5",
                params![replacement.change.resource_id.ok_or(PersistenceError::StackApplyIdentity)?.to_string(),job.status.stack_id.to_string(),change.kind.as_str(),change.key,old.to_string()])?
        } else {
            tx.execute("DELETE FROM stack_bindings WHERE stack_id=?1 AND kind=?2 AND logical_key=?3 AND resource_id=?4",
                params![job.status.stack_id.to_string(),change.kind.as_str(),change.key,old.to_string()])?
        };
        if changed != 1 {
            return Err(PersistenceError::StaleStackPlan);
        }
        tx.execute(
            "UPDATE stacks SET revision=revision+1 WHERE id=?1",
            [job.status.stack_id.to_string()],
        )?;
        job.status.changes[index].state = StepState::Succeeded;
        tx.execute(
            "UPDATE stack_applies SET status=?1 WHERE operation_id=?2 AND state='running'",
            params![encode(&job.status)?, operation.to_string()],
        )?;
        tx.commit()?;
        Ok(job.status)
    }
}

fn guard(
    conn: &Connection,
    job: &StoredApply,
    index: usize,
    expected: Option<&Binding>,
) -> Result<(), PersistenceError> {
    let step = job
        .status
        .changes
        .get(index)
        .ok_or(PersistenceError::StackApplyIdentity)?;
    let change = &step.change;
    if job.status.state != ApplyState::Running
        || step.state != StepState::Running
        || change.action != Action::Delete
    {
        return Err(PersistenceError::StaleStackPlan);
    }
    let resource = change
        .resource_id
        .ok_or(PersistenceError::StackApplyIdentity)?;
    let replacing = job.status.changes.iter().any(|s| {
        s.change.action == Action::Create
            && s.change.kind == change.kind
            && s.change.key == change.key
            && s.replacement_for == Some(resource)
    });
    if !(if replacing {
        job.status.lifecycle.replace
    } else {
        job.status.lifecycle.prune
    }) {
        return Err(PersistenceError::StackApplyIdentity);
    }
    if expected
        .is_some_and(|b| b.kind != change.kind || b.key != change.key || b.resource_id != resource)
    {
        return Err(PersistenceError::StackApplyIdentity);
    }
    let owner: Option<String> = conn.query_row("SELECT resource_id FROM stack_bindings WHERE stack_id=?1 AND kind=?2 AND logical_key=?3", params![job.status.stack_id.to_string(), change.kind.as_str(), change.key], |r| r.get(0)).optional()?;
    if owner.as_deref() != Some(resource.to_string().as_str()) {
        return Err(PersistenceError::StaleStackPlan);
    }
    Ok(())
}

fn verify_disk(
    conn: &Connection,
    step: &firecrab_stack::ApplyStep,
) -> Result<(), PersistenceError> {
    if step.change.kind != ResourceKind::Vm {
        return Ok(());
    }
    let disk: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT storage_root,disk_generation FROM vms WHERE id=?1",
            [step
                .change
                .resource_id
                .ok_or(PersistenceError::StackApplyIdentity)?
                .to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match (disk, &step.vm_disk) {
        (None, None) => Ok(()),
        (Some((root, generation)), Some(expected))
            if root == expected.storage_root
                && generation == expected.generation.map(|v| v.to_string()) =>
        {
            Ok(())
        }
        _ => Err(PersistenceError::StaleStackPlan),
    }
}

fn exists(conn: &Connection, kind: ResourceKind, resource: Uuid) -> Result<bool, PersistenceError> {
    let table = match kind {
        ResourceKind::Network => "micro_networks",
        ResourceKind::Vm => "vms",
        ResourceKind::Service => "micro_services",
    };
    Ok(conn.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id=?1)"),
        [resource.to_string()],
        |r| r.get(0),
    )?)
}
