//! Physical image publication and private, generation-checked execution authority.
use crate::persistence::iac_kernels::KernelRootPin;
use crate::{
    persistence::{
        Store,
        iac_images::{ImageIntent, ImageVersion},
    },
    templates::{TemplateError, TemplateRegistry, TemplateSpec, TemplateVersion},
};
use firecrab_stack::{NativeAction, ResourceOwnership, TemplatePin};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;
fn denied() -> TemplateError {
    TemplateError::ImageMutationDenied
}
/// One publication lock protects shared artifact references across every image alias.
pub(crate) fn lock(templates: &TemplateRegistry) -> Result<File, TemplateError> {
    use std::os::fd::{AsRawFd, FromRawFd};
    templates.physical_root()?;
    // SAFETY: this registry retains its physical directory; the lock name is fixed.
    let fd = unsafe {
        libc::openat(
            templates.image_root().as_raw_fd(),
            c".iac-image.lock".as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: openat returned a new owned descriptor.
    let file = unsafe { File::from_raw_fd(fd) };
    if !file.metadata()?.is_file() {
        return Err(denied());
    }
    file.try_lock().map_err(|_| denied())?;
    templates.physical_root()?;
    Ok(file)
}
fn claim(file: &File, endpoint: Uuid) -> Result<(), TemplateError> {
    let mut handle = file;
    handle.seek(SeekFrom::Start(0))?;
    let mut marker = String::new();
    Read::by_ref(&mut handle)
        .take(128)
        .read_to_string(&mut marker)?;
    if !marker.is_empty() {
        return if marker.trim().parse::<Uuid>().ok() == Some(endpoint) {
            Ok(())
        } else {
            Err(denied())
        };
    }
    handle.seek(SeekFrom::Start(0))?;
    handle.write_all(endpoint.to_string().as_bytes())?;
    handle.sync_all()?;
    Ok(())
}

/// Exact physical catalog observation retained through a logical ownership commit.
pub(crate) struct ImagePresence {
    templates: TemplateRegistry,
    target: firecrab_stack::ManagedResource,
    endpoint: Uuid,
    root: KernelRootPin,
    observed: String,
    present: bool,
    guard: File,
}
impl ImagePresence {
    pub(crate) fn check(
        &self,
        target: &firecrab_stack::ManagedResource,
        endpoint: Uuid,
        require_present: bool,
    ) -> Result<(), crate::persistence::PersistenceError> {
        use crate::persistence::PersistenceError as Error;
        if target != &self.target || endpoint != self.endpoint {
            return Err(Error::IacEndpointConflict);
        }
        if require_present && !self.present {
            return Err(Error::InvalidStackResource);
        }
        if self.templates.physical_root().ok().as_ref() != Some(&self.root) {
            return Err(Error::StaleIacOwner);
        }
        let versions = self
            .templates
            .observed_images(&target.id)
            .map_err(|_| Error::StaleIacOwner)?;
        if serde_json::to_string(&versions).map_err(|_| Error::StaleIacOwner)? != self.observed {
            return Err(Error::StaleIacOwner);
        }
        for version in versions.into_iter().filter(|_| require_present) {
            let actual = self
                .templates
                .preview_image(&version.spec)
                .map_err(|_| Error::InvalidStackResource)?;
            if version
                .pin
                .as_ref()
                .is_some_and(|pin| pin != &actual.stack_pin())
            {
                return Err(Error::StaleIacOwner);
            }
        }
        Ok(())
    }
    pub(crate) fn claim(&self) -> Result<(), crate::persistence::PersistenceError> {
        claim(&self.guard, self.endpoint)
            .map_err(|_| crate::persistence::PersistenceError::IacOwnershipConflict)
    }
}
pub(crate) fn prepare_transfer(
    templates: &TemplateRegistry,
    target: &firecrab_stack::ManagedResource,
    require_present: bool,
) -> Result<ImagePresence, crate::persistence::PersistenceError> {
    use crate::persistence::PersistenceError as Error;
    if target.kind != "image" || !crate::persistence::iac_images::valid_alias(&target.id) {
        return Err(Error::InvalidStackResource);
    }
    let guard = lock(templates).map_err(|_| Error::IacResourceBusy)?;
    let store = templates.iac_store().ok_or(Error::IacEndpointConflict)?;
    let versions = templates
        .observed_images(&target.id)
        .map_err(|_| Error::InvalidStackResource)?;
    // An automatic deployment's private file is owned by its service controller.
    if require_present
        && store.deploy_images()?.iter().any(|owned| {
            versions
                .iter()
                .any(|version| version.spec.rootfs == owned.rootfs)
        })
    {
        return Err(Error::IacOwnershipConflict);
    }
    let presence = ImagePresence {
        templates: templates.clone(),
        target: target.clone(),
        endpoint: store.iac_endpoint_id()?,
        root: templates
            .physical_root()
            .map_err(|_| Error::InvalidStackResource)?,
        observed: serde_json::to_string(&versions).map_err(|_| Error::InvalidStackResource)?,
        present: !versions.is_empty(),
        guard,
    };
    presence.check(target, presence.endpoint, require_present)?;
    Ok(presence)
}
/// A caller cannot create an observed authority by supplying owner metadata in HTTP headers.
#[derive(Clone)]
pub(crate) struct CapturedImage {
    store: Store,
    ownership: ResourceOwnership,
    revision: u64,
    root: KernelRootPin,
    publication: Option<Arc<File>>,
}
impl CapturedImage {
    fn check(&self, root: &Path, alias: &str) -> Result<(), TemplateError> {
        self.root.check(root).map_err(|_| denied())?;
        let target = crate::persistence::iac_images::resource(alias);
        if target != self.ownership.resource
            || self.store.image_revision(alias).map_err(|_| denied())? != self.revision
            || self
                .store
                .iac_resource_owner(&target)
                .map_err(|_| denied())?
                != self.ownership
            || !self
                .store
                .iac_public_write_allowed(&target)
                .map_err(|_| denied())?
        {
            return Err(denied());
        }
        Ok(())
    }
}
/// Only admitted native work constructs this physical publication capability.
#[derive(Clone)]
pub(crate) struct ImageWork {
    store: Store,
    intent: ImageIntent,
    current_root: PathBuf,
    _lock: Arc<File>,
}
impl ImageWork {
    pub(crate) fn new(
        templates: &TemplateRegistry,
        intent: ImageIntent,
        file: File,
    ) -> Result<Self, TemplateError> {
        let store = templates.iac_store().ok_or_else(denied)?.clone();
        if intent.definition().root != templates.physical_root()? {
            return Err(denied());
        }
        let work = Self {
            store,
            intent,
            current_root: templates.image_root_path().to_owned(),
            _lock: Arc::new(file),
        };
        work.check()?;
        claim(&work._lock, work.intent.operation.ownership.endpoint_id)?;
        Ok(work)
    }
    pub(crate) fn check(&self) -> Result<(), TemplateError> {
        self.intent
            .definition()
            .root
            .check(&self.current_root)
            .map_err(|_| denied())?;
        self.store.check_image(&self.intent).map_err(|_| denied())
    }
    pub(crate) fn intent(&self) -> &ImageIntent {
        &self.intent
    }
    pub(crate) fn effects(&self) -> Result<(), TemplateError> {
        self.check()?;
        self.store.image_effects(&self.intent).map_err(|_| denied())
    }
    pub(crate) fn kernel(
        &self,
        relative: &Path,
        sha256: &str,
        boot_args: &str,
    ) -> Result<String, TemplateError> {
        self.check()?;
        if let Some(original) = self.original_kernel()? {
            if original.relative != relative || original.sha256 != sha256 {
                return Err(denied());
            }
            return Ok(original.boot_args);
        }
        self.store
            .prepare_image_kernel(
                &self.intent,
                &crate::persistence::iac_images::ImageKernelPin {
                    relative: relative.to_owned(),
                    sha256: sha256.into(),
                    boot_args: boot_args.into(),
                },
            )
            .map_err(|_| denied())?;
        Ok(boot_args.into())
    }
    pub(crate) fn original_kernel(
        &self,
    ) -> Result<Option<crate::persistence::iac_images::ImageKernelPin>, TemplateError> {
        self.check()?;
        self.store
            .prepared_image_kernel(&self.intent)
            .map_err(|_| denied())
    }
    pub(crate) fn prepare(
        &self,
        spec: &TemplateSpec,
        pin: TemplatePin,
    ) -> Result<(), TemplateError> {
        self.check()?;
        self.store
            .prepare_image(
                &self.intent,
                &ImageVersion {
                    spec: spec.clone(),
                    pin,
                },
            )
            .map_err(|_| denied())
    }
    fn publication(
        &self,
        root: &Path,
        spec: &TemplateSpec,
        version: &TemplateVersion,
    ) -> Result<(), TemplateError> {
        self.check()?;
        if root != self.current_root
            || spec.alias != self.intent.definition().alias
            || self.intent.operation.action == NativeAction::DeleteImage
        {
            return Err(denied());
        }
        self.prepare(spec, version.stack_pin())?;
        self.effects()
    }
    fn removal(&self, root: &Path, alias: &str) -> Result<(), TemplateError> {
        self.check()?;
        if root != self.current_root
            || alias != self.intent.definition().alias
            || self.intent.operation.action != NativeAction::DeleteImage
        {
            return Err(denied());
        }
        self.effects()
    }
}
#[derive(Clone)]
pub(crate) enum ImageAuthority {
    Operation(Arc<ImageWork>),
    Observed(CapturedImage),
}
/// Capture before asynchronous source work; a later release/reclaim cannot authorize its callback.
pub(crate) fn capture(
    templates: &TemplateRegistry,
    alias: &str,
) -> Result<ImageAuthority, TemplateError> {
    let store = templates.iac_store().ok_or_else(denied)?.clone();
    let (ownership, revision) = store.capture_image_writer(alias).map_err(|_| denied())?;
    let authority = CapturedImage {
        revision,
        store,
        ownership,
        root: templates.physical_root()?,
        publication: None,
    };
    authority.check(templates.image_root_path(), alias)?;
    Ok(ImageAuthority::Observed(authority))
}
/// Raw producers acquire the publication lock; typed workers already retain it.
pub(crate) fn publication_lock(
    templates: &TemplateRegistry,
    alias: &str,
) -> Result<Option<File>, TemplateError> {
    if let Some(ImageAuthority::Observed(authority)) = templates.image_authority()
        && authority.publication.is_some()
    {
        authority.check(templates.image_root_path(), alias)?;
        return Ok(None);
    }
    if let Some(ImageAuthority::Operation(work)) = templates.image_authority() {
        work.check()?;
        if work.intent.definition().alias != alias {
            return Err(denied());
        }
        return Ok(None);
    }
    if templates.iac_store().is_none() {
        #[cfg(test)]
        {
            return Ok(None);
        }
        #[cfg(not(test))]
        {
            return Err(denied());
        }
    }
    let file = lock(templates)?;
    let authority = match templates.image_authority() {
        Some(ImageAuthority::Observed(authority)) => authority.clone(),
        _ => match capture(templates, alias)? {
            ImageAuthority::Observed(authority) => authority,
            _ => return Err(denied()),
        },
    };
    authority.check(templates.image_root_path(), alias)?;
    claim(&file, authority.ownership.endpoint_id)?;
    Ok(Some(file))
}
/// Hold physical publication authority across both rootfs copying and catalog registration.
pub(crate) fn publication_scope(
    templates: &TemplateRegistry,
    alias: &str,
) -> Result<TemplateRegistry, TemplateError> {
    if let Some(ImageAuthority::Operation(work)) = templates.image_authority() {
        work.check()?;
        if work.intent.definition().alias != alias {
            return Err(denied());
        }
        return Ok(templates.clone());
    }
    if templates.iac_store().is_none() {
        #[cfg(test)]
        {
            return Ok(templates.clone());
        }
        #[cfg(not(test))]
        {
            return Err(denied());
        }
    }
    let mut authority = match templates.image_authority() {
        Some(ImageAuthority::Observed(authority)) => authority.clone(),
        _ => match capture(templates, alias)? {
            ImageAuthority::Observed(authority) => authority,
            _ => return Err(denied()),
        },
    };
    if authority.publication.is_none() {
        let file = lock(templates)?;
        authority.check(templates.image_root_path(), alias)?;
        claim(&file, authority.ownership.endpoint_id)?;
        authority.publication = Some(Arc::new(file));
    }
    templates
        .clone()
        .with_image_authority(ImageAuthority::Observed(authority))
}
pub(crate) fn before_publication(
    templates: &TemplateRegistry,
    spec: &TemplateSpec,
    version: &TemplateVersion,
) -> Result<(), TemplateError> {
    if let Some(ImageAuthority::Operation(work)) = templates.image_authority() {
        work.publication(templates.image_root_path(), spec, version)?;
    }
    Ok(())
}
pub(crate) fn before_removal(
    templates: &TemplateRegistry,
    alias: &str,
) -> Result<(), TemplateError> {
    if let Some(ImageAuthority::Operation(work)) = templates.image_authority() {
        work.removal(templates.image_root_path(), alias)?;
    }
    Ok(())
}

/// Observed producers hold SQLite authority through the short atomic catalog write.
pub(crate) fn write<T>(
    templates: &TemplateRegistry,
    alias: &str,
    action: impl FnOnce() -> Result<T, TemplateError>,
) -> Result<T, TemplateError> {
    if let Some(ImageAuthority::Operation(work)) = templates.image_authority() {
        work.check()?;
        if work.intent.definition().alias != alias {
            return Err(denied());
        }
        return action();
    }
    let captured = match templates.image_authority() {
        Some(ImageAuthority::Observed(authority)) => authority.clone(),
        _ => {
            if templates.iac_store().is_none() {
                #[cfg(test)]
                {
                    return action();
                }
                #[cfg(not(test))]
                {
                    return Err(denied());
                }
            }
            match capture(templates, alias)? {
                ImageAuthority::Observed(authority) => authority,
                _ => return Err(denied()),
            }
        }
    };
    if captured.ownership.resource != crate::persistence::iac_images::resource(alias) {
        return Err(denied());
    }
    captured
        .store
        .with_image_writer(&captured.ownership, captured.revision, || {
            captured
                .root
                .check(templates.image_root_path())
                .map_err(|_| denied())?;
            action()
        })
}
impl ImageAuthority {
    pub(crate) fn endpoint(&self) -> Uuid {
        match self {
            Self::Operation(work) => work.intent.operation.ownership.endpoint_id,
            Self::Observed(authority) => authority.ownership.endpoint_id,
        }
    }
}

impl std::fmt::Debug for ImageAuthority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ImageAuthority")
            .field("endpoint", &self.endpoint())
            .finish_non_exhaustive()
    }
}
