use super::*;

use crate::templates::TemplateRegistry;

fn write_file(path: &std::path::Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create fixture parent");
    }
    std::fs::write(path, bytes).expect("write fixture file");
}

fn empty_registry() -> (tempfile::TempDir, TemplateRegistry) {
    let directory = tempfile::tempdir().expect("create fixture directory");
    let registry = TemplateRegistry::from_specs(directory.path(), []).expect("empty registry");
    (directory, registry)
}

fn named_image(image_root: &std::path::Path, alias: &str, ext4_bytes: &[u8]) -> NamedOciImage {
    let kernel = std::path::PathBuf::from("kernel/vmlinux-ubuntu-26.04-x86_64");
    write_file(&image_root.join(&kernel), b"unclassified-kernel");
    let ext4 = image_root.join("scratch/packed.ext4");
    write_file(&ext4, ext4_bytes);
    NamedOciImage {
        image: OciBootableImage {
            rootfs: OciExt4Image {
                path: ext4,
                size_bytes: ext4_bytes.len() as u64,
                payload_bytes: ext4_bytes.len() as u64,
                free_bytes: 1024 * 1024,
                toolbox: Sha256Digest::of_bytes(b"toolbox-fixture"),
            },
            kernel,
            initrd: None,
            boot_args: "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw".to_owned(),
            architecture: Architecture::HOST,
        },
        alias: alias.to_owned(),
        version: "1.27".to_owned(),
    }
}

#[test]
fn registering_publishes_the_ext4_and_pins_the_alias() {
    let (_keep, templates) = empty_registry();
    let named = named_image(templates.image_root_path(), "nginx-1.27", b"ext4-bytes");

    let registered = register_named_oci_image(named, &templates).expect("register named image");

    assert_eq!(registered.alias(), "nginx-1.27");
    assert_eq!(registered.version(), "1.27");
    assert_eq!(registered.rootfs().as_os_str(), "rootfs/nginx-1.27.ext4");
    let dest = templates.image_root_path().join("rootfs/nginx-1.27.ext4");
    assert_eq!(
        std::fs::read(&dest).expect("published rootfs"),
        b"ext4-bytes"
    );
    let template = templates
        .resolve_alias("nginx-1.27")
        .expect("alias is installed");
    assert_eq!(template.version, "1.27");
    assert_eq!(
        template.rootfs.relative_path().as_os_str(),
        "rootfs/nginx-1.27.ext4"
    );
    assert_eq!(
        template.kernel.relative_path().as_os_str(),
        "kernel/vmlinux-ubuntu-26.04-x86_64"
    );
    assert!(template.initrd.is_none());
    assert!(template.boot_args.contains("root=/dev/vda"));
}

#[test]
fn registering_leaves_the_source_ext4_in_place() {
    let (_keep, templates) = empty_registry();
    let named = named_image(templates.image_root_path(), "nginx-1.27", b"ext4-bytes");
    let source = named.image().rootfs().path().to_path_buf();

    register_named_oci_image(named, &templates).expect("register");

    assert_eq!(
        std::fs::read(&source).expect("source remains"),
        b"ext4-bytes"
    );
}

#[test]
fn registering_refuses_to_overwrite_an_existing_rootfs() {
    let (_keep, templates) = empty_registry();
    let dest = templates.image_root_path().join("rootfs/nginx-1.27.ext4");
    write_file(&dest, b"already-there");
    let named = named_image(templates.image_root_path(), "nginx-1.27", b"new-bytes");

    let error = register_named_oci_image(named, &templates).expect_err("existing dest");

    assert!(matches!(
        error,
        ResolveError::RegisterDestinationExists { .. }
    ));
    assert_eq!(
        std::fs::read(&dest).expect("existing dest"),
        b"already-there"
    );
    assert!(templates.resolve_alias("nginx-1.27").is_none());
}

#[test]
fn a_failed_registration_removes_the_partial_rootfs() {
    let (_keep, templates) = empty_registry();
    let named = named_image(templates.image_root_path(), "nginx-1.27", b"ext4-bytes");
    std::fs::remove_file(templates.image_root_path().join(named.image().kernel()))
        .expect("hide the kernel so register_spec fails");

    let error = register_named_oci_image(named, &templates).expect_err("missing kernel");

    assert!(matches!(error, ResolveError::RegisterFailed { .. }));
    assert!(
        !templates
            .image_root_path()
            .join("rootfs/nginx-1.27.ext4")
            .exists()
    );
    assert!(
        !templates
            .image_root_path()
            .join("rootfs/nginx-1.27.ext4.partial")
            .exists()
    );
    assert!(templates.resolve_alias("nginx-1.27").is_none());
}

#[test]
fn a_failed_registration_does_not_delete_the_source_ext4() {
    let (_keep, templates) = empty_registry();
    let named = named_image(templates.image_root_path(), "nginx-1.27", b"ext4-bytes");
    let source = named.image().rootfs().path().to_path_buf();
    std::fs::remove_file(templates.image_root_path().join(named.image().kernel()))
        .expect("hide the kernel");

    let _ = register_named_oci_image(named, &templates).expect_err("missing kernel");

    assert_eq!(
        std::fs::read(&source).expect("source remains"),
        b"ext4-bytes"
    );
}
