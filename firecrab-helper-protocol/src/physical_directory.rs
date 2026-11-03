//! Directory identity includes birth time; mutable ctime is never a substitute.

use std::{fs::File, io, os::unix::fs::MetadataExt, path::Path};

/// A physical directory object, independent of its contents or display name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    /// Containing filesystem device.
    pub device: u64,
    /// Filesystem inode.
    pub inode: u64,
    /// Creation time, in seconds since the Unix epoch.
    pub created_seconds: u64,
    /// Creation time's fractional nanoseconds.
    pub created_nanoseconds: u32,
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "stable directory identity is unavailable",
    )
}

/// Read all identity fields from one open directory, including on Linux musl.
/// Filesystems without a valid birth time fail instead of weakening the proof.
pub fn observe(path: &Path) -> io::Result<Identity> {
    #[cfg(target_os = "linux")]
    let file: File = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::empty(),
    )?
    .into();
    #[cfg(not(target_os = "linux"))]
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() || metadata.ino() == 0 {
        return Err(unsupported());
    }
    #[cfg(target_os = "linux")]
    let (seconds, nanos) = {
        // Rust's musl Metadata::created does not request STATX_BTIME. Query the
        // same open descriptor directly through rustix's Linux syscall backend.
        let stat = rustix::fs::statx(
            &file,
            "",
            rustix::fs::AtFlags::EMPTY_PATH,
            rustix::fs::StatxFlags::BTIME | rustix::fs::StatxFlags::INO,
        )?;
        if stat.stx_mask & rustix::fs::StatxFlags::BTIME.bits() == 0
            || stat.stx_ino != metadata.ino()
        {
            return Err(unsupported());
        }
        (
            u64::try_from(stat.stx_btime.tv_sec).map_err(|_| unsupported())?,
            stat.stx_btime.tv_nsec,
        )
    };
    #[cfg(not(target_os = "linux"))]
    let (seconds, nanos) = {
        let created = metadata
            .created()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| unsupported())?;
        (created.as_secs(), created.subsec_nanos())
    };
    if (seconds == 0 && nanos == 0) || nanos >= 1_000_000_000 {
        return Err(unsupported());
    }
    Ok(Identity {
        device: metadata.dev(),
        inode: metadata.ino(),
        created_seconds: seconds,
        created_nanoseconds: nanos,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_directory_identity_survives_content_changes_but_not_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("installation");
        std::fs::create_dir(&path).unwrap();
        let original = observe(&path).unwrap();
        std::fs::write(path.join("mutable-settings"), b"changed").unwrap();
        assert_eq!(observe(&path).unwrap(), original);
        let moved = root.path().join("original");
        std::fs::rename(&path, &moved).unwrap();
        assert_eq!(observe(&moved).unwrap(), original);
        std::fs::create_dir(&path).unwrap();
        assert_ne!(observe(&path).unwrap(), original);
        assert!(observe(&moved.join("mutable-settings")).is_err());
    }
}
