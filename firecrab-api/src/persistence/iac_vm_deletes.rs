//! VM cleanup owns a durable fence before touching artifacts or releasing an address.

use super::{PersistenceError, Store, iac_native, iac_owners};
use crate::artifacts::cleanup::VmCleanupStorage;
use crate::model::VmRecord;
use firecrab_stack::{
    ManagedResource, Manager, NativeAction, NativeOperation, NativeOperationState,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS iac_vm_deletes (
 operation_id TEXT PRIMARY KEY REFERENCES iac_native_operations(operation_id),
 storage_root TEXT NOT NULL, retain_disks INTEGER NOT NULL CHECK(retain_disks IN (0,1)),
 storage_pin TEXT, storage_review_generation INTEGER
) STRICT";

/// Preserve legacy receipts while leaving their unrecorded physical input explicitly unknown.
pub(super) fn initialize(conn: &Connection) -> Result<(), PersistenceError> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    tx.execute_batch(SCHEMA)?;
    for (column, sql) in [
        (
            "storage_pin",
            "ALTER TABLE iac_vm_deletes ADD COLUMN storage_pin TEXT",
        ),
        (
            "storage_review_generation",
            "ALTER TABLE iac_vm_deletes ADD COLUMN storage_review_generation INTEGER",
        ),
    ] {
        if !tx
            .prepare("SELECT 1 FROM pragma_table_info('iac_vm_deletes') WHERE name=?1")?
            .exists([column])?
        {
            tx.execute(sql, [])?;
        }
    }
    tx.commit()?;
    Ok(())
}

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
    storage: Option<VmCleanupStorage>,
    storage_review_generation: Option<u64>,
}

impl VmDeleteIntent {
    /// The selected root is immutable for this operation; it never selects a fallback.
    pub(crate) fn storage_root(&self) -> &str {
        &self.storage_root
    }

    /// Legacy unfinished work lacks proof and requires explicit original-target review.
    pub(crate) fn storage_proof(&self) -> Option<&VmCleanupStorage> {
        self.storage.as_ref()
    }

    /// Redacted retained identity remains discoverable after metadata deletion.
    pub(crate) fn storage_view(
        &self,
        proof: &VmCleanupStorage,
    ) -> Result<serde_json::Value, PersistenceError> {
        Ok(
            serde_json::json!({"operationId":self.operation.operation_id,"endpointId":self.operation.ownership.endpoint_id,
            "vmId":proof.vm_id(),"storageRoot":self.storage_root,"diskGeneration":proof.disk_generation(),
            "retainDisks":self.retain_disks,"physicalSha256":proof.fingerprint().map_err(|_|PersistenceError::IacOperationIdentity)?,
            "artifactDirectoryExistedAtObservation":proof.artifacts_existed()}),
        )
    }
}

/// An operator must explicitly approve the original target of an unproven legacy operation.
#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StorageReview {
    /// Original persistent API namespace.
    pub expected_endpoint_id: Uuid,
    /// Attempt observed before this one-time review.
    pub expected_execution_generation: u64,
    /// Exact candidate directory and disk-generation proof.
    pub physical_sha256: String,
    /// Explicit operator assertion that this is the original storage target.
    pub confirm_original_storage: bool,
}

impl StorageReview {
    /// Reject missing confirmation, invalid namespace and malformed scope selectors.
    pub(crate) fn valid(&self) -> bool {
        !self.expected_endpoint_id.is_nil()
            && self.expected_execution_generation < i64::MAX as u64
            && self.confirm_original_storage
            && self.physical_sha256.len() == 64
            && self
                .physical_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }
}

fn review_receipt(
    conn: &Connection,
    id: Uuid,
    request: &StorageReview,
) -> Result<Option<serde_json::Value>, PersistenceError> {
    if !request.valid() {
        return Err(PersistenceError::IacOperationIdentity);
    }
    if iac_owners::endpoint(conn)? != request.expected_endpoint_id {
        return Err(PersistenceError::IacEndpointConflict);
    }
    let intent = load(conn, id)?.ok_or(PersistenceError::IacOperationIdentity)?;
    if intent.operation.ownership.endpoint_id != request.expected_endpoint_id {
        return Err(PersistenceError::IacEndpointConflict);
    }
    let Some(reviewed) = intent.storage_review_generation else {
        return Ok(None);
    };
    let proof = intent
        .storage_proof()
        .ok_or(PersistenceError::IacOperationIdentity)?;
    if reviewed != request.expected_execution_generation
        || proof
            .fingerprint()
            .map_err(|_| PersistenceError::IacOperationIdentity)?
            != request.physical_sha256
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    let mut receipt = intent.storage_view(proof)?;
    receipt["reviewedExecutionGeneration"] = serde_json::json!(reviewed);
    receipt["nextExecutionGeneration"] = serde_json::json!(
        reviewed
            .checked_add(1)
            .ok_or(PersistenceError::IacOperationIdentity)?
    );
    Ok(Some(receipt))
}

