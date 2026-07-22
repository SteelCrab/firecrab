use std::collections::HashMap;
use std::env;
use std::ffi::CString;
use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use thiserror::Error;

const RESOLVE_NO_XDEV: u64 = 0x01;
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
const RESOLVE_BENEATH: u64 = 0x08;

#[derive(Debug, Error)]
pub enum TemplateError {
    #[error("template artifact path must be a non-empty relative path")]
    InvalidPath,
    #[error("template artifact is not a regular file: {0}")]
    NotRegularFile(PathBuf),
    #[error("template artifact changed after registry validation: {0}")]
    ArtifactChanged(PathBuf),
    #[error("template registry contains a duplicate version: {0}/{1}")]
    DuplicateVersion(String, String),
    #[error("template registry contains a duplicate alias: {0}")]
    DuplicateAlias(String),
    #[error("template registry I/O failed: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone)]
pub struct TemplateSpec {
    pub alias: String,
    pub version: String,
    pub kernel: PathBuf,
    pub rootfs: PathBuf,
    pub boot_args: String,
}

#[derive(Debug, Clone)]
pub struct VerifiedArtifact {
    relative_path: PathBuf,
    device: u64,
    inode: u64,
    length: u64,
    sha256: String,
}

impl VerifiedArtifact {
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub fn length(&self) -> u64 {
        self.length
    }
}

#[derive(Debug, Clone)]
pub struct TemplateVersion {
    pub name: String,
    pub version: String,
    pub kernel: VerifiedArtifact,
    pub rootfs: VerifiedArtifact,
    pub boot_args: String,
}

impl TemplateVersion {
    pub fn boot_args_sha256(&self) -> String {
        sha256_bytes(self.boot_args.as_bytes())
    }
}

#[derive(Debug, Clone)]
pub struct TemplateRegistry {
    image_root: Arc<File>,
    image_root_path: PathBuf,
    aliases: HashMap<String, (String, String)>,
    versions: HashMap<(String, String), Arc<TemplateVersion>>,
    /// Caches `open_verified`'s full-file hash by (device, inode), so many
    /// VMs starting at once against the same untouched multi-GB template
    /// don't each independently re-read and re-hash it (`docs/task-vm-startup-progress.md`'s
    /// "stuck at disk prep with many VMs" bug). Invalidated by length or
    /// mtime moving, which any real content change updates.
    verify_cache: Arc<Mutex<HashMap<(u64, u64), CachedHash>>>,
}

#[derive(Debug, Clone)]
struct CachedHash {
    length: u64,
    mtime: (i64, i64),
    sha256: String,
}

impl TemplateRegistry {
    pub fn load_default() -> Result<Self, TemplateError> {
        let default_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../images");
        let image_root = env::var_os("FIRECRAB_IMAGE_ROOT")
            .map(PathBuf::from)
            .unwrap_or(default_root);

        Self::from_specs(
            &image_root,
            [
                TemplateSpec {
                    alias: "ubuntu-26.04".to_owned(),
                    version: "ubuntu-26.04-v1".to_owned(),
                    kernel: PathBuf::from("kernel/vmlinux-7.1.2-x86_64"),
                    rootfs: PathBuf::from("rootfs/ubuntu-rootfs-26.04-amd64.ext4"),
                    boot_args: "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw"
                        .to_owned(),
                },
                TemplateSpec {
                    alias: "alpine-3.24".to_owned(),
                    version: "alpine-3.24.1-v1".to_owned(),
                    // Same generic kernel as ubuntu-26.04: it's distro-agnostic
                    // (virtio/ext4/serial support, no guest-specific config).
                    kernel: PathBuf::from("kernel/vmlinux-7.1.2-x86_64"),
                    rootfs: PathBuf::from("rootfs/alpine-rootfs-3.24.1-x86_64.ext4"),
                    boot_args: "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw"
                        .to_owned(),
                },
            ],
        )
    }

