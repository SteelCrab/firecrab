//! Independent disk snapshots that share blocks when the host supports CoW.
mod retention;

pub(super) use retention::Repair;

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{managed_home, report};

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Method {
    CopyOnWrite,
    FullCopy,
}

#[derive(Debug, Serialize)]
pub(super) struct Snapshot {
    file: PathBuf,
    source: PathBuf,
    pub method: Method,
    logical_bytes: u64,
}

/// The clone API must succeed before a snapshot is reported as copy-on-write.
pub(super) fn create(source: &Path, destination: &Path) -> io::Result<Snapshot> {
    create_with(source, destination, |source, destination| {
        reflink_copy::reflink(source, destination)
    })
}

fn create_with(
    source: &Path,
    destination: &Path,
    clone: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<Snapshot> {
    for path in [source, destination] {
        managed_home::reject_symlink_components(path).map_err(io::Error::other)?;
    }
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "snapshot source must be a regular file",
        ));
    }
    match fs::symlink_metadata(destination) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "snapshot destination already exists",
            ));
        }
    }
    let method = match clone(source, destination) {
        Ok(()) => Method::CopyOnWrite,
        Err(error) if clone_unavailable(&error) => {
            report!(
                "[SNAPSHOT] CoW unavailable ({error}); using a full copy: {}",
                destination.display()
            );
            let mut output = File::create_new(destination)?;
            protect(destination)?;
            io::copy(&mut File::open(source)?, &mut output)?;
            Method::FullCopy
        }
        Err(error) => return Err(error),
    };
    protect(destination)?;
    let snapshot = OpenOptions::new()
        .read(true)
        .write(true)
        .open(destination)?;
    if snapshot.metadata()?.len() != metadata.len() {
        return Err(io::Error::other("snapshot size differs from the source"));
    }
    snapshot.sync_all()?;
    if method == Method::CopyOnWrite {
        report!(
            "[SNAPSHOT] CoW: {} (logical bytes={}; unchanged blocks shared)",
            destination.display(),
            metadata.len()
        );
    }
    Ok(Snapshot {
        file: destination.to_path_buf(),
        source: source.to_path_buf(),
        method,
        logical_bytes: metadata.len(),
    })
}

fn clone_unavailable(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::Unsupported | io::ErrorKind::CrossesDevices | io::ErrorKind::InvalidInput
    ) {
        return true;
    }
    cfg!(windows) && error.raw_os_error().is_some_and(windows_clone_unavailable)
}

fn windows_clone_unavailable(code: i32) -> bool {
    // The Windows bindings return HRESULT_FROM_WIN32 codes through io::Error.
    let code = code as u32;
    let code = if code & 0xffff_0000 == 0x8007_0000 {
        code & 0xffff
    } else {
        code
    };
    // Invalid function, cross-volume clone, unsupported operation, invalid parameter.
    matches!(code, 1 | 17 | 50 | 87)
}

fn protect(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub(super) fn record(directory: &Path, backup: &Snapshot, working: &Snapshot) -> io::Result<()> {
    let file = File::create_new(directory.join("snapshots.json"))?;
    serde_json::to_writer_pretty(
        &file,
        &serde_json::json!({
            "schema": 1,
            "backup": backup,
            "working_copy": working,
        }),
    )
    .map_err(io::Error::other)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

    #[test]
    fn snapshots_isolate_writes_and_survive_source_and_sibling_deletion() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.raw");
        let backup = root.path().join("before.raw");
        let working = root.path().join("working.raw");
        fs::write(&source, vec![7; 1024 * 1024]).unwrap();
        let first = create(&source, &backup).unwrap();
        let second = create(&backup, &working).unwrap();
        record(root.path(), &first, &second).unwrap();
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(root.path().join("snapshots.json")).unwrap()).unwrap();
        assert_eq!(
            manifest["backup"]["method"],
            serde_json::to_value(&first.method).unwrap()
        );
        assert_eq!(manifest["working_copy"]["logical_bytes"], 1024 * 1024);
        let mut file = OpenOptions::new().write(true).open(&working).unwrap();
        file.seek(SeekFrom::Start(4096)).unwrap();
        file.write_all(&[9; 4096]).unwrap();
        file.sync_all().unwrap();
        drop(file);
        assert_eq!(&fs::read(&backup).unwrap()[4096..8192], &[7; 4096]);
        assert_eq!(fs::read(&source).unwrap(), fs::read(&backup).unwrap());
        fs::write(&source, b"changed source").unwrap();
        assert_eq!(fs::metadata(&backup).unwrap().len(), 1024 * 1024);
        fs::remove_file(&source).unwrap();
        fs::remove_file(&backup).unwrap();
        assert_eq!(&fs::read(&working).unwrap()[4096..8192], &[9; 4096]);
        #[cfg(target_os = "macos")]
        assert_eq!(first.method, Method::CopyOnWrite);
    }

    #[test]
    fn unsupported_clone_falls_back_without_overwriting_existing_files() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("snapshot");
        fs::write(&source, b"preserved").unwrap();
        let snapshot = create_with(&source, &destination, |_, _| {
            Err(io::ErrorKind::Unsupported.into())
        })
        .unwrap();
        assert_eq!(snapshot.method, Method::FullCopy);
        assert_eq!(fs::read(&destination).unwrap(), b"preserved");
        assert_eq!(
            create(&source, &destination).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(destination).unwrap(), b"preserved");
    }

    #[test]
    fn clone_permission_or_io_failure_does_not_start_a_full_copy() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("snapshot");
        fs::write(&source, b"preserved").unwrap();
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::StorageFull,
            io::ErrorKind::Other,
        ] {
            assert_eq!(
                create_with(&source, &destination, |_, _| Err(kind.into()))
                    .unwrap_err()
                    .kind(),
                kind
            );
            assert!(!destination.exists());
        }
    }

    #[test]
    fn windows_unsupported_results_allow_fallback_but_permission_and_disk_full_do_not() {
        for code in [1, 17, 50, 87] {
            assert!(windows_clone_unavailable(code));
            assert!(windows_clone_unavailable(
                (0x8007_0000 | code as u32) as i32
            ));
        }
        for code in [5, 32, 112] {
            assert!(!windows_clone_unavailable(code));
            assert!(!windows_clone_unavailable(
                (0x8007_0000 | code as u32) as i32
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_snapshot_sources_and_destinations_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let link = root.path().join("link");
        let destination = root.path().join("snapshot");
        fs::write(&source, b"preserved").unwrap();
        std::os::unix::fs::symlink(&source, &link).unwrap();
        assert!(create(&link, &destination).is_err());
        assert!(create(&source, &link).is_err());
        assert_eq!(fs::read(source).unwrap(), b"preserved");
    }
}