fn load(conn: &Connection, id: Uuid) -> Result<Option<VmDeleteIntent>, PersistenceError> {
    let Some(operation) = iac_native::read(conn, id)? else {
        return Ok(None);
    };
    if operation.action != NativeAction::DeleteVm || operation.ownership.resource.kind != "vm" {
        return Err(PersistenceError::IacOperationIdentity);
    }
    let (storage_root, retain_disks,storage,review_generation): (String,bool,Option<String>,Option<i64>) = conn.query_row(
        "SELECT storage_root,retain_disks,storage_pin,storage_review_generation FROM iac_vm_deletes WHERE operation_id=?1",
        [id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?,row.get(2)?,row.get(3)?)),
    )?;
    let storage: Option<VmCleanupStorage> = storage
        .map(|raw| serde_json::from_str(&raw).map_err(|_| PersistenceError::IacOperationIdentity))
        .transpose()?;
    let storage_review_generation = review_generation
        .map(|value| u64::try_from(value).map_err(|_| PersistenceError::IacOperationIdentity))
        .transpose()?;
    if operation.retain_disks != Some(retain_disks) {
        return Err(PersistenceError::IacOperationIdentity);
    }
    if storage
        .as_ref()
        .is_some_and(|proof| proof.vm_id().to_string() != operation.ownership.resource.id)
        || (storage_review_generation.is_some() && storage.is_none())
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    Ok(Some(VmDeleteIntent {
        operation,
        retain_disks,
        storage_root,
        storage,
        storage_review_generation,
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
    let storage: Option<(String,Option<String>)> = conn
        .query_row(
            "SELECT storage_root,disk_generation FROM vms WHERE id=?1 AND state IN ('created','stopped','error')",
            [vm.to_string()],
            |row| Ok((row.get(0)?,row.get(1)?)),
        )
        .optional()?;
    if storage.as_ref().map(|row| row.0.as_str()) != Some(intent.storage_root.as_str())
        || intent.storage.as_ref().is_some_and(|proof| {
            storage.as_ref().and_then(|row| row.1.as_ref())
                != proof.disk_generation().map(|id| id.to_string()).as_ref()
        })
    {
        return Err(PersistenceError::StaleIacOwner);
    }
    Ok(vm)
}

impl Store {
    /// Read a terminal review acknowledgement without consulting current filesystem state.
    pub(crate) fn vm_delete_storage_review_receipt(
        &self,
        id: Uuid,
        request: &StorageReview,
    ) -> Result<Option<serde_json::Value>, PersistenceError> {
        review_receipt(&self.lock(), id, request)
    }

    /// Review adds the missing legacy proof once and fences every older cleanup attempt.
    pub(crate) fn review_vm_delete_storage(
        &self,
        id: Uuid,
        request: &StorageReview,
        proof: VmCleanupStorage,
        current_root: &std::path::Path,
    ) -> Result<serde_json::Value, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(receipt) = review_receipt(&tx, id, request)? {
            return Ok(receipt);
        }
        let mut intent = load(&tx, id)?.ok_or(PersistenceError::IacOperationIdentity)?;
        if intent.storage.is_some() || intent.operation.state != NativeOperationState::NeedsRecovery
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        iac_native::check_recovery_attempt(
            &intent.operation,
            request.expected_execution_generation,
        )?;
        if iac_owners::read(&tx, &intent.operation.ownership.resource)?
            != intent.operation.ownership
        {
            return Err(PersistenceError::StaleIacOwner);
        }
        super::iac_dependencies::validate(&tx, id)?;
        let row:Option<(String,Option<String>)>=tx.query_row("SELECT storage_root,disk_generation FROM vms WHERE id=?1 AND state IN ('created','stopped','error')",
            [&intent.operation.ownership.resource.id],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
        if proof.vm_id().to_string() != intent.operation.ownership.resource.id
            || row.as_ref().map(|value| value.0.as_str()) != Some(intent.storage_root.as_str())
            || row.as_ref().and_then(|value| value.1.as_ref())
                != proof.disk_generation().map(|id| id.to_string()).as_ref()
            || proof
                .fingerprint()
                .map_err(|_| PersistenceError::IacOperationIdentity)?
                != request.physical_sha256
        {
            return Err(PersistenceError::StaleIacExecution);
        }
        proof
            .check(current_root)
            .map_err(|_| PersistenceError::IacOperationIdentity)?;
        if let Ok(storage) = Uuid::parse_str(&intent.storage_root) {
            let already:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM iac_native_dependencies WHERE operation_id=?1 AND kind='storage_root' AND resource_id=?2)",params![id.to_string(),storage.to_string()],|row|row.get(0))?;
            if !already {
                super::iac_dependencies::reserve(
                    &tx,
                    &intent.operation,
                    ManagedResource {
                        kind: "storage_root".into(),
                        id: storage.to_string(),
                    },
                )?;
            }
        }
        let encoded =
            serde_json::to_string(&proof).map_err(|_| PersistenceError::IacOperationIdentity)?;
        if tx.execute("UPDATE iac_vm_deletes SET storage_pin=?2,storage_review_generation=?3 WHERE operation_id=?1 AND storage_pin IS NULL",
            params![id.to_string(),encoded,request.expected_execution_generation as i64])?!=1 { return Err(PersistenceError::StaleIacExecution); }
        intent.operation.execution_generation = intent
            .operation
            .execution_generation
            .checked_add(1)
            .filter(|value| *value <= i64::MAX as u64)
            .ok_or(PersistenceError::IacOperationIdentity)?;
        iac_native::save(&tx, &intent.operation)?;
        let result =
            review_receipt(&tx, id, request)?.ok_or(PersistenceError::IacOperationIdentity)?;
        tx.commit()?;
        Ok(result)
    }

    /// Reading a retry never dispatches cleanup again.
    pub(crate) fn native_vm_delete(
        &self,
        id: Uuid,
    ) -> Result<Option<VmDeleteIntent>, PersistenceError> {
        load(&self.lock(), id)
    }

    /// A worker acknowledgement must never replay filesystem cleanup after native success.
    pub(crate) fn completed_vm_delete(
        &self,
        vm: Uuid,
    ) -> Result<Option<VmDeleteIntent>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT operation_id FROM iac_native_operations WHERE kind='vm' AND resource_id=?1
            AND state='succeeded' AND json_extract(status,'$.action')='delete_vm' LIMIT 2",
        )?;
        let ids = statement
            .query_map([vm.to_string()], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if ids.len() > 1 {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let Some(id) = ids.first() else {
            return Ok(None);
        };
        let id = Uuid::parse_str(id).map_err(|_| PersistenceError::IacOperationIdentity)?;
        let intent = load(&conn, id)?.ok_or(PersistenceError::IacOperationIdentity)?;
        if intent.operation.state != NativeOperationState::Succeeded {
            return Err(PersistenceError::IacOperationIdentity);
        }
        Ok(Some(intent))
    }

    #[cfg(test)]
    pub(crate) fn begin_native_vm_delete(
        &self,
        vm: &VmRecord,
        id: Uuid,
        retain_disks: bool,
    ) -> Result<VmDeleteIntent, PersistenceError> {
        self.begin_vm_delete(vm, id, retain_disks, None, None)
    }

    /// Production dispatch always admits a proof obtained from the actual selected filesystem.
    pub(crate) fn begin_vm_delete_pinned(
        &self,
        vm: &VmRecord,
        id: Uuid,
        retain_disks: bool,
        stack: Option<Uuid>,
        storage: VmCleanupStorage,
    ) -> Result<VmDeleteIntent, PersistenceError> {
        if storage.vm_id() != vm.id || storage.disk_generation() != vm.disk_generation {
            return Err(PersistenceError::IacOperationIdentity);
        }
        self.begin_vm_delete(vm, id, retain_disks, stack, Some(storage))
    }

    /// Trusted parent controllers may remove their generated VMs; public routes separately
    /// reject those children. Terraform ownership and another native writer still apply.
    fn begin_vm_delete(
        &self,
        vm: &VmRecord,
        id: Uuid,
        retain_disks: bool,
        stack_operation: Option<Uuid>,
        storage: Option<VmCleanupStorage>,
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
            storage,
            storage_review_generation: None,
        };
        iac_native::save(&tx, &intent.operation)?;
        super::iac_dependencies::reserve_vm_parents(&tx, &intent.operation, vm.id)?;
        tx.execute(
            "INSERT INTO iac_vm_deletes(operation_id,storage_root,retain_disks,storage_pin) VALUES(?1,?2,?3,?4)",
            params![id.to_string(), intent.storage_root, retain_disks,intent.storage.as_ref().map(serde_json::to_string).transpose().map_err(|_|PersistenceError::IacOperationIdentity)?],
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
    #[cfg(test)]
    pub(crate) fn recover_native_vm_delete(
        &self,
        id: Uuid,
    ) -> Result<VmDeleteIntent, PersistenceError> {
        let generation = self
            .iac_native_operation(id)?
            .ok_or(PersistenceError::IacOperationIdentity)?
            .execution_generation;
        self.recover_native_vm_delete_at(id, generation)
    }
    /// Compare the observed attempt inside the transaction before resuming its original work.
    pub(crate) fn recover_native_vm_delete_at(
        &self,
        id: Uuid,
        expected_generation: u64,
    ) -> Result<VmDeleteIntent, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut intent = load(&tx, id)?.ok_or(PersistenceError::IacOperationIdentity)?;
        if intent.operation.state == NativeOperationState::Succeeded {
            return Ok(intent);
        }
        crate::persistence::iac_native::check_recovery_attempt(
            &intent.operation,
            expected_generation,
        )?;
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
            .filter(|v| *v <= i64::MAX as u64)
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
