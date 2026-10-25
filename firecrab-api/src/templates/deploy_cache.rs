//! Delete only journaled Deploy rootfs files after excluding template users.

use super::*;
use std::ffi::CString;

pub(crate) fn owned_rootfs(path: &Path) -> bool {
    let Some(name) = path
        .strip_prefix("rootfs/deploy")
        .ok()
        .and_then(|path| path.to_str())
    else {
        return false;
    };
    !name.contains('/')
        && name
            .strip_suffix(".ext4")
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
}

impl TemplateRegistry {
    /// The callback reads durable VM, pool and release references under these
    /// locks. A creator that resolved earlier retains an Arc until persistence.
    pub(crate) fn prune_deploy_image(
        &self,
        spec: &TemplateSpec,
        unreferenced: impl FnOnce() -> io::Result<bool>,
    ) -> io::Result<bool> {
        if !owned_rootfs(&spec.rootfs) {
            return Err(io::Error::other("invalid Deploy cache rootfs"));
        }
        let _mutation = self.mutations.lock().unwrap_or_else(|p| p.into_inner());
        let mut aliases = self.aliases.lock().unwrap_or_else(|p| p.into_inner());
        let mut versions = self.versions.lock().unwrap_or_else(|p| p.into_inner());
        let key = (spec.alias.clone(), spec.version.clone());
        let owned = versions.get(&key).is_some_and(|version| {
            version.version == spec.version && version.rootfs.relative_path() == spec.rootfs
        });
        let partial = spec.rootfs.with_extension("ext4.partial");
        for (version_key, version) in versions.iter() {
            let artifacts = std::iter::once(version.kernel.relative_path())
                .chain(version.initrd.as_ref().map(|initrd| initrd.relative_path()));
            if artifacts.clone().any(|path| path == spec.rootfs)
                || artifacts
                    .chain(std::iter::once(version.rootfs.relative_path()))
                    .any(|path| path == partial)
            {
                return Ok(false);
            }
            if version.rootfs.relative_path() != spec.rootfs {
                continue;
            }
            if !owned || *version_key != key || Arc::strong_count(version) != 1 {
                return Ok(false);
            }
        }
        if !unreferenced()? {
            return Ok(false);
        }
        if owned {
            // Unlike an explicit best-effort image delete, GC must not forget
            // other registrations or proceed after an unreadable manifest.
            let _manifest = self.manifest_lock.lock().unwrap_or_else(|p| p.into_inner());
            let mut entries: Vec<TemplateSpec> = match fs::read(self.registrations_path()) {
                Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => vec![],
                Err(error) => return Err(error),
            };
            entries.retain(|entry| {
                entry.alias != spec.alias
                    || entry.version != spec.version
                    || entry.rootfs != spec.rootfs
            });
            self.write_registrations(&entries)
                .map_err(io::Error::other)?;
            if aliases.get(&spec.alias) == Some(&key) {
                aliases.remove(&spec.alias);
            }
            versions.remove(&key);
        }
        // Open each directory without following symlinks, then unlink one UUID
        // filename through that directory descriptor. Keep kernels and shared OCI blobs.
        let rootfs = match open_beneath(&self.image_root, Path::new("rootfs")) {
            Ok(root) => root,
            Err(TemplateError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(true);
            }
            Err(error) => return Err(io::Error::other(error)),
        };
        let directory = match open_beneath(&rootfs, Path::new("deploy")) {
            Ok(root) => root,
            Err(TemplateError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(true);
            }
            Err(error) => return Err(io::Error::other(error)),
        };
        let file = spec
            .rootfs
            .file_name()
            .expect("validated UUID path")
            .to_string_lossy();
        for file in [format!("{file}.partial"), file.into_owned()] {
            let name = CString::new(file).map_err(io::Error::other)?;
            // SAFETY: directory is an owned descriptor and name is one validated filename.
            if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::NotFound {
                    return Err(error);
                }
            }
        }
        Ok(true)
    }
}
