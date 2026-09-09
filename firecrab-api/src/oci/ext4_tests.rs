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

/// merge.rs's OCI layer extraction is unprivileged (`preserve_ownerships(false)`),
/// so a fixture tree here — like a real merged layer tree — is owned by
/// whichever uid runs the test, never root. `mkfs.ext4 -d` copies that
/// ownership verbatim unless `run_mkfs` corrects it, and a guest that boots
/// with non-root-owned directories fails any package postinst that checks
/// for that (systemd 261's "unsafe path transition", issue #250).
#[tokio::test]
async fn packed_inodes_are_root_owned_even_though_the_source_tree_is_not() {
    let directory = tempdir().expect("create fixture directory");
    let tree = tree_with_payload(directory.path(), 4096);
    let destination = directory.path().join("rootfs.ext4");

    let source_uid = std::fs::metadata(tree.join("app/payload"))
        .expect("stat fixture file")
        .uid();
    assert_ne!(
        source_uid, 0,
        "fixture must start non-root-owned to exercise the fix; test is running as root"
    );

    ext4::write_provisioned_ext4(&provisioned(tree), &destination)
        .await
        .expect("pack tree");

    let output = std::process::Command::new("debugfs")
        .args(["-R", "stat /app/payload"])
        .arg(&destination)
        .output()
        .expect("run debugfs stat");
    let stat = String::from_utf8_lossy(&output.stdout);
    let tokens: Vec<&str> = stat.split_whitespace().collect();
    let field = |name: &str| {
        tokens
            .iter()
            .position(|t| *t == name)
            .map(|i| tokens[i + 1])
            .unwrap_or_else(|| panic!("debugfs stat missing {name} field: {stat}"))
    };
    assert_eq!(
        field("User:"),
        "0",
        "packed file must be root-owned: {stat}"
    );
    assert_eq!(
        field("Group:"),
        "0",
        "packed file must be root-owned: {stat}"
    );
}

#[test]
fn run_mkfs_reports_a_readable_error_when_fakeroot_chown_cannot_reach_the_tree() {
    let directory = tempdir().expect("create fixture directory");
    let missing_tree = directory.path().join("no-such-tree");
    let image = directory.path().join("image.ext4");
    let destination = directory.path().join("rootfs.ext4");
    std::fs::write(&image, []).expect("create empty image placeholder");

    let error = ext4::run_mkfs(&missing_tree, &image, &destination)
        .expect_err("chown -R over a nonexistent tree must fail");
    assert_matches!(error, ResolveError::Ext4Build { ref detail, .. } if detail.contains("fakeroot chown"), "{error}");
}

#[test]
fn run_mkfs_reports_a_readable_error_when_mkfs_ext4_itself_fails() {
    let directory = tempdir().expect("create fixture directory");
    let tree = tree_with_payload(directory.path(), 1024);
    // mkfs.ext4 cannot create its output under a parent that does not exist.
    let image = directory.path().join("no-such-dir/image.ext4");
    let destination = directory.path().join("rootfs.ext4");

    let error =
        ext4::run_mkfs(&tree, &image, &destination).expect_err("mkfs.ext4 must fail to run");
    assert_matches!(error, ResolveError::Ext4Build { .. }, "{error}");
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
