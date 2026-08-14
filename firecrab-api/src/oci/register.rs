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
    let image_root = templates.image_root_path();
    let relative = rootfs_relative(named.alias());
    let destination = image_root.join(&relative);
    let source = named.image().rootfs().path();
    let published = publish_rootfs(source, &destination)?;
    let spec = TemplateSpec {
        alias: named.alias().to_owned(),
        version: named.version().to_owned(),
        kernel: named.image().kernel().to_path_buf(),
        initrd: named.image().initrd().map(Path::to_path_buf),
        rootfs: relative.clone(),
        boot_args: named.image().boot_args().to_owned(),
    };
    if let Err(error) = templates.register_spec(spec) {
        published.revert();
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
    if let Err(error) = std::fs::rename(&partial, destination) {
        let _ = std::fs::remove_file(&partial);
        let _ = std::fs::remove_file(destination);
        return Err(register_io("publish rootfs", destination.to_owned(), error));
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
