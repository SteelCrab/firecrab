//! Immutable, integrity-verified VM boot template registry: resolves a
//! stable alias (e.g. `ubuntu-26.04`) to a pinned kernel+rootfs version,
//! re-verifying each artifact's identity (device/inode/length) and content
//! hash on every open so a template swapped out from under a running
//! registry is detected instead of silently served.

use std::collections::HashMap;
use std::env;
#[cfg(target_os = "macos")]
use std::ffi::CStr;
#[cfg(target_os = "linux")]
use std::ffi::CString;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::image_install::Architecture;
use crate::m2image_manifest;
pub(crate) mod deploy_cache;

/// Manifest of every template registered at runtime (image install or web
/// build), relative to the image root. Rebuilt into the registry on the next
/// startup so a template that was never part of [`default_specs`] — a web
/// build's derived alias, or an in-place rebuild's new version — does not
/// silently disappear on restart, taking every VM created from it with it.
const REGISTRATIONS_FILE: &str = ".templates.json";

/// `openat2` `RESOLVE_*` flag: reject crossing a mount point.
#[cfg(target_os = "linux")]
const RESOLVE_NO_XDEV: u64 = 0x01;
/// `openat2` `RESOLVE_*` flag: reject magic-link procfs-style resolution.
#[cfg(target_os = "linux")]
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
/// `openat2` `RESOLVE_*` flag: reject any symlink in the path.
#[cfg(target_os = "linux")]
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
/// `openat2` `RESOLVE_*` flag: keep resolution confined beneath the dirfd.
#[cfg(target_os = "linux")]
const RESOLVE_BENEATH: u64 = 0x08;

/// Failure modes for building or reading from a [`TemplateRegistry`].
#[derive(Debug, Error)]
pub enum TemplateError {
    /// An artifact path was absolute, empty, or escaped the image root.
    #[error("template artifact path must be a non-empty relative path")]
    InvalidPath,
    /// The original image writer, operation or physical namespace is unavailable.
    #[error("image mutation authority is unavailable or changed")]
    ImageMutationDenied,
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
    /// An existing alias/version cannot silently select different boot bytes or arguments.
    #[error("template version is immutable: {0}/{1}")]
    ImmutableVersion(String, String),
    /// Two [`TemplateSpec`]s declared the same alias.
    #[error("template registry contains a duplicate alias: {0}")]
    DuplicateAlias(String),
    /// A kernel artifact declares a CPU architecture this host cannot boot.
    #[error("kernel {path} is built for {found}, but this host is {host}")]
    KernelArchitectureMismatch {
        /// The offending kernel, relative to the image root.
        path: PathBuf,
        /// The architecture the kernel's own header declares.
        found: Architecture,
        /// The architecture this build of the API runs on.
        host: Architecture,
    },
    /// A kernel artifact is a well-formed ELF for an architecture Firecracker
    /// cannot boot on any host — distinct from one this check simply could
    /// not classify, which is allowed through.
    #[error(
        "kernel {path} targets an architecture firecrab does not support (ELF machine {machine:#06x})"
    )]
    UnsupportedKernelArchitecture {
        /// The offending kernel, relative to the image root.
        path: PathBuf,
        /// The ELF `e_machine` value that identified it.
        machine: u16,
    },
    /// A filesystem operation failed while building the registry.
    #[error("template registry I/O failed: {0}")]
    Io(#[from] io::Error),
}

