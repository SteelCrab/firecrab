//! Kernel workers retain a physical root, source pin and private execution generation.
use crate::{
    image_install::{Architecture, ImageInstallTracker},
    oci::{ResolveError, Sha256Digest, kernel},
    persistence::{
        Store,
        iac_kernels::{
            KernelDefinition, KernelIntent, KernelOutcome, KernelRequest, KernelRootPin,
            KernelSourcePin,
        },
    },
    templates::TemplateRegistry,
};
use std::{
    fs::File,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
use uuid::Uuid;

fn failure(message: &str) -> ResolveError {
    ResolveError::Malformed(message.into())
}
/// Only validated journal admission can construct a source/build/publish authority.
#[derive(Clone)]
pub(crate) struct KernelWork {
    store: Store,
    intent: KernelIntent,
    current_root: PathBuf,
}
impl KernelWork {
    /// Check both physical scope and the exact native attempt before an artifact change.
    pub(crate) fn check(&self) -> Result<(), ResolveError> {
        self.check_root()?;
        if self.intent.definition().recipe != kernel::BUILD_REVISION {
            return Err(failure("Original kernel build recipe is unavailable"));
        }
        self.store
            .check_kernel(&self.intent)
            .map_err(|_| failure("Original kernel execution changed"))
    }
    /// A stale worker may clean its unique staging only while its original root is still mapped.
    pub(crate) fn check_root(&self) -> Result<(), ResolveError> {
        self.intent
            .definition()
            .root
            .check(&self.current_root)
            .map_err(|_| failure("Original kernel installation root changed"))
    }
    /// Record possible effects before download, compilation or removal.
    pub(crate) fn effects(&self) -> Result<(), ResolveError> {
        self.check()?;
        self.store
            .kernel_effects(&self.intent)
            .map_err(|_| failure("Cannot retain original kernel effects"))
    }
    /// Prepared source checksums survive interruption and upstream metadata changes.
    pub(crate) fn source(&self) -> Result<Option<KernelSourcePin>, ResolveError> {
        self.check()?;
        self.store
            .kernel_source(&self.intent)
            .map_err(|_| failure("Cannot read original kernel source"))
    }
    /// The original source URL and checksum become immutable before its archive is downloaded.
    pub(crate) fn prepare(&self, url: &str, sha256: &str) -> Result<(), ResolveError> {
        self.check()?;
        self.store
            .prepare_kernel_source(
                &self.intent,
                &KernelSourcePin {
                    url: url.into(),
                    sha256: sha256.into(),
                },
            )
            .map_err(|_| failure("Original kernel source checksum changed"))
    }
    /// Repair never publishes different bytes over a previously pinned kernel image.
    pub(crate) fn check_image(&self, digest: &Sha256Digest) -> Result<(), ResolveError> {
        self.check()?;
        if self
            .intent
            .definition()
            .expected_image_sha256
            .as_deref()
            .is_some_and(|pin| pin != digest.encoded())
        {
            return Err(failure("Original kernel image digest changed"));
        }
        Ok(())
    }
}
/// A dedicated filesystem lock excludes builders, removers and ownership admission across APIs.
pub(crate) fn runtime_lock(root: &Path, version: &str) -> Result<File, ResolveError> {
    lock_file(root, version, false)
}
fn lock_file(root: &Path, version: &str, shared: bool) -> Result<File, ResolveError> {
    if crate::kernel_manager::cache_path(version).is_none() {
        return Err(failure("Invalid kernel version"));
    }
    let directory = root
        .join(".iac-kernel-locks")
        .join(Architecture::HOST.as_str());
    std::fs::create_dir_all(&directory)
        .map_err(|_| failure("Cannot open kernel installation lock"))?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join(format!("{version}.lock")))
        .map_err(|_| failure("Cannot open kernel installation lock"))?;
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return Err(failure("Invalid kernel installation lock"));
    }
    if shared {
        file.try_lock_shared()
    } else {
        file.try_lock()
    }
    .map_err(|_| failure("Original kernel operation is still executing"))?;
    Ok(file)
}
/// A physical artifact namespace cannot silently move between independent API databases.
fn claim_namespace(file: &File, endpoint: Uuid) -> Result<(), ResolveError> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut handle = file;
    let mut saved = String::new();
    handle
        .seek(SeekFrom::Start(0))
        .map_err(|_| failure("Cannot inspect kernel namespace"))?;
    Read::by_ref(&mut handle)
        .take(128)
        .read_to_string(&mut saved)
        .map_err(|_| failure("Cannot inspect kernel namespace"))?;
    if !saved.is_empty() {
        if saved.trim().parse::<Uuid>().ok() != Some(endpoint) {
            return Err(failure(
                "Kernel namespace belongs to another API installation",
            ));
        }
        return Ok(());
    }
    handle
        .seek(SeekFrom::Start(0))
        .map_err(|_| failure("Cannot retain kernel namespace"))?;
    handle
        .write_all(endpoint.to_string().as_bytes())
        .and_then(|_| handle.sync_data())
        .map_err(|_| failure("Cannot retain kernel namespace"))?;
    Ok(())
}
/// Only the original typed journal may claim a mutable physical namespace.
pub(crate) fn authorize_namespace(
    file: &File,
    templates: &TemplateRegistry,
    intent: &KernelIntent,
) -> Result<KernelWork, ResolveError> {
    let store = templates
        .iac_store()
        .ok_or_else(|| failure("Kernel mutation requires its original API context"))?
        .clone();
    let work = KernelWork {
        store,
        intent: intent.clone(),
        current_root: templates.image_root_path().to_owned(),
    };
    work.check()?;
    claim_namespace(file, intent.operation.ownership.endpoint_id)?;
    Ok(work)
}
/// A borrowed kernel remains locked until the image referencing it is registered.
#[derive(Clone)]
pub(crate) struct KernelArtifactLease {
    relative: PathBuf,
    scope: KernelRootPin,
    _guard: std::sync::Arc<File>,
}
impl std::fmt::Debug for KernelArtifactLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KernelArtifactLease")
            .field("relative", &self.relative)
            .finish_non_exhaustive()
    }
}
impl PartialEq for KernelArtifactLease {
    fn eq(&self, other: &Self) -> bool {
        self.relative == other.relative && self.scope == other.scope
    }
}
impl Eq for KernelArtifactLease {}
impl KernelArtifactLease {
    /// The original relative path contains no caller-provided path components.
    pub(crate) fn relative(&self) -> &Path {
        &self.relative
    }
    /// Publication cannot redirect the borrowed kernel through a changed root mapping.
    pub(crate) fn check(&self, root: &Path) -> Result<(), ResolveError> {
        self.scope
            .check(root)
            .map_err(|_| failure("Borrowed kernel installation root changed"))
    }
}
fn borrow(
    templates: &TemplateRegistry,
    version: &str,
    guard: File,
) -> Result<KernelArtifactLease, ResolveError> {
    let relative = crate::kernel_manager::cache_path(version)
        .ok_or_else(|| failure("Invalid kernel version"))?;
    let scope = KernelRootPin::capture(templates.image_root_path())
        .map_err(|_| failure("Cannot identify borrowed kernel root"))?;
    Ok(KernelArtifactLease {
        relative,
        scope,
        _guard: std::sync::Arc::new(guard),
    })
}
/// Observe a physically verified artifact without treating a receipt as current readiness.
pub(crate) async fn installed_pin(root: &Path, version: &str) -> Option<(String, Option<String>)> {
    if !kernel::cached_kernel_is_valid(root, Architecture::HOST, version).await {
        return None;
    }
    if let Some(receipt) = kernel::built_kernel(root, Architecture::HOST, version).await {
        return Some((receipt.image_sha256, Some(receipt.source_sha256)));
    }
    let relative = crate::kernel_manager::cache_path(version)?;
    kernel::inspect_kernel(&root.join(relative), Architecture::HOST)
        .await
        .ok()
        .map(|hash| (hash.encoded().to_owned(), None))
}
/// Capture the actual root and any previously pinned bytes before saving a new intent.
pub(crate) async fn definition(
    templates: &TemplateRegistry,
    version: String,
    releases_enabled: bool,
) -> Result<KernelDefinition, ResolveError> {
    let root = templates.image_root_path();
    let receipt = kernel::recorded_kernel(root, Architecture::HOST, &version).await;
    let expected_image_sha256 = receipt
        .as_ref()
        .map(|r| r.image_sha256.clone())
        .or(installed_pin(root, &version).await.map(|p| p.0));
    let initial_source = receipt.map(|r| KernelSourcePin {
        url: r.source_url,
        sha256: r.source_sha256,
    });
    Ok(KernelDefinition {
        version,
        architecture: Architecture::HOST.as_str().into(),
        root: templates
            .physical_root()
            .map_err(|_| failure("Cannot identify kernel installation root"))?,
        releases_enabled,
        expected_image_sha256,
        initial_source,
        recipe: kernel::BUILD_REVISION.into(),
    })
}
/// Start a supervised worker; dropping its HTTP or image caller does not cancel admitted work.
pub(crate) fn spawn_install(
    store: Store,
    templates: TemplateRegistry,
    intent: KernelIntent,
    guard: File,
    tracker: Option<ImageInstallTracker>,
) -> tokio::task::JoinHandle<Result<PathBuf, ResolveError>> {
    tokio::spawn(async move {
        let work = KernelWork {
            store: store.clone(),
            intent: intent.clone(),
            current_root: templates.image_root_path().to_owned(),
        };
        let version = intent.definition().version.clone();
        let started = tracker
            .as_ref()
            .map(|tracker| tracker.begin_with(&version, "original kernel installation admitted"));
        if matches!(started, Some(Err(_))) {
            let _ = store.fail_kernel(&intent);
            return Err(failure("Kernel progress tracker is already in use"));
        }
        let tracked = tracker.clone();
        let worker = tokio::spawn(async move {
            let _guard = guard;
            work.check()?;
            claim_namespace(&_guard, work.intent.operation.ownership.endpoint_id)?;
            let relative = kernel::ensure_managed_kernel_authorized(
                work.intent.definition().root.path(),
                Architecture::HOST,
                &work.intent.definition().version,
                work.intent
                    .definition()
                    .releases_enabled
                    .then_some("enabled"),
                tracked.as_ref(),
                Some(&work),
            )
            .await?;
            work.check()?;
            let (image_sha256, source_sha256) = installed_pin(
                work.intent.definition().root.path(),
                &work.intent.definition().version,
            )
            .await
            .ok_or_else(|| failure("Kernel readiness is unconfirmed"))?;
            work.store
                .finish_kernel(
                    &work.intent,
                    &KernelOutcome {
                        version: work.intent.definition().version.clone(),
                        architecture: Architecture::HOST.as_str().into(),
                        installed: true,
                        image_sha256: Some(image_sha256),
                        source_sha256,
                    },
                )
                .map_err(|_| failure("Cannot acknowledge original kernel installation"))?;
            Ok::<_, ResolveError>(relative)
        });
        let result = worker
            .await
            .unwrap_or_else(|_| Err(failure("Original kernel worker interrupted")));
        if result.is_err() {
            let _ = store.fail_kernel(&intent);
        }
        if let Some(tracker) = tracker {
            if result.is_ok() {
                tracker.finish_ok_with(&version, "original kernel installation verified");
            } else {
                tracker.finish_err_with(
                    &version,
                    "Inspect and recover the original kernel operation",
                );
            }
        }
        result
    })
}
/// Borrow an already verified installation without permission to repair or replace it.
pub(crate) async fn borrow_existing(
    templates: &TemplateRegistry,
    version: &str,
) -> Result<KernelArtifactLease, ResolveError> {
    let store = templates
        .iac_store()
        .ok_or_else(|| failure("Kernel borrowing requires its original API context"))?;
    let guard = lock_file(templates.image_root_path(), version, true)?;
    let target = crate::persistence::iac_kernels::resource(Architecture::HOST.as_str(), version);
    store
        .kernel_read_allowed(&target)
        .map_err(|_| failure("Original kernel work is unfinished"))?;
    if installed_pin(templates.image_root_path(), version)
        .await
        .is_none()
    {
        return Err(failure("Kernel is not verified"));
    }
    borrow(templates, version, guard)
}

