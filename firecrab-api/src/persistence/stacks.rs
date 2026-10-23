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
        if !stack.bindings.is_empty() {
            return Err(PersistenceError::StackNotEmpty);
        }
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

fn read_stack(conn: &Connection, name: &str) -> Result<Option<StackRecord>, PersistenceError> {
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

fn validate_resource(conn: &Connection, binding: &Binding) -> Result<(), PersistenceError> {
    let id = binding.resource_id.to_string();
    let query = match binding.kind {
        ResourceKind::Network => "SELECT EXISTS(SELECT 1 FROM micro_networks WHERE id = ?1)",
        ResourceKind::Vm => {
            "SELECT EXISTS(SELECT 1 FROM vms WHERE id = ?1 AND purpose = 'instance')"
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
                .instances
                .iter()
                .any(|instance| instance.vm_id == binding.resource_id)
        {
            return Err(PersistenceError::InvalidStackResource);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