    pub fn from_specs(
        image_root: &Path,
        specs: impl IntoIterator<Item = TemplateSpec>,
    ) -> Result<Self, TemplateError> {
        // Firecracker opens artifacts by path, so keep a canonical path next
        // to the descriptor used for verified reads.
        let image_root_path = image_root.canonicalize()?;
        let image_root = Arc::new(open_image_root(image_root)?);
        let mut aliases = HashMap::new();
        let mut versions = HashMap::new();

        for spec in specs {
            let version = Arc::new(TemplateVersion {
                name: spec.alias.clone(),
                version: spec.version.clone(),
                kernel: verify_artifact(&image_root, &spec.kernel)?,
                rootfs: verify_artifact(&image_root, &spec.rootfs)?,
                boot_args: spec.boot_args,
            });
            let version_key = (spec.alias.clone(), spec.version);
            if versions
                .insert(version_key.clone(), version.clone())
                .is_some()
            {
                return Err(TemplateError::DuplicateVersion(
                    version_key.0,
                    version_key.1,
                ));
            }
            if aliases.insert(spec.alias.clone(), version_key).is_some() {
                return Err(TemplateError::DuplicateAlias(spec.alias));
            }
        }

        Ok(Self {
            image_root,
            image_root_path,
            aliases,
            versions,
            verify_cache: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn artifact_path(&self, artifact: &VerifiedArtifact) -> PathBuf {
        self.image_root_path.join(&artifact.relative_path)
    }

    pub fn resolve_alias(&self, alias: &str) -> Option<Arc<TemplateVersion>> {
        let (name, version) = self.aliases.get(alias)?;
        self.resolve_version(name, version)
    }

    pub fn resolve_version(&self, name: &str, version: &str) -> Option<Arc<TemplateVersion>> {
        self.versions
            .get(&(name.to_owned(), version.to_owned()))
            .cloned()
    }

    pub fn open_verified(&self, artifact: &VerifiedArtifact) -> Result<File, TemplateError> {
        let file = open_beneath(&self.image_root, &artifact.relative_path)?;
        let metadata = regular_file_metadata(&file, &artifact.relative_path)?;
        if metadata.dev() != artifact.device
            || metadata.ino() != artifact.inode
            || metadata.len() != artifact.length
        {
            return Err(TemplateError::ArtifactChanged(
                artifact.relative_path.clone(),
            ));
        }
        if self.hash_cached(&file, &metadata)? != artifact.sha256 {
            return Err(TemplateError::ArtifactChanged(
                artifact.relative_path.clone(),
            ));
        }
        Ok(file)
    }

    /// Full-file SHA256, reusing a cached value for this exact (device,
    /// inode, length, mtime) instead of re-reading the whole file — that
    /// combination only repeats for content that's genuinely unchanged
    /// since it was last hashed (any real edit bumps mtime).
    fn hash_cached(&self, file: &File, metadata: &Metadata) -> io::Result<String> {
        let key = (metadata.dev(), metadata.ino());
        let mtime = (metadata.mtime(), metadata.mtime_nsec());
        {
            let cache = self.verify_cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(cached) = cache.get(&key)
                && cached.length == metadata.len()
                && cached.mtime == mtime
            {
                return Ok(cached.sha256.clone());
            }
        }
        let sha256 = sha256_file(file)?;
        self.verify_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                key,
                CachedHash {
                    length: metadata.len(),
                    mtime,
                    sha256: sha256.clone(),
                },
            );
        Ok(sha256)
    }
}

fn validate_relative_path(path: &Path) -> Result<(), TemplateError> {
    let mut has_component = false;
    for component in path.components() {
        match component {
            Component::Normal(_) => has_component = true,
            _ => return Err(TemplateError::InvalidPath),
        }
    }
    if !has_component {
        return Err(TemplateError::InvalidPath);
    }
    Ok(())
}

fn open_image_root(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

fn open_beneath(root: &File, path: &Path) -> Result<File, TemplateError> {
    validate_relative_path(path)?;
    let bytes = path.as_os_str().as_encoded_bytes();
    let c_path = CString::new(bytes).map_err(|_| TemplateError::InvalidPath)?;
    let how = OpenHow {
        flags: (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64,
        mode: 0,
        resolve: RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_XDEV,
    };

    // SAFETY: pointers reference initialized values for the duration of the syscall.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            root.as_raw_fd(),
            c_path.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(TemplateError::Io(io::Error::last_os_error()));
    }

    // SAFETY: openat2 returned a new owned descriptor on success.
    Ok(unsafe { File::from_raw_fd(fd as i32) })
}

fn verify_artifact(root: &File, path: &Path) -> Result<VerifiedArtifact, TemplateError> {
    let file = open_beneath(root, path)?;
    let metadata = regular_file_metadata(&file, path)?;
    Ok(VerifiedArtifact {
        relative_path: path.to_owned(),
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        sha256: sha256_file(&file)?,
    })
}

fn regular_file_metadata(file: &File, path: &Path) -> Result<Metadata, TemplateError> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(TemplateError::NotRegularFile(path.to_owned()));
    }
    Ok(metadata)
}

fn sha256_file(file: &File) -> io::Result<String> {
    let mut file = file.try_clone()?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::time::{Duration, SystemTime};

    use tempfile::tempdir;

    use super::*;

    fn create_registry(root: &Path) -> TemplateRegistry {
        fs::write(root.join("kernel"), b"kernel").unwrap();
        fs::write(root.join("rootfs"), b"rootfs").unwrap();
        TemplateRegistry::from_specs(
            root,
            [
                TemplateSpec {
                    alias: "ubuntu-rootfs-26.04".to_owned(),
                    version: "v1".to_owned(),
                    kernel: PathBuf::from("kernel"),
                    rootfs: PathBuf::from("rootfs"),
                    boot_args: "console=ttyS0".to_owned(),
                },
                TemplateSpec {
                    alias: "ubuntu-26.04".to_owned(),
                    version: "v1".to_owned(),
                    kernel: PathBuf::from("kernel"),
                    rootfs: PathBuf::from("rootfs"),
                    boot_args: "console=ttyS0".to_owned(),
                },
            ],
        )
        .unwrap()
    }

    #[test]
    fn resolves_an_immutable_version() {
        let directory = tempdir().unwrap();
        let registry = create_registry(directory.path());
        let template = registry.resolve_alias("ubuntu-rootfs-26.04").unwrap();

        assert_eq!(template.version, "v1");
        assert_eq!(template.rootfs.sha256(), sha256_bytes(b"rootfs"));
        assert!(registry.resolve_version(&template.name, "v1").is_some());
    }

    #[test]
    fn resolves_the_legacy_ubuntu_release_alias() {
        let directory = tempdir().unwrap();
        let registry = create_registry(directory.path());
        let template = registry.resolve_alias("ubuntu-26.04").unwrap();

        assert_eq!(template.version, "v1");
    }

    #[test]
    fn rejects_parent_and_symlink_paths() {
        let directory = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("artifact"), b"outside").unwrap();
        symlink(
            outside.path().join("artifact"),
            directory.path().join("link"),
        )
        .unwrap();

        let parent_result = TemplateRegistry::from_specs(
            directory.path(),
            [TemplateSpec {
                alias: "bad".to_owned(),
                version: "v1".to_owned(),
                kernel: PathBuf::from("../artifact"),
                rootfs: PathBuf::from("../artifact"),
                boot_args: String::new(),
            }],
        );
        assert!(matches!(parent_result, Err(TemplateError::InvalidPath)));

        let link_result = TemplateRegistry::from_specs(
            directory.path(),
            [TemplateSpec {
                alias: "bad".to_owned(),
                version: "v1".to_owned(),
                kernel: PathBuf::from("link"),
                rootfs: PathBuf::from("link"),
                boot_args: String::new(),
            }],
        );
        assert!(matches!(link_result, Err(TemplateError::Io(_))));
    }

