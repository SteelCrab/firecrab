//! Atomic reservations for data disks. Attachments survive API and VM restarts.

use super::*;
use firecrab_api_types::{MicroBlockStorageResponse, VmBlockStorageMount};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS micro_block_storages (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    size_gib INTEGER NOT NULL CHECK (size_gib BETWEEN 1 AND 65535),
    storage_root TEXT NOT NULL,
    phase TEXT NOT NULL CHECK (phase IN ('creating', 'ready', 'deleting')),
    attached_vm_id TEXT,
    mount_path TEXT,
    CHECK ((attached_vm_id IS NULL) = (mount_path IS NULL)),
    CHECK (phase = 'ready' OR attached_vm_id IS NULL)
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS block_storage_mount
    ON micro_block_storages(attached_vm_id, mount_path);
CREATE TRIGGER IF NOT EXISTS release_block_storages_after_vm_delete
    AFTER DELETE ON vms BEGIN
        UPDATE micro_block_storages SET attached_vm_id = NULL, mount_path = NULL
        WHERE attached_vm_id = OLD.id;
    END;
";

const SELECT: &str = "SELECT id, name, size_gib, storage_root,
    CASE WHEN phase = 'ready' THEN
        CASE WHEN attached_vm_id IS NULL THEN 'available' ELSE 'attached' END
    ELSE phase END, attached_vm_id, mount_path FROM micro_block_storages";

fn decode(row: &rusqlite::Row<'_>) -> rusqlite::Result<MicroBlockStorageResponse> {
    let uuid = |index| -> rusqlite::Result<Uuid> {
        let text: String = row.get(index)?;
        Uuid::parse_str(&text).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
    };
    let attached: Option<String> = row.get(5)?;
    Ok(MicroBlockStorageResponse {
        id: uuid(0)?,
        name: row.get(1)?,
        size_gib: row.get(2)?,
        storage_root: row.get(3)?,
        state: row.get(4)?,
        attached_vm_id: attached.map(|_| uuid(5)).transpose()?,
        mount_path: row.get(6)?,
    })
}

impl Store {
    /// Returns volumes ordered by name; pending disk work remains visible.
    pub(crate) fn block_storages(
        &self,
    ) -> Result<Vec<MicroBlockStorageResponse>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare(&format!("{SELECT} ORDER BY name, id"))?;
        Ok(statement
            .query_map([], decode)?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Finds one volume without resolving or exposing a host path.
    pub(crate) fn block_storage(
        &self,
        id: Uuid,
    ) -> Result<MicroBlockStorageResponse, PersistenceError> {
        self.lock()
            .query_row(&format!("{SELECT} WHERE id = ?1"), [id.to_string()], decode)
            .optional()?
            .ok_or(PersistenceError::MissingBlockStorage { id })
    }

    /// Reserves a name before disk creation so interrupted work can be recovered.
    pub(crate) fn insert_block_storage(
        &self,
        volume: &MicroBlockStorageResponse,
        registered_pool: Option<Uuid>,
    ) -> Result<(), PersistenceError> {
        let conn = self.lock();
        if let Some(pool_id) = registered_pool {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM micro_storages WHERE id = ?1)",
                [pool_id.to_string()],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(PersistenceError::BlockStorageConflict {
                    message: "MicroStorage no longer exists",
                });
            }
        }
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM micro_block_storages WHERE name = ?1)",
            [&volume.name],
            |row| row.get(0),
        )?;
        if exists {
            return Err(PersistenceError::BlockStorageConflict {
                message: "volume name is already registered",
            });
        }
        conn.execute(
            "INSERT INTO micro_block_storages (id, name, size_gib, storage_root, phase)
             VALUES (?1, ?2, ?3, ?4, 'creating')",
            params![
                volume.id.to_string(),
                volume.name,
                volume.size_gib,
                volume.storage_root
            ],
        )?;
        Ok(())
    }

    /// Publishes only a fully formatted, synced disk.
    pub(crate) fn finish_block_storage_create(&self, id: Uuid) -> Result<(), PersistenceError> {
        let changed = self.lock().execute(
            "UPDATE micro_block_storages SET phase = 'ready' WHERE id = ?1 AND phase = 'creating'",
            [id.to_string()],
        )?;
        if changed == 0 {
            return Err(PersistenceError::BlockStorageConflict {
                message: "volume is not being created",
            });
        }
        Ok(())
    }

    /// Reserves an interrupted or failed creation for cleanup by its owning worker.
    pub(crate) fn abort_block_storage_create(&self, id: Uuid) -> Result<(), PersistenceError> {
        self.lock().execute(
            "UPDATE micro_block_storages SET phase = 'deleting' WHERE id = ?1 AND phase = 'creating'",
            [id.to_string()],
        )?;
        Ok(())
    }

    /// Reserves an unattached disk against concurrent attach before removing its file.
    pub(crate) fn begin_block_storage_delete(&self, id: Uuid) -> Result<(), PersistenceError> {
        let conn = self.lock();
        let retained: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM project_workspaces WHERE volume_id=?1)",
            [id.to_string()],
            |r| r.get(0),
        )?;
        if retained {
            return Err(PersistenceError::BlockStorageConflict {
                message: "volume is retained by a project workspace",
            });
        }
        let volume = conn
            .query_row(&format!("{SELECT} WHERE id = ?1"), [id.to_string()], decode)
            .optional()?
            .ok_or(PersistenceError::MissingBlockStorage { id })?;
        if volume.state == "creating" {
            return Err(PersistenceError::BlockStorageConflict {
                message: "volume creation is still in progress",
            });
        }
        if volume.attached_vm_id.is_some() {
            return Err(PersistenceError::BlockStorageConflict {
                message: "detach the volume before deleting it",
            });
        }
        conn.execute(
            "UPDATE micro_block_storages SET phase = 'deleting' WHERE id = ?1",
            [id.to_string()],
        )?;
        Ok(())
    }

    /// Removes metadata only after file cleanup; a failed cleanup remains retryable.
    pub(crate) fn finish_block_storage_delete(&self, id: Uuid) -> Result<(), PersistenceError> {
        self.lock().execute(
            "DELETE FROM micro_block_storages WHERE id = ?1 AND phase = 'deleting' AND attached_vm_id IS NULL",
            [id.to_string()],
        )?;
        Ok(())
    }

    /// Atomically reserves every requested disk, or leaves all reservations unchanged.
    pub(crate) fn attach_block_storages(
        &self,
        vm_id: Uuid,
        mounts: &[VmBlockStorageMount],
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
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
            let reserved: Option<(String, String)> = transaction
                .query_row(
                    "SELECT vm_id,project_id FROM project_workspaces WHERE volume_id=?1",
                    [mount.volume_id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if reserved.is_some_and(|(vm, _)| {
                vm != vm_id.to_string() || mount.mount_path != "/workspace/shared"
            }) {
                return Err(PersistenceError::BlockStorageConflict {
                    message: "volume is retained by another project workspace binding",
                });
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
    ) -> Result<(), PersistenceError> {
        let conn = self.lock();
        let retained: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM project_workspaces WHERE volume_id=?1)",
            [id.to_string()],
            |r| r.get(0),
        )?;
        if retained {
            return Err(PersistenceError::BlockStorageConflict {
                message: "rebind retained volumes through their project workspace",
            });
        }
        let changed = conn.execute(
            "UPDATE micro_block_storages SET attached_vm_id = NULL, mount_path = NULL
             WHERE id = ?1 AND attached_vm_id = ?2",
            params![id.to_string(), vm_id.to_string()],
        )?;
        if changed == 0 {
            return Err(PersistenceError::BlockStorageConflict {
                message: "volume attachment changed; refresh and retry",
            });
        }
        Ok(())
    }

    /// Returns one VM's disks in stable order for its boot configuration.
    pub(crate) fn vm_block_storages(
        &self,
        vm_id: Uuid,
    ) -> Result<Vec<MicroBlockStorageResponse>, PersistenceError> {
        let conn = self.lock();
        let mut statement =
            conn.prepare(&format!("{SELECT} WHERE attached_vm_id = ?1 ORDER BY id"))?;
        Ok(statement
            .query_map([vm_id.to_string()], decode)?
            .collect::<rusqlite::Result<_>>()?)
    }
}
