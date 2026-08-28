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

// Keep the compliance edge-case suite inside the binary crate so llvm-cov
// attributes execution to the production `oci/compliance.rs` module.
mod compliance_coverage {
    use super::super::compliance::{
        GeneratedSbom, generate_spdx, remove_bundle, write_spdx_bundle,
    };
    use crate::image_install::Architecture;
    use rusqlite::{Connection, params};
    use serde_json::Value;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use std::path::Path;

    const RPMTAG_NAME: u32 = 1000;
    const RPMTAG_VERSION: u32 = 1001;
    const RPMTAG_RELEASE: u32 = 1002;
    const RPMTAG_EPOCH: u32 = 1003;
    const RPMTAG_LICENSE: u32 = 1014;
    const RPMTAG_ARCH: u32 = 1022;
    const RPMTAG_SOURCERPM: u32 = 1044;
    const RPM_INT32_TYPE: u32 = 4;
    const RPM_STRING_TYPE: u32 = 6;

    #[test]
    fn rpm_sqlite_database_generates_spdx_and_deduplicates() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("usr/lib/sysimage/rpm/rpmdb.sqlite");
        fs::create_dir_all(database.parent().unwrap()).unwrap();

        let connection = Connection::open(&database).unwrap();
        connection
            .execute(
                "CREATE TABLE Packages (hnum INTEGER PRIMARY KEY, blob BLOB NOT NULL)",
                [],
            )
            .unwrap();

        let blob = rpm_fixture(&[
            (RPMTAG_NAME, RPM_STRING_TYPE, b"bash\0".to_vec(), 1),
            (RPMTAG_VERSION, RPM_STRING_TYPE, b"5.2\0".to_vec(), 1),
            (RPMTAG_RELEASE, RPM_STRING_TYPE, b"1.fc42\0".to_vec(), 1),
            (RPMTAG_ARCH, RPM_STRING_TYPE, b"aarch64\0".to_vec(), 1),
            (
                RPMTAG_LICENSE,
                RPM_STRING_TYPE,
                b"GPL-3.0-or-later\0".to_vec(),
                1,
            ),
            (
                RPMTAG_SOURCERPM,
                RPM_STRING_TYPE,
                b"bash-5.2-1.fc42.src.rpm\0".to_vec(),
                1,
            ),
            (
                RPMTAG_EPOCH,
                RPM_INT32_TYPE,
                2_u32.to_be_bytes().to_vec(),
                1,
            ),
        ]);
        connection
            .execute(
                "INSERT INTO Packages (hnum, blob) VALUES (?1, ?2)",
                params![1_i64, blob.clone()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO Packages (hnum, blob) VALUES (?1, ?2)",
                params![2_i64, blob],
            )
            .unwrap();
        drop(connection);

        fs::create_dir_all(directory.path().join("etc")).unwrap();
        fs::write(directory.path().join("etc/os-release"), "ID='fedora'\n").unwrap();

        let generated = generate_spdx(directory.path(), "fedora-test", "42", Architecture::Aarch64)
            .unwrap()
            .unwrap();

        assert_eq!(generated.package_manager, "rpm");
        assert_eq!(generated.package_count, 1);
        let json: Value = serde_json::from_slice(&generated.bytes).unwrap();
        assert_eq!(json["packages"][1]["name"], "bash");
        assert_eq!(json["packages"][1]["versionInfo"], "2:5.2-1.fc42");
        assert!(
            json["packages"][1]["comment"]
                .as_str()
                .unwrap()
                .contains("source-package=bash-5.2-1.fc42.src.rpm")
        );
        assert!(
            json["packages"][0]["comment"]
                .as_str()
                .unwrap()
                .contains("distribution=fedora; architecture=aarch64; package-manager=rpm")
        );
    }

