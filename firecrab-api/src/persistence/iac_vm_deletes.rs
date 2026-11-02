//! VM cleanup owns a durable fence before touching artifacts or releasing an address.

use super::{PersistenceError, Store, iac_native, iac_owners};
use crate::model::VmRecord;
use firecrab_stack::{
    ManagedResource, Manager, NativeAction, NativeOperation, NativeOperationState,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS iac_vm_deletes (
 operation_id TEXT PRIMARY KEY REFERENCES iac_native_operations(operation_id),
 storage_root TEXT NOT NULL, retain_disks INTEGER NOT NULL CHECK(retain_disks IN (0,1))
) STRICT";

pub(super) fn resource(id: Uuid) -> ManagedResource {
    ManagedResource {
        kind: "vm".into(),
        id: id.to_string(),
    }
}

/// Never let an old create retry recreate an identity whose cleanup was acknowledged.
pub(super) fn ensure_available(conn: &Connection, vm: Uuid) -> Result<(), PersistenceError> {
    iac_native::ensure_idle(conn, &resource(vm))?;
    let retired: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM iac_native_operations WHERE kind='vm' AND resource_id=?1
         AND state='succeeded' AND json_extract(status,'$.action')='delete_vm')",
        [vm.to_string()],
        |row| row.get(0),
    )?;
    if retired {
        return Err(PersistenceError::IacOperationIdentity);
    }
    Ok(())
}

/// Retention and storage identity are private, immutable inputs to this exact operation.
#[derive(Clone)]
pub(crate) struct VmDeleteIntent {
    pub operation: NativeOperation,
    pub retain_disks: bool,
    storage_root: String,
}

