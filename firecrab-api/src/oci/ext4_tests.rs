use super::*;

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
