//! Private immutable backup bytes and restartable, fenced two-file installation.
use super::*;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::io::{Read, Seek, SeekFrom};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Original {
    pub(super) database: Option<String>,
    pub(super) volume: Option<String>,
    pub(super) wal: Option<String>,
    pub(super) shm: Option<String>,
    /// Hash of a restored DB whose imported task ordering was translated.
    #[serde(default)]
    pub(super) installed_database: Option<String>,
}
/// Replace only our validated staging file, then publish and sync its parent directory.
pub(super) fn publish(path: &FsPath, bytes: &[u8]) -> Result<(), Error> {
    let partial = path.with_extension("partial");
    let mut file = db::private_file(&partial, true)?;
    file.set_len(0)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(partial, path)?;
    File::open(path.parent().ok_or(Error::Missing)?)?.sync_all()?;
    Ok(())
}
fn hash(path: &FsPath) -> Result<String, Error> {
    let _ = db::private_file(path, false)?;
    checkpoint::digest(path)
}
fn optional_hash(path: &FsPath) -> Result<Option<String>, Error> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
        Ok(_) => hash(path).map(Some),
    }
}
fn valid_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn connection(path: &FsPath) -> Result<Connection, Error> {
    let _ = db::private_file(path, false)?;
    Ok(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?)
}
/// Read-only validation must not change the archived DB owner/epoch or its integrity hash.
pub(super) fn database(state: &AppState, path: &FsPath, b: &Binding) -> Result<(u32, u64), Error> {
    let c = connection(path)?;
    let application: i64 = c.pragma_query_value(None, "application_id", |r| r.get(0))?;
    let schema: u32 = c.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let integrity: String = c.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    let foreign: bool = c.prepare("PRAGMA foreign_key_check")?.exists([])?;
    let row:(String,String,String,i64,bool)=c.query_row("SELECT project_id,database_id,volume_id,revision,initialized FROM identity WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    if application != 0x46575031
        || schema == 0
        || schema > db::SCHEMA_VERSION
        || integrity != "ok"
        || foreign
        || !row.4
        || row.0 != b.project_id.to_string()
        || row.1 != b.database_id.to_string()
        || row.2 != b.volume_id.to_string()
    {
        return Err(Error::Conflict(
            "Backup database identity, integrity or initialization is invalid",
        ));
    }
    let routing = state.store.sandbox_storage_status(b.project_id)?;
    if routing["storage"] != "controller" {
        if routing["storage"] != "project" {
            return Err(Error::Conflict(
                "Finish Sandbox storage migration before restoring",
            ));
        }
        let marker: Option<(String, String)> = c
            .query_row(
                "SELECT operation,source_sha FROM sandbox_migration WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if marker
            != routing["operationId"]
                .as_str()
                .zip(routing["sourceSha256"].as_str())
                .map(|(a, b)| (a.to_owned(), b.to_owned()))
        {
            return Err(Error::Conflict(
                "Backup does not match canonical Sandbox storage",
            ));
        }
    } else if schema >= 8 {
        let marker: bool =
            c.query_row("SELECT EXISTS(SELECT 1 FROM sandbox_migration)", [], |r| {
                r.get(0)
            })?;
        if marker {
            return Err(Error::Conflict(
                "Backup Sandbox authority differs from this controller",
            ));
        }
    }
    let tasks: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='ai_sandbox_tasks')",
        [],
        |r| r.get(0),
    )?;
    if tasks
        && c.query_row::<bool, _, _>(
            "SELECT EXISTS(SELECT 1 FROM ai_sandbox_task_index WHERE busy=1)",
            [],
            |r| r.get(0),
        )?
    {
        return Err(Error::Conflict("Backup contains unfinished Sandbox tasks"));
    }
    if schema >= 2
        && c.query_row::<bool, _, _>(
            "SELECT EXISTS(SELECT 1 FROM file_operations WHERE state='pending')",
            [],
            |r| r.get(0),
        )?
    {
        return Err(Error::Conflict(
            "Reconcile pending file operations before backup",
        ));
    }
    Ok((
        schema,
        u64::try_from(row.3).map_err(|_| Error::Conflict("Invalid backup revision"))?,
    ))
}
fn copy(source: &FsPath, target: &FsPath) -> Result<String, Error> {
    let mut input = db::private_file(source, false)?;
    let mut output = db::private_file(target, true)?;
    output.set_len(0)?;
    let mut digest = Sha256::new();
    let mut bytes = vec![0; 1024 * 1024];
    let zeros = vec![0; bytes.len()];
    loop {
        let n = input.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        digest.update(&bytes[..n]);
        // Keep sparse data disks sparse. Length is committed before the file is published.
        if bytes[..n] == zeros[..n] {
            output.seek(SeekFrom::Current(n as i64))?;
        } else {
            output.write_all(&bytes[..n])?;
        }
    }
    output.set_len(input.metadata()?.len())?;
    output.sync_all()?;
    File::open(target.parent().ok_or(Error::Missing)?)?.sync_all()?;
    Ok(format!("{:x}", digest.finalize()))
}
fn snapshot(source: &Connection, path: &FsPath) -> Result<(), Error> {
    let file = db::private_file(path, true)?;
    file.set_len(0)?;
    source.backup(rusqlite::MAIN_DB, path, None)?;
    // A standalone backup must never depend on an unarchived WAL.
    let copy = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    copy.pragma_update(None, "journal_mode", "DELETE")?;
    drop(copy);
    file.sync_all()?;
    Ok(())
}
fn volume(path: &FsPath, id: Uuid, bytes: u64) -> Result<(), Error> {
    let mut file = db::private_file(path, false)?;
    let mut header = [0; 120];
    file.seek(SeekFrom::Start(1024))?;
    file.read_exact(&mut header)?;
    if file.metadata()?.len() != bytes
        || header[56..58] != [0x53, 0xef]
        || header[104..120] != *id.as_bytes()
    {
        return Err(Error::Conflict(
            "Backup volume size, filesystem or UUID is invalid",
        ));
    }
    Ok(())
}
pub(super) fn completed(
    state: &AppState,
    root: &FsPath,
    b: &Binding,
    id: Uuid,
) -> Result<ProjectBackupManifest, Error> {
    let path = root.join(id.to_string());
    db::private_dir(&path)?;
    let f = db::private_file(&path.join("manifest.json"), false)?;
    if f.metadata()?.len() > 16384 {
        return Err(Error::Conflict("Invalid backup manifest"));
    }
    let m: ProjectBackupManifest = serde_json::from_reader(f)?;
    let c = &m.checkpoint;
    if c.id != id
        || c.project_id != b.project_id
        || c.database_id != b.database_id
        || c.volume_id != b.volume_id
        || c.generation == 0
        || c.generation > b.generation
        || !valid_hash(&c.database_sha256)
        || !valid_hash(&c.volume_sha256)
        || !valid_hash(&m.control_database_sha256)
        || hash(&path.join("project.db"))? != c.database_sha256
        || hash(&path.join("data.ext4"))? != c.volume_sha256
        || hash(&path.join("control.db"))? != m.control_database_sha256
    {
        return Err(Error::Conflict(
            "Backup integrity or resource identity does not match",
        ));
    }
    let expected = state.store.block_storage(b.volume_id)?;
    if m.volume_bytes != (u64::from(expected.size_gib) << 30) {
        return Err(Error::Conflict(
            "Backup volume size differs from its registered size",
        ));
    }
    volume(&path.join("data.ext4"), b.volume_id, m.volume_bytes)?;
    if database(state, &path.join("project.db"), b)? != (c.schema_version, c.revision) {
        return Err(Error::Conflict(
            "Backup schema or revision differs from its manifest",
        ));
    }
    Ok(m)
}
pub(super) fn create(
    state: &AppState,
    root: &FsPath,
    live: &FsPath,
    r: &mut Record,
) -> Result<(), Error> {
    let path = root.join(r.public.id.to_string());
    db::private_dir(&path)?;
    if path.join("manifest.json").exists() {
        r.public.manifest = Some(completed(state, root, &r.binding, r.public.id)?);
        return Ok(());
    }
    let disk = state.store.block_storage(r.binding.volume_id)?;
    let source = block_storage::verify_disk(state, &disk)
        .map_err(|_| Error::Conflict("Retained disk is unavailable"))?;
    if r.public.phase == "pending" {
        let (schema, revision) = database(state, &live.join("project.db"), &r.binding)?;
        snapshot(
            &connection(&live.join("project.db"))?,
            &path.join("project.db"),
        )?;
        let file = db::private_file(&path.join("control.db"), true)?;
        file.set_len(0)?;
        state.store.backup_control(&path.join("control.db"))?;
        file.sync_all()?;
        r.public.manifest = Some(ProjectBackupManifest {
            checkpoint: ProjectWorkspaceCheckpoint {
                id: r.public.id,
                project_id: r.binding.project_id,
                database_id: r.binding.database_id,
                volume_id: r.binding.volume_id,
                generation: r.binding.generation,
                schema_version: schema,
                revision,
                database_sha256: hash(&path.join("project.db"))?,
                volume_sha256: String::new(),
                created_at: ai_sandbox::now(),
            },
            volume_bytes: u64::from(disk.size_gib) << 30,
            control_database_sha256: hash(&path.join("control.db"))?,
        });
        progress(state, r, "database_frozen")?;
    }
    let m = r
        .public
        .manifest
        .as_mut()
        .ok_or(Error::Conflict("Frozen backup metadata is missing"))?;
    if hash(&path.join("project.db"))? != m.checkpoint.database_sha256
        || hash(&path.join("control.db"))? != m.control_database_sha256
    {
        return Err(Error::Conflict("Frozen backup bytes changed"));
    }
    m.checkpoint.volume_sha256 = copy(&source, &path.join("data.ext4"))?;
    publish(&path.join("manifest.json"), &serde_json::to_vec(m)?)?;
    File::open(root)?.sync_all()?;
    r.public.manifest = Some(completed(state, root, &r.binding, r.public.id)?);
    Ok(())
}
fn suffix(path: &FsPath, label: &str, id: Uuid) -> PathBuf {
    path.with_file_name(format!(
        "{}.{}-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        label,
        id
    ))
}
/// A crash after either rename can resume only from the journaled old/new hashes.
fn install(path: &FsPath, id: Uuid, new: &str, old: &Option<String>) -> Result<(), Error> {
    let stage = suffix(path, "restore", id);
    let previous = suffix(path, "before", id);
    let target = optional_hash(path)?;
    let saved = optional_hash(&previous)?;
    if optional_hash(&stage)?.is_none() && target.as_deref() == Some(new) && saved == *old {
        return Ok(());
    }
    if hash(&stage)? != new || (saved.is_some() && saved != *old) {
        return Err(Error::Conflict(
            "Restore staging or preserved original changed",
        ));
    }
    if target.is_some() {
        if target != *old || saved.is_some() {
            return Err(Error::Conflict("Restore destination changed"));
        }
        fs::rename(path, &previous)?;
        File::open(path.parent().ok_or(Error::Missing)?)?.sync_all()?;
    } else if saved != *old {
        return Err(Error::Conflict("Restore original is missing"));
    }
    fs::rename(stage, path)?;
    File::open(path.parent().ok_or(Error::Missing)?)?.sync_all()?;
    Ok(())
}
fn preserve(path: &FsPath, id: Uuid, expected: &Option<String>) -> Result<(), Error> {
    let previous = suffix(path, "before", id);
    if optional_hash(path)?.is_none() && optional_hash(&previous)? == *expected {
        return Ok(());
    }
    if optional_hash(path)? != *expected || optional_hash(&previous)?.is_some() {
        return Err(Error::Conflict("Original database journal changed"));
    }
    if expected.is_some() {
        fs::rename(path, previous)?;
        File::open(path.parent().ok_or(Error::Missing)?)?.sync_all()?;
    }
    Ok(())
}
/// Translate only the imported checkpoint's task index; preserve the source archive.
fn translate_imported_ordinals(
    state: &AppState,
    path: &FsPath,
    binding: &Binding,
    checkpoint: Uuid,
) -> Result<Option<String>, Error> {
    let Some(ordinals) = state
        .store
        .imported_checkpoint_ordinals(binding.project_id, checkpoint)?
    else {
        return Ok(None);
    };
    let _file = db::private_file(path, false)?;
    let mut conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='ai_sandbox_task_index')", [], |r| r.get(0))?;
    if !exists {
        return Ok(None);
    }
    // This is a disposable staging copy. A crash before the prepared receipt
    // causes it to be recopied from the verified checkpoint on retry.
    conn.pragma_update(None, "journal_mode", "MEMORY")?;
    let tx = conn.transaction()?;
    let entries: Vec<(String, i64)> = {
        let mut query =
            tx.prepare("SELECT id,ordinal FROM ai_sandbox_task_index WHERE project_id=?1")?;
        let rows = query.query_map([binding.project_id.to_string()], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    if state.store.sandbox_storage_status(binding.project_id)?["storage"] == "project" {
        let ids = entries.iter().map(|(id, _)| id.clone()).collect();
        state
            .store
            .require_checkpoint_tasks(binding.project_id, &ids)?;
    }
    for (id, old) in entries {
        let new = ordinals
            .get(&id)
            .copied()
            .filter(|n| *n > 0)
            .ok_or(Error::Conflict("Imported task ordinal is missing"))?;
        if new != old {
            tx.execute(
                "UPDATE ai_sandbox_task_index SET ordinal=?2 WHERE id=?1",
                rusqlite::params![id, new],
            )?;
        }
    }
    tx.commit()?;
    drop(conn);
    db::private_file(path, false)?.sync_all()?;
    Ok(Some(hash(path)?))
}

/// Read the hash of the actual staged DB, including any recorded ordinal translation.
fn restored_database_hash(r: &Record) -> Result<&str, Error> {
    let manifest = r.public.manifest.as_ref().ok_or(Error::Missing)?;
    Ok(r.original
        .as_ref()
        .and_then(|o| o.installed_database.as_deref())
        .unwrap_or(&manifest.checkpoint.database_sha256))
}

fn prepare_restore(
    state: &AppState,
    root: &FsPath,
    live: &FsPath,
    r: &mut Record,
) -> Result<(), Error> {
    let id = r.public.id;
    let source = r.public.request.restore_checkpoint.ok_or(Error::Missing)?;
    if r.public.phase == "pending" {
        restore_source(state, &r.binding, source)?;
        let m = completed(state, root, &r.binding, source)?;
        let disk = state.store.block_storage(r.binding.volume_id)?;
        let target = block_storage::disk_path(state, &disk)
            .map_err(|_| Error::Conflict("Registered disk path unavailable"))?;
        db::private_dir(target.parent().ok_or(Error::Missing)?)?;
        let db_path = live.join("project.db");
        let stage = suffix(&db_path, "restore", id);
        if copy(&root.join(source.to_string()).join("project.db"), &stage)?
            != m.checkpoint.database_sha256
        {
            return Err(Error::Conflict("Database staging integrity failed"));
        }
        let installed_database = translate_imported_ordinals(state, &stage, &r.binding, source)?;
        let stage = suffix(&target, "restore", id);
        if copy(&root.join(source.to_string()).join("data.ext4"), &stage)?
            != m.checkpoint.volume_sha256
        {
            return Err(Error::Conflict("Volume staging integrity failed"));
        }
        // All old files, including a damaged database's WAL, are preserved independently.
        r.original = Some(Original {
            database: optional_hash(&db_path)?,
            volume: optional_hash(&target)?,
            wal: optional_hash(&live.join("project.db-wal"))?,
            shm: optional_hash(&live.join("project.db-shm"))?,
            installed_database,
        });
        r.public.manifest = Some(m);
        progress(state, r, "prepared")?;
    }
    Ok(())
}
pub(super) fn verify_installed(state: &AppState, r: &Record) -> Result<(), Error> {
    let current = raw_binding(state, r.binding.project_id)?;
    if current.database_id != r.binding.database_id
        || current.volume_id != r.binding.volume_id
        || current.vm_id != r.binding.vm_id
        || current.generation != r.public.restored_generation.unwrap_or(r.binding.generation)
    {
        return Err(Error::Conflict("Restored resource registration changed"));
    }
    let live = db::project_directory(&state.workspaces.root, &r.binding, false)?;
    let disk = state.store.block_storage(r.binding.volume_id)?;
    let target = block_storage::disk_path(state, &disk).map_err(|_| Error::Missing)?;
    let m = r.public.manifest.as_ref().ok_or(Error::Missing)?;
    if hash(&live.join("project.db"))? != restored_database_hash(r)?
        || hash(&target)? != m.checkpoint.volume_sha256
    {
        return Err(Error::Conflict("Restored pair integrity is unconfirmed"));
    }
    Ok(())
}
#[cfg(test)]
pub(super) fn interrupt_after_database(
    state: &AppState,
    root: &FsPath,
    live: &FsPath,
    r: &mut Record,
) -> Result<(), Error> {
    prepare_restore(state, root, live, r)?;
    progress(state, r, "installing")?;
    let old = r.original.as_ref().unwrap();
    preserve(&live.join("project.db-wal"), r.public.id, &old.wal)?;
    preserve(&live.join("project.db-shm"), r.public.id, &old.shm)?;
    install(
        &live.join("project.db"),
        r.public.id,
        restored_database_hash(r)?,
        &old.database,
    )
}
pub(super) fn restore(
    state: &AppState,
    root: &FsPath,
    live: &FsPath,
    r: &mut Record,
) -> Result<(), Error> {
    let id = r.public.id;
    prepare_restore(state, root, live, r)?;
    let m = r
        .public
        .manifest
        .as_ref()
        .ok_or(Error::Conflict("Restore manifest is missing"))?
        .clone();
    let old = r
        .original
        .clone()
        .ok_or(Error::Conflict("Restore original journal is missing"))?;
    if r.public.phase == "prepared" {
        progress(state, r, "installing")?;
    }
    if r.public.phase != "installing" {
        return Err(Error::Conflict("Invalid restore phase"));
    }
    let disk = state.store.block_storage(r.binding.volume_id)?;
    let target = block_storage::disk_path(state, &disk)
        .map_err(|_| Error::Conflict("Registered disk path unavailable"))?;
    preserve(&live.join("project.db-wal"), id, &old.wal)?;
    preserve(&live.join("project.db-shm"), id, &old.shm)?;
    install(
        &live.join("project.db"),
        id,
        restored_database_hash(r)?,
        &old.database,
    )?;
    install(&target, id, &m.checkpoint.volume_sha256, &old.volume)?;
    verify_installed(state, r)
}

pub(super) fn restore_source(state: &AppState, b: &Binding, source: Uuid) -> Result<(), Error> {
    let receipt = state.store.backup_record(source)?.ok_or(Error::Missing)?;
    if receipt.public.project_id != b.project_id
        || receipt.public.request.restore_checkpoint.is_some()
        || receipt.public.phase != "complete"
        || receipt.active
        || receipt.public.cleanup_id.is_some()
    {
        return Err(Error::Conflict(
            "Backup is incomplete or selected for cleanup",
        ));
    }
    Ok(())
}
