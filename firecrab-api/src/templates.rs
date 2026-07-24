//! Immutable, integrity-verified VM boot template registry: resolves a
//! stable alias (e.g. `ubuntu-26.04`) to a pinned kernel+rootfs version,
//! re-verifying each artifact's identity (device/inode/length) and content
//! hash on every open so a template swapped out from under a running
//! registry is detected instead of silently served.

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

/// `openat2` `RESOLVE_*` flag: reject crossing a mount point.
const RESOLVE_NO_XDEV: u64 = 0x01;
/// `openat2` `RESOLVE_*` flag: reject magic-link procfs-style resolution.
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
/// `openat2` `RESOLVE_*` flag: reject any symlink in the path.
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
/// `openat2` `RESOLVE_*` flag: keep resolution confined beneath the dirfd.
const RESOLVE_BENEATH: u64 = 0x08;

/// Failure modes for building or reading from a [`TemplateRegistry`].
#[derive(Debug, Error)]
pub enum TemplateError {
    /// An artifact path was absolute, empty, or escaped the image root.
    #[error("template artifact path must be a non-empty relative path")]
    InvalidPath,
    /// The resolved path exists but isn't a regular file.
    #[error("template artifact is not a regular file: {0}")]
    NotRegularFile(PathBuf),
    /// An artifact's identity/content no longer matches what was verified
    /// at registry construction time.
    #[error("template artifact changed after registry validation: {0}")]
    ArtifactChanged(PathBuf),
    /// Two [`TemplateSpec`]s declared the same `(alias, version)` pair.
    #[error("template registry contains a duplicate version: {0}/{1}")]
    DuplicateVersion(String, String),
    /// Two [`TemplateSpec`]s declared the same alias.
    #[error("template registry contains a duplicate alias: {0}")]
    DuplicateAlias(String),
    /// A filesystem operation failed while building the registry.
    #[error("template registry I/O failed: {0}")]
    Io(#[from] io::Error),
}

/// One template to register: an alias, its pinned version tag, and the
/// artifacts/boot args that version resolves to.
#[derive(Debug, Clone)]
pub struct TemplateSpec {
    /// Stable, user-facing name (e.g. `ubuntu-26.04`); what the API accepts.
    pub alias: String,
    /// Internal version tag this alias currently pins to.
    pub version: String,
    /// Path to the kernel image, relative to the image root.
    pub kernel: PathBuf,
    /// Path to the initrd image, relative to the image root — only needed
    /// when the kernel doesn't build virtio_blk/ext4 in (e.g. Alpine's
    /// `linux-virt`, which ships them as modules).
    pub initrd: Option<PathBuf>,
    /// Path to the rootfs image, relative to the image root.
    pub rootfs: PathBuf,
    /// Firecracker kernel command line for this template.
    pub boot_args: String,
}

/// An artifact whose identity and content have been hashed and pinned;
/// [`TemplateRegistry::open_verified`] re-checks both before every read.
#[derive(Debug, Clone)]
pub struct VerifiedArtifact {
    /// Path relative to the registry's image root.
    relative_path: PathBuf,
    /// Device number at verification time.
    device: u64,
    /// Inode number at verification time.
    inode: u64,
    /// File length at verification time.
    length: u64,
    /// Full-file SHA256 at verification time.
    sha256: String,
}

impl VerifiedArtifact {
    /// The artifact's pinned SHA256 hex digest.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// The artifact's pinned length in bytes.
    pub fn length(&self) -> u64 {
        self.length
    }
}

/// One resolved, immutable version of a template: a name/version pair with
/// its verified kernel and rootfs artifacts.
#[derive(Debug, Clone)]
pub struct TemplateVersion {
    /// The alias this version was registered under.
    pub name: String,
    /// This version's own tag (distinct from the alias it's reached through).
    pub version: String,
    /// Verified kernel image.
    pub kernel: VerifiedArtifact,
    /// Verified initrd image, if this template needs one.
    pub initrd: Option<VerifiedArtifact>,
    /// Verified rootfs image.
    pub rootfs: VerifiedArtifact,
    /// Firecracker kernel command line.
    pub boot_args: String,
}

impl TemplateVersion {
    /// SHA256 of `boot_args`, so callers can detect a boot-args change
    /// without re-hashing the (potentially multi-GB) rootfs.
    pub fn boot_args_sha256(&self) -> String {
        sha256_bytes(self.boot_args.as_bytes())
    }
}

/// Registry of verified template versions, resolved by alias or by exact
/// `(name, version)`.
#[derive(Debug, Clone)]
pub struct TemplateRegistry {
    /// Directory fd artifacts are opened beneath via `openat2`.
    image_root: Arc<File>,
    /// Canonical path of the image root, for building absolute paths.
    image_root_path: PathBuf,
    /// alias -> `(name, version)` it currently resolves to.
    aliases: HashMap<String, (String, String)>,
    /// `(name, version)` -> the resolved, verified template.
    versions: HashMap<(String, String), Arc<TemplateVersion>>,
    /// Caches `open_verified`'s full-file hash by (device, inode), so many
    /// VMs starting at once against the same untouched multi-GB template
    /// don't each independently re-read and re-hash it (`docs/task-vm-startup-progress.md`'s
    /// "stuck at disk prep with many VMs" bug). Invalidated by length or
    /// mtime moving, which any real content change updates.
    verify_cache: Arc<Mutex<HashMap<(u64, u64), CachedHash>>>,
}

/// One entry in [`TemplateRegistry::verify_cache`].
#[derive(Debug, Clone)]
struct CachedHash {
    /// File length when this hash was computed.
    length: u64,
    /// `(mtime seconds, mtime nanoseconds)` when this hash was computed.
    mtime: (i64, i64),
    /// The cached SHA256 hex digest.
    sha256: String,
}

impl TemplateRegistry {
    /// Loads the registry's built-in template set (`ubuntu-26.04`,
    /// `alpine-3.24`) from `images/` (or `FIRECRAB_IMAGE_ROOT` if set).
    pub fn load_default() -> Result<Self, TemplateError> {
        let default_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../images");
        let image_root = env::var_os("FIRECRAB_IMAGE_ROOT")
            .map(PathBuf::from)
            .unwrap_or(default_root);

        Self::from_specs(&image_root, default_specs())
    }