/// Internal image preparation uses the same durable journal instead of a separate kernel writer.
pub(crate) async fn ensure(
    templates: &TemplateRegistry,
    requested: &str,
    releases_url: Option<&str>,
) -> Result<KernelArtifactLease, ResolveError> {
    let store = templates
        .iac_store()
        .ok_or_else(|| failure("Kernel preparation requires its original API context"))?
        .clone();
    let version = if requested == "stable" {
        kernel::latest_stable(templates.image_root_path(), releases_url)
            .await?
            .version
    } else {
        requested.to_owned()
    };
    let guard = lock_file(templates.image_root_path(), &version, true)?;
    let target = crate::persistence::iac_kernels::resource(Architecture::HOST.as_str(), &version);
    if installed_pin(templates.image_root_path(), &version)
        .await
        .is_some()
    {
        store
            .kernel_read_allowed(&target)
            .map_err(|_| failure("Original kernel work is unfinished"))?;
        return borrow(templates, &version, guard);
    }
    drop(guard);
    let guard = runtime_lock(templates.image_root_path(), &version)?;
    let definition = definition(templates, version.clone(), releases_url.is_some()).await?;
    let (intent, dispatch) = store
        .begin_kernel(
            Uuid::new_v4(),
            KernelRequest::Install(requested.into()),
            definition,
        )
        .map_err(|_| failure("Kernel installation authority is managed or reserved"))?;
    if !dispatch {
        return Err(failure("Observe the original kernel operation"));
    }
    spawn_install(store.clone(), templates.clone(), intent, guard, None)
        .await
        .map_err(|_| failure("Kernel supervisor interrupted"))??;
    let guard = lock_file(templates.image_root_path(), &version, true)?;
    store
        .kernel_read_allowed(&target)
        .map_err(|_| failure("Kernel changed after installation"))?;
    if installed_pin(templates.image_root_path(), &version)
        .await
        .is_none()
    {
        return Err(failure("Kernel changed after installation"));
    }
    borrow(templates, &version, guard)
}

