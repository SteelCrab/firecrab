//! A legacy boot review and its immutable acknowledgement commit together.

use super::{PersistenceError as Error, Store, iac_native, iac_owners};
use crate::{model::VmRecord, vm_boot::Review};
use firecrab_stack::{ManagedResource, NativeAction, NativeOperation, NativeOperationState};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS iac_vm_boot_reviews (
 operation_id TEXT PRIMARY KEY REFERENCES iac_native_operations(operation_id),
 request TEXT NOT NULL
) STRICT;";

fn request_key(vm: Uuid, request: &Review) -> Result<String, Error> {
    serde_json::to_string(&(vm, request)).map_err(|_| Error::IacOperationIdentity)
}

fn receipt(
    conn: &Connection,
    vm: Uuid,
    request: &Review,
) -> Result<Option<NativeOperation>, Error> {
    if iac_owners::endpoint(conn)? != request.expected_endpoint_id {
        return Err(Error::IacEndpointConflict);
    }
    let Some(operation) = iac_native::read(conn, request.operation_id)? else {
        return Ok(None);
    };
    let original: Option<String> = conn
        .query_row(
            "SELECT request FROM iac_vm_boot_reviews WHERE operation_id=?1",
            [request.operation_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if operation.action != NativeAction::ReviewVmBootPin
        || original.as_ref() != Some(&request_key(vm, request)?)
        || operation.state != NativeOperationState::Succeeded
    {
        return Err(Error::IacOperationIdentity);
    }
    Ok(Some(operation))
}

impl Store {
    /// A terminal retry does not read current registry files or acquire fresh authority.
    pub(crate) fn vm_boot_review_receipt(
        &self,
        vm: Uuid,
        request: &Review,
    ) -> Result<Option<NativeOperation>, Error> {
        receipt(&self.lock(), vm, request)
    }

    /// Only an independent, inactive, unowned legacy record can acquire its missing proof.
    pub(crate) fn review_vm_boot_pin(
        &self,
        vm: &VmRecord,
        request: &Review,
    ) -> Result<NativeOperation, Error> {
        if !request.valid() || vm.id.is_nil() {
            return Err(Error::IacOperationIdentity);
        }
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(result) = receipt(&tx, vm.id, request)? {
            return Ok(result);
        }
        let resource = ManagedResource {
            kind: "vm".into(),
            id: vm.id.to_string(),
        };
        if !iac_owners::public_write_allowed(&tx, &resource)? {
            return Err(Error::IacOwnershipConflict);
        }
        let ownership = iac_owners::read(&tx, &resource)?;
        if ownership.owner.is_some() {
            return Err(Error::IacOwnershipConflict);
        }
        if ownership.generation != request.expected_generation {
            return Err(Error::StaleIacOwner);
        }
        if vm.boot_pin.is_some() || vm.purpose != crate::model::VmPurpose::Instance {
            return Err(Error::IacOperationIdentity);
        }
        super::stacks::validate_resource(
            &tx,
            &firecrab_stack::Binding {
                kind: firecrab_stack::ResourceKind::Vm,
                key: "boot-review".into(),
                resource_id: vm.id,
            },
        )?;
        if tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM vm_pool_members WHERE vm_id=?1)",
            [vm.id.to_string()],
            |row| row.get::<_, bool>(0),
        )? {
            return Err(Error::IacParentManaged);
        }
        let occupied: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM iac_owner_changes WHERE operation_id=?1)
            OR EXISTS(SELECT 1 FROM iac_host_owner_intents WHERE operation_id=?1)",
            [request.operation_id.to_string()],
            |row| row.get(0),
        )?;
        if occupied {
            return Err(Error::IacOperationIdentity);
        }
        let pin = &request.pin;
        let changed = tx.execute("UPDATE vms SET boot_pin=?2 WHERE id=?1 AND boot_pin IS NULL
            AND state IN ('created','stopped','error') AND template=?3 AND template_version=?4
            AND template_kernel_sha256=?5 AND template_rootfs_sha256=?6 AND template_boot_args_sha256=?7",
            params![vm.id.to_string(),serde_json::to_string(pin).map_err(|_|Error::IacOperationIdentity)?,
                vm.template,pin.version,pin.kernel_sha256,pin.rootfs_sha256,pin.boot_args_sha256])?;
        if changed != 1 {
            return Err(Error::StaleIacExecution);
        }
        let operation = NativeOperation {
            operation_id: request.operation_id,
            ownership,
            action: NativeAction::ReviewVmBootPin,
            state: NativeOperationState::Succeeded,
            error_code: None,
            execution_generation: 0,
            configuration_generation: None,
            retain_disks: None,
        };
        iac_native::save(&tx, &operation)?;
        tx.execute(
            "INSERT INTO iac_vm_boot_reviews(operation_id,request) VALUES(?1,?2)",
            params![
                request.operation_id.to_string(),
                request_key(vm.id, request)?
            ],
        )?;
        tx.commit()?;
        Ok(operation)
    }
}
