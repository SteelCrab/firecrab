//! Frozen source -> durable copy -> atomic registry cutover. No dual writes.
use super::super::ai_sandbox::{index_task, verify_references};
use super::*;
use crate::ai_sandbox::SavedTask;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
};

/// Stream records rather than aggregate retained file bodies in memory.
fn copy_or_hash(
    source: &Connection,
    project: Uuid,
    target: Option<&Connection>,
) -> Result<(String, usize), PersistenceError> {
    let mut q=source.prepare("SELECT c.ordinal,t.id,t.project_id,t.spec,a.attempt_id,a.vm_id,a.runtime_id,a.caller,a.source_path,d.identity FROM ai_sandbox_tasks t JOIN sandbox_task_catalog c ON c.id=t.id JOIN ai_sandbox_attempts a ON a.task_id=t.id JOIN ai_sandbox_admissions d ON d.task_id=t.id WHERE t.project_id=?1 AND c.project_id=t.project_id AND c.attempt_id=a.attempt_id ORDER BY c.ordinal")?;
    let mut rows = q.query([project.to_string()])?;
    let mut hash = Sha256::new();
    let mut count = 0;
    while let Some(row) = rows.next()? {
        let ordinal: i64 = row.get(0)?;
        let id: String = row.get(1)?;
        let p: String = row.get(2)?;
        let spec: String = row.get(3)?;
        let attempt: String = row.get(4)?;
        let vm: Option<String> = row.get(5)?;
        let runtime: Option<String> = row.get(6)?;
        let caller: String = row.get(7)?;
        let path: Option<String> = row.get(8)?;
        let admission: String = row.get(9)?;
        let saved: SavedTask = decode(spec.clone())?;
        if saved.task.id.to_string() != id
            || saved.task.project_id != project
            || p != project.to_string()
        {
            return Err(corrupt());
        }
        verify_references(source, &saved)?;
        if saved.task.state.active() || saved.guest_pending || saved.guest_staged {
            return Err(unavailable());
        }
        let raw = serde_json::to_vec(&(
            ordinal, &id, &p, &spec, &attempt, &vm, &runtime, &caller, &path, &admission,
        ))
        .map_err(|_| corrupt())?;
        hash.update((raw.len() as u64).to_be_bytes());
        hash.update(raw);
        count += 1;
        if let Some(t) = target {
            t.execute(
                "INSERT INTO ai_sandbox_attempts VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![id, p, attempt, vm, runtime, caller, path],
            )?;
            t.execute(
                "INSERT INTO ai_sandbox_admissions VALUES(?1,?2)",
                params![id, admission],
            )?;
            t.execute(
                "INSERT INTO ai_sandbox_tasks(rowid,id,project_id,spec) VALUES(?1,?2,?3,?4)",
                params![ordinal, id, p, spec],
            )?;
            index_task(t, ordinal, &saved)?;
        }
    }
    let total: i64 = source.query_row(
        "SELECT COUNT(*) FROM ai_sandbox_tasks WHERE project_id=?1",
        [project.to_string()],
        |r| r.get(0),
    )?;
    if count != usize::try_from(total).map_err(|_| corrupt())? {
        return Err(corrupt());
    }
    Ok((format!("{:x}", hash.finalize()), count))
}
fn backup_source(source: &Connection, db: &ProjectDb, r: &Route) -> Result<(), PersistenceError> {
    let root = db.directory.join("sandbox-migrations");
    crate::project_workspace::db::private_dir(&root).map_err(|_| unavailable())?;
    let directory = root.join(r.operation.to_string());
    crate::project_workspace::db::private_dir(&directory).map_err(|_| unavailable())?;
    let published = directory.join("source.db");
    let absent = match fs::symlink_metadata(&published) {
        Ok(_) => false,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => return Err(unavailable()),
    };
    if absent {
        // Interrupted unpublished stages are retained; retry gets a fresh stage.
        let partial = directory.join(format!("source-{}.partial", Uuid::new_v4()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&partial)
            .map_err(|_| unavailable())?;
        drop(file);
        source.backup(rusqlite::MAIN_DB, &partial, None)?;
        File::open(&partial)
            .and_then(|f| f.sync_all())
            .map_err(|_| unavailable())?;
        fs::rename(&partial, &published).map_err(|_| unavailable())?;
        File::open(&directory)
            .and_then(|f| f.sync_all())
            .map_err(|_| unavailable())?;
    }
    let file =
        crate::project_workspace::db::private_file(&published, false).map_err(|_| unavailable())?;
    let backup = Connection::open_with_flags(
        &published,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let valid: String = backup.query_row("PRAGMA quick_check", [], |q| q.get(0))?;
    if valid != "ok"
        || copy_or_hash(&backup, r.binding.project_id, None)? != (r.source_sha.clone(), r.count)
    {
        return Err(unavailable());
    }
    // Hash the complete private archive; it can contain controller secrets and is never exposed to guests.
    let mut reader = file;
    let mut digest = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = reader.read(&mut buf).map_err(|_| unavailable())?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
    }
    let manifest = directory.join("source.sha256");
    let sum = format!("{:x}", digest.finalize());
    match fs::symlink_metadata(&manifest) {
        Ok(_) => {
            let mut f = crate::project_workspace::db::private_file(&manifest, false)
                .map_err(|_| unavailable())?;
            if f.metadata().map_err(|_| unavailable())?.len() != 64 {
                return Err(unavailable());
            }
            let mut old = String::new();
            f.read_to_string(&mut old).map_err(|_| unavailable())?;
            if old != sum {
                return Err(unavailable());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let partial = directory.join(format!("checksum-{}.partial", Uuid::new_v4()));
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&partial)
                .map_err(|_| unavailable())?;
            f.write_all(sum.as_bytes())
                .and_then(|_| f.sync_all())
                .map_err(|_| unavailable())?;
            fs::rename(partial, &manifest).map_err(|_| unavailable())?;
        }
        Err(_) => return Err(unavailable()),
    }

    File::open(&directory)
        .and_then(|f| f.sync_all())
        .map_err(|_| unavailable())?;
    Ok(())
}
impl Store {
    pub(crate) fn migrate_sandbox_tasks(
        &self,
        binding: &Binding,
        db: &ProjectDb,
        operation: Uuid,
        checkpoint: Uuid,
    ) -> Result<(), PersistenceError> {
        let (_, _, initialized) = db.document().map_err(|_| unavailable())?;
        if binding.provisioning || !initialized || operation.is_nil() || checkpoint.is_nil() {
            return Err(unavailable());
        }
        let identity: (String, String, String) = db.conn.query_row(
            "SELECT project_id,database_id,volume_id FROM identity WHERE singleton=1 AND epoch=?1",
            [db.epoch],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if identity
            != (
                binding.project_id.to_string(),
                binding.database_id.to_string(),
                binding.volume_id.to_string(),
            )
        {
            return Err(unavailable());
        }
        let _gate = self.sandbox_gate.lock().unwrap_or_else(|p| p.into_inner());
        let source = self.lock();
        let _durable = Durable::new(&source)?;
        let tx = source.unchecked_transaction()?;
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT phase,document FROM sandbox_task_routes WHERE project_id=?1",
                [binding.project_id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let route = if let Some((phase, raw)) = existing {
            let r = decode_route(&binding.project_id.to_string(), raw)?;
            if r.operation != operation || r.checkpoint != checkpoint || r.binding != *binding {
                return Err(unavailable());
            }
            if phase == "project" {
                let marker: Option<(String, String)> = db
                    .conn
                    .query_row(
                        "SELECT operation,source_sha FROM sandbox_migration WHERE singleton=1",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                if marker != Some((r.operation.to_string(), r.source_sha)) {
                    return Err(unavailable());
                }
                return Ok(());
            }
            if phase != "prepared" {
                return Err(unavailable());
            }
            r
        } else {
            let (source_sha, count) = copy_or_hash(&tx, binding.project_id, None)?;
            let r = Route {
                binding: binding.clone(),
                operation,
                checkpoint,
                source_sha,
                count,
            };
            tx.execute(
                "INSERT INTO sandbox_task_routes VALUES(?1,'prepared',?2)",
                params![binding.project_id.to_string(), ai_sandbox::encode(&r)?],
            )?;
            r
        };
        tx.commit()?;
        backup_source(&source, db, &route)?;
        let marker: Option<(String, String)> = db
            .conn
            .query_row(
                "SELECT operation,source_sha FROM sandbox_migration WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some(marker) = marker {
            if marker != (operation.to_string(), route.source_sha.clone()) {
                return Err(unavailable());
            }
            // Reconstruct only the catalog projection needed for the same streaming checksum.
            db.conn.execute_batch("CREATE TEMP TABLE IF NOT EXISTS sandbox_task_catalog(ordinal INTEGER,id TEXT,project_id TEXT,attempt_id TEXT);")?;
            db.conn
                .execute("DELETE FROM temp.sandbox_task_catalog", [])?;
            db.conn.execute("INSERT INTO temp.sandbox_task_catalog SELECT i.ordinal,i.id,i.project_id,a.attempt_id FROM ai_sandbox_task_index i JOIN ai_sandbox_attempts a ON a.task_id=i.id",[])?;
            if copy_or_hash(&db.conn, binding.project_id, None)?
                != (route.source_sha.clone(), route.count)
            {
                return Err(unavailable());
            }
        } else {
            let target = db.conn.unchecked_transaction()?;
            let existing: i64 =
                target.query_row("SELECT count(*) FROM ai_sandbox_tasks", [], |r| r.get(0))?;
            if existing != 0 {
                return Err(unavailable());
            }
            if copy_or_hash(&source, binding.project_id, Some(&target))?
                != (route.source_sha.clone(), route.count)
            {
                return Err(unavailable());
            }
            target.execute(
                "INSERT INTO sandbox_migration VALUES(1,?1,?2)",
                params![operation.to_string(), route.source_sha],
            )?;
            target.commit()?;
        }
        let cutover = source.unchecked_transaction()?;
        if copy_or_hash(&cutover, binding.project_id, None)?
            != (route.source_sha.clone(), route.count)
        {
            return Err(unavailable());
        }
        let changed=cutover.execute("UPDATE sandbox_task_routes SET phase='cutover' WHERE project_id=?1 AND phase='prepared' AND document=?2",params![binding.project_id.to_string(),ai_sandbox::encode(&route)?])?;
        if changed != 1 {
            return Err(unavailable());
        }
        for table in [
            "ai_sandbox_tasks",
            "ai_sandbox_task_index",
            "ai_sandbox_attempts",
        ] {
            cutover.execute(
                &format!("DELETE FROM {table} WHERE project_id=?1"),
                [binding.project_id.to_string()],
            )?;
        }
        cutover.execute("DELETE FROM ai_sandbox_admissions WHERE task_id IN (SELECT id FROM sandbox_task_catalog WHERE project_id=?1)",[binding.project_id.to_string()])?;
        cutover.execute(
            "UPDATE sandbox_task_catalog SET ready=1 WHERE project_id=?1",
            [binding.project_id.to_string()],
        )?;
        cutover.execute(
            "UPDATE sandbox_task_routes SET phase='project' WHERE project_id=?1",
            [binding.project_id.to_string()],
        )?;
        cutover.commit()?;
        Ok(())
    }
}
