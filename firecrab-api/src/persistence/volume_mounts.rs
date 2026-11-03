//! Atomic attachment writes shared by VM creation and the explicit attachment API.

use super::*;
use firecrab_api_types::VmBlockStorageMount;

impl Store {
    /// The VM creator already controls its fresh UUID. Borrowed disks and relations remain guarded.
    pub(crate) fn attach_block_storages(
        &self,
        vm: Uuid,
        mounts: &[VmBlockStorageMount],
    ) -> Result<(), PersistenceError> {
        self.attach_block_storages_authorized(vm, mounts, None)
    }

    pub(crate) fn attach_block_storage_checked(
        &self,
        vm: Uuid,
        mount: VmBlockStorageMount,
        authority: &super::iac_volume_attachments::AttachmentAuthority,
    ) -> Result<(), PersistenceError> {
        self.attach_block_storages_authorized(vm, &[mount], Some(authority))
    }
    fn attach_block_storages_authorized(
        &self,
        vm_id: Uuid,
        mounts: &[VmBlockStorageMount],
        authority: Option<&super::iac_volume_attachments::AttachmentAuthority>,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        super::iac_native::ensure_idle(&transaction, &super::iac_vm_deletes::resource(vm_id))?;
        if let Some(authority) = authority {
            let [mount] = mounts else {
                return Err(PersistenceError::InvalidIacOwner);
            };
            super::iac_volume_attachments::check_authority(
                &transaction,
                mount.volume_id,
                vm_id,
                authority,
            )?;
        }
        let instance: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM vms WHERE id = ?1 AND purpose = 'instance')",
            [vm_id.to_string()],
            |row| row.get(0),
        )?;
        if !instance {
            return Err(PersistenceError::BlockStorageConflict {
                message: "only user MicroVMs can own volumes",
            });
        }
        for mount in mounts {
            for target in [
                firecrab_stack::ManagedResource {
                    kind: "volume".into(),
                    id: mount.volume_id.to_string(),
                },
                super::iac_volume_attachments::resource(mount.volume_id, vm_id),
            ] {
                if !super::iac_owners::public_write_allowed(&transaction, &target)? {
                    return Err(PersistenceError::IacOwnershipConflict);
                }
            }
            let mut statement = transaction.prepare(
                "SELECT id, mount_path FROM micro_block_storages WHERE attached_vm_id = ?1",
            )?;
            let existing = statement
                .query_map([vm_id.to_string()], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if existing.iter().any(|(id, path)| {
                id != &mount.volume_id.to_string()
                    && crate::block_storage::paths_overlap(path, &mount.mount_path)
            }) {
                return Err(PersistenceError::BlockStorageConflict {
                    message: "volume mount paths must not overlap",
                });
            }
            let changed = transaction.execute(
                "UPDATE micro_block_storages SET attached_vm_id = ?2, mount_path = ?3
                 WHERE id = ?1 AND phase = 'ready' AND
                 (attached_vm_id IS NULL OR (attached_vm_id = ?2 AND mount_path = ?3))",
                params![
                    mount.volume_id.to_string(),
                    vm_id.to_string(),
                    mount.mount_path
                ],
            )?;
            if changed == 0 {
                return Err(PersistenceError::BlockStorageConflict {
                    message: "volume is missing, busy, or already attached",
                });
            }
        }
        let count: u32 = transaction.query_row(
            "SELECT COUNT(*) FROM micro_block_storages WHERE attached_vm_id = ?1",
            [vm_id.to_string()],
            |row| row.get(0),
        )?;
        if count > crate::block_storage::MAX_VOLUMES_PER_VM as u32 {
            return Err(PersistenceError::BlockStorageConflict {
                message: "a VM can have at most 8 data volumes",
            });
        }
        transaction.commit()?;
        Ok(())
    }

    /// Clears the expected reservation only; callers hold the owning VM's lifecycle lock.
    pub(crate) fn detach_block_storage(
        &self,
        id: Uuid,
        vm_id: Uuid,
        authority: &super::iac_volume_attachments::AttachmentAuthority,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        super::iac_volume_attachments::check_authority(&transaction, id, vm_id, authority)?;
        let changed = transaction.execute(
            "UPDATE micro_block_storages SET attached_vm_id = NULL, mount_path = NULL
             WHERE id = ?1 AND attached_vm_id = ?2",
            params![id.to_string(), vm_id.to_string()],
        )?;
        if changed == 0 {
            return Err(PersistenceError::BlockStorageConflict {
                message: "volume attachment changed; refresh and retry",
            });
        }
        transaction.commit()?;
        Ok(())
    }
}