/// Physical verification and its exclusive filesystem lock cannot be supplied by an HTTP caller.
pub(crate) struct KernelPresence {
    target: firecrab_stack::ManagedResource,
    endpoint: Uuid,
    root: KernelRootPin,
    artifact: PathBuf,
    fingerprint: [u64; 5],
    guard: File,
}
fn fingerprint(path: &Path) -> std::io::Result<[u64; 5]> {
    use std::os::unix::fs::MetadataExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other("kernel is not a regular file"));
    }
    Ok([
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime() as u64,
        metadata.mtime_nsec() as u64,
    ])
}
impl KernelPresence {
    /// SQLite ownership admission checks the same endpoint, resource and unchanged physical artifact.
    pub(crate) fn check(
        &self,
        target: &firecrab_stack::ManagedResource,
        endpoint: Uuid,
    ) -> Result<(), crate::persistence::PersistenceError> {
        if target != &self.target || endpoint != self.endpoint {
            return Err(crate::persistence::PersistenceError::IacEndpointConflict);
        }
        if self.root.check(self.root.path()).is_err()
            || fingerprint(&self.artifact).ok() != Some(self.fingerprint)
        {
            return Err(crate::persistence::PersistenceError::StaleIacOwner);
        }
        Ok(())
    }
    /// Claim only the same physical installation after logical admission has passed.
    pub(crate) fn claim(&self) -> Result<(), crate::persistence::PersistenceError> {
        claim_namespace(&self.guard, self.endpoint)
            .map_err(|_| crate::persistence::PersistenceError::IacOwnershipConflict)
    }
}
/// Observe exact cache identity while excluding builds, removals and other transfers.
pub(crate) async fn prepare_transfer(
    templates: &TemplateRegistry,
    target: &firecrab_stack::ManagedResource,
) -> Result<KernelPresence, crate::persistence::PersistenceError> {
    crate::persistence::iac_kernels::validate_identity(target)?;
    let (_, version) = target
        .id
        .split_once(':')
        .ok_or(crate::persistence::PersistenceError::InvalidStackResource)?;
    let guard = runtime_lock(templates.image_root_path(), version)
        .map_err(|_| crate::persistence::PersistenceError::IacResourceBusy)?;
    if installed_pin(templates.image_root_path(), version)
        .await
        .is_none()
    {
        return Err(crate::persistence::PersistenceError::InvalidStackResource);
    }
    let artifact = templates.image_root_path().join(
        crate::kernel_manager::cache_path(version)
            .ok_or(crate::persistence::PersistenceError::InvalidStackResource)?,
    );
    let fingerprint = fingerprint(&artifact)
        .map_err(|_| crate::persistence::PersistenceError::InvalidStackResource)?;
    let root = KernelRootPin::capture(templates.image_root_path())
        .map_err(|_| crate::persistence::PersistenceError::InvalidStackResource)?;
    let endpoint = templates
        .iac_store()
        .ok_or(crate::persistence::PersistenceError::IacEndpointConflict)?
        .iac_endpoint_id()?;
    Ok(KernelPresence {
        target: target.clone(),
        endpoint,
        root,
        artifact,
        fingerprint,
        guard,
    })
}
