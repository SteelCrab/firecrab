use super::*;
use core::assert_matches;

use std::os::unix::fs::MetadataExt as _;
use tempfile::tempdir;

fn write_file(path: &std::path::Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create fixture parent");
    }
    std::fs::write(path, bytes).expect("write fixture file");
}

#[test]
fn an_empty_tree_plans_the_minimum_headroom_image() {
    assert_eq!(ext4::plan_ext4_size(0), 32 * 1024 * 1024);
}

#[test]
fn a_one_megabyte_payload_adds_metadata_and_headroom_then_aligns() {
    let payload = 1024 * 1024;
    // 1 MiB + 25% + 32 MiB = 33.25 MiB → 34 MiB
    assert_eq!(ext4::plan_ext4_size(payload), 34 * 1024 * 1024);
}

#[test]
fn a_hundred_megabyte_payload_is_not_a_fixed_two_gigabyte_image() {
    let payload = 100 * 1024 * 1024;
    let size = ext4::plan_ext4_size(payload);
    assert_eq!(size, 157 * 1024 * 1024);
    assert!(size < 2 * 1024 * 1024 * 1024);
    assert!(size >= payload + ext4::HEADROOM_BYTES);
}

#[tokio::test]
async fn measuring_a_tree_counts_regular_bytes_and_symlink_targets() {
    let directory = tempdir().expect("create fixture directory");
    let tree = directory.path().join("tree");
    write_file(&tree.join("bin/app"), &vec![0_u8; 1000]);
    std::os::unix::fs::symlink("app", tree.join("bin/alias")).expect("symlink");

    let payload = ext4::measure_tree_payload(&tree).expect("measure tree");
    assert_eq!(payload, 1000 + 3); // "app"
}

#[tokio::test]
async fn measuring_a_tree_counts_hard_links_once() {
    let directory = tempdir().expect("create fixture directory");
    let tree = directory.path().join("tree");
    write_file(&tree.join("share/data"), &vec![0_u8; 4096]);
    std::fs::hard_link(tree.join("share/data"), tree.join("share/copy")).expect("hard link");

    let payload = ext4::measure_tree_payload(&tree).expect("measure tree");
    assert_eq!(payload, 4096);
    assert!(std::fs::hard_link(tree.join("share/data"), tree.join("share/copy")).is_err());
    let meta = std::fs::symlink_metadata(tree.join("share/data")).unwrap();
    assert!(meta.nlink() >= 2);
}

fn provisioned(tree: std::path::PathBuf) -> ProvisionedRootfs {
    ProvisionedRootfs {
        path: tree,
        toolbox: Sha256Digest::of_bytes(b"toolbox-fixture"),
    }
}

fn tree_with_payload(root: &std::path::Path, bytes: usize) -> std::path::PathBuf {
    tree_with_filled_payload(root, bytes, 0)
}

fn tree_with_filled_payload(root: &std::path::Path, bytes: usize, fill: u8) -> std::path::PathBuf {
    let tree = root.join("tree");
    write_file(&tree.join("app/payload"), &vec![fill; bytes]);
    tree
}

#[tokio::test]
async fn packing_a_small_tree_leaves_required_headroom() {
    let directory = tempdir().expect("create fixture directory");
    let tree = tree_with_payload(directory.path(), 64 * 1024);
    let destination = directory.path().join("rootfs.ext4");
    let image = ext4::write_provisioned_ext4(&provisioned(tree.clone()), &destination)
        .await
        .expect("pack a small tree");

    assert_eq!(image.path(), destination.as_path());
    assert_eq!(image.payload_bytes(), 64 * 1024);
    assert_eq!(image.size_bytes(), ext4::plan_ext4_size(64 * 1024));
    assert!(
        image.free_bytes() >= ext4::MIN_FREE_AFTER_PACK,
        "packed image must not be full: {} free",
        image.free_bytes()
    );
    assert!(image.free_bytes() > image.payload_bytes());
    assert_eq!(
        image.toolbox_digest(),
        &Sha256Digest::of_bytes(b"toolbox-fixture")
    );
    assert!(destination.is_file());
    assert_eq!(
        std::fs::metadata(&destination).expect("stat image").len(),
        image.size_bytes()
    );
    // Source tree is the caller's and must survive packing.
    assert_eq!(
        std::fs::read(tree.join("app/payload")).unwrap().len(),
        64 * 1024
    );
}

