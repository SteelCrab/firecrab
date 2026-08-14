use super::*;

use crate::templates::{TemplateRegistry, TemplateSpec};

fn parse(reference: &str) -> ImageReference {
    ImageReference::parse(reference).expect(reference)
}

fn name_of(reference: &str) -> OciTemplateName {
    name::template_name_from_reference(&parse(reference)).expect(reference)
}

#[test]
fn a_docker_hub_official_tag_becomes_name_plus_tag() {
    let named = name_of("nginx:1.27");
    assert_eq!(named.alias, "nginx-1.27");
    assert_eq!(named.version, "1.27");
}

#[test]
fn a_bare_name_uses_the_implicit_latest_tag() {
    let named = name_of("nginx");
    assert_eq!(named.alias, "nginx-latest");
    assert_eq!(named.version, "latest");
}

#[test]
fn a_user_repository_replaces_slashes() {
    let named = name_of("myuser/app:v2");
    assert_eq!(named.alias, "myuser-app-v2");
    assert_eq!(named.version, "v2");
}

#[test]
fn docker_hub_library_is_not_part_of_the_alias() {
    let named = name_of("docker.io/library/alpine:3.24");
    assert_eq!(named.alias, "alpine-3.24");
    assert_eq!(named.version, "3.24");
}

#[test]
fn a_private_registry_stays_in_the_alias() {
    let named = name_of("ghcr.io/owner/repo:1.0");
    assert_eq!(named.alias, "ghcr.io-owner-repo-1.0");
    assert_eq!(named.version, "1.0");
}

#[test]
fn a_registry_port_does_not_break_the_alias() {
    let named = name_of("localhost:5000/app:v1");
    assert_eq!(named.alias, "localhost-5000-app-v1");
    assert_eq!(named.version, "v1");
}

#[test]
fn a_digest_pin_uses_a_short_hash_in_the_alias_and_the_full_digest_as_version() {
    let digest = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let named = name_of(&format!("nginx@{digest}"));
    assert_eq!(named.alias, "nginx-sha256-0123456789ab");
    assert_eq!(named.version, digest);
}

#[test]
fn an_unusable_reference_is_refused_instead_of_minting_an_empty_alias() {
    let reference = ImageReference {
        registry: DOCKER_HUB_REGISTRY.to_owned(),
        repository: String::new(),
        version: ImageVersion::Tag(String::new()),
    };
    let error = name::template_name_from_reference(&reference).expect_err("empty name");
    assert!(matches!(error, ResolveError::AliasUnusable { .. }));
}

#[test]
fn alias_letters_are_lowercased() {
    let named = name_of("myuser/app:Release");
    assert_eq!(named.alias, "myuser-app-release");
    assert_eq!(named.version, "Release");
}

fn registry_with(alias: &str) -> (tempfile::TempDir, TemplateRegistry) {
    let directory = tempfile::tempdir().expect("create fixture directory");
    let root = directory.path();
    std::fs::create_dir_all(root.join("kernel")).expect("create kernel dir");
    std::fs::write(root.join("kernel/vmlinux"), b"kernel").expect("write kernel");
    std::fs::write(root.join("rootfs.ext4"), b"rootfs").expect("write rootfs");
    let registry = TemplateRegistry::from_specs(
        root,
        [TemplateSpec {
            alias: alias.to_owned(),
            version: "installed".to_owned(),
            kernel: std::path::PathBuf::from("kernel/vmlinux"),
            initrd: None,
            rootfs: std::path::PathBuf::from("rootfs.ext4"),
            boot_args: "console=ttyS0".to_owned(),
        }],
    )
    .expect("register fixture alias");
    (directory, registry)
}

fn empty_registry() -> (tempfile::TempDir, TemplateRegistry) {
    let directory = tempfile::tempdir().expect("create fixture directory");
    let registry = TemplateRegistry::from_specs(directory.path(), []).expect("empty registry");
    (directory, registry)
}

#[test]
fn claiming_rejects_an_alias_that_is_already_installed() {
    let (_keep, templates) = registry_with("nginx-1.27");
    let error =
        name::claim_template_name(&parse("nginx:1.27"), &templates).expect_err("installed alias");
    match error {
        ResolveError::AliasCollision { alias, occupant } => {
            assert_eq!(alias, "nginx-1.27");
            assert_eq!(occupant, "nginx-1.27");
        }
        other => panic!("expected AliasCollision, got {other}"),
    }
    assert!(templates.resolve_alias("nginx-1.27").is_some());
}

#[test]
fn claiming_rejects_a_catalog_alias_even_when_it_is_not_installed() {
    let (_keep, templates) = empty_registry();
    assert!(templates.resolve_alias("ubuntu-26.04").is_none());

    let error =
        name::claim_template_name(&parse("ubuntu:26.04"), &templates).expect_err("catalog alias");
    match error {
        ResolveError::AliasCollision { alias, occupant } => {
            assert_eq!(alias, "ubuntu-26.04");
            assert_eq!(occupant, "ubuntu-26.04");
        }
        other => panic!("expected AliasCollision, got {other}"),
    }
}

#[test]
fn claiming_accepts_a_free_alias_and_does_not_register_it() {
    let (_keep, templates) = empty_registry();
    let named = name::claim_template_name(&parse("nginx:1.27"), &templates).expect("free alias");
    assert_eq!(named.alias, "nginx-1.27");
    assert_eq!(named.version, "1.27");
    assert!(templates.resolve_alias("nginx-1.27").is_none());
}

#[test]
fn naming_a_bootable_image_records_the_alias_and_leaves_the_pair_in_place() {
    let (_keep, templates) = empty_registry();
    let image = OciBootableImage {
        rootfs: OciExt4Image {
            path: std::path::PathBuf::from("/tmp/oci.ext4"),
            size_bytes: 8 * 1024 * 1024,
            payload_bytes: 64,
            free_bytes: 1024 * 1024,
            toolbox: Sha256Digest::of_bytes(b"toolbox-fixture"),
        },
        kernel: std::path::PathBuf::from("kernel/vmlinux-ubuntu-26.04-x86_64"),
        initrd: None,
        boot_args: "console=ttyS0".to_owned(),
        architecture: Architecture::HOST,
    };

    let named = name_oci_image(image, &parse("nginx:1.27"), &templates).expect("name image");

    assert_eq!(named.alias(), "nginx-1.27");
    assert_eq!(named.version(), "1.27");
    assert_eq!(
        named.image().kernel().as_os_str(),
        "kernel/vmlinux-ubuntu-26.04-x86_64"
    );
    assert!(templates.resolve_alias("nginx-1.27").is_none());
}
