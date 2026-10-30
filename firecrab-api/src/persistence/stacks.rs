//! Host-local stack ownership. Binding writes are atomic and never mutate resources.

use std::str::FromStr;

use firecrab_api_types::MicroServiceDeployment;
use firecrab_stack::{Binding, ResourceKind, StackRecord, logical_name};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use super::{PersistenceError, Store};

/// Only identities and UUID mappings are persisted; manifests/env values are not stored here.
pub(super) const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS stacks (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0)
) STRICT;
CREATE TABLE IF NOT EXISTS stack_bindings (
    stack_id TEXT NOT NULL REFERENCES stacks(id),
    kind TEXT NOT NULL CHECK (kind IN ('network', 'vm', 'service')),
    logical_key TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    PRIMARY KEY (stack_id, kind, logical_key),
    UNIQUE (kind, resource_id)
) STRICT;";

impl Store {
    /// Active applies protect existing bindings and prospective replacement IDs.
    /// Terminal reservations stay inspectable but must not block later edits.
    pub(crate) fn stack_resource_apply_busy(
        &self,
        kind: ResourceKind,
        resource: Uuid,
    ) -> Result<bool, PersistenceError> {
        Ok(self.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM stack_applies a
             WHERE a.state IN ('queued','running') AND (
                 EXISTS(SELECT 1 FROM stack_bindings b
                        WHERE b.stack_id=a.stack_id AND b.kind=?1 AND b.resource_id=?2)
                 OR EXISTS(SELECT 1 FROM stack_apply_reservations r
                           WHERE r.operation_id=a.operation_id AND r.kind=?1 AND r.resource_id=?2)
             ))",
            params![kind.as_str(), resource.to_string()],
            |row| row.get(0),
        )?)
    }

    /// Recover the newest accepted content pin for this exact bound VM UUID.
    /// This also protects later manual starts and API restart recovery.
    /// Inventory excludes pending updates so admission itself does not create drift.
    /// VM starts include them to enforce the accepted pin during a first reconciliation.
    pub(crate) fn stack_vm_pin(
        &self,
        vm: Uuid,
        accepted_updates: bool,
    ) -> Result<Option<firecrab_stack::TemplatePin>, PersistenceError> {
        let conn = self.lock();
        let owner: Option<(String, String)> = conn.query_row(
            "SELECT stack_id,logical_key FROM stack_bindings WHERE kind='vm' AND resource_id=?1",
            [vm.to_string()], |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()?;
        let Some((owner, key)) = owner else {
            return Ok(None);
        };
        let mut statement =
            conn.prepare("SELECT status FROM stack_applies WHERE stack_id=?1 ORDER BY rowid DESC")?;
        let rows = statement.query_map([owner], |r| r.get::<_, String>(0))?;
        for raw in rows {
            let status: firecrab_stack::ApplyStatus =
                serde_json::from_str(&raw?).map_err(|_| PersistenceError::StackApplyIdentity)?;
            if status.changes.iter().any(|step| {
                step.change.kind == ResourceKind::Vm
                    && step.change.key == key
                    && step.change.resource_id == Some(vm)
                    && step.change.action != firecrab_stack::Action::Delete
                    && (accepted_updates
                        || step.change.action == firecrab_stack::Action::Create
                        || step.state == firecrab_stack::StepState::Succeeded)
            }) && let Some(pin) = status.inputs.templates.get(&key)
            {
                return Ok(Some(pin.clone()));
            }
        }
        Ok(None)
    }

    /// A Git-managed service has one writer. Internal stack execution must prove
    /// that its accepted operation is currently running this exact service step.
    pub(crate) fn stack_service_mutation_allowed(
        &self,
        service: Uuid,
        operation: Option<Uuid>,
    ) -> Result<bool, PersistenceError> {
        let conn = self.lock();
        let owner: Option<String> = conn
            .query_row(
                "SELECT stack_id FROM stack_bindings WHERE kind='service' AND resource_id=?1",
                [service.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let Some(owner) = owner else {
            return Ok(true);
        };
        let git: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM stack_git_sources WHERE stack_id=?1)",
            [&owner],
            |r| r.get(0),
        )?;
        let active: Option<String> = conn.query_row(
            "SELECT status FROM stack_applies WHERE stack_id=?1 AND state IN ('queued','running')",
            [&owner], |r| r.get(0),
        ).optional()?;
        if !git && active.is_none() {
            return Ok(true);
        }
        let Some(active) = active else {
            return Ok(false);
        };
        let status: firecrab_stack::ApplyStatus =
            serde_json::from_str(&active).map_err(|_| PersistenceError::StackApplyIdentity)?;
        Ok(operation == Some(status.operation_id)
            && status.changes.iter().any(|step| {
                step.change.kind == ResourceKind::Service
                    && step.change.resource_id == Some(service)
                    && step.state == firecrab_stack::StepState::Running
            }))
    }

    /// The current opt-in policy authorizes only image automation, never concurrent Stack work.
    pub(crate) fn stack_service_auto_allowed(
        &self,
        service: Uuid,
    ) -> Result<bool, PersistenceError> {
        Ok(!self.lock().query_row("SELECT EXISTS(SELECT 1 FROM stack_applies a JOIN stack_bindings b ON a.stack_id=b.stack_id WHERE b.kind='service' AND b.resource_id=?1 AND a.state IN ('queued','running'))", [service.to_string()], |r| r.get::<_, bool>(0))?)
    }

    /// Delete empty metadata atomically. Never release bindings or delete host resources.
    pub fn delete_empty_stack(&self, name: &str, id: Uuid) -> Result<(), PersistenceError> {
        if !logical_name(name) || id.is_nil() {
            return Err(PersistenceError::StackConflict);
        }
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stack = read_stack(&tx, name)?.ok_or(PersistenceError::MissingStack)?;
        if stack.id != id {
            return Err(PersistenceError::StackConflict);
        }
        super::stack_applies::ensure_idle(&tx, stack.id)?;
        super::stack_sources::ensure_idle(&tx, stack.id)?;
        if !stack.bindings.is_empty() {
            return Err(PersistenceError::StackNotEmpty);
        }
        tx.execute(
            "DELETE FROM stack_git_checks WHERE stack_id = ?1",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM stack_git_sources WHERE stack_id = ?1",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM stack_applies WHERE stack_id = ?1",
            [id.to_string()],
        )?;
        tx.execute("DELETE FROM stacks WHERE id = ?1", [id.to_string()])?;
        tx.commit()?;
        Ok(())
    }

    /// Register once by name. Repeated registration returns the same UUID and revision.
    pub fn register_stack(&self, name: &str) -> Result<StackRecord, PersistenceError> {
        if !logical_name(name) {
            return Err(PersistenceError::StackConflict);
        }
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO stacks (id, name) VALUES (?1, ?2) ON CONFLICT(name) DO NOTHING",
            params![Uuid::new_v4().to_string(), name],
        )?;
        let stack = read_stack(&tx, name)?.ok_or(PersistenceError::MissingStack)?;
        tx.commit()?;
        Ok(stack)
    }

    /// Read the registration and its stable, sorted bindings without changing its revision.
    pub fn stack(&self, name: &str) -> Result<Option<StackRecord>, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let record = read_stack(&tx, name)?;
        tx.commit()?;
        Ok(record)
    }

    /// List registrations and binding sets in name order.
    pub fn list_stacks(&self) -> Result<Vec<StackRecord>, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let mut statement = tx.prepare("SELECT name FROM stacks ORDER BY name")?;
        let names = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let records = names
            .into_iter()
            .map(|name| read_stack(&tx, &name)?.ok_or(PersistenceError::MissingStack))
            .collect::<Result<_, _>>()?;
        tx.commit()?;
        Ok(records)
    }

    /// Atomically claim an existing UUID; no overwrite, implicit adoption or duplicate revision.
    pub fn bind_stack_resource(
        &self,
        name: &str,
        binding: &Binding,
    ) -> Result<StackRecord, PersistenceError> {
        if !logical_name(&binding.key) || binding.resource_id.is_nil() {
            return Err(PersistenceError::InvalidStackResource);
        }
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stack = read_stack(&tx, name)?.ok_or(PersistenceError::MissingStack)?;
        if let Some(current) = stack
            .bindings
            .iter()
            .find(|b| b.kind == binding.kind && b.key == binding.key)
        {
            if current != binding {
                return Err(PersistenceError::StackConflict);
            }
            tx.commit()?;
            return Ok(stack);
        }
        super::stack_applies::ensure_idle(&tx, stack.id)?;
        if stack.bindings.len() >= firecrab_stack::MAX_RESOURCES {
            return Err(PersistenceError::StackBindingLimit);
        }
        validate_resource(&tx, binding)?;
        let owner: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM stack_bindings WHERE kind = ?1 AND resource_id = ?2)",
            params![binding.kind.as_str(), binding.resource_id.to_string()],
            |row| row.get(0),
        )?;
        if owner {
            return Err(PersistenceError::StackConflict);
        }
        tx.execute("INSERT INTO stack_bindings (stack_id, kind, logical_key, resource_id) VALUES (?1, ?2, ?3, ?4)", params![stack.id.to_string(), binding.kind.as_str(), binding.key, binding.resource_id.to_string()])?;
        tx.execute(
            "UPDATE stacks SET revision = revision + 1 WHERE id = ?1",
            [stack.id.to_string()],
        )?;
        let updated = read_stack(&tx, name)?.ok_or(PersistenceError::MissingStack)?;
        tx.commit()?;
        Ok(updated)
    }

    /// Release only the exact kind/key/UUID supplied. The resource and its disks are retained.
    pub fn unbind_stack_resource(
        &self,
        name: &str,
        binding: &Binding,
    ) -> Result<StackRecord, PersistenceError> {
        if !logical_name(&binding.key) || binding.resource_id.is_nil() {
            return Err(PersistenceError::InvalidStackResource);
        }
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stack = read_stack(&tx, name)?.ok_or(PersistenceError::MissingStack)?;
        let Some(current) = stack
            .bindings
            .iter()
            .find(|b| b.kind == binding.kind && b.key == binding.key)
        else {
            tx.commit()?;
            return Ok(stack);
        };
        if current != binding {
            return Err(PersistenceError::StackConflict);
        }
        super::stack_applies::ensure_idle(&tx, stack.id)?;
        tx.execute("DELETE FROM stack_bindings WHERE stack_id = ?1 AND kind = ?2 AND logical_key = ?3 AND resource_id = ?4", params![stack.id.to_string(), binding.kind.as_str(), binding.key, binding.resource_id.to_string()])?;
        tx.execute(
            "UPDATE stacks SET revision = revision + 1 WHERE id = ?1",
            [stack.id.to_string()],
        )?;
        let updated = read_stack(&tx, name)?.ok_or(PersistenceError::MissingStack)?;
        tx.commit()?;
        Ok(updated)
    }
}