#[tokio::test]
async fn packing_refuses_to_overwrite_an_existing_destination() {
    let directory = tempdir().expect("create fixture directory");
    let tree = tree_with_payload(directory.path(), 1024);
    let destination = directory.path().join("rootfs.ext4");
    std::fs::write(&destination, b"already here").expect("seed destination");

    let error = ext4::write_provisioned_ext4(&provisioned(tree), &destination)
        .await
        .expect_err("existing destination must fail");
    assert_matches!(error, ResolveError::Ext4DestinationExists { .. });
    assert_eq!(std::fs::read(&destination).unwrap(), b"already here");
}

#[tokio::test]
async fn an_undersized_image_fails_and_leaves_no_destination() {
    let directory = tempdir().expect("create fixture directory");
    let tree = tree_with_filled_payload(directory.path(), 2 * 1024 * 1024, 0xA5);
    let destination = directory.path().join("rootfs.ext4");

    let error =
        ext4::write_provisioned_ext4_with_size(&provisioned(tree), &destination, 3 * 1024 * 1024)
            .await
            .expect_err("a 3 MiB image cannot hold 2 MiB plus headroom");

    assert_matches!(
        error,
        ResolveError::Ext4Full { .. } | ResolveError::Ext4Build { .. },
        "undersize must fail loudly, got {error}"
    );
    assert!(
        !destination.exists(),
        "must not publish a full or failed image"
    );
    let leftovers: Vec<_> = std::fs::read_dir(directory.path())
        .unwrap()
        .filter_map(|entry| {
            let name = entry.ok()?.file_name();
            let name = name.to_string_lossy();
            name.contains("partial").then_some(name.into_owned())
        })
        .collect();
    assert!(
        leftovers.is_empty(),
        "partial images must be removed: {leftovers:?}"
    );
}

#[tokio::test]
async fn a_planned_image_over_the_operator_ceiling_fails_before_mkfs() {
    let directory = tempdir().expect("create fixture directory");
    let tree = tree_with_payload(directory.path(), 1024);
    let destination = directory.path().join("rootfs.ext4");

    let error =
        ext4::write_provisioned_ext4_with_limit(&provisioned(tree), &destination, 1024 * 1024)
            .await
            .expect_err("1 MiB ceiling cannot fit the planned image");
    assert_matches!(error,
            ResolveError::Ext4TooLarge { limit, .. } if limit == 1024 * 1024, "expected Ext4TooLarge with 1 MiB limit, got {error}");
    assert!(!destination.exists());
}

#[tokio::test]
async fn ext4_refusals_render_operator_readable_messages() {
    let rendered = [
        ResolveError::Ext4Full {
            path: std::path::PathBuf::from("/images/rootfs.ext4"),
            size_bytes: 8 * 1024 * 1024,
            free_bytes: 0,
            required_bytes: ext4::MIN_FREE_AFTER_PACK,
        },
        ResolveError::Ext4TooLarge {
            path: std::path::PathBuf::from("/images/rootfs.ext4"),
            size_bytes: 40 * 1024 * 1024 * 1024,
            limit: ext4::DEFAULT_MAX_ROOTFS_BYTES,
        },
        ResolveError::Ext4Build {
            path: std::path::PathBuf::from("/images/rootfs.ext4"),
            detail: "No space left on device".to_owned(),
        },
        ResolveError::Ext4Cancelled {
            path: std::path::PathBuf::from("/images/rootfs.ext4"),
        },
    ]
    .map(|error| error.to_string());

    assert!(rendered[0].contains("is full after packing"));
    assert!(rendered[0].contains("1048576 required"));
    assert!(rendered[1].contains("exceeding the"));
    assert!(rendered[2].contains("No space left on device"));
    assert!(rendered[3].contains("cancelled"));
}
