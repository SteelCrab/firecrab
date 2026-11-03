//! Positive file allowlists and retained per-file proofs; never recursively delete directories.
use super::*;
use std::os::unix::fs::MetadataExt;

/// Positive archive allowlist includes SQLite crash journals but excludes arbitrary directory entries.
const ARCHIVE_FILES: &[&str] = &[
    "project.db",
    "data.ext4",
    "control.db",
    "manifest.json",
    "manifest.partial",
    "project.db-journal",
    "project.db-wal",
    "project.db-shm",
    "control.db-journal",
    "control.db-wal",
    "control.db-shm",
];
/// File locations are derived from resource identities, never supplied as caller-controlled paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) enum Location {
    /// A validated basename inside the selected checkpoint archive.
    Archive(String),
    /// Database preserved by the selected completed restoration.
    DatabaseBefore,
    /// Original WAL preserved separately from the restored live database.
    WalBefore,
    /// Original shared-memory sidecar preserved during installation.
    ShmBefore,
    /// Original volume with the selected restoration ID suffix.
    VolumeBefore,
    /// Uninstalled database copy belonging to an aborted restoration.
    DatabaseStage,
    /// Uninstalled volume copy belonging to an aborted restoration.
    VolumeStage,
}
/// Durable identity and contents of one admitted unlink target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Proof {
    /// Backup or restore receipt owning this artifact.
    target: Uuid,
    /// Typed location used to rederive and validate the path on every retry.
    location: Location,
    /// Original resolved path; changed pool/project locations cannot redirect deletion.
    path: PathBuf,
    /// Full logical file digest captured before admission.
    sha256: String,
    /// Logical file length used for reconciliation accounting, not allocated sparse blocks.
    bytes: u64,
    /// Filesystem identity captured with the original private file descriptor.
    device: u64,
    /// File identity prevents replacing an admitted artifact with another file.
    inode: u64,
}
/// Derive only positively allowed filenames in existing project/archive/volume namespaces.
fn path(
    state: &AppState,
    b: &Binding,
    target: Uuid,
    location: &Location,
) -> Result<PathBuf, Error> {
    let before = |name: &str| format!("{name}.before-{target}");
    let stage = |name: &str| format!("{name}.restore-{target}");
    Ok(match location {
        Location::Archive(name) => {
            if !ARCHIVE_FILES.contains(&name.as_str()) {
                return Err(Error::Conflict("Invalid cleanup file name"));
            }
            root(&state.workspaces, b.project_id, false)?
                .join(target.to_string())
                .join(name)
        }
        Location::VolumeBefore | Location::VolumeStage => {
            let volume = state.store.block_storage(b.volume_id)?;
            let live = block_storage::disk_path(state, &volume)
                .map_err(|_| Error::Conflict("Retained volume location changed"))?;
            db::private_dir(live.parent().ok_or(Error::Missing)?)?;
            let name = live
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or(Error::Missing)?;
            live.with_file_name(if matches!(location, Location::VolumeBefore) {
                before(name)
            } else {
                stage(name)
            })
        }
        _ => {
            let live = db::project_directory(&state.workspaces.root, b, false)?;
            live.join(match location {
                Location::DatabaseBefore => before("project.db"),
                Location::WalBefore => before("project.db-wal"),
                Location::ShmBefore => before("project.db-shm"),
                Location::DatabaseStage => stage("project.db"),
                _ => unreachable!(),
            })
        }
    })
}
/// Reject unknown archive entries before planning or resuming any further deletion.
fn archive_entries(directory: &FsPath) -> Result<Vec<String>, Error> {
    match fs::symlink_metadata(directory) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.into()),
        Ok(_) => db::private_dir(directory)?,
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(directory)? {
        let name = entry?
            .file_name()
            .into_string()
            .map_err(|_| Error::Conflict("Unexpected cleanup artifact"))?;
        if !ARCHIVE_FILES.contains(&name.as_str()) {
            return Err(Error::Conflict(
                "Unknown backup artifacts require inspection before cleanup",
            ));
        }
        names.push(name);
    }
    names.sort();
    Ok(names)
}
/// Open without following aliases and capture contents together with the file identity.
fn hash(path: &FsPath) -> Result<(String, u64, u64, u64), Error> {
    let file = db::private_file(path, false)?;
    let meta = file.metadata()?;
    Ok((
        checkpoint::digest(path)?,
        meta.len(),
        meta.dev(),
        meta.ino(),
    ))
}
/// Append only existing allowed files; preserved originals require their restore-journal hash.
fn collect(
    state: &AppState,
    b: &Binding,
    target: Uuid,
    location: Location,
    expected: Option<&str>,
    required: bool,
    files: &mut Vec<Proof>,
) -> Result<(), Error> {
    let path = path(state, b, target, &location)?;
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => return Ok(()),
        Err(e) => return Err(e.into()),
        Ok(_) => {}
    }
    let (sha256, bytes, device, inode) = hash(&path)?;
    if expected.is_some_and(|s| s != sha256) {
        return Err(Error::Conflict("Preserved restore original changed"));
    }
    files.push(Proof {
        target,
        location,
        path,
        sha256,
        bytes,
        device,
        inode,
    });
    Ok(())
}
/// Choose checkpoint bytes, aborted staging or completed-restore originals without selecting live files.
pub(super) fn plan(state: &AppState, b: &Binding, targets: &[Backup]) -> Result<Vec<Proof>, Error> {
    let mut files = vec![];
    for r in targets {
        let id = r.public.id;
        if r.public.request.restore_checkpoint.is_none() {
            let archive = root(&state.workspaces, b.project_id, false)?.join(id.to_string());
            for name in archive_entries(&archive)? {
                collect(
                    state,
                    b,
                    id,
                    Location::Archive(name),
                    None,
                    true,
                    &mut files,
                )?;
            }
            continue;
        }
        if r.public.phase == "complete" {
            let original = r
                .original
                .as_ref()
                .ok_or(Error::Conflict("Restore preservation journal is missing"))?;
            for (location, expected) in [
                (Location::DatabaseBefore, &original.database),
                (Location::WalBefore, &original.wal),
                (Location::ShmBefore, &original.shm),
                (Location::VolumeBefore, &original.volume),
            ] {
                if let Some(expected) = expected {
                    collect(state, b, id, location, Some(expected), true, &mut files)?;
                }
            }
        }
        for location in [Location::DatabaseStage, Location::VolumeStage] {
            collect(state, b, id, location, None, false, &mut files)?;
        }
    }
    Ok(files)
}
/// Validate every remaining entry before deleting any further byte after a restart.
pub(super) fn verify(state: &AppState, r: &Record) -> Result<(), Error> {
    for id in &r.public.request.target_ids {
        let backup = state.store.backup_record(*id)?.ok_or(Error::Missing)?;
        if backup.public.project_id != r.binding.project_id
            || backup.public.cleanup_id != Some(r.public.id)
            || backup.active
            || backup.public.protected
        {
            return Err(Error::Conflict(
                "Cleanup target ownership or protection changed",
            ));
        }
        if backup.public.request.restore_checkpoint.is_none() {
            let directory =
                root(&state.workspaces, r.binding.project_id, false)?.join(id.to_string());
            for name in archive_entries(&directory)? {
                if !r.files.iter().any(|p| {
                    p.target == *id && matches!(&p.location,Location::Archive(n) if n==&name)
                }) {
                    return Err(Error::Conflict(
                        "Unplanned backup artifact appeared during cleanup",
                    ));
                }
            }
        }
    }
    for file in &r.files {
        if !r.public.request.target_ids.contains(&file.target)
            || path(state, &r.binding, file.target, &file.location)? != file.path
        {
            return Err(Error::Conflict("Cleanup file location changed"));
        }
        match fs::symlink_metadata(&file.path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
            Ok(_) => {}
        }
        let proof = hash(&file.path)?;
        if proof != (file.sha256.clone(), file.bytes, file.device, file.inode) {
            return Err(Error::Conflict("Cleanup file identity or contents changed"));
        }
    }
    Ok(())
}
/// Unlink the next admitted file and sync its parent before advancing durable progress.
pub(super) fn remove_one(state: &AppState, r: &mut Record) -> Result<bool, Error> {
    let Some(file) = r.files.get(r.public.removed_files as usize) else {
        return Ok(false);
    };
    // The entire remaining plan was verified under the operation/owner locks first.
    match fs::remove_file(&file.path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    if let Some(parent) = file.path.parent().filter(|p| p.exists()) {
        File::open(parent)?.sync_all()?;
    }
    r.public.logical_bytes = r.public.logical_bytes.saturating_add(file.bytes);
    r.public.removed_files += 1;
    r.public.updated_at = ai_sandbox::now();
    state.store.save_cleanup_record(r)?;
    Ok(true)
}
/// Remove only empty selected archive directories; never recurse into unknown content.
pub(super) fn remove_empty_archives(state: &AppState, r: &Record) -> Result<(), Error> {
    let root = root(&state.workspaces, r.binding.project_id, false)?;
    for id in &r.public.request.target_ids {
        let target = state.store.backup_record(*id)?.ok_or(Error::Missing)?;
        if target.public.request.restore_checkpoint.is_some() {
            continue;
        }
        let directory = root.join(id.to_string());
        if !archive_entries(&directory)?.is_empty() {
            return Err(Error::Conflict("Backup archive is not empty"));
        }
        match fs::remove_dir(&directory) {
            Ok(()) => File::open(&root)?.sync_all()?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