    #[test]
    fn detects_artifact_replacement() {
        let directory = tempdir().unwrap();
        let registry = create_registry(directory.path());
        let template = registry.resolve_alias("ubuntu-rootfs-26.04").unwrap();
        fs::write(directory.path().join("rootfs"), b"changed").unwrap();

        assert!(matches!(
            registry.open_verified(&template.rootfs),
            Err(TemplateError::ArtifactChanged(_))
        ));
    }

    /// Many VMs starting at once each call `open_verified` for the same
    /// template; this proves that doesn't fail the 2nd+ time (the naive
    /// bug would be a stale/poisoned cache making repeats worse, not just
    /// slower — see `docs/task-vm-startup-progress.md`).
    #[test]
    fn open_verified_succeeds_repeatedly_for_an_unchanged_artifact() {
        let directory = tempdir().unwrap();
        let registry = create_registry(directory.path());
        let template = registry.resolve_alias("ubuntu-rootfs-26.04").unwrap();

        registry.open_verified(&template.rootfs).unwrap();
        registry.open_verified(&template.rootfs).unwrap();
        registry.open_verified(&template.rootfs).unwrap();
    }

    /// The hash cache keys on (device, inode, length, mtime), not just
    /// (device, inode, length) — a same-length in-place content edit (which
    /// bumps mtime) must still be caught, not served a stale cached hash.
    #[test]
    fn cache_is_invalidated_by_a_same_length_content_change() {
        let directory = tempdir().unwrap();
        let registry = create_registry(directory.path());
        let template = registry.resolve_alias("ubuntu-rootfs-26.04").unwrap();
        registry.open_verified(&template.rootfs).unwrap();

        let rootfs_path = directory.path().join("rootfs");
        fs::write(&rootfs_path, b"ROOTFS").unwrap(); // same length as "rootfs", different bytes
        let file = File::open(&rootfs_path).unwrap();
        file.set_modified(SystemTime::now() + Duration::from_secs(5))
            .unwrap();

        assert!(matches!(
            registry.open_verified(&template.rootfs),
            Err(TemplateError::ArtifactChanged(_))
        ));
    }
}