    #[test]
    fn rpm_header_defaults_cover_optional_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("var/lib/rpm/rpmdb.sqlite");
        create_rpm_database(
            &database,
            vec![rpm_fixture(&[
                (RPMTAG_NAME, RPM_STRING_TYPE, b"minimal\0".to_vec(), 1),
                (RPMTAG_VERSION, RPM_STRING_TYPE, b"1.0\0".to_vec(), 1),
            ])],
        );

        let generated = generate_spdx(directory.path(), "minimal", "latest", Architecture::X86_64)
            .unwrap()
            .unwrap();

        let json: Value = serde_json::from_slice(&generated.bytes).unwrap();
        assert_eq!(json["packages"][1]["versionInfo"], "1.0");
        assert!(json["packages"][1].get("comment").is_none());
        assert!(
            json["packages"][0]["comment"]
                .as_str()
                .unwrap()
                .contains("distribution=rpm")
        );
    }

    #[test]
    fn rpm_database_reports_invalid_sqlite_and_header_data() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("var/lib/rpm/rpmdb.sqlite");
        fs::create_dir_all(database.parent().unwrap()).unwrap();
        fs::write(&database, b"not a sqlite database").unwrap();

        let error =
            generate_spdx(directory.path(), "broken", "latest", Architecture::X86_64).unwrap_err();
        assert!(error.contains("RPM database"));

        fs::remove_file(&database).unwrap();
        create_rpm_database(&database, vec![vec![0_u8; 64]]);
        let error = generate_spdx(
            directory.path(),
            "broken-header",
            "latest",
            Architecture::X86_64,
        )
        .unwrap_err();
        assert!(error.contains("missing a package header magic"));
    }

    #[test]
    fn rpm_header_reports_truncated_store_and_invalid_utf8() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("var/lib/rpm/rpmdb.sqlite");

        let mut truncated = vec![0x8e, 0xad, 0xe8, 1, 0, 0, 0, 0];
        truncated.extend_from_slice(&0_u32.to_be_bytes());
        truncated.extend_from_slice(&64_u32.to_be_bytes());
        create_rpm_database(&database, vec![truncated]);
        let error = generate_spdx(
            directory.path(),
            "truncated",
            "latest",
            Architecture::X86_64,
        )
        .unwrap_err();
        assert!(error.contains("data store is truncated"));

        fs::remove_file(&database).unwrap();
        create_rpm_database(
            &database,
            vec![rpm_fixture(&[
                (RPMTAG_NAME, RPM_STRING_TYPE, vec![0xff, 0], 1),
                (RPMTAG_VERSION, RPM_STRING_TYPE, b"1\0".to_vec(), 1),
            ])],
        );
        let error = generate_spdx(
            directory.path(),
            "invalid-utf8",
            "latest",
            Architecture::X86_64,
        )
        .unwrap_err();
        assert!(error.contains("RPM string is not UTF-8"));
    }

    #[test]
    fn apk_defaults_and_empty_database_error_are_exercised() {
        let directory = tempfile::tempdir().unwrap();
        let db = directory.path().join("lib/apk/db");
        fs::create_dir_all(&db).unwrap();
        fs::write(
            db.join("installed"),
            "not-a-field\nP:tiny\nV:1\nZ:ignored\n\nP:missing-version\nA:x86_64\n",
        )
        .unwrap();

        let generated = generate_spdx(directory.path(), "tiny", "latest", Architecture::X86_64)
            .unwrap()
            .unwrap();
        assert_eq!(generated.package_count, 1);
        let json: Value = serde_json::from_slice(&generated.bytes).unwrap();
        assert!(
            json["packages"][1]["comment"]
                .as_str()
                .unwrap()
                .contains("source-package=tiny")
        );

        fs::write(db.join("installed"), "P:no-version\n").unwrap();
        let error =
            generate_spdx(directory.path(), "empty", "latest", Architecture::X86_64).unwrap_err();
        assert!(error.contains("contained no installed packages"));
    }

    #[test]
    fn dpkg_parser_handles_continuations_and_incomplete_records() {
        let directory = tempfile::tempdir().unwrap();
        let db = directory.path().join("var/lib/dpkg");
        fs::create_dir_all(&db).unwrap();
        fs::write(
            db.join("status"),
            concat!(
                "Package: curl\n",
                "Status: install ok installed\n",
                "Version: 8.0\n",
                "Description: first line\n",
                " continuation line\n\n",
                "Status: install ok installed\n",
                "Version: 1\n\n",
                "Package: missing-version\n",
                "Status: install ok installed\n\n",
                "Package: removed\n",
                "Status: deinstall ok config-files\n",
                "Version: 2\n\n",
                "this line has no colon\n"
            ),
        )
        .unwrap();

        let generated = generate_spdx(directory.path(), "debian-test", "sid", Architecture::X86_64)
            .unwrap()
            .unwrap();
        assert_eq!(generated.package_manager, "dpkg");
        assert_eq!(generated.package_count, 1);
        let json: Value = serde_json::from_slice(&generated.bytes).unwrap();
        assert!(
            json["packages"][1]["comment"]
                .as_str()
                .unwrap()
                .contains("source-package=curl")
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_package_database_is_not_followed() {
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir_all(outside.path().join("apk/db")).unwrap();
        fs::write(outside.path().join("apk/db/installed"), "P:escape\nV:1\n").unwrap();
        fs::create_dir_all(directory.path()).unwrap();
        symlink(outside.path(), directory.path().join("lib")).unwrap();

        assert!(
            generate_spdx(directory.path(), "escape", "latest", Architecture::X86_64,)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn bundle_can_be_replaced_and_removed() {
        let directory = tempfile::tempdir().unwrap();
        let first = GeneratedSbom {
            bytes: b"{\"generation\":1}\n".to_vec(),
            package_manager: "apk",
            package_count: 1,
        };
        let second = GeneratedSbom {
            bytes: b"{\"generation\":2}\n".to_vec(),
            package_manager: "apk",
            package_count: 1,
        };

        let path = write_spdx_bundle(directory.path(), "replace-me", Architecture::X86_64, &first)
            .unwrap();
        write_spdx_bundle(
            directory.path(),
            "replace-me",
            Architecture::X86_64,
            &second,
        )
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), second.bytes);

        remove_bundle(directory.path(), "replace-me", Architecture::X86_64);
        assert!(!path.parent().unwrap().exists());
        remove_bundle(directory.path(), "replace-me", Architecture::X86_64);
    }

    fn create_rpm_database(path: &Path, blobs: Vec<Vec<u8>>) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let connection = Connection::open(path).unwrap();
        connection
            .execute(
                "CREATE TABLE Packages (hnum INTEGER PRIMARY KEY, blob BLOB NOT NULL)",
                [],
            )
            .unwrap();
        for (index, blob) in blobs.into_iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO Packages (hnum, blob) VALUES (?1, ?2)",
                    params![index as i64 + 1, blob],
                )
                .unwrap();
        }
    }

    fn rpm_fixture(entries: &[(u32, u32, Vec<u8>, u32)]) -> Vec<u8> {
        let mut indexes = Vec::new();
        let mut store = Vec::new();
        for (tag, kind, value, count) in entries {
            indexes.extend_from_slice(&tag.to_be_bytes());
            indexes.extend_from_slice(&kind.to_be_bytes());
            indexes.extend_from_slice(&(store.len() as u32).to_be_bytes());
            indexes.extend_from_slice(&count.to_be_bytes());
            store.extend_from_slice(value);
        }
        let mut header = vec![0x8e, 0xad, 0xe8, 1, 0, 0, 0, 0];
        header.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        header.extend_from_slice(&(store.len() as u32).to_be_bytes());
        header.extend_from_slice(&indexes);
        header.extend_from_slice(&store);
        header
    }
}
