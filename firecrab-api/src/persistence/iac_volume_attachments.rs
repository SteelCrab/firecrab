//! Attachment ownership is scoped to the exact volume and VM, independent of disk contents.

use super::{PersistenceError, Store, iac_native, iac_owners};
use firecrab_stack::{ManagedResource, ResourceOwnership};
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

pub(super) fn resource(volume: Uuid, vm: Uuid) -> ManagedResource {
    ManagedResource {
        kind: "volume_attachment".into(),
        id: format!("{volume}:{vm}"),
    }
}

fn vm_idle(conn: &Connection, vm: &str) -> Result<(), PersistenceError> {
    let id = Uuid::parse_str(vm).map_err(|_| PersistenceError::InvalidIacOwner)?;
    iac_native::ensure_idle(conn, &super::iac_vm_deletes::resource(id))?;
    if super::stacks::resource_apply_busy(conn, firecrab_stack::ResourceKind::Vm, id)? {
        return Err(PersistenceError::IacResourceBusy);
    }
    let busy: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM vms WHERE id=?1
        AND state IN ('starting','pausing','resuming','stopping'))",
        [vm],
        |row| row.get(0),
    )?;
    if busy {
        return Err(PersistenceError::IacResourceBusy);
    }
    Ok(())
}

pub(super) fn volume_parent_idle(conn: &Connection, volume: &str) -> Result<(), PersistenceError> {
    let vm: Option<String> = conn
        .query_row(
            "SELECT attached_vm_id FROM micro_block_storages WHERE id=?1",
            [volume],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    if let Some(vm) = vm {
        vm_idle(conn, &vm)?;
    }
    Ok(())
}

pub(super) fn validate(
    conn: &Connection,
    target: &ManagedResource,
    adopting: bool,
) -> Result<(), PersistenceError> {
    let (volume, vm) = target
        .id
        .split_once(':')
        .ok_or(PersistenceError::InvalidIacOwner)?;
    vm_idle(conn, vm)?;
    let row: Option<(String, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT phase,attached_vm_id,mount_path FROM micro_block_storages WHERE id=?1",
            [volume],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if !adopting {
        return Ok(());
    }
    let (phase, attached, path) = row.ok_or(PersistenceError::InvalidStackResource)?;
    if phase != "ready" {
        return Err(PersistenceError::IacResourceBusy);
    }
    if attached.as_deref() != Some(vm)
        || !path
            .as_deref()
            .is_some_and(crate::block_storage::valid_mount_path)
    {
        return Err(PersistenceError::InvalidStackResource);
    }
    let independent: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM vms WHERE id=?1 AND purpose='instance')
        AND NOT EXISTS(SELECT 1 FROM cluster_worker_receipts WHERE id=?1)",
        [vm],
        |row| row.get(0),
    )?;
    if !independent {
        return Err(PersistenceError::InvalidStackResource);
    }
    // Observing an independently owned parent grants no permission to change that parent.
    let _ = iac_owners::public_write_allowed(
        conn,
        &ManagedResource {
            kind: "volume".into(),
            id: volume.into(),
        },
    )?;
    super::iac_resources::ensure_independent(
        conn,
        &ManagedResource {
            kind: "vm".into(),
            id: vm.into(),
        },
    )?;
    let mut stmt = conn.prepare("SELECT deployment FROM micro_service_deployments")?;
    let deployments = stmt.query_map([], |row| row.get::<_, String>(0))?;
    for raw in deployments {
        let deployment: firecrab_api_types::MicroServiceDeployment =
            serde_json::from_str(&raw?).map_err(|_| PersistenceError::IacOwnershipConflict)?;
        if deployment.owned_vm_ids().any(|id| id.to_string() == vm) {
            return Err(PersistenceError::IacParentManaged);
        }
    }
    Ok(())
}

/// Parent deletion must not implicitly detach another writer's configuration.
pub(super) fn ensure_vm_deletable(conn: &Connection, vm: Uuid) -> Result<(), PersistenceError> {
    let managed: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM iac_resource_owners
        WHERE kind='volume_attachment' AND manager IS NOT NULL AND substr(resource_id,-37)=':'||?1)
        OR EXISTS(SELECT 1 FROM micro_block_storages v JOIN iac_resource_owners o
        ON o.kind='volume' AND o.resource_id=v.id WHERE v.attached_vm_id=?1 AND o.manager IS NOT NULL)",
        [vm.to_string()], |row| row.get(0))?;
    if managed {
        return Err(PersistenceError::IacManagedDependent);
    }
    let active: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM iac_native_operations WHERE kind='volume_attachment'
        AND substr(resource_id,-37)=':'||?1 AND state IN ('running','needs_recovery'))",
        [vm.to_string()],
        |row| row.get(0),
    )?;
    if active {
        return Err(PersistenceError::IacResourceBusy);
    }
    Ok(())
}

pub(super) fn ensure_volume_deletable(
    conn: &Connection,
    volume: Uuid,
) -> Result<(), PersistenceError> {
    let prefix = format!("{volume}:");
    let managed: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM iac_resource_owners WHERE kind='volume_attachment'
        AND substr(resource_id,1,37)=?1 AND manager IS NOT NULL)",
        [&prefix],
        |row| row.get(0),
    )?;
    if managed {
        return Err(PersistenceError::IacManagedDependent);
    }
    let active: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM iac_native_operations WHERE kind='volume_attachment'
        AND substr(resource_id,1,37)=?1 AND state IN ('running','needs_recovery'))",
        [&prefix],
        |row| row.get(0),
    )?;
    if active {
        return Err(PersistenceError::IacResourceBusy);
    }
    Ok(())
}

/// Captured before disk verification. There is no deserializable authority token.
pub(crate) struct AttachmentAuthority {
    snapshots: [ResourceOwnership; 3],
}

pub(super) fn check_authority(
    conn: &Connection,
    volume: Uuid,
    vm: Uuid,
    expected: &AttachmentAuthority,
) -> Result<(), PersistenceError> {
    for (target, snapshot) in targets(volume, vm).iter().zip(&expected.snapshots) {
        if &snapshot.resource != target || iac_owners::read(conn, target)? != *snapshot {
            return Err(PersistenceError::StaleIacOwner);
        }
        if !iac_owners::public_write_allowed(conn, target)? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
    }
    Ok(())
}
fn targets(volume: Uuid, vm: Uuid) -> [ManagedResource; 3] {
    [
        ManagedResource {
            kind: "volume".into(),
            id: volume.to_string(),
        },
        super::iac_vm_deletes::resource(vm),
        resource(volume, vm),
    ]
}

impl Store {
    pub(crate) fn attachment_authority(
        &self,
        volume: Uuid,
        vm: Uuid,
    ) -> Result<AttachmentAuthority, PersistenceError> {
        if volume.is_nil() || vm.is_nil() {
            return Err(PersistenceError::InvalidIacOwner);
        }
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let [volume, vm, attachment] = targets(volume, vm);
        let snapshots = [
            iac_owners::read(&tx, &volume)?,
            iac_owners::read(&tx, &vm)?,
            iac_owners::read(&tx, &attachment)?,
        ];
        for snapshot in &snapshots {
            if !iac_owners::public_write_allowed(&tx, &snapshot.resource)? {
                return Err(PersistenceError::IacOwnershipConflict);
            }
        }
        tx.commit()?;
        Ok(AttachmentAuthority { snapshots })
    }
    pub(crate) fn ensure_vm_attachments_unmanaged(&self, vm: Uuid) -> Result<(), PersistenceError> {
        ensure_vm_deletable(&self.lock(), vm)
    }
}