    /// Builds a registry from an explicit image root and template specs,
    /// verifying every artifact's identity and hash up front and rejecting
    /// duplicate aliases/versions.
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
            let initrd = spec
                .initrd
                .as_ref()
                .map(|path| verify_artifact(&image_root, path))
                .transpose()?;
            let version = Arc::new(TemplateVersion {
                name: spec.alias.clone(),
                version: spec.version.clone(),
                kernel: verify_artifact(&image_root, &spec.kernel)?,
                initrd,
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

    /// Absolute host path for a verified artifact (e.g. to hand to
    /// Firecracker, which opens boot files by path, not fd).
    pub fn artifact_path(&self, artifact: &VerifiedArtifact) -> PathBuf {
        self.image_root_path.join(&artifact.relative_path)
    }

    /// Resolves a user-facing alias (e.g. `ubuntu-26.04`) to its pinned
    /// version.
    pub fn resolve_alias(&self, alias: &str) -> Option<Arc<TemplateVersion>> {
        let (name, version) = self.aliases.get(alias)?;
        self.resolve_version(name, version)
    }

    /// Resolves an exact `(name, version)` pair, bypassing alias indirection.
    pub fn resolve_version(&self, name: &str, version: &str) -> Option<Arc<TemplateVersion>> {
        self.versions
            .get(&(name.to_owned(), version.to_owned()))
            .cloned()
    }

    /// Re-verifies `artifact`'s identity (device/inode/length) and content
    /// hash, then returns an open handle to it. Fails if either check no
    /// longer matches what was pinned at registry construction time.
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
            let cache = self
                .verify_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
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

/// The templates [`TemplateRegistry::load_default`] installs, kept as a
/// standalone function (rather than inline in `load_default`) so the exact
/// alias/kernel/initrd/boot_args values are unit-testable without needing
/// real image files on disk — `from_specs` itself always needs those to
/// verify against, but the spec values themselves don't.
fn default_specs() -> [TemplateSpec; 2] {
    [
        TemplateSpec {
            alias: "ubuntu-26.04".to_owned(),
            version: "ubuntu-26.04-v2".to_owned(),
            // Ubuntu's own linux-image-generic kernel (see
            // install-ubuntu-roofs.sh) rather than a self-built
            // vanilla one — virtio_blk/ext4 are builtin, no initrd
            // needed (task-distro-standard-kernels.md).
            kernel: PathBuf::from("kernel/vmlinux-ubuntu-26.04-x86_64"),
            initrd: None,
            rootfs: PathBuf::from("rootfs/ubuntu-rootfs-26.04-amd64.ext4"),
            boot_args: "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw".to_owned(),
        },
        TemplateSpec {
            alias: "alpine-3.24".to_owned(),
            version: "alpine-3.24.1-v3".to_owned(),
            // Alpine's own linux-virt kernel (see
            // install-alpine-rootfs.sh); unlike Ubuntu's, its
            // virtio_blk/ext4 are modules, so the initrd Alpine
            // itself builds for it is required to reach the root
            // device at all. `rootfstype=ext4` is also required —
            // without an explicit -t, mkinitfs's mount call can't
            // guess a filesystem type whose module (ext4, also not
            // builtin here) isn't loaded yet, and fails with a
            // misleading "No such file or directory" instead of
            // triggering the kernel's on-demand module load.
            kernel: PathBuf::from("kernel/vmlinux-alpine-virt-x86_64"),
            initrd: Some(PathBuf::from("kernel/initramfs-alpine-virt-x86_64")),
            rootfs: PathBuf::from("rootfs/alpine-rootfs-3.24.1-x86_64.ext4"),
            boot_args: "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rootfstype=ext4 rw"
                .to_owned(),
        },
    ]
}

/// Rejects absolute paths, empty paths, and any `.`/`..` component.
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

/// Opens the image root directory itself, as a dirfd for later `openat2`
/// calls beneath it.
fn open_image_root(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

/// Argument struct for the raw `openat2(2)` syscall.
#[repr(C)]
struct OpenHow {
    /// `open(2)`-style flags.
    flags: u64,
    /// Mode bits, unused here (no file creation).
    mode: u64,
    /// `RESOLVE_*` resolution-restriction flags.
    resolve: u64,
}

/// Opens `path` relative to `root` via `openat2` with `RESOLVE_BENEATH` (and
/// no-symlinks/no-magiclinks/no-xdev), so a malicious or mistaken template
/// path can't escape the image root even via a symlink.
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

/// Opens `path` beneath `root` and pins its identity/hash into a
/// [`VerifiedArtifact`].
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

/// Fetches metadata and rejects anything that isn't a regular file.
fn regular_file_metadata(file: &File, path: &Path) -> Result<Metadata, TemplateError> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(TemplateError::NotRegularFile(path.to_owned()));
    }
    Ok(metadata)
}

/// Streams the whole file through SHA256 in 64 KiB chunks.
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

/// SHA256 hex digest of an in-memory byte slice.
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
                    initrd: None,
                    rootfs: PathBuf::from("rootfs"),
                    boot_args: "console=ttyS0".to_owned(),
                },
                TemplateSpec {
                    alias: "ubuntu-26.04".to_owned(),
                    version: "v1".to_owned(),
                    kernel: PathBuf::from("kernel"),
                    initrd: None,
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
                initrd: None,
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
                initrd: None,
                rootfs: PathBuf::from("link"),
                boot_args: String::new(),
            }],
        );
        assert!(matches!(link_result, Err(TemplateError::Io(_))));
    }

    #[test]
    fn initrd_is_verified_like_kernel_and_rootfs_when_present() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("kernel"), b"kernel").unwrap();
        fs::write(directory.path().join("rootfs"), b"rootfs").unwrap();
        fs::write(directory.path().join("initrd"), b"initrd").unwrap();

        let registry = TemplateRegistry::from_specs(
            directory.path(),
            [TemplateSpec {
                alias: "alpine-virt".to_owned(),
                version: "v1".to_owned(),
                kernel: PathBuf::from("kernel"),
                initrd: Some(PathBuf::from("initrd")),
                rootfs: PathBuf::from("rootfs"),
                boot_args: "console=ttyS0".to_owned(),
            }],
        )
        .unwrap();
        let template = registry.resolve_alias("alpine-virt").unwrap();

        let initrd = template.initrd.as_ref().expect("initrd was declared");
        assert_eq!(initrd.sha256(), sha256_bytes(b"initrd"));

        fs::write(directory.path().join("initrd"), b"tampered").unwrap();
        assert!(matches!(
            registry.open_verified(initrd),
            Err(TemplateError::ArtifactChanged(_))
        ));
    }

    #[test]
    fn default_specs_match_each_distros_own_kernel_and_initrd_requirement() {
        let specs = default_specs();
        let ubuntu = specs
            .iter()
            .find(|spec| spec.alias == "ubuntu-26.04")
            .expect("ubuntu-26.04 is one of the default specs");
        assert_eq!(
            ubuntu.kernel,
            PathBuf::from("kernel/vmlinux-ubuntu-26.04-x86_64")
        );
        assert_eq!(ubuntu.initrd, None);

        let alpine = specs
            .iter()
            .find(|spec| spec.alias == "alpine-3.24")
            .expect("alpine-3.24 is one of the default specs");
        assert_eq!(
            alpine.kernel,
            PathBuf::from("kernel/vmlinux-alpine-virt-x86_64")
        );
        assert_eq!(
            alpine.initrd,
            Some(PathBuf::from("kernel/initramfs-alpine-virt-x86_64"))
        );
        // virtio_blk/ext4 being modules on this kernel is exactly why the
        // initrd above is required — and why the kernel needs an explicit
        // hint to find the root filesystem type before that module loads.
        assert!(alpine.boot_args.contains("rootfstype=ext4"));
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
