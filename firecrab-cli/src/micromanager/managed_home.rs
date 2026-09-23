//! The managed home holds the guest's disks and keys, and `--purge` deletes it,
//! so every host refuses the same unsafe targets before touching it.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("refusing to use managed path containing a symlink: {0}")]
    UnsafeManagedPath(PathBuf),
    #[error("refusing to purge unsafe managed path {0}")]
    UnsafePurge(PathBuf),
    #[error("could not {action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Deletes the managed home after re-checking it, and treats "already gone" as done.
pub fn purge(path: &Path) -> Result<(), Error> {
    validate_purge(path)?;
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error("purge managed data", path, source)),
    }
}

/// Only an absolute, symlink-free directory named `micromanager` may be purged.
pub fn validate_purge(path: &Path) -> Result<(), Error> {
    validate_purge_path(path)?;
    reject_symlink_components(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(Error::UnsafePurge(path.to_owned()))
        }
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(Error::UnsafePurge(path.to_owned())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error("inspect managed data", path, source)),
    }
}

/// A symlinked ancestor would let a purge or a write land somewhere else.
/// The first component is exempt because system roots are often links.
pub fn reject_symlink_components(path: &Path) -> Result<(), Error> {
    let mut current = PathBuf::new();
    let mut normal_component_count = 0;
    for component in path.components() {
        current.push(component.as_os_str());
        if matches!(component, Component::Normal(_)) {
            normal_component_count += 1;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() && normal_component_count > 1 => {
                return Err(Error::UnsafeManagedPath(current));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(source) => {
                return Err(io_error("inspect managed path", &current, source));
            }
        }
    }
    Ok(())
}

fn validate_purge_path(path: &Path) -> Result<(), Error> {
    let safe = path.is_absolute()
        && path.file_name().is_some_and(|name| name == "micromanager")
        && path.components().count() >= 4
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir));
    if safe {
        Ok(())
    } else {
        Err(Error::UnsafePurge(path.to_owned()))
    }
}

fn io_error(action: &'static str, path: &Path, source: io::Error) -> Error {
    Error::Io {
        action,
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_managed_home_is_purged_and_a_missing_one_is_fine() {
        let directory = tempfile::tempdir().expect("temp dir");
        let managed_home = directory.path().join("state/micromanager");
        fs::create_dir_all(managed_home.join("data")).expect("managed home");
        fs::write(managed_home.join("data/delete-me"), b"data").expect("data");

        purge(&managed_home).expect("purge succeeds");
        assert!(!managed_home.exists());
        purge(&managed_home).expect("purging twice is a no-op");
    }

    #[test]
    fn unsafe_roots_are_refused() {
        for path in [
            "/tmp",
            "relative/micromanager",
            "/a/b/../micromanager",
            "/micromanager",
        ] {
            assert!(
                matches!(validate_purge(Path::new(path)), Err(Error::UnsafePurge(_))),
                "{path} must be refused"
            );
        }
    }

    #[test]
    fn a_file_named_like_the_managed_home_is_refused() {
        let directory = tempfile::tempdir().expect("temp dir");
        let managed_home = directory.path().join("state/micromanager");
        fs::create_dir_all(managed_home.parent().expect("parent")).expect("parent");
        fs::write(&managed_home, b"not a directory").expect("file");
        assert!(matches!(
            validate_purge(&managed_home),
            Err(Error::UnsafePurge(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_ancestor_is_refused() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temp dir");
        let real_parent = directory.path().join("real");
        let real_managed_home = real_parent.join("micromanager");
        fs::create_dir_all(&real_managed_home).expect("managed home");
        let linked_parent = directory.path().join("linked");
        symlink(&real_parent, &linked_parent).expect("symlink");

        assert!(matches!(
            purge(&linked_parent.join("micromanager")),
            Err(Error::UnsafeManagedPath(_))
        ));
        assert!(real_managed_home.is_dir());
    }
}