fn load(conn: &Connection, id: Uuid) -> Result<Option<VmDeleteIntent>, PersistenceError> {
    let Some(operation) = iac_native::read(conn, id)? else {
        return Ok(None);
    };
    if operation.action != NativeAction::DeleteVm || operation.ownership.resource.kind != "vm" {
        return Err(PersistenceError::IacOperationIdentity);
    }
    let (storage_root, retain_disks) = conn.query_row(
        "SELECT storage_root,retain_disks FROM iac_vm_deletes WHERE operation_id=?1",
        [id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if operation.retain_disks != Some(retain_disks) {
        return Err(PersistenceError::IacOperationIdentity);
    }
    Ok(Some(VmDeleteIntent {
        operation,
        retain_disks,
        storage_root,
    }))
}

fn validate(conn: &Connection, intent: &VmDeleteIntent) -> Result<Uuid, PersistenceError> {
    if iac_native::read(conn, intent.operation.operation_id)?.as_ref() != Some(&intent.operation)
        || intent.operation.state != NativeOperationState::Running
        || iac_owners::read(conn, &intent.operation.ownership.resource)?
            != intent.operation.ownership
    {
        return Err(PersistenceError::StaleIacOwner);
    }
    super::iac_dependencies::validate(conn, intent.operation.operation_id)?;
    let vm = Uuid::parse_str(&intent.operation.ownership.resource.id)
        .map_err(|_| PersistenceError::IacOperationIdentity)?;
    let storage: Option<String> = conn
        .query_row(
            "SELECT storage_root FROM vms WHERE id=?1 AND state IN ('created','stopped','error')",
            [vm.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if storage.as_ref() != Some(&intent.storage_root) {
        return Err(PersistenceError::StaleIacOwner);
    }
    Ok(vm)
}

impl Store {
    /// Reading a retry never dispatches cleanup again.
    pub(crate) fn native_vm_delete(
        &self,
        id: Uuid,
    ) -> Result<Option<VmDeleteIntent>, PersistenceError> {
        load(&self.lock(), id)
    }

    pub(crate) fn begin_native_vm_delete(
        &self,
        vm: &VmRecord,
        id: Uuid,
        retain_disks: bool,
    ) -> Result<VmDeleteIntent, PersistenceError> {
        self.begin_vm_delete(vm, id, retain_disks, None)
    }

    /// The exact running delete step grants its own child admission, never public authority.
    pub(crate) fn begin_stack_vm_delete(
        &self,
        vm: &VmRecord,
        id: Uuid,
        retain_disks: bool,
        stack: Uuid,
    ) -> Result<VmDeleteIntent, PersistenceError> {
        self.begin_vm_delete(vm, id, retain_disks, Some(stack))
    }

    /// Trusted parent controllers may remove their generated VMs; public routes separately
    /// reject those children. Terraform ownership and another native writer still apply.
    fn begin_vm_delete(
        &self,
        vm: &VmRecord,
        id: Uuid,
        retain_disks: bool,
        stack_operation: Option<Uuid>,
    ) -> Result<VmDeleteIntent, PersistenceError> {
        if id.is_nil() || vm.id.is_nil() {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(intent) = load(&tx, id)? {
            if intent.operation.ownership.resource != resource(vm.id)
                || intent.retain_disks != retain_disks
            {
                return Err(PersistenceError::IacOperationIdentity);
            }
            return Ok(intent);
        }
        super::iac_volume_attachments::ensure_vm_deletable(&tx, vm.id)?;
        let target = resource(vm.id);
        iac_native::ensure_idle(&tx, &target)?;
        let ownership = iac_owners::read(&tx, &target)?;
        if let Some(operation) = stack_operation {
            let source = super::stack_applies::read(&tx, operation)?
                .ok_or(PersistenceError::StackApplyIdentity)?;
            let authorized = retain_disks != source.status.lifecycle.delete_vm_disks
                && source.status.state == firecrab_stack::ApplyState::Running
                && source.status.changes.iter().any(|step| {
                    step.change.kind == firecrab_stack::ResourceKind::Vm
                        && step.change.resource_id == Some(vm.id)
                        && step.change.action == firecrab_stack::Action::Delete
                        && step.state == firecrab_stack::StepState::Running
                        && ownership.owner.as_ref().is_some_and(|owner| {
                            owner.manager == Manager::Stack
                                && owner.id == source.status.stack_id
                                && owner.key == step.change.key
                        })
                });
            if !authorized {
                return Err(PersistenceError::StackApplyIdentity);
            }
        } else {
            if let Some(owner) = ownership.owner.as_ref() {
                if owner.manager == Manager::Terraform {
                    return Err(PersistenceError::IacOwnershipConflict);
                }
                super::stack_applies::ensure_idle(&tx, owner.id)?;
                super::stack_sources::ensure_idle(&tx, owner.id)?;
            }
            if super::stacks::resource_apply_busy(&tx, firecrab_stack::ResourceKind::Vm, vm.id)? {
                return Err(PersistenceError::StackApplyBusy);
            }
        }
        let intent = VmDeleteIntent {
            operation: NativeOperation {
                operation_id: id,
                ownership,
                action: NativeAction::DeleteVm,
                state: NativeOperationState::Running,
                error_code: None,
                execution_generation: 0,
                configuration_generation: None,
                retain_disks: Some(retain_disks),
            },
            retain_disks,
            storage_root: vm.storage_root.clone(),
        };
        iac_native::save(&tx, &intent.operation)?;
        super::iac_dependencies::reserve_vm_parents(&tx, &intent.operation, vm.id)?;
        tx.execute(
            "INSERT INTO iac_vm_deletes VALUES(?1,?2,?3)",
            params![id.to_string(), intent.storage_root, retain_disks],
        )?;
        validate(&tx, &intent)?;
        tx.commit()?;
        Ok(intent)
    }

    /// Recheck the exact attempt immediately before filesystem dispatch.
    pub(crate) fn validate_native_vm_delete(
        &self,
        intent: &VmDeleteIntent,
    ) -> Result<(), PersistenceError> {
        validate(&self.lock(), intent).map(|_| ())
    }

    /// Unknown cleanup retains the VM row, address, authority and original retention policy.
    pub(crate) fn fail_native_vm_delete(
        &self,
        intent: &VmDeleteIntent,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if iac_native::read(&tx, intent.operation.operation_id)?.as_ref() != Some(&intent.operation)
            || intent.operation.state != NativeOperationState::Running
        {
            return Err(PersistenceError::StaleIacOwner);
        }
        let mut operation = intent.operation.clone();
        operation.state = NativeOperationState::NeedsRecovery;
        operation.error_code = Some("vm_cleanup_incomplete".into());
        iac_native::save(&tx, &operation)?;
        tx.commit()?;
        Ok(())
    }

    /// Recovery advances the attempt; an old worker cannot finish a newer attempt.
    pub(crate) fn recover_native_vm_delete(
        &self,
        id: Uuid,
    ) -> Result<VmDeleteIntent, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut intent = load(&tx, id)?.ok_or(PersistenceError::IacOperationIdentity)?;
        if intent.operation.state == NativeOperationState::Succeeded {
            return Ok(intent);
        }
        if intent.operation.state != NativeOperationState::NeedsRecovery {
            return Err(PersistenceError::IacResourceBusy);
        }
        if iac_owners::read(&tx, &intent.operation.ownership.resource)?
            != intent.operation.ownership
        {
            return Err(PersistenceError::StaleIacOwner);
        }
        intent.operation.execution_generation = intent
            .operation
            .execution_generation
            .checked_add(1)
            .ok_or(PersistenceError::IacOperationIdentity)?;
        intent.operation.state = NativeOperationState::Running;
        intent.operation.error_code = None;
        iac_native::save(&tx, &intent.operation)?;
        validate(&tx, &intent)?;
        tx.commit()?;
        Ok(intent)
    }

    /// Artifact cleanup precedes this transaction. Metadata, lease and acknowledgement commit together.
    pub(crate) fn finish_native_vm_delete(
        &self,
        intent: &VmDeleteIntent,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let vm = validate(&tx, intent)?;
        super::iac_volume_attachments::ensure_vm_deletable(&tx, vm)?;
        tx.execute("DELETE FROM vms WHERE id=?1", [vm.to_string()])?;
        tx.execute("DELETE FROM vm_shells WHERE vm_id=?1", [vm.to_string()])?;
        tx.execute("DELETE FROM port_forwards WHERE vm_id=?1", [vm.to_string()])?;
        match crate::ipam::release(&tx, vm) {
            Ok(()) | Err(crate::ipam::IpamError::NotLeased { .. }) => {}
            Err(_) => return Err(PersistenceError::IacOperationIdentity),
        }
        let mut operation = intent.operation.clone();
        operation.state = NativeOperationState::Succeeded;
        operation.error_code = None;
        iac_native::save(&tx, &operation)?;
        tx.commit()?;
        Ok(())
    }
}
