//! Immutable, integrity-verified VM boot template registry: resolves a
//! stable alias (e.g. `ubuntu-26.04`) to a pinned kernel+rootfs version,
//! re-verifying each artifact's identity (device/inode/length) and content
//! hash on every open so a template swapped out from under a running
//! registry is detected instead of silently served.

use std::collections::HashMap;
use std::env;
use std::ffi::CString;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Manifest of every template registered at runtime (image install or web
/// build), relative to the image root. Rebuilt into the registry on the next
/// startup so a template that was never part of [`default_specs`] — a web
/// build's derived alias, or an in-place rebuild's new version — does not
/// silently disappear on restart, taking every VM created from it with it.
const REGISTRATIONS_FILE: &str = ".templates.json";

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
///
/// Serializable because runtime registrations are persisted verbatim into
/// [`REGISTRATIONS_FILE`] — a spec is exactly the input `register_spec`
/// needs, so replaying it at startup reproduces the same registry state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

    /// Path relative to the registry image root (used when removing files).
    pub fn relative_path(&self) -> &Path {
        &self.relative_path
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

    /// Whether this image must be started with Firecracker's PCI transport
    /// enabled. On x86_64, Rocky Linux 9.8 needs PCI because its stock kernel
    /// disables `CONFIG_VIRTIO_MMIO`; Ubuntu 26.04 also uses PCI because its
    /// stock kernel rejects Firecracker 1.16's command-line MMIO regions as
    /// busy. Both distributions use virtio-mmio on aarch64.
    pub fn requires_pci_transport(&self) -> bool {
        requires_pci_transport_for_arch(&self.name, std::env::consts::ARCH)
    }

    /// Returns the kernel command line adjusted for the transport selected
    /// by [`Self::requires_pci_transport`]. This intentionally happens at
    /// start time so VMs pinned to registrations made before the transport
    /// fix also recover on their next start.
    pub fn runtime_boot_args(&self) -> String {
        let enable_pci = self.requires_pci_transport();
        let mut args: Vec<&str> = self
            .boot_args
            .split_whitespace()
            .filter(|arg| !(enable_pci && *arg == "pci=off"))
            .collect();

        // The EL9 initramfs contains virtio_net, but does not load it merely
        // because the PCI device exists. Force it before switching to the
        // rootfs so NetworkManager sees the interface immediately.
        if self.name == "rocky-9.8" && !args.contains(&"rd.driver.pre=virtio_net") {
            args.push("rd.driver.pre=virtio_net");
        }

        // Both distro images configure their network manager for `eth0`.
        // PCI's predictable naming would otherwise expose the NIC as an
        // ens*/enp* device and leave that configuration unmatched.
        if enable_pci && !args.contains(&"net.ifnames=0") {
            args.push("net.ifnames=0");
        }

        args.join(" ")
    }
}

fn requires_pci_transport_for_arch(name: &str, architecture: &str) -> bool {
    matches!(name, "rocky-9.8" | "ubuntu-26.04") && architecture != "aarch64"
}

/// Registry of verified template versions, resolved by alias or by exact
/// `(name, version)`. Alias maps are behind locks so a successful image
/// install can register a newly downloaded template without restarting.
#[derive(Debug)]
pub struct TemplateRegistry {
    /// Directory fd artifacts are opened beneath via `openat2`.
    image_root: Arc<File>,
    /// Canonical path of the image root, for building absolute paths.
    image_root_path: PathBuf,
    /// alias -> `(name, version)` it currently resolves to.
    aliases: Arc<Mutex<HashMap<String, (String, String)>>>,
    /// `(name, version)` -> the resolved, verified template.
    versions: Arc<Mutex<HashMap<(String, String), Arc<TemplateVersion>>>>,
    /// Caches `open_verified`'s full-file hash by (device, inode), so many
    /// VMs starting at once against the same untouched multi-GB template
    /// don't each independently re-read and re-hash it (`public-docs/api.md`'s
    /// "stuck at disk prep with many VMs" bug). Invalidated by length or
    /// mtime moving, which any real content change updates.
    verify_cache: Arc<Mutex<HashMap<(u64, u64), CachedHash>>>,
    /// Serializes the read-modify-write of [`REGISTRATIONS_FILE`], which two
    /// concurrent registrations would otherwise interleave into a manifest
    /// missing one of them.
    manifest_lock: Arc<Mutex<()>>,
}

impl Clone for TemplateRegistry {
    fn clone(&self) -> Self {
        Self {
            image_root: self.image_root.clone(),
            image_root_path: self.image_root_path.clone(),
            aliases: self.aliases.clone(),
            versions: self.versions.clone(),
            verify_cache: self.verify_cache.clone(),
            manifest_lock: self.manifest_lock.clone(),
        }
    }
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
    /// Loads the host architecture's built-in template set from `images/`
    /// (or `FIRECRAB_IMAGE_ROOT` if set).
    pub fn load_default() -> Result<Self, TemplateError> {
        let default_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../images");
        let image_root = env::var_os("FIRECRAB_IMAGE_ROOT")
            .map(PathBuf::from)
            .unwrap_or(default_root);

        Self::load_from(&image_root)
    }

    /// [`load_default`](Self::load_default) against an explicit image root.
    ///
    /// A template whose files aren't there yet is skipped with a warning
    /// rather than failing startup: on a fresh install the daemons come up
    /// before any multi-gigabyte image has been built, and an API that
    /// refuses to start would take the dashboard and every existing VM down
    /// with it. Anything that *is* present is still verified in full by
    /// [`from_specs`](Self::from_specs).
    pub fn load_from(image_root: &Path) -> Result<Self, TemplateError> {
        if !image_root.exists() {
            fs::create_dir_all(image_root)?;
        }

        let (present, missing): (Vec<_>, Vec<_>) = default_specs()
            .into_iter()
            .partition(|spec| artifacts_present(image_root, spec));
        for spec in &missing {
            tracing::warn!(
                template = spec.alias,
                image_root = %image_root.display(),
                "template artifacts are missing; skipping this template"
            );
        }

        let registry = Self::from_specs(image_root, present)?;
        registry.restore_registrations();
        Ok(registry)
    }

    /// Replays [`REGISTRATIONS_FILE`] over the built-in set, so templates
    /// registered while the previous process was running (an installed
    /// image, a web build's derived alias, an in-place rebuild) resolve
    /// again. Replaying through the normal `register_spec` path means a
    /// restored alias is verified exactly the way a live registration is,
    /// and an entry that supersedes a built-in alias re-pins it while the
    /// built-in version stays addressable for VMs still pinned to it.
    ///
    /// Best-effort per entry: a manifest naming files that have since been
    /// deleted must not take the whole API down at startup, for the same
    /// reason [`load_from`](Self::load_from) skips unbuilt built-ins.
    fn restore_registrations(&self) {
        for spec in self.read_registrations() {
            if !artifacts_present(&self.image_root_path, &spec) {
                tracing::warn!(
                    template = spec.alias,
                    "registered template's artifacts are missing; skipping this template"
                );
                continue;
            }
            let alias = spec.alias.clone();
            if let Err(error) = self.register_spec_inner(spec, false) {
                tracing::warn!(
                    template = alias,
                    error = %error,
                    "registered template could not be restored; skipping this template"
                );
            }
        }
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
            aliases: Arc::new(Mutex::new(aliases)),
            versions: Arc::new(Mutex::new(versions)),
            verify_cache: Arc::new(Mutex::new(HashMap::new())),
            manifest_lock: Arc::new(Mutex::new(())),
        })
    }

    /// Absolute host path for a verified artifact (e.g. to hand to
    /// Firecracker, which opens boot files by path, not fd).
    pub fn artifact_path(&self, artifact: &VerifiedArtifact) -> PathBuf {
        self.image_root_path.join(&artifact.relative_path)
    }

    /// Canonical image root directory (where install places downloaded artifacts).
    pub fn image_root_path(&self) -> &Path {
        &self.image_root_path
    }

    /// Built-in template specs the project knows about (installed or not).
    pub fn known_specs() -> Vec<TemplateSpec> {
        default_specs()
    }

    /// Look up a built-in spec by alias.
    pub fn known_spec(alias: &str) -> Option<TemplateSpec> {
        default_specs().into_iter().find(|spec| spec.alias == alias)
    }

    /// Resolves a user-facing alias (e.g. `ubuntu-26.04`) to its pinned
    /// version.
    pub fn resolve_alias(&self, alias: &str) -> Option<Arc<TemplateVersion>> {
        let aliases = self
            .aliases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (name, version) = aliases.get(alias)?;
        let versions = self
            .versions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        versions.get(&(name.clone(), version.clone())).cloned()
    }

    /// Resolves an exact `(name, version)` pair, bypassing alias indirection.
    pub fn resolve_version(&self, name: &str, version: &str) -> Option<Arc<TemplateVersion>> {
        self.versions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&(name.to_owned(), version.to_owned()))
            .cloned()
    }

    /// Every alias currently pinned in the registry, sorted by alias name.
    /// Used by `GET /api/images` so the dashboard dropdown matches the server
    /// without a hardcoded frontend list.
    pub fn list_aliases(&self) -> Vec<Arc<TemplateVersion>> {
        let aliases = self
            .aliases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let versions = self
            .versions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut entries: Vec<_> = aliases
            .values()
            .filter_map(|(name, version)| versions.get(&(name.clone(), version.clone())).cloned())
            .collect();
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        entries
    }

    /// Drop an alias from the live registry and return the unpinned version
    /// plus absolute paths of artifact files that no other registered
    /// template still references (safe to delete from disk).
    ///
    /// Returns `None` when the alias was not installed. Does not touch the
    /// filesystem itself — the caller deletes the returned paths.
    pub fn unregister_alias(&self, alias: &str) -> Option<(Arc<TemplateVersion>, Vec<PathBuf>)> {
        let mut aliases = self
            .aliases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut versions = self
            .versions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let key = aliases.remove(alias)?;
        let removed = versions.remove(&key)?;

        // Paths still held by other versions must not be deleted.
        let orphan_paths = self.orphan_paths(&removed, &versions);

        drop(versions);
        drop(aliases);
        self.forget_registration(alias);

        Some((removed, orphan_paths))
    }

    /// Verify artifacts already under the image root and pin them as a live
    /// alias. Used after a successful download/install (and after a web
    /// build's finalize) so create can use the template without restarting
    /// the API — and recorded in [`REGISTRATIONS_FILE`] so it survives one.
    pub fn register_spec(&self, spec: TemplateSpec) -> Result<Arc<TemplateVersion>, TemplateError> {
        self.register_spec_inner(spec, true)
    }

    /// `persist` is false only while replaying the manifest at startup —
    /// rewriting each entry as it is restored would be pure churn.
    fn register_spec_inner(
        &self,
        spec: TemplateSpec,
        persist: bool,
    ) -> Result<Arc<TemplateVersion>, TemplateError> {
        let initrd = spec
            .initrd
            .as_ref()
            .map(|path| verify_artifact(&self.image_root, path))
            .transpose()?;
        let version = Arc::new(TemplateVersion {
            name: spec.alias.clone(),
            version: spec.version.clone(),
            kernel: verify_artifact(&self.image_root, &spec.kernel)?,
            initrd,
            rootfs: verify_artifact(&self.image_root, &spec.rootfs)?,
            boot_args: spec.boot_args.clone(),
        });
        let version_key = (spec.alias.clone(), version.version.clone());

        // Persisted before the in-memory maps change so a manifest that
        // can't be written fails the registration outright, rather than
        // handing back a template that silently evaporates on restart.
        if persist {
            self.persist_registration(&spec)?;
        }

        let mut versions = self
            .versions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut aliases = self
            .aliases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // Replacing an existing pin is allowed (reinstall); only the maps
        // update — on-disk files were overwritten atomically by the installer.
        versions.insert(version_key.clone(), version.clone());
        aliases.insert(spec.alias, version_key);
        Ok(version)
    }

    /// Absolute paths of `removed`'s artifacts that no version in
    /// `remaining` still references — i.e. safe for the caller to delete.
    fn orphan_paths(
        &self,
        removed: &TemplateVersion,
        remaining: &HashMap<(String, String), Arc<TemplateVersion>>,
    ) -> Vec<PathBuf> {
        let still_referenced: std::collections::HashSet<PathBuf> = remaining
            .values()
            .flat_map(|template| {
                std::iter::once(template.kernel.relative_path().to_path_buf())
                    .chain(std::iter::once(
                        template.rootfs.relative_path().to_path_buf(),
                    ))
                    .chain(
                        template
                            .initrd
                            .as_ref()
                            .map(|artifact| artifact.relative_path().to_path_buf()),
                    )
            })
            .collect();

        std::iter::once(removed.kernel.relative_path())
            .chain(std::iter::once(removed.rootfs.relative_path()))
            .chain(
                removed
                    .initrd
                    .as_ref()
                    .map(|artifact| artifact.relative_path()),
            )
            .filter(|relative| !still_referenced.contains(*relative))
            .map(|relative| self.image_root_path.join(relative))
            .collect()
    }

    /// Absolute path of the runtime registration manifest.
    fn registrations_path(&self) -> PathBuf {
        self.image_root_path.join(REGISTRATIONS_FILE)
    }

    /// Every runtime registration recorded so far, newest write wins. A
    /// missing or unreadable manifest reads as empty: it only ever augments
    /// the built-in set, so losing it degrades to pre-persistence behavior
    /// instead of failing startup.
    fn read_registrations(&self) -> Vec<TemplateSpec> {
        let path = self.registrations_path();
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::warn!(
                    path = %path.display(),
                    error = %error,
                    "template registration manifest is unreadable; ignoring it"
                );
                Vec::new()
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %error,
                    "template registration manifest could not be read; ignoring it"
                );
                Vec::new()
            }
        }
    }

    /// Records `spec` as `alias`'s current registration, replacing any
    /// earlier entry for the same alias — the registry itself only ever
    /// resolves one version per alias, so a second entry could only ever
    /// disagree with the live pin.
    fn persist_registration(&self, spec: &TemplateSpec) -> Result<(), TemplateError> {
        let _guard = self
            .manifest_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut entries = self.read_registrations();
        entries.retain(|entry| entry.alias != spec.alias);
        entries.push(spec.clone());
        self.write_registrations(&entries)
    }

    /// Drops `alias` from the manifest so a deleted template doesn't come
    /// back (as a warning about missing artifacts) on the next startup.
    fn forget_registration(&self, alias: &str) {
        let _guard = self
            .manifest_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut entries = self.read_registrations();
        let before = entries.len();
        entries.retain(|entry| entry.alias != alias);
        if entries.len() == before {
            return;
        }
        if let Err(error) = self.write_registrations(&entries) {
            // The alias is already gone from the live registry and its files
            // are about to be deleted, so a stale entry is only startup
            // noise (it is skipped as "artifacts are missing") — not worth
            // failing an otherwise-complete delete over.
            tracing::warn!(
                alias,
                error = %error,
                "could not update the template registration manifest after delete"
            );
        }
    }

    /// Writes the manifest through a temporary file so an interrupted write
    /// can never leave a half-serialized manifest that reads as empty.
    fn write_registrations(&self, entries: &[TemplateSpec]) -> Result<(), TemplateError> {
        let path = self.registrations_path();
        let temporary = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(entries)
            .map_err(|error| TemplateError::Io(io::Error::other(error)))?;
        fs::write(&temporary, &bytes)?;
        fs::rename(&temporary, &path)?;
        Ok(())
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
/// Whether every file a spec needs is already on disk. A cheap existence
/// check only — [`TemplateRegistry::from_specs`] still opens each one safely
/// beneath the image root and hashes it.
fn artifacts_present(image_root: &Path, spec: &TemplateSpec) -> bool {
    std::iter::once(&spec.kernel)
        .chain(std::iter::once(&spec.rootfs))
        .chain(spec.initrd.as_ref())
        .all(|relative| image_root.join(relative).is_file())
}

fn default_specs() -> Vec<TemplateSpec> {
    default_specs_for_arch(std::env::consts::ARCH)
}

fn default_specs_for_arch(host_arch: &str) -> Vec<TemplateSpec> {
    // Every built-in distribution supports both Firecracker host
    // architectures. x86_64 kernels are extracted ELF vmlinux files; ARM64
    // kernels retain their distro PE32+ Image format.
    let alpine_arch = match host_arch {
        "aarch64" => "aarch64",
        _ => "x86_64",
    };
    let alpine_boot_args = if alpine_arch == "aarch64" {
        "keep_bootcon console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rootfstype=ext4 rw"
    } else {
        "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rootfstype=ext4 rw"
    };
    let (ubuntu_kernel, ubuntu_rootfs_arch, ubuntu_boot_args) = if alpine_arch == "aarch64" {
        (
            "kernel/Image-ubuntu-26.04-aarch64",
            "arm64",
            "keep_bootcon console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw",
        )
    } else {
        (
            "kernel/vmlinux-ubuntu-26.04-x86_64",
            "amd64",
            "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw",
        )
    };
    let alpine_kernel = if alpine_arch == "aarch64" {
        "kernel/Image-alpine-virt-aarch64".to_owned()
    } else {
        "kernel/vmlinux-alpine-virt-x86_64".to_owned()
    };
    let (rocky_kernel, rocky_boot_args) = if alpine_arch == "aarch64" {
        (
            "kernel/Image-rocky-9.8-aarch64",
            "keep_bootcon console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rootfstype=ext4 rw",
        )
    } else {
        (
            "kernel/vmlinux-rocky-9.8-x86_64",
            "console=ttyS0 reboot=k panic=1 net.ifnames=0 root=/dev/vda rootfstype=ext4 rw",
        )
    };

    let mut specs = vec![
        TemplateSpec {
            alias: "ubuntu-26.04".to_owned(),
            version: "ubuntu-26.04-v2".to_owned(),
            // Ubuntu's own linux-image-generic kernel (see
            // install-ubuntu-roofs.sh) rather than a self-built
            // vanilla one — virtio_blk/ext4 are builtin, no initrd
            // needed (public-docs/images.md).
            kernel: PathBuf::from(ubuntu_kernel),
            initrd: None,
            rootfs: PathBuf::from(format!(
                "rootfs/ubuntu-rootfs-26.04-{ubuntu_rootfs_arch}.ext4"
            )),
            boot_args: ubuntu_boot_args.to_owned(),
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
            kernel: PathBuf::from(alpine_kernel),
            initrd: Some(PathBuf::from(format!(
                "kernel/initramfs-alpine-virt-{alpine_arch}"
            ))),
            rootfs: PathBuf::from(format!("rootfs/alpine-rootfs-3.24.1-{alpine_arch}.ext4")),
            boot_args: alpine_boot_args.to_owned(),
        },
    ];

    specs.push(TemplateSpec {
        alias: "rocky-9.8".to_owned(),
        version: "rocky-9.8-v1".to_owned(),
        // Rocky 9.8 keeps virtio storage/network and ext4 in modules, so both
        // architectures use a generic dracut initramfs. x86_64 boots through
        // virtio-pci; aarch64 uses Firecracker's normal virtio-mmio transport.
        kernel: PathBuf::from(rocky_kernel),
        initrd: Some(PathBuf::from(format!(
            "kernel/initramfs-rocky-9.8-{alpine_arch}"
        ))),
        rootfs: PathBuf::from(format!("rootfs/rocky-rootfs-9.8-{alpine_arch}.ext4")),
        boot_args: rocky_boot_args.to_owned(),
    });

    specs
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

    #[test]
    fn load_from_skips_templates_whose_images_are_not_built_yet() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("images");

        // A fresh install: the image root does not even exist yet.
        let registry = TemplateRegistry::load_from(&root).expect("startup must not fail");
        assert!(
            root.is_dir(),
            "the image root is created so later builds land there"
        );
        assert!(
            registry.resolve_alias("alpine-3.24").is_none(),
            "an unbuilt template must not resolve"
        );

        // Building one template's files in makes exactly that one usable; the
        // other stays skipped rather than failing the whole registry.
        let alpine = default_specs()
            .into_iter()
            .find(|spec| spec.alias == "alpine-3.24")
            .expect("alpine is a default template");
        for relative in [&alpine.kernel, &alpine.rootfs]
            .into_iter()
            .chain(alpine.initrd.as_ref())
        {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"image").unwrap();
        }

        let registry = TemplateRegistry::load_from(&root).expect("one built template");
        assert!(registry.resolve_alias("alpine-3.24").is_some());
        assert!(registry.resolve_alias("ubuntu-26.04").is_none());
        assert!(registry.resolve_alias("rocky-9.8").is_none());
    }

    /// A template registered at runtime (web build, or an install of a
    /// built-in) only lived in memory: after a restart its alias vanished
    /// from `GET /api/images`, its rootfs became an orphan file, and every
    /// VM created from it failed to start with "template X/Y is not
    /// registered". The registration manifest is what closes that.
    #[test]
    fn registered_templates_survive_a_restart() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::create_dir_all(root.join("kernel")).unwrap();
        fs::write(root.join("kernel/vmlinux"), b"kernel").unwrap();
        fs::write(root.join("my-nginx-base-abc123.ext4"), b"rootfs").unwrap();

        let registry = TemplateRegistry::load_from(root).unwrap();
        assert!(registry.resolve_alias("my-nginx-base").is_none());
        registry
            .register_spec(TemplateSpec {
                alias: "my-nginx-base".to_owned(),
                version: "my-nginx-base-abc123".to_owned(),
                kernel: PathBuf::from("kernel/vmlinux"),
                initrd: None,
                rootfs: PathBuf::from("my-nginx-base-abc123.ext4"),
                boot_args: "console=ttyS0 root=/dev/vda rw".to_owned(),
            })
            .unwrap();

        // A brand new registry over the same image root == an API restart.
        let restarted = TemplateRegistry::load_from(root).unwrap();
        let restored = restarted
            .resolve_alias("my-nginx-base")
            .expect("a registered template must survive a restart");
        assert_eq!(restored.version, "my-nginx-base-abc123");
        assert_eq!(restored.boot_args, "console=ttyS0 root=/dev/vda rw");
        // Reachable by its pinned version too, which is what a VM created
        // from it resolves through on start.
        assert!(
            restarted
                .resolve_version("my-nginx-base", "my-nginx-base-abc123")
                .is_some()
        );
    }

    /// Deleting a template must not leave it in the manifest, or every
    /// later startup would re-report it as a template with missing files.
    #[test]
    fn unregistering_a_template_drops_it_from_the_restart_manifest() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join("kernel"), b"kernel").unwrap();
        fs::write(root.join("rootfs"), b"rootfs").unwrap();

        let registry = TemplateRegistry::load_from(root).unwrap();
        registry
            .register_spec(TemplateSpec {
                alias: "derived".to_owned(),
                version: "derived-v1".to_owned(),
                kernel: PathBuf::from("kernel"),
                initrd: None,
                rootfs: PathBuf::from("rootfs"),
                boot_args: String::new(),
            })
            .unwrap();
        assert_eq!(registry.read_registrations().len(), 1);

        registry.unregister_alias("derived").unwrap();

        assert!(registry.read_registrations().is_empty());
        assert!(
            TemplateRegistry::load_from(root)
                .unwrap()
                .resolve_alias("derived")
                .is_none()
        );
    }

    /// A manifest naming files that were deleted out from under it must not
    /// take API startup down with it — same reasoning as skipping a
    /// built-in template whose images aren't built yet.
    #[test]
    fn a_registration_whose_artifacts_are_gone_is_skipped_at_startup() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join("kernel"), b"kernel").unwrap();
        fs::write(root.join("rootfs"), b"rootfs").unwrap();

        let registry = TemplateRegistry::load_from(root).unwrap();
        registry
            .register_spec(TemplateSpec {
                alias: "derived".to_owned(),
                version: "derived-v1".to_owned(),
                kernel: PathBuf::from("kernel"),
                initrd: None,
                rootfs: PathBuf::from("rootfs"),
                boot_args: String::new(),
            })
            .unwrap();
        fs::remove_file(root.join("rootfs")).unwrap();

        let restarted = TemplateRegistry::load_from(root).expect("startup must not fail");
        assert!(restarted.resolve_alias("derived").is_none());
    }

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
    fn unregister_alias_returns_orphan_paths_and_clears_resolve() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join("kernel"), b"kernel").unwrap();
        fs::write(root.join("rootfs"), b"rootfs").unwrap();
        let registry = TemplateRegistry::from_specs(
            root,
            [TemplateSpec {
                alias: "demo".to_owned(),
                version: "v1".to_owned(),
                kernel: PathBuf::from("kernel"),
                initrd: None,
                rootfs: PathBuf::from("rootfs"),
                boot_args: "console=ttyS0".to_owned(),
            }],
        )
        .unwrap();
        assert!(registry.resolve_alias("demo").is_some());
        assert!(registry.unregister_alias("missing").is_none());

        let (version, orphans) = registry.unregister_alias("demo").unwrap();
        assert_eq!(version.name, "demo");
        assert!(registry.resolve_alias("demo").is_none());
        assert_eq!(orphans.len(), 2);
        assert!(version.kernel.relative_path() == Path::new("kernel"));
    }

    #[test]
    fn unregister_keeps_shared_artifacts_referenced_by_another_alias() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join("kernel"), b"kernel").unwrap();
        fs::write(root.join("rootfs-a"), b"a").unwrap();
        fs::write(root.join("rootfs-b"), b"b").unwrap();
        let registry = TemplateRegistry::from_specs(
            root,
            [
                TemplateSpec {
                    alias: "a".to_owned(),
                    version: "v1".to_owned(),
                    kernel: PathBuf::from("kernel"),
                    initrd: None,
                    rootfs: PathBuf::from("rootfs-a"),
                    boot_args: String::new(),
                },
                TemplateSpec {
                    alias: "b".to_owned(),
                    version: "v1".to_owned(),
                    kernel: PathBuf::from("kernel"),
                    initrd: None,
                    rootfs: PathBuf::from("rootfs-b"),
                    boot_args: String::new(),
                },
            ],
        )
        .unwrap();
        let (_version, orphans) = registry.unregister_alias("a").unwrap();
        // Shared kernel must not be listed for deletion.
        assert!(
            !orphans
                .iter()
                .any(|path| path.file_name().and_then(|n| n.to_str()) == Some("kernel"))
        );
        assert!(
            orphans
                .iter()
                .any(|path| path.file_name().and_then(|n| n.to_str()) == Some("rootfs-a"))
        );
        assert!(registry.resolve_alias("b").is_some());
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
        let alpine_arch = match std::env::consts::ARCH {
            "aarch64" => "aarch64",
            _ => "x86_64",
        };
        let ubuntu = specs
            .iter()
            .find(|spec| spec.alias == "ubuntu-26.04")
            .expect("ubuntu-26.04 is one of the default specs");
        let expected_ubuntu_kernel = if alpine_arch == "aarch64" {
            "kernel/Image-ubuntu-26.04-aarch64"
        } else {
            "kernel/vmlinux-ubuntu-26.04-x86_64"
        };
        assert_eq!(ubuntu.kernel, PathBuf::from(expected_ubuntu_kernel));
        assert_eq!(ubuntu.initrd, None);
        assert_eq!(
            ubuntu.rootfs,
            PathBuf::from(if alpine_arch == "aarch64" {
                "rootfs/ubuntu-rootfs-26.04-arm64.ext4"
            } else {
                "rootfs/ubuntu-rootfs-26.04-amd64.ext4"
            })
        );
        assert_eq!(
            ubuntu.boot_args.contains("keep_bootcon"),
            alpine_arch == "aarch64"
        );

        let alpine = specs
            .iter()
            .find(|spec| spec.alias == "alpine-3.24")
            .expect("alpine-3.24 is one of the default specs");
        let expected_alpine_kernel = if alpine_arch == "aarch64" {
            "kernel/Image-alpine-virt-aarch64"
        } else {
            "kernel/vmlinux-alpine-virt-x86_64"
        };
        assert_eq!(alpine.kernel, PathBuf::from(expected_alpine_kernel));
        assert_eq!(
            alpine.initrd,
            Some(PathBuf::from(format!(
                "kernel/initramfs-alpine-virt-{alpine_arch}"
            )))
        );
        // virtio_blk/ext4 being modules on this kernel is exactly why the
        // initrd above is required — and why the kernel needs an explicit
        // hint to find the root filesystem type before that module loads.
        assert!(alpine.boot_args.contains("rootfstype=ext4"));
        assert!(alpine.boot_args.contains("console=ttyS0"));
        assert_eq!(
            alpine.boot_args.contains("keep_bootcon"),
            alpine_arch == "aarch64"
        );

        let rocky = specs
            .iter()
            .find(|spec| spec.alias == "rocky-9.8")
            .expect("rocky-9 is one of the default specs");
        assert_eq!(rocky.version, "rocky-9.8-v1");
        if alpine_arch == "aarch64" {
            assert_eq!(
                rocky.kernel,
                PathBuf::from("kernel/Image-rocky-9.8-aarch64")
            );
            assert_eq!(
                rocky.initrd,
                Some(PathBuf::from("kernel/initramfs-rocky-9.8-aarch64"))
            );
            assert_eq!(
                rocky.rootfs,
                PathBuf::from("rootfs/rocky-rootfs-9.8-aarch64.ext4")
            );
            assert!(rocky.boot_args.contains("keep_bootcon"));
            assert!(rocky.boot_args.contains("pci=off"));
        } else {
            assert_eq!(
                rocky.kernel,
                PathBuf::from("kernel/vmlinux-rocky-9.8-x86_64")
            );
            assert_eq!(
                rocky.initrd,
                Some(PathBuf::from("kernel/initramfs-rocky-9.8-x86_64"))
            );
            assert_eq!(
                rocky.rootfs,
                PathBuf::from("rootfs/rocky-rootfs-9.8-x86_64.ext4")
            );
            assert!(!rocky.boot_args.contains("pci=off"));
            assert!(rocky.boot_args.contains("net.ifnames=0"));
        }
        assert!(rocky.boot_args.contains("rootfstype=ext4"));

        let directory = tempdir().unwrap();
        for spec in &specs {
            for relative in [&spec.kernel, &spec.rootfs]
                .into_iter()
                .chain(spec.initrd.as_ref())
            {
                let path = directory.path().join(relative);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, b"image").unwrap();
            }
        }
        let registry = TemplateRegistry::from_specs(directory.path(), specs).unwrap();

        let ubuntu = registry
            .resolve_alias("ubuntu-26.04")
            .expect("Ubuntu template resolves");
        assert_eq!(ubuntu.requires_pci_transport(), alpine_arch != "aarch64");
        assert_eq!(
            ubuntu.runtime_boot_args().contains("pci=off"),
            alpine_arch == "aarch64"
        );
        assert!(ubuntu.runtime_boot_args().contains("root=/dev/vda"));
        assert_eq!(
            ubuntu.runtime_boot_args().contains("net.ifnames=0"),
            alpine_arch != "aarch64"
        );

        let rocky = registry
            .resolve_alias("rocky-9.8")
            .expect("Rocky template resolves");
        assert_eq!(rocky.requires_pci_transport(), alpine_arch != "aarch64");
        assert_eq!(
            rocky.runtime_boot_args().contains("pci=off"),
            alpine_arch == "aarch64"
        );
        assert_eq!(
            rocky
                .runtime_boot_args()
                .matches("rd.driver.pre=virtio_net")
                .count(),
            1
        );

        let alpine = registry
            .resolve_alias("alpine-3.24")
            .expect("Alpine template resolves");
        assert!(!alpine.requires_pci_transport());
        assert!(alpine.runtime_boot_args().contains("pci=off"));
    }

    #[test]
    fn arm64_default_specs_use_arm_artifacts_for_every_distribution() {
        let specs = default_specs_for_arch("aarch64");
        assert_eq!(
            specs
                .iter()
                .map(|spec| spec.alias.as_str())
                .collect::<Vec<_>>(),
            vec!["ubuntu-26.04", "alpine-3.24", "rocky-9.8"]
        );

        let ubuntu = specs
            .iter()
            .find(|spec| spec.alias == "ubuntu-26.04")
            .unwrap();
        assert_eq!(
            ubuntu.kernel,
            PathBuf::from("kernel/Image-ubuntu-26.04-aarch64")
        );
        assert_eq!(
            ubuntu.rootfs,
            PathBuf::from("rootfs/ubuntu-rootfs-26.04-arm64.ext4")
        );
        assert!(ubuntu.boot_args.contains("keep_bootcon"));
        assert!(ubuntu.boot_args.contains("pci=off"));

        let alpine = specs
            .iter()
            .find(|spec| spec.alias == "alpine-3.24")
            .unwrap();
        assert_eq!(
            alpine.kernel,
            PathBuf::from("kernel/Image-alpine-virt-aarch64")
        );
        assert_eq!(
            alpine.initrd,
            Some(PathBuf::from("kernel/initramfs-alpine-virt-aarch64"))
        );
        assert_eq!(
            alpine.rootfs,
            PathBuf::from("rootfs/alpine-rootfs-3.24.1-aarch64.ext4")
        );

        let rocky = specs.iter().find(|spec| spec.alias == "rocky-9.8").unwrap();
        assert_eq!(rocky.version, "rocky-9.8-v1");
        assert_eq!(
            rocky.kernel,
            PathBuf::from("kernel/Image-rocky-9.8-aarch64")
        );
        assert_eq!(
            rocky.initrd,
            Some(PathBuf::from("kernel/initramfs-rocky-9.8-aarch64"))
        );
        assert_eq!(
            rocky.rootfs,
            PathBuf::from("rootfs/rocky-rootfs-9.8-aarch64.ext4")
        );
        assert!(rocky.boot_args.contains("keep_bootcon"));
        assert!(rocky.boot_args.contains("pci=off"));
        assert!(!requires_pci_transport_for_arch("rocky-9.8", "aarch64"));
        assert!(requires_pci_transport_for_arch("rocky-9.8", "x86_64"));
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
    /// slower — see `public-docs/api.md`).
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
