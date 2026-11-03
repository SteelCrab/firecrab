//! Publishes a packed OCI ext4 and registers it as a template.
//!
//! The earlier stages produced a named, kernel-paired image that still lives
//! at a scratch path. This stage copies it to `rootfs/<alias>.ext4`, records
//! a `TemplateSpec`, and deletes that published file if registration fails.
//! The source ext4 and the shared catalog kernel stay where they are.

use super::*;

use crate::templates::{TemplateRegistry, TemplateSpec};

/// Relative location of a registered OCI rootfs.
fn rootfs_relative(alias: &str) -> PathBuf {
    Path::new("rootfs").join(format!("{alias}.ext4"))
}

/// Copies the packed ext4 into the image root and registers the spec.
pub(super) fn register_named_oci_image(
    named: NamedOciImage,
    templates: &TemplateRegistry,
) -> Result<RegisteredOciImage, ResolveError> {
    register_named_image(named, templates, None)
}

/// Deploy owns a unique, journaled destination. Manual OCI imports retain their
/// existing paths and are never adopted by automatic cache collection.
pub(super) fn register_named_image(
    named: NamedOciImage,
    templates: &TemplateRegistry,
    cache_owner: Option<&crate::persistence::Store>,
) -> Result<RegisteredOciImage, ResolveError> {
    let scoped =
        crate::image_lifecycle::publication_scope(templates, named.alias()).map_err(|_| {
            ResolveError::Malformed("Original image publication authority changed".into())
        })?;
    let templates = &scoped;
    let image_root = templates.image_root_path();
    if let Some(lease) = &named.image.kernel_lease {
        lease.check(image_root)?;
    }
    let retained = match templates.image_authority() {
        Some(crate::image_lifecycle::ImageAuthority::Operation(work)) => {
            work.check()
                .map_err(|_| ResolveError::Malformed("Original image authority changed".into()))?;
            Some(super::retained_import_spec(work.intent())?)
        }
        _ => None,
    };
    let relative = if let Some(spec) = &retained {
        spec.rootfs.clone()
    } else if cache_owner.is_some() {
        Path::new("rootfs/deploy").join(format!("{}.ext4", Uuid::new_v4()))
    } else {
        rootfs_relative(named.alias())
    };
    let destination = image_root.join(&relative);
    let source = named.image().rootfs().path();
    let spec = TemplateSpec {
        alias: named.alias().to_owned(),
        version: named.version().to_owned(),
        kernel: named.image().kernel().to_path_buf(),
        initrd: named.image().initrd().map(Path::to_path_buf),
        rootfs: relative.clone(),
        boot_args: named.image().boot_args().to_owned(),
    };
    if templates
        .observed_images(&spec.alias)
        .map_err(|_| ResolveError::Malformed("Original image catalog is unavailable".into()))?
        .iter()
        .any(|entry| entry.spec.version == spec.version)
    {
        let mut preview = spec.clone();
        preview.rootfs = source
            .strip_prefix(image_root)
            .map_err(|_| {
                ResolveError::Malformed("Image preparation escaped its original root".into())
            })?
            .to_owned();
        let pin = templates
            .preview_image(&preview)
            .map_err(|_| ResolveError::Malformed("Image preparation is unverified".into()))?
            .stack_pin();
        templates.check_image_version(&spec, &pin).map_err(|_| {
            ResolveError::Malformed("Original image version has different boot bytes".into())
        })?;
    }
    if let Some(store) = cache_owner {
        store
            .remember_deploy_image(&spec)
            .map_err(|error| ResolveError::RegisterFailed {
                alias: named.alias().to_owned(),
                detail: format!("could not journal Deploy image: {error}"),
            })?;
    }
    if let Some(crate::image_lifecycle::ImageAuthority::Operation(work)) =
        templates.image_authority()
    {
        work.effects()
            .map_err(|_| ResolveError::Malformed("Original image operation changed".into()))?;
    }
    let published = publish_rootfs(source, &destination)?;
    if let Err(error) = templates.register_spec(spec) {
        // The original operation retains its completed ext4 for exact recovery.
        if retained.is_none() && templates.physical_root().is_ok() {
            published.revert();
        }
        return Err(ResolveError::RegisterFailed {
            alias: named.alias().to_owned(),
            detail: error.to_string(),
        });
    }
    Ok(RegisteredOciImage {
        alias: named.alias().to_owned(),
        version: named.version().to_owned(),
        rootfs: relative,
    })
}

/// A published rootfs that can still be withdrawn if registration fails.
struct PublishedRootfs {
    path: PathBuf,
    owned: bool,
}

impl PublishedRootfs {
    fn revert(&self) {
        if self.owned {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn publish_rootfs(source: &Path, destination: &Path) -> Result<PublishedRootfs, ResolveError> {
    match std::fs::symlink_metadata(destination) {
        Ok(_) => {
            return Err(ResolveError::RegisterDestinationExists {
                path: destination.to_owned(),
            });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source_error) => {
            return Err(register_io(
                "inspect destination",
                destination.to_owned(),
                source_error,
            ));
        }
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|source_error| {
            register_io("create rootfs directory", parent.to_owned(), source_error)
        })?;
    }
    let mut partial = destination.as_os_str().to_os_string();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    let _ = std::fs::remove_file(&partial);
    if let Err(error) = std::fs::copy(source, &partial) {
        let _ = std::fs::remove_file(&partial);
        return Err(register_io("copy rootfs", partial, error));
    }
    if let Err(error) = std::fs::File::open(&partial).and_then(|file| file.sync_all()) {
        let _ = std::fs::remove_file(&partial);
        return Err(register_io("sync rootfs", partial, error));
    }
    if let Err(error) = std::fs::hard_link(&partial, destination) {
        let _ = std::fs::remove_file(&partial);
        return Err(register_io("publish rootfs", destination.to_owned(), error));
    }
    std::fs::remove_file(&partial)
        .map_err(|error| register_io("remove rootfs staging", partial, error))?;
    if let Some(parent) = destination.parent() {
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| register_io("sync rootfs directory", parent.to_owned(), error))?;
    }
    Ok(PublishedRootfs {
        path: destination.to_owned(),
        owned: true,
    })
}

fn register_io(operation: &'static str, path: PathBuf, source: io::Error) -> ResolveError {
    ResolveError::RegisterIo {
        operation,
        path,
        source,
    }
}