pub(super) fn read_stack(
    conn: &Connection,
    name: &str,
) -> Result<Option<StackRecord>, PersistenceError> {
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT id, revision FROM stacks WHERE name = ?1",
            [name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((id, revision)) = row else {
        return Ok(None);
    };
    let revision = u64::try_from(revision).map_err(|_| corrupt(&id))?;
    let mut statement = conn.prepare("SELECT kind, logical_key, resource_id FROM stack_bindings WHERE stack_id = ?1 ORDER BY CASE kind WHEN 'network' THEN 0 WHEN 'vm' THEN 1 ELSE 2 END, logical_key")?;
    let rows = statement.query_map([&id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let bindings = rows
        .map(|row| {
            let (kind, key, resource_id) = row?;
            let kind = ResourceKind::from_str(&kind).map_err(|_| corrupt(&id))?;
            if !logical_name(&key) {
                return Err(corrupt(&id));
            }
            Ok(Binding {
                kind,
                key,
                resource_id: uuid(&resource_id)?,
            })
        })
        .collect::<Result<_, PersistenceError>>()?;
    Ok(Some(StackRecord {
        id: uuid(&id)?,
        name: name.to_owned(),
        revision,
        bindings,
    }))
}

fn uuid(value: &str) -> Result<Uuid, PersistenceError> {
    Uuid::parse_str(value)
        .ok()
        .filter(|id| !id.is_nil())
        .ok_or_else(|| corrupt(value))
}

fn corrupt(id: &str) -> PersistenceError {
    PersistenceError::CorruptRecord {
        id: id.to_owned(),
        reason: "invalid stack ownership record".to_owned(),
    }
}

pub(super) fn validate_resource(
    conn: &Connection,
    binding: &Binding,
) -> Result<(), PersistenceError> {
    let id = binding.resource_id.to_string();
    let query = match binding.kind {
        ResourceKind::Network => "SELECT EXISTS(SELECT 1 FROM micro_networks WHERE id = ?1)",
        ResourceKind::Vm => {
            "SELECT EXISTS(SELECT 1 FROM vms WHERE id = ?1 AND purpose = 'instance'
                AND NOT EXISTS(SELECT 1 FROM cluster_worker_receipts WHERE id = ?1))"
        }
        ResourceKind::Service => "SELECT EXISTS(SELECT 1 FROM micro_services WHERE id = ?1)",
    };
    let exists: bool = conn.query_row(query, [&id], |row| row.get(0))?;
    if !exists {
        return Err(PersistenceError::InvalidStackResource);
    }
    if binding.kind != ResourceKind::Vm {
        return Ok(());
    }
    let mut statement = conn.prepare("SELECT deployment FROM micro_service_deployments")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows {
        let deployment: MicroServiceDeployment =
            serde_json::from_str(&row?).map_err(|_| corrupt(&id))?;
        if deployment.phase.is_busy()
            || deployment
                .owned_vm_ids()
                .any(|id| id == binding.resource_id)
        {
            return Err(PersistenceError::InvalidStackResource);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