/// Runtime catalog records retain exact boot hashes as well as their physical locators.
/// Legacy records without a pin remain readable until explicitly registered again.
#[derive(Deserialize, Serialize)]
struct RegisteredTemplate {
    #[serde(flatten)]
    spec: TemplateSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pin: Option<firecrab_stack::TemplatePin>,
    #[serde(default, skip_serializing_if = "is_false")]
    retired: bool,
}
fn is_false(value: &bool) -> bool {
    !*value
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
    /// Physical locators for the already verified immutable version.
    pub(crate) fn spec(&self) -> TemplateSpec {
        TemplateSpec {
            alias: self.name.clone(),
            version: self.version.clone(),
            kernel: self.kernel.relative_path().to_owned(),
            rootfs: self.rootfs.relative_path().to_owned(),
            initrd: self
                .initrd
                .as_ref()
                .map(|artifact| artifact.relative_path().to_owned()),
            boot_args: self.boot_args.clone(),
        }
    }
    /// Redacted content identity for stack plan approval and execution.
    pub(crate) fn stack_pin(&self) -> firecrab_stack::TemplatePin {
        firecrab_stack::TemplatePin {
            version: self.version.clone(),
            kernel_sha256: self.kernel.sha256().to_owned(),
            rootfs_sha256: self.rootfs.sha256().to_owned(),
            initrd_sha256: self.initrd.as_ref().map(|a| a.sha256().to_owned()),
            boot_args_sha256: self.boot_args_sha256(),
        }
    }

    /// SHA256 of `boot_args`, so callers can detect a boot-args change
    /// without re-hashing the (potentially multi-GB) rootfs.
    pub fn boot_args_sha256(&self) -> String {
        sha256_bytes(self.boot_args.as_bytes())
    }

    /// Whether this image must be started with Firecracker's PCI transport
    /// enabled. On x86_64, Rocky Linux needs PCI because its stock kernel
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
        if self.name.starts_with("rocky-") && !args.contains(&"rd.driver.pre=virtio_net") {
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
    m2image_manifest::image(name)
        .is_some_and(|image| matches!(image.distribution.as_str(), "rocky" | "ubuntu"))
        && architecture != "aarch64"
}

/// `(name, version)` -> the resolved, verified template it names.
type VersionMap = HashMap<(String, String), Arc<TemplateVersion>>;
/// Removed selection and the exact artifact paths no remaining version references.
#[cfg(test)]
pub(crate) type RemovedAlias = (Arc<TemplateVersion>, Vec<PathBuf>);

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
    versions: Arc<Mutex<VersionMap>>,
    /// Caches `open_verified`'s full-file hash by (device, inode), so many
    /// VMs starting at once against the same untouched multi-GB template
    /// don't each independently re-read and re-hash it (`public-docs/reference/api.md`'s
    /// "stuck at disk prep with many VMs" bug). Invalidated by length or
    /// mtime moving, which any real content change updates.
    verify_cache: Arc<Mutex<HashMap<(u64, u64), CachedHash>>>,
    /// Serializes the read-modify-write of [`REGISTRATIONS_FILE`], which two
    /// concurrent registrations would otherwise interleave into a manifest
    /// missing one of them.
    manifest_lock: Arc<Mutex<()>>,
    /// Serializes registration and cache collection through file deletion.
    mutations: Arc<Mutex<()>>,
    /// Private endpoint context for artifact jobs; clones retain the same authority boundary.
    iac_store: Option<crate::persistence::Store>,
    /// Private artifact producer authority, separate from the API's ordinary state.
    image_authority: Option<crate::image_lifecycle::ImageAuthority>,
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
            mutations: self.mutations.clone(),
            iac_store: self.iac_store.clone(),
            image_authority: self.image_authority.clone(),
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
    /// Bind one API state's private execution context; older worker clones keep their original store.
    pub(crate) fn with_iac_store(mut self, store: &crate::persistence::Store) -> Self {
        self.iac_store = Some(store.clone());
        self.image_authority = None;
        self
    }
    /// Native image pipelines borrow the endpoint context, never an owner supplied by a header.
    pub(crate) fn iac_store(&self) -> Option<&crate::persistence::Store> {
        self.iac_store.as_ref()
    }

    pub(crate) fn image_authority(&self) -> Option<&crate::image_lifecycle::ImageAuthority> {
        self.image_authority.as_ref()
    }
    /// Only a sealed producer capability for this endpoint can enter its worker clone.
    pub(crate) fn with_image_authority(
        mut self,
        authority: crate::image_lifecycle::ImageAuthority,
    ) -> Result<Self, TemplateError> {
        if self
            .iac_store
            .as_ref()
            .ok_or(TemplateError::ImageMutationDenied)?
            .iac_endpoint_id()
            .map_err(|_| TemplateError::ImageMutationDenied)?
            != authority.endpoint()
        {
            return Err(TemplateError::ImageMutationDenied);
        }
        self.image_authority = Some(authority);
        Ok(self)
    }

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
        for registration in self.read_registrations() {
            let spec = registration.spec;
            // The last persisted selection remains authoritative even when its files are gone.
            // Keep earlier versions addressable without silently rolling the alias back.
            self.aliases
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&spec.alias);
            self.versions
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&(spec.alias.clone(), spec.version.clone()));
            if registration.retired {
                continue;
            }
            if !artifacts_present(&self.image_root_path, &spec) {
                tracing::warn!(
                    template = spec.alias,
                    "registered template's artifacts are missing; skipping this template"
                );
                continue;
            }
            let alias = spec.alias.clone();
            if let Err(error) = self.register_spec_inner(spec, false, registration.pin) {
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
            mutations: Arc::new(Mutex::new(())),
            iac_store: None,
            image_authority: None,
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

    /// Pinned image-root directory fd for `openat2` checks.
    pub(crate) fn image_root(&self) -> &File {
        self.image_root.as_ref()
    }

    /// The mapped root must still be the directory this registry originally opened.
    pub(crate) fn physical_root(
        &self,
    ) -> Result<crate::persistence::iac_kernels::KernelRootPin, TemplateError> {
        crate::persistence::iac_kernels::KernelRootPin::from_directory(
            &self.image_root_path,
            &self.image_root,
        )
        .map_err(|_| TemplateError::ImageMutationDenied)
    }

    /// Include unready catalog versions; a missing artifact is not a missing registration.
    pub(crate) fn observed_images(
        &self,
        alias: &str,
    ) -> Result<Vec<crate::persistence::iac_images::ObservedImageVersion>, TemplateError> {
        use crate::persistence::iac_images::ObservedImageVersion;
        self.physical_root()?;
        let saved = self.read_registrations_checked()?;
        if saved
            .iter()
            .any(|entry| entry.spec.alias == alias && entry.retired)
        {
            return Ok(Vec::new());
        }
        let mut result: Vec<_> = saved
            .into_iter()
            .filter(|entry| entry.spec.alias == alias)
            .map(|entry| ObservedImageVersion {
                spec: entry.spec,
                pin: entry.pin,
            })
            .collect();
        for version in self
            .versions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
        {
            if version.name == alias
                && !result
                    .iter()
                    .any(|entry| entry.spec.version == version.version)
            {
                result.push(ObservedImageVersion {
                    spec: TemplateSpec {
                        alias: version.name.clone(),
                        version: version.version.clone(),
                        kernel: version.kernel.relative_path().to_owned(),
                        rootfs: version.rootfs.relative_path().to_owned(),
                        initrd: version
                            .initrd
                            .as_ref()
                            .map(|value| value.relative_path().to_owned()),
                        boot_args: version.boot_args.clone(),
                    },
                    pin: Some(version.stack_pin()),
                });
            }
        }
        Ok(result)
    }

    /// Verify the exact proposed files before retaining their immutable preparation.
    pub(crate) fn preview_image(
        &self,
        spec: &TemplateSpec,
    ) -> Result<Arc<TemplateVersion>, TemplateError> {
        self.physical_root()?;
        self.build_version(spec)
    }

    pub(crate) fn artifact_referenced(&self, relative: &Path) -> Result<bool, TemplateError> {
        let uses = |spec: &TemplateSpec| {
            spec.kernel == relative
                || spec.rootfs == relative
                || spec.initrd.as_deref() == Some(relative)
        };
        if self
            .read_registrations_checked()?
            .iter()
            .any(|entry| !entry.retired && uses(&entry.spec))
        {
            return Ok(true);
        }
        Ok(self
            .versions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .any(|version| {
                version.kernel.relative_path() == relative
                    || version.rootfs.relative_path() == relative
                    || version
                        .initrd
                        .as_ref()
                        .is_some_and(|value| value.relative_path() == relative)
            }))
    }
    pub(crate) fn image_artifact_digest(
        &self,
        relative: &Path,
    ) -> Result<Option<String>, TemplateError> {
        self.physical_root()?;
        match verify_artifact(&self.image_root, relative) {
            Ok(artifact) => Ok(Some(artifact.sha256().into())),
            Err(TemplateError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Unlink only an original, unchanged artifact through its retained parent directory.
    pub(crate) fn remove_image_artifact(
        &self,
        relative: &Path,
        sha256: &str,
    ) -> Result<(), TemplateError> {
        self.physical_root()?;
        let alias = match self.image_authority() {
            Some(crate::image_lifecycle::ImageAuthority::Operation(work)) => {
                &work.intent().definition().alias
            }
            _ => return Err(TemplateError::ImageMutationDenied),
        };
        if matches!(catalog_owner_of(relative),Some(owner) if owner != *alias) {
            return Ok(());
        }
        if relative.starts_with(".oci/kernel") || self.artifact_referenced(relative)? {
            return Ok(());
        }
        let artifact = match verify_artifact(&self.image_root, relative) {
            Ok(artifact) => artifact,
            Err(TemplateError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if artifact.sha256() != sha256 {
            return Err(TemplateError::ImageMutationDenied);
        }
        let parent = relative.parent().ok_or(TemplateError::InvalidPath)?;
        let directory = if parent.as_os_str().is_empty() {
            self.image_root.try_clone()?
        } else {
            open_beneath(&self.image_root, parent)?
        };
        let name = std::ffi::CString::new(
            relative
                .file_name()
                .ok_or(TemplateError::InvalidPath)?
                .as_encoded_bytes(),
        )
        .map_err(|_| TemplateError::InvalidPath)?;
        self.physical_root()?;
        // SAFETY: unlink one validated filename through the original parent descriptor.
        if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::NotFound {
                return Err(error.into());
            }
        }
        Ok(())
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

    /// Historical installed versions retain kernel dependencies even after alias selection changes.
    pub(crate) fn references_kernel(&self, relative: &Path) -> Result<bool, TemplateError> {
        if self
            .versions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .any(|version| version.kernel.relative_path() == relative)
        {
            return Ok(true);
        }
        // Unready registered images retain dependencies. Corruption is not permission
        // to delete a kernel whose saved references cannot be inspected.
        Ok(self
            .read_registrations_checked()?
            .iter()
            .any(|entry| !entry.retired && entry.spec.kernel == relative))
    }

    /// Read both maps under the same lock order as alias resolution.
    pub(crate) fn stack_snapshot(&self) -> Vec<firecrab_stack::TemplateSnapshot> {
        let aliases = self.aliases.lock().unwrap_or_else(|p| p.into_inner());
        let versions = self.versions.lock().unwrap_or_else(|p| p.into_inner());
        versions
            .iter()
            .map(|(key, template)| firecrab_stack::TemplateSnapshot {
                name: template.name.clone(),
                pin: template.stack_pin(),
                current: aliases.get(&template.name) == Some(key),
            })
            .collect()
    }

    /// Drop an alias from the live registry and return the unpinned version
    /// plus absolute paths of artifact files that no other registered
    /// template still references (safe to delete from disk).
    ///
    /// Returns `None` when the alias was not installed. Does not touch the
    /// filesystem itself — the caller deletes the returned paths.
    #[cfg(test)]
    pub fn unregister_alias(&self, alias: &str) -> Option<RemovedAlias> {
        self.unregister_alias_checked(alias).ok().flatten()
    }
    /// Delete an unused alias only after its durable catalog mutation succeeds.
    #[cfg(test)]
    pub(crate) fn unregister_alias_checked(
        &self,
        alias: &str,
    ) -> Result<Option<RemovedAlias>, TemplateError> {
        let _publication = crate::image_lifecycle::publication_lock(self, alias)?;
        let _mutation = self.mutations.lock().unwrap_or_else(|p| p.into_inner());
        let mut aliases = self
            .aliases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut versions = self
            .versions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let Some(key) = aliases.get(alias).cloned() else {
            return Ok(None);
        };
        let Some(removed) = versions.get(&key).cloned() else {
            return Ok(None);
        };
        crate::image_lifecycle::before_removal(self, alias)?;
        crate::image_lifecycle::write(self, alias, || self.forget_registration(&removed))?;
        aliases.remove(alias);
        versions.remove(&key);
        let historical: Vec<_> = versions
            .keys()
            .filter(|key| key.0 == alias)
            .cloned()
            .collect();
        let mut retired = vec![removed.clone()];
        for key in historical {
            if let Some(version) = versions.remove(&key) {
                retired.push(version);
            }
        }
        // An explicit unused-image deletion includes its retained versions, but keeps
        // artifacts still borrowed by another alias and independently managed kernels.
        let mut orphan_paths: Vec<_> = retired
            .iter()
            .flat_map(|version| self.orphan_paths(version, &versions))
            .collect();
        orphan_paths.sort();
        orphan_paths.dedup();

        drop(versions);
        drop(aliases);
        Ok(Some((removed, orphan_paths)))
    }

    /// Verify artifacts already under the image root and pin them as a live
    /// alias. Used after a successful download/install (and after a web
    /// build's finalize) so create can use the template without restarting
    /// the API — and recorded in [`REGISTRATIONS_FILE`] so it survives one.
    pub fn register_spec(&self, spec: TemplateSpec) -> Result<Arc<TemplateVersion>, TemplateError> {
        self.register_spec_inner(spec, true, None)
    }

    /// An unready catalog version still retains its immutable content identity.
    pub(crate) fn check_image_version(
        &self,
        spec: &TemplateSpec,
        pin: &firecrab_stack::TemplatePin,
    ) -> Result<(), TemplateError> {
        if self.read_registrations_checked()?.iter().any(|entry| {
            !entry.retired
                && entry.spec.alias == spec.alias
                && entry.spec.version == spec.version
                && entry.pin.as_ref().is_some_and(|saved| saved != pin)
        }) {
            return Err(TemplateError::ImmutableVersion(
                spec.alias.clone(),
                spec.version.clone(),
            ));
        }
        Ok(())
    }

    /// Retire even an unready registration without reconstructing missing boot artifacts.
    pub(crate) fn retire_image(&self, alias: &str) -> Result<(), TemplateError> {
        let _publication = crate::image_lifecycle::publication_lock(self, alias)?;
        let _mutation = self.mutations.lock().unwrap_or_else(|p| p.into_inner());
        crate::image_lifecycle::before_removal(self, alias)?;
        crate::image_lifecycle::write(self, alias, || {
            let _manifest = self.manifest_lock.lock().unwrap_or_else(|p| p.into_inner());
            let mut entries = self.read_registrations_checked()?;
            if !entries.iter().any(|entry| entry.spec.alias == alias)
                && let Some(version) = self.resolve_alias(alias)
            {
                entries.push(RegisteredTemplate {
                    spec: TemplateSpec {
                        alias: alias.into(),
                        version: version.version.clone(),
                        kernel: version.kernel.relative_path().to_owned(),
                        rootfs: version.rootfs.relative_path().to_owned(),
                        initrd: version
                            .initrd
                            .as_ref()
                            .map(|value| value.relative_path().to_owned()),
                        boot_args: version.boot_args.clone(),
                    },
                    pin: Some(version.stack_pin()),
                    retired: true,
                });
            }
            for entry in &mut entries {
                if entry.spec.alias == alias {
                    entry.retired = true;
                }
            }
            self.write_registrations(&entries)
        })?;
        if let Some(store) = self.iac_store() {
            store
                .record_image_availability(alias, true)
                .map_err(|_| TemplateError::ImageMutationDenied)?;
        }
        let mut aliases = self.aliases.lock().unwrap_or_else(|p| p.into_inner());
        let mut versions = self.versions.lock().unwrap_or_else(|p| p.into_inner());
        aliases.remove(alias);
        versions.retain(|(name, _), _| name != alias);
        Ok(())
    }

    /// Select a new immutable version while retaining earlier boot pins and their files.
    /// Historical artifacts are considered for removal only during explicit unused-image deletion.
    #[cfg(test)]
    pub fn replace_alias(
        &self,
        spec: TemplateSpec,
    ) -> Result<(Arc<TemplateVersion>, Vec<PathBuf>), TemplateError> {
        self.register_spec_inner(spec, true, None)
            .map(|version| (version, Vec::new()))
    }

    /// `persist` is false only while replaying the manifest at startup —
    /// rewriting each entry as it is restored would be pure churn.
    fn register_spec_inner(
        &self,
        spec: TemplateSpec,
        persist: bool,
        expected_pin: Option<firecrab_stack::TemplatePin>,
    ) -> Result<Arc<TemplateVersion>, TemplateError> {
        let _publication = if persist {
            crate::image_lifecycle::publication_lock(self, &spec.alias)?
        } else {
            None
        };
        let _mutation = self.mutations.lock().unwrap_or_else(|p| p.into_inner());
        let version = self.build_version(&spec)?;
        if expected_pin.is_some_and(|pin| pin != version.stack_pin()) {
            return Err(TemplateError::ImmutableVersion(spec.alias, spec.version));
        }
        let version_key = (spec.alias.clone(), version.version.clone());
        if self
            .versions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&version_key)
            .is_some_and(|existing| existing.stack_pin() != version.stack_pin())
        {
            return Err(TemplateError::ImmutableVersion(spec.alias, spec.version));
        }

        // Persisted before the in-memory maps change so a manifest that
        // can't be written fails the registration outright, rather than
        // handing back a template that silently evaporates on restart.
        if persist {
            self.check_image_version(&spec, &version.stack_pin())?;
            crate::image_lifecycle::before_publication(self, &spec, &version)?;
            crate::image_lifecycle::write(self, &spec.alias, || {
                self.persist_registration(&spec, version.stack_pin())
            })?;
            if let Some(store) = self.iac_store() {
                store
                    .record_image_availability(&spec.alias, false)
                    .map_err(|_| TemplateError::ImageMutationDenied)?;
            }
        }

        let mut aliases = self
            .aliases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut versions = self
            .versions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // Reinstallation may restore identical bytes at a verified physical path.
        // A changed boot identity requires a distinct immutable version.
        versions.insert(version_key.clone(), version.clone());
        aliases.insert(spec.alias, version_key);
        Ok(version)
    }

    fn build_version(&self, spec: &TemplateSpec) -> Result<Arc<TemplateVersion>, TemplateError> {
        let initrd = spec
            .initrd
            .as_ref()
            .map(|path| verify_artifact(&self.image_root, path))
            .transpose()?;
        verify_kernel_architecture(&self.image_root, &spec.kernel)?;
        Ok(Arc::new(TemplateVersion {
            name: spec.alias.clone(),
            version: spec.version.clone(),
            kernel: verify_artifact(&self.image_root, &spec.kernel)?,
            initrd,
            rootfs: verify_artifact(&self.image_root, &spec.rootfs)?,
            boot_args: spec.boot_args.clone(),
        }))
    }

    /// Absolute paths of `removed`'s artifacts that no version in
    /// `remaining` still references — i.e. safe for the caller to delete.
    #[cfg(test)]
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
            .filter(|relative| {
                if still_referenced.contains(*relative) {
                    return false;
                }
                // The independently managed kernel catalog owns its cache.
                // Removing the last OCI image that currently references a
                // kernel must not silently uninstall that kernel from the
                // separate kernel-management screen.
                if crate::kernel_manager::version_for_path(relative).is_some() {
                    return false;
                }
                // Catalog artifacts belong to their known_spec alias. An OCI
                // import borrows the Ubuntu kernel; deleting that import must
                // not take the kernel with it or the next import cannot pair.
                !matches!(catalog_owner_of(relative), Some(owner) if owner != removed.name)
            })
            .map(|relative| self.image_root_path.join(relative))
            .collect()
    }

    /// Startup keeps the rest of the host available if the catalog is corrupt.
    /// Writers always use the checked read and cannot replace corruption with an empty catalog.
    fn read_registrations(&self) -> Vec<RegisteredTemplate> {
        self.read_registrations_checked().unwrap_or_else(|error| {
            tracing::warn!(%error, "template registration catalog could not be restored");
            Vec::new()
        })
    }
    fn read_registrations_checked(&self) -> Result<Vec<RegisteredTemplate>, TemplateError> {
        use std::os::fd::FromRawFd;
        let catalog =
            std::ffi::CString::new(REGISTRATIONS_FILE).map_err(|_| TemplateError::InvalidPath)?;
        // SAFETY: this registry retains the directory descriptor; the C string is fixed.
        let fd = unsafe {
            libc::openat(
                self.image_root.as_raw_fd(),
                catalog.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::NotFound {
                Ok(Vec::new())
            } else {
                Err(error.into())
            };
        }
        // SAFETY: openat returned a new owned descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        if !file.metadata()?.is_file() {
            return Err(TemplateError::ImageMutationDenied);
        }
        serde_json::from_reader(file).map_err(|_| {
            TemplateError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid image registration catalog",
            ))
        })
    }

    /// Retain each immutable version; the last entry for an alias selects its head.
    /// Re-selecting an older version moves only that entry, preserving all other pins.
    fn persist_registration(
        &self,
        spec: &TemplateSpec,
        pin: firecrab_stack::TemplatePin,
    ) -> Result<(), TemplateError> {
        let _guard = self.manifest_lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut entries = self.read_registrations_checked()?;
        // Retain pre-seeded versions before a later software catalog can stop naming them.
        for version in self
            .versions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .filter(|version| version.name == spec.alias)
        {
            if !entries.iter().any(|entry| {
                !entry.retired
                    && entry.spec.alias == version.name
                    && entry.spec.version == version.version
            }) {
                entries.push(RegisteredTemplate {
                    spec: version.spec(),
                    pin: Some(version.stack_pin()),
                    retired: false,
                });
            }
        }
        entries.retain(|entry| {
            entry.spec.alias != spec.alias || (!entry.retired && entry.spec.version != spec.version)
        });
        entries.push(RegisteredTemplate {
            spec: spec.clone(),
            pin: Some(pin),
            retired: false,
        });
        self.write_registrations(&entries)
    }

    /// A tombstone prevents retained/shared files from resurrecting a deleted built-in alias.
    #[cfg(test)]
    fn forget_registration(&self, removed: &TemplateVersion) -> Result<(), TemplateError> {
        let _guard = self.manifest_lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut entries = self.read_registrations_checked()?;
        entries.retain(|entry| entry.spec.alias != removed.name);
        entries.push(RegisteredTemplate {
            spec: removed.spec(),
            pin: Some(removed.stack_pin()),
            retired: true,
        });
        self.write_registrations(&entries)
    }

    /// A unique synced temporary file cannot collide with another catalog publisher.
    fn write_registrations(&self, entries: &[RegisteredTemplate]) -> Result<(), TemplateError> {
        use std::os::fd::FromRawFd;
        let name = std::ffi::CString::new(format!(".templates-write-{}.tmp", uuid::Uuid::new_v4()))
            .map_err(|_| TemplateError::InvalidPath)?;
        let root = self.image_root.as_raw_fd();
        // SAFETY: the directory descriptor is retained by self; the unique name is NUL-terminated.
        let fd = unsafe {
            libc::openat(
                root,
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: openat returned a new owned descriptor.
        let mut file = unsafe { File::from_raw_fd(fd) };
        let result = (|| {
            serde_json::to_writer_pretty(&mut file, entries)
                .map_err(|error| TemplateError::Io(io::Error::other(error)))?;
            file.sync_all()?;
            // SAFETY: both names are relative to the same retained physical directory.
            let catalog = std::ffi::CString::new(REGISTRATIONS_FILE)
                .map_err(|_| TemplateError::InvalidPath)?;
            if unsafe { libc::renameat(root, name.as_ptr(), root, catalog.as_ptr()) } < 0 {
                return Err(TemplateError::Io(io::Error::last_os_error()));
            }
            self.image_root.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            // SAFETY: unlink only this attempt's unique temporary name in the original directory.
            let _ = unsafe { libc::unlinkat(root, name.as_ptr(), 0) };
        }
        result
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
/// Alias whose compiled-in spec names this relative artifact, if any.
fn catalog_owner_of(relative: &Path) -> Option<String> {
    default_specs().into_iter().find_map(|spec| {
        let owned = std::iter::once(spec.kernel.as_path())
            .chain(std::iter::once(spec.rootfs.as_path()))
            .chain(spec.initrd.as_deref())
            .any(|path| path == relative);
        owned.then_some(spec.alias)
    })
}

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
    // The JSON release manifest is compiled into the API, so build,
    // packaging, R2 paths, and runtime registration cannot drift apart.
    let architecture = if host_arch == "aarch64" {
        "aarch64"
    } else {
        "x86_64"
    };
    m2image_manifest::load()
        .images
        .into_iter()
        .map(|image| {
            let artifact = image
                .artifacts
                .get(architecture)
                .unwrap_or_else(|| panic!("{} lacks {architecture} artifacts", image.alias));
            TemplateSpec {
                version: image.template_version,
                alias: image.alias,
                kernel: PathBuf::from(&artifact.kernel),
                initrd: artifact.initrd.as_deref().map(PathBuf::from),
                rootfs: PathBuf::from(&artifact.rootfs),
                boot_args: artifact.boot_args.clone(),
            }
        })
        .collect()
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
#[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
    {
        let bytes = path.as_os_str().as_encoded_bytes();
        let c_path = CString::new(bytes).map_err(|_| TemplateError::InvalidPath)?;
        let how = OpenHow {
            flags: (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK) as u64,
            mode: 0,
            resolve: RESOLVE_BENEATH
                | RESOLVE_NO_SYMLINKS
                | RESOLVE_NO_MAGICLINKS
                | RESOLVE_NO_XDEV,
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
    #[cfg(target_os = "macos")]
    {
        // `openat2` is Linux-only. `..` is already rejected; `F_GETPATH` plus
        // `O_NOFOLLOW` opens the same regular file unit tests use.
        let mut buffer = [0i8; 1024];
        // SAFETY: `buffer` is MAXPATHLEN bytes, which is what F_GETPATH writes.
        let rc = unsafe { libc::fcntl(root.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) };
        if rc == -1 {
            return Err(TemplateError::Io(io::Error::last_os_error()));
        }
        let root_path = unsafe { CStr::from_ptr(buffer.as_ptr()) };
        let root_path = root_path.to_str().map_err(|_| TemplateError::InvalidPath)?;
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(Path::new(root_path).join(path))
            .map_err(TemplateError::Io)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (root, path);
        Err(TemplateError::Io(io::Error::new(
            io::ErrorKind::Unsupported,
            "openat2 is unavailable",
        )))
    }
}

/// What a kernel artifact's header says about it.
///
/// The split between [`Foreign`](KernelFormat::Foreign) and
/// [`Unrecognized`](KernelFormat::Unrecognized) is the point of this type:
/// only one of them may be treated leniently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KernelFormat {
    /// A kernel for an architecture Firecracker supports.
    Bootable(Architecture),
    /// A well-formed ELF for an architecture Firecracker cannot boot at all,
    /// carrying the `e_machine` value that identified it.
    Foreign {
        /// The ELF `e_machine` value, for the error message.
        machine: u16,
    },
    /// Nothing this function can classify. Hosts that installed a template
    /// before this check existed hold artifacts that land here, so it stays
    /// permissive.
    Unrecognized,
}

/// Classifies a kernel artifact from its own header.
///
/// x86_64 boots an ELF `vmlinux`; ARM64 boots Linux's `Image` container.
pub(crate) fn kernel_architecture(header: &[u8]) -> KernelFormat {
    if header.starts_with(b"\x7fELF") {
        // e_machine, a little-endian u16 at offset 18. Both architectures
        // Firecrab supports are little-endian, so the byte order is fixed.
        // An ELF too short to hold the field is corrupt rather than foreign,
        // and gets the same benefit of the doubt as an unknown format.
        let (Some(low), Some(high)) = (header.get(18), header.get(19)) else {
            return KernelFormat::Unrecognized;
        };
        let machine = u16::from_le_bytes([*low, *high]);
        return match machine {
            0x3e => KernelFormat::Bootable(Architecture::X86_64),
            0xb7 => KernelFormat::Bootable(Architecture::Aarch64),
            _ => KernelFormat::Foreign { machine },
        };
    }
    // ARM64 `Image`: a 64-byte header with its magic at offset 56. The
    // leading `MZ` only appears on EFI-bootable builds, so the magic — not
    // the first two bytes — is what identifies the format.
    if header.get(56..60) == Some(&0x644d_5241_u32.to_le_bytes()[..]) {
        return KernelFormat::Bootable(Architecture::Aarch64);
    }
    KernelFormat::Unrecognized
}

/// Rejects a kernel this host cannot boot. Firecracker's own failure mode
/// there is a silent hang with no console output, so the cost of letting one
/// through is an unbootable VM with nothing to diagnose.
pub(crate) fn verify_kernel_architecture(root: &File, path: &Path) -> Result<(), TemplateError> {
    let mut file = open_beneath(root, path)?;
    let mut header = Vec::new();
    // A kernel shorter than one header is simply unclassifiable, so read
    // what exists rather than requiring 64 bytes.
    (&mut file).take(64).read_to_end(&mut header)?;

    match kernel_architecture(&header) {
        KernelFormat::Bootable(found) if found != Architecture::HOST => {
            Err(TemplateError::KernelArchitectureMismatch {
                path: path.to_owned(),
                found,
                host: Architecture::HOST,
            })
        }
        KernelFormat::Foreign { machine } => Err(TemplateError::UnsupportedKernelArchitecture {
            path: path.to_owned(),
            machine,
        }),
        KernelFormat::Bootable(_) | KernelFormat::Unrecognized => Ok(()),
    }
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
    use core::assert_matches;

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
            registry.resolve_alias("alpine-3.24.1").is_none(),
            "an unbuilt template must not resolve"
        );

        // Building one template's files in makes exactly that one usable; the
        // other stays skipped rather than failing the whole registry.
        let alpine = default_specs()
            .into_iter()
            .find(|spec| spec.alias == "alpine-3.24.1")
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
        assert!(registry.resolve_alias("alpine-3.24.1").is_some());
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

    /// A 64-byte ELF64 header carrying `machine` in `e_machine`. That field
    /// is the only part architecture detection reads.
    fn elf_kernel(machine: u16) -> Vec<u8> {
        let mut header = vec![0_u8; 64];
        header[0..4].copy_from_slice(b"\x7fELF");
        header[4] = 2; // ELFCLASS64
        header[5] = 1; // ELFDATA2LSB
        header[6] = 1; // EV_CURRENT
        header[16..18].copy_from_slice(&2_u16.to_le_bytes()); // ET_EXEC
        header[18..20].copy_from_slice(&machine.to_le_bytes());
        header
    }

    /// A 64-byte ARM64 `Image` header. Linux puts its magic at offset 56;
    /// the leading `MZ` only appears on EFI-bootable builds, so the magic is
    /// what identifies the format.
    fn arm64_image_kernel() -> Vec<u8> {
        let mut header = vec![0_u8; 64];
        header[0..2].copy_from_slice(b"MZ");
        header[56..60].copy_from_slice(&0x644d_5241_u32.to_le_bytes());
        header
    }

    fn kernel_bytes_for(architecture: Architecture) -> Vec<u8> {
        match architecture {
            Architecture::Aarch64 => arm64_image_kernel(),
            Architecture::X86_64 => elf_kernel(0x3e),
        }
    }

    /// The three outcomes are deliberately distinct. `Unrecognized` has to
    /// stay permissive — templates registered before this check existed hold
    /// artifacts it cannot classify — while `Foreign` is a kernel it *can*
    /// identify as unbootable, which must not inherit that leniency.
    #[test]
    fn kernel_architecture_classifies_a_header_by_its_format() {
        let cases: [(&str, Vec<u8>, KernelFormat); 8] = [
            (
                "x86_64 ELF vmlinux",
                elf_kernel(0x3e),
                KernelFormat::Bootable(Architecture::X86_64),
            ),
            (
                "aarch64 ELF vmlinux",
                elf_kernel(0xb7),
                KernelFormat::Bootable(Architecture::Aarch64),
            ),
            (
                "aarch64 PE Image",
                arm64_image_kernel(),
                KernelFormat::Bootable(Architecture::Aarch64),
            ),
            (
                "32-bit ARM ELF",
                elf_kernel(0x28),
                KernelFormat::Foreign { machine: 0x28 },
            ),
            (
                "RISC-V ELF",
                elf_kernel(0xf3),
                KernelFormat::Foreign { machine: 0xf3 },
            ),
            (
                "ELF truncated before e_machine",
                b"\x7fELF\x02\x01\x01".to_vec(),
                KernelFormat::Unrecognized,
            ),
            (
                "pre-check placeholder",
                b"kernel".to_vec(),
                KernelFormat::Unrecognized,
            ),
            ("empty file", Vec::new(), KernelFormat::Unrecognized),
        ];

        for (name, header, expected) in cases {
            assert_eq!(kernel_architecture(&header), expected, "{name}");
        }
    }

    /// Firecracker cannot boot a kernel built for another architecture, and
    /// the symptom is a silent hang rather than an error — so registration
    /// is where it has to be caught.
    #[test]
    fn registering_a_kernel_for_another_architecture_is_rejected() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::write(
            root.join("kernel"),
            kernel_bytes_for(Architecture::HOST.other()),
        )
        .unwrap();
        fs::write(root.join("rootfs"), b"rootfs").unwrap();

        let registry = TemplateRegistry::load_from(root).unwrap();
        let error = registry
            .register_spec(TemplateSpec {
                alias: "foreign".to_owned(),
                version: "foreign-v1".to_owned(),
                kernel: PathBuf::from("kernel"),
                initrd: None,
                rootfs: PathBuf::from("rootfs"),
                boot_args: String::new(),
            })
            .unwrap_err();

        assert_matches!(error, TemplateError::KernelArchitectureMismatch { found, .. }
                if found == Architecture::HOST.other(), "{error:?}");
        assert!(registry.resolve_alias("foreign").is_none());
    }

    /// A RISC-V kernel is not a mismatch against this host so much as a
    /// kernel Firecracker cannot boot anywhere. It is identifiable, so the
    /// leniency granted to unclassifiable artifacts must not cover it.
    #[test]
    fn registering_a_kernel_for_an_unsupported_architecture_is_rejected() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join("kernel"), elf_kernel(0xf3)).unwrap();
        fs::write(root.join("rootfs"), b"rootfs").unwrap();

        let registry = TemplateRegistry::load_from(root).unwrap();
        let error = registry
            .register_spec(TemplateSpec {
                alias: "riscv".to_owned(),
                version: "riscv-v1".to_owned(),
                kernel: PathBuf::from("kernel"),
                initrd: None,
                rootfs: PathBuf::from("rootfs"),
                boot_args: String::new(),
            })
            .unwrap_err();

        assert_matches!(error, TemplateError::UnsupportedKernelArchitecture { .. });
        assert!(error.to_string().contains("0x00f3"), "{error}");
        assert!(registry.resolve_alias("riscv").is_none());
    }

    /// An artifact this check cannot classify has to keep registering, or
    /// every host that installed a template before the check existed would
    /// lose it on the next restart.
    #[test]
    fn registering_an_unclassifiable_kernel_is_still_allowed() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join("kernel"), b"kernel").unwrap();
        fs::write(root.join("rootfs"), b"rootfs").unwrap();

        let registry = TemplateRegistry::load_from(root).unwrap();
        registry
            .register_spec(TemplateSpec {
                alias: "legacy".to_owned(),
                version: "legacy-v1".to_owned(),
                kernel: PathBuf::from("kernel"),
                initrd: None,
                rootfs: PathBuf::from("rootfs"),
                boot_args: String::new(),
            })
            .expect("an unclassifiable kernel must keep registering");

        assert!(registry.resolve_alias("legacy").is_some());
    }

    #[test]
    fn registering_a_kernel_for_this_host_is_accepted() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join("kernel"), kernel_bytes_for(Architecture::HOST)).unwrap();
        fs::write(root.join("rootfs"), b"rootfs").unwrap();

        let registry = TemplateRegistry::load_from(root).unwrap();
        registry
            .register_spec(TemplateSpec {
                alias: "native".to_owned(),
                version: "native-v1".to_owned(),
                kernel: PathBuf::from("kernel"),
                initrd: None,
                rootfs: PathBuf::from("rootfs"),
                boot_args: String::new(),
            })
            .expect("a kernel matching the host must register");

        assert!(registry.resolve_alias("native").is_some());
    }

    /// Deleting a template must not leave it in the manifest, or every
    /// later startup would re-report it as a template with missing files.
    #[test]
    fn unregistering_a_template_retires_its_restart_selection() {
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

        assert!(
            registry
                .read_registrations()
                .iter()
                .all(|entry| entry.retired)
        );
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
    fn unregistering_an_oci_alias_does_not_delete_the_catalog_kernel_it_borrowed() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let ubuntu = TemplateRegistry::known_spec("ubuntu-26.04").expect("ubuntu catalog spec");
        if let Some(parent) = ubuntu.kernel.parent() {
            fs::create_dir_all(root.join(parent)).unwrap();
        }
        fs::create_dir_all(root.join("rootfs")).unwrap();
        fs::write(root.join(&ubuntu.kernel), b"catalog-kernel").unwrap();
        fs::write(root.join("rootfs/nginx-1.27.ext4"), b"oci-rootfs").unwrap();
        let registry = TemplateRegistry::from_specs(
            root,
            [TemplateSpec {
                alias: "nginx-1.27".to_owned(),
                version: "1.27".to_owned(),
                kernel: ubuntu.kernel.clone(),
                initrd: None,
                rootfs: PathBuf::from("rootfs/nginx-1.27.ext4"),
                boot_args: ubuntu.boot_args.clone(),
            }],
        )
        .unwrap();

        let (_version, orphans) = registry.unregister_alias("nginx-1.27").unwrap();
        assert!(
            !orphans.iter().any(|path| path.ends_with(&ubuntu.kernel)),
            "catalog kernel must stay for the next OCI import: {orphans:?}"
        );
        assert!(
            orphans
                .iter()
                .any(|path| path.ends_with("rootfs/nginx-1.27.ext4")),
            "the imported rootfs is this alias's own file: {orphans:?}"
        );
    }

    #[test]
    fn managed_kernel_cache_survives_removing_its_last_image_reference() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let kernel = crate::kernel_manager::cache_path("7.2.2").expect("catalog kernel");
        fs::create_dir_all(root.join(kernel.parent().unwrap())).unwrap();
        fs::write(root.join(&kernel), b"managed-kernel").unwrap();
        fs::write(root.join("rootfs.ext4"), b"rootfs").unwrap();
        let registry = TemplateRegistry::from_specs(
            root,
            [TemplateSpec {
                alias: "managed-image".to_owned(),
                version: "v1".to_owned(),
                kernel: kernel.clone(),
                initrd: None,
                rootfs: PathBuf::from("rootfs.ext4"),
                boot_args: String::new(),
            }],
        )
        .unwrap();

        let (_version, orphans) = registry.unregister_alias("managed-image").unwrap();
        assert!(!orphans.iter().any(|path| path.ends_with(&kernel)));
        assert!(orphans.iter().any(|path| path.ends_with("rootfs.ext4")));
    }

    #[test]
    fn replacing_an_alias_keeps_shared_rootfs_and_persists_the_new_pin() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join("kernel-old"), b"old").unwrap();
        fs::write(root.join("kernel-new"), b"new").unwrap();
        fs::write(root.join("rootfs"), b"rootfs").unwrap();
        let registry = TemplateRegistry::load_from(root).unwrap();
        registry
            .register_spec(TemplateSpec {
                alias: "demo".to_owned(),
                version: "v1".to_owned(),
                kernel: PathBuf::from("kernel-old"),
                initrd: None,
                rootfs: PathBuf::from("rootfs"),
                boot_args: String::new(),
            })
            .unwrap();

        let (_updated, orphans) = registry
            .replace_alias(TemplateSpec {
                alias: "demo".to_owned(),
                version: "v2".to_owned(),
                kernel: PathBuf::from("kernel-new"),
                initrd: None,
                rootfs: PathBuf::from("rootfs"),
                boot_args: String::new(),
            })
            .unwrap();
        assert_eq!(registry.resolve_alias("demo").unwrap().version, "v2");
        assert!(registry.resolve_version("demo", "v1").is_some());
        assert!(orphans.is_empty());
        assert!(!orphans.iter().any(|path| path.ends_with("rootfs")));

        let restarted = TemplateRegistry::load_from(root).unwrap();
        assert_eq!(restarted.resolve_alias("demo").unwrap().version, "v2");
        let original = restarted.resolve_version("demo", "v1").unwrap();
        restarted.open_verified(&original.kernel).unwrap();
        // Missing current artifacts must not turn observation into an implicit rollback.
        fs::remove_file(root.join("kernel-new")).unwrap();
        let missing = TemplateRegistry::load_from(root).unwrap();
        assert!(missing.resolve_alias("demo").is_none());
        assert!(missing.resolve_version("demo", "v1").is_some());
    }

    #[test]
    fn immutable_version_rejects_changed_boot_arguments_without_changing_restart_selection() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join("kernel"), b"kernel").unwrap();
        fs::write(root.join("rootfs"), b"rootfs").unwrap();
        let registry = TemplateRegistry::load_from(root).unwrap();
        let spec = TemplateSpec {
            alias: "immutable".into(),
            version: "v1".into(),
            kernel: "kernel".into(),
            rootfs: "rootfs".into(),
            initrd: None,
            boot_args: "console=ttyS0".into(),
        };
        let original = registry.register_spec(spec.clone()).unwrap();
        let mut changed = spec;
        changed.boot_args.push_str(" quiet");
        assert!(matches!(
            registry.replace_alias(changed),
            Err(TemplateError::ImmutableVersion(..))
        ));
        assert_eq!(
            registry.resolve_alias("immutable").unwrap().stack_pin(),
            original.stack_pin()
        );
        let restarted = TemplateRegistry::load_from(root).unwrap();
        assert_eq!(
            restarted.resolve_alias("immutable").unwrap().stack_pin(),
            original.stack_pin()
        );
    }

    #[test]
    fn restart_rejects_artifacts_that_no_longer_match_the_recorded_boot_pin() {
        let directory = tempdir().unwrap();
        let registry = create_registry(directory.path());
        registry
            .register_spec(TemplateSpec {
                alias: "tamper".into(),
                version: "v1".into(),
                kernel: "kernel".into(),
                rootfs: "rootfs".into(),
                initrd: None,
                boot_args: "console=ttyS0".into(),
            })
            .unwrap();
        fs::write(
            directory.path().join("rootfs"),
            b"changed after registration",
        )
        .unwrap();
        let restarted = TemplateRegistry::load_from(directory.path()).unwrap();
        assert!(restarted.resolve_alias("tamper").is_none());
        assert!(restarted.resolve_version("tamper", "v1").is_none());
    }

    #[test]
    fn corrupt_catalog_blocks_registration_and_deletion_without_forgetting_live_images() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let registry = create_registry(root);
        let spec = TemplateSpec {
            alias: "retained".into(),
            version: "v1".into(),
            kernel: "kernel".into(),
            rootfs: "rootfs".into(),
            initrd: None,
            boot_args: String::new(),
        };
        let original = registry.register_spec(spec.clone()).unwrap().stack_pin();
        let invalid = b"invalid-catalog-contents";
        fs::write(root.join(REGISTRATIONS_FILE), invalid).unwrap();
        let mut newer = spec;
        newer.version = "v2".into();
        assert!(registry.register_spec(newer).is_err());
        assert!(registry.unregister_alias_checked("retained").is_err());
        assert_eq!(fs::read(root.join(REGISTRATIONS_FILE)).unwrap(), invalid);
        assert_eq!(
            registry.resolve_alias("retained").unwrap().stack_pin(),
            original
        );
        assert_eq!(fs::read(root.join("rootfs")).unwrap(), b"rootfs");
    }

    #[test]
    fn alias_replacement_persists_a_preseeded_version_before_the_catalog_can_change() {
        let directory = tempdir().unwrap();
        let registry = create_registry(directory.path());
        let original = registry.resolve_alias("ubuntu-rootfs-26.04").unwrap();
        assert!(registry.read_registrations().is_empty());
        let mut next = original.spec();
        next.version = "v2".into();
        registry.replace_alias(next).unwrap();
        let restarted = TemplateRegistry::load_from(directory.path()).unwrap();
        assert_eq!(
            restarted.resolve_alias(&original.name).unwrap().version,
            "v2"
        );
        assert_eq!(
            restarted
                .resolve_version(&original.name, &original.version)
                .unwrap()
                .stack_pin(),
            original.stack_pin()
        );
    }

    #[test]
    fn recorded_builtin_pin_rejects_changed_bytes_from_alias_and_exact_version_lookup() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let spec = TemplateRegistry::known_specs().remove(0);
        for relative in [&spec.kernel, &spec.rootfs]
            .into_iter()
            .chain(spec.initrd.as_ref())
        {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"original-built-in-artifact").unwrap();
        }
        let registry = TemplateRegistry::load_from(root).unwrap();
        registry.register_spec(spec.clone()).unwrap();
        fs::write(root.join(&spec.rootfs), b"changed-built-in-artifact").unwrap();
        let restarted = TemplateRegistry::load_from(root).unwrap();
        assert!(restarted.resolve_alias(&spec.alias).is_none());
        assert!(
            restarted
                .resolve_version(&spec.alias, &spec.version)
                .is_none()
        );
        assert!(restarted.references_kernel(&spec.kernel).unwrap());
    }

    #[test]
    fn deleted_builtin_alias_stays_retired_when_another_image_keeps_its_files() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let spec = TemplateRegistry::known_specs().remove(0);
        for relative in [&spec.kernel, &spec.rootfs]
            .into_iter()
            .chain(spec.initrd.as_ref())
        {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"shared-catalog-artifact").unwrap();
        }
        let registry = TemplateRegistry::load_from(root).unwrap();
        let mut borrowed = spec.clone();
        borrowed.alias = "borrowed-source".into();
        registry.register_spec(borrowed.clone()).unwrap();
        let (_, orphans) = registry
            .unregister_alias_checked(&spec.alias)
            .unwrap()
            .unwrap();
        assert!(orphans.is_empty());
        let restarted = TemplateRegistry::load_from(root).unwrap();
        assert!(restarted.resolve_alias(&spec.alias).is_none());
        assert!(restarted.resolve_alias(&borrowed.alias).is_some());
        assert!(root.join(&spec.rootfs).is_file());
        restarted.register_spec(spec.clone()).unwrap();
        assert!(
            TemplateRegistry::load_from(root)
                .unwrap()
                .resolve_alias(&spec.alias)
                .is_some()
        );
    }

    #[test]
    fn unregistering_the_catalog_alias_still_deletes_its_own_kernel() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let ubuntu = TemplateRegistry::known_spec("ubuntu-26.04").expect("ubuntu catalog spec");
        if let Some(parent) = ubuntu.kernel.parent() {
            fs::create_dir_all(root.join(parent)).unwrap();
        }
        if let Some(parent) = ubuntu.rootfs.parent() {
            fs::create_dir_all(root.join(parent)).unwrap();
        }
        fs::write(root.join(&ubuntu.kernel), b"catalog-kernel").unwrap();
        fs::write(root.join(&ubuntu.rootfs), b"catalog-rootfs").unwrap();
        let registry = TemplateRegistry::from_specs(root, [ubuntu.clone()]).unwrap();

        let (_version, orphans) = registry.unregister_alias("ubuntu-26.04").unwrap();
        assert!(
            orphans.iter().any(|path| path.ends_with(&ubuntu.kernel)),
            "the catalog alias still owns its kernel: {orphans:?}"
        );
        assert!(
            orphans.iter().any(|path| path.ends_with(&ubuntu.rootfs)),
            "the catalog alias still owns its rootfs: {orphans:?}"
        );
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
        assert_matches!(parent_result, Err(TemplateError::InvalidPath));

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
        assert_matches!(link_result, Err(TemplateError::Io(_)));
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
        let result = registry.open_verified(initrd);
        assert_matches!(result, Err(TemplateError::ArtifactChanged(_)));
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
            .find(|spec| spec.alias == "alpine-3.24.1")
            .expect("alpine-3.24.1 is one of the default specs");
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
            .expect("rocky-9.8 is one of the default specs");
        assert_eq!(rocky.version, "rocky-9.8-v4");
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
            .resolve_alias("alpine-3.24.1")
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
            vec!["alpine-3.24.1", "ubuntu-26.04", "rocky-9.8"]
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
            .find(|spec| spec.alias == "alpine-3.24.1")
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
        assert_eq!(rocky.version, "rocky-9.8-v4");
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

        let result = registry.open_verified(&template.rootfs);
        assert_matches!(result, Err(TemplateError::ArtifactChanged(_)));
    }

    /// Many VMs starting at once each call `open_verified` for the same
    /// template; this proves that doesn't fail the 2nd+ time (the naive
    /// bug would be a stale/poisoned cache making repeats worse, not just
    /// slower — see `public-docs/reference/api.md`).
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

        let result = registry.open_verified(&template.rootfs);
        assert_matches!(result, Err(TemplateError::ArtifactChanged(_)));
    }
}
