use super::*;

use std::io::Cursor;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

use tar::{Builder, EntryType, Header};
use tempfile::{TempDir, tempdir};

fn header(entry_type: EntryType, size: u64, mode: u32) -> Header {
    let mut header = Header::new_gnu();
    header.set_entry_type(entry_type);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(size);
    header
}

fn append_entry(
    builder: &mut Builder<Vec<u8>>,
    path: &str,
    entry_type: EntryType,
    target: Option<&str>,
    data: &[u8],
    mode: u32,
) {
    let mut header = header(entry_type, data.len() as u64, mode);
    if let Some(target) = target {
        header
            .set_link_name_literal(target.as_bytes())
            .expect("set fixture link target");
    }
    builder
        .append_data(&mut header, path, Cursor::new(data))
        .expect("append fixture tar entry");
}

fn append_raw_entry(
    builder: &mut Builder<Vec<u8>>,
    path: &str,
    entry_type: EntryType,
    data: &[u8],
    mode: u32,
) {
    let mut header = header(entry_type, data.len() as u64, mode);
    let name = &mut header.as_old_mut().name;
    assert!(path.len() < name.len(), "fixture path fits raw tar header");
    name[..path.len()].copy_from_slice(path.as_bytes());
    header.set_cksum();
    builder
        .append(&header, Cursor::new(data))
        .expect("append raw fixture tar entry");
}

fn finish(builder: Builder<Vec<u8>>) -> Vec<u8> {
    builder.into_inner().expect("finish fixture tar")
}

fn layer(directory: &TempDir, name: &str, bytes: &[u8]) -> DecompressedLayer {
    let compressed_bytes = format!("compressed merge fixture {name}").into_bytes();
    let compressed_digest = Sha256Digest::of_bytes(&compressed_bytes);
    let compressed_path = directory.path().join(format!("{name}.blob"));
    std::fs::write(&compressed_path, &compressed_bytes).expect("write compressed fixture");
    let path = directory.path().join(format!("{name}.tar"));
    std::fs::write(&path, bytes).expect("write tar fixture");
    DecompressedLayer {
        source: CachedBlob {
            descriptor: Descriptor {
                media_type: OCI_LAYER_MEDIA_TYPE.to_owned(),
                digest: compressed_digest,
                size: compressed_bytes.len() as u64,
            },
            path: compressed_path,
        },
        diff_id: Sha256Digest::of_bytes(bytes),
        path,
        size: bytes.len() as u64,
    }
}

async fn validate(layers: Vec<DecompressedLayer>) -> Vec<ValidatedLayer> {
    validate_decompressed_layers(layers)
        .await
        .expect("validate merge fixtures")
}

fn assert_no_partial_trees(parent: &Path) {
    for entry in std::fs::read_dir(parent).expect("read merge parent") {
        let name = entry
            .expect("read merge parent entry")
            .file_name()
            .to_string_lossy()
            .into_owned();
        assert!(
            !name.starts_with(".firecrab-oci-merge-") || !name.ends_with(".partial"),
            "unexpected partial merge tree {name}"
        );
    }
}

#[tokio::test]
async fn layers_merge_in_manifest_order_and_replace_path_types() {
    let directory = tempdir().expect("create fixture directory");
    let mut base = Builder::new(Vec::new());
    append_raw_entry(&mut base, "./", EntryType::Directory, &[], 0o1750);
    append_entry(&mut base, "etc/", EntryType::Directory, None, &[], 0o750);
    append_entry(
        &mut base,
        "etc/version",
        EntryType::Regular,
        None,
        b"base",
        0o644,
    );
    append_entry(
        &mut base,
        "etc/lower-kept",
        EntryType::Regular,
        None,
        b"kept",
        0o644,
    );
    append_entry(
        &mut base,
        "file-to-dir",
        EntryType::Regular,
        None,
        b"old file",
        0o644,
    );
    append_entry(
        &mut base,
        "dir-to-file/",
        EntryType::Directory,
        None,
        &[],
        0o755,
    );
    append_entry(
        &mut base,
        "dir-to-file/old",
        EntryType::Regular,
        None,
        b"old child",
        0o644,
    );
    append_entry(&mut base, "tmp/", EntryType::Directory, None, &[], 0o1777);
    let base = finish(base);

    let mut top = Builder::new(Vec::new());
    append_entry(&mut top, "etc/", EntryType::Directory, None, &[], 0o700);
    append_entry(
        &mut top,
        "etc/version",
        EntryType::Regular,
        None,
        b"top",
        0o600,
    );
    append_entry(
        &mut top,
        "file-to-dir/",
        EntryType::Directory,
        None,
        &[],
        0o711,
    );
    append_entry(
        &mut top,
        "file-to-dir/new",
        EntryType::Regular,
        None,
        b"new child",
        0o644,
    );
    append_entry(
        &mut top,
        "dir-to-file",
        EntryType::Regular,
        None,
        b"new file",
        0o644,
    );
    append_entry(
        &mut top,
        "bin/copy",
        EntryType::Link,
        Some("bin/target"),
        &[],
        0o644,
    );
    append_entry(
        &mut top,
        "bin/target",
        EntryType::Regular,
        None,
        b"binary",
        0o4755,
    );
    append_entry(
        &mut top,
        "bin/current",
        EntryType::Symlink,
        Some("target"),
        &[],
        0o777,
    );
    append_entry(
        &mut top,
        "bin/chain-a",
        EntryType::Link,
        Some("bin/chain-b"),
        &[],
        0o644,
    );
    append_entry(
        &mut top,
        "bin/chain-b",
        EntryType::Link,
        Some("bin/target"),
        &[],
        0o644,
    );
    let top = finish(top);
    let layers = validate(vec![
        layer(&directory, "ordered-base", &base),
        layer(&directory, "ordered-top", &top),
    ])
    .await;
    let destination = directory.path().join("rootfs");

    let merged = merge_validated_layers(&layers, &destination)
        .await
        .expect("merge ordered layers");

    assert_eq!(merged.path(), destination);
    assert_eq!(
        std::fs::read(destination.join("etc/version")).unwrap(),
        b"top"
    );
    assert_eq!(
        std::fs::read(destination.join("file-to-dir/new")).unwrap(),
        b"new child"
    );
    assert_eq!(
        std::fs::read(destination.join("etc/lower-kept")).unwrap(),
        b"kept"
    );
    assert_eq!(
        std::fs::metadata(destination.join("etc"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o700
    );
    assert_eq!(
        std::fs::read(destination.join("dir-to-file")).unwrap(),
        b"new file"
    );
    assert!(!destination.join("dir-to-file/old").exists());
    assert_eq!(
        std::fs::read_link(destination.join("bin/current")).unwrap(),
        Path::new("target")
    );
    let target = std::fs::metadata(destination.join("bin/target")).unwrap();
    let copy = std::fs::metadata(destination.join("bin/copy")).unwrap();
    assert_eq!(target.ino(), copy.ino());
    assert_eq!(target.permissions().mode() & 0o7777, 0o755);
    assert_eq!(
        target.ino(),
        std::fs::metadata(destination.join("bin/chain-a"))
            .unwrap()
            .ino()
    );
    assert_eq!(
        std::fs::metadata(destination.join("file-to-dir"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o711
    );
    assert_eq!(
        std::fs::metadata(&destination)
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o1750
    );
    assert_eq!(std::fs::metadata(&destination).unwrap().mtime(), 0);
    assert_eq!(
        std::fs::metadata(destination.join("tmp"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o1777
    );
}

#[tokio::test]
async fn whiteouts_affect_only_lower_entries_and_are_never_materialized() {
    let directory = tempdir().expect("create fixture directory");
    let mut base = Builder::new(Vec::new());
    append_entry(
        &mut base,
        "etc/value",
        EntryType::Regular,
        None,
        b"lower",
        0o644,
    );
    append_entry(
        &mut base,
        "var/gone/child",
        EntryType::Regular,
        None,
        b"remove me",
        0o644,
    );
    append_entry(
        &mut base,
        "var/keep",
        EntryType::Regular,
        None,
        b"keep me",
        0o644,
    );
    let base = finish(base);

    let mut top = Builder::new(Vec::new());
    append_entry(
        &mut top,
        "etc/value",
        EntryType::Regular,
        None,
        b"same-layer replacement",
        0o644,
    );
    append_entry(
        &mut top,
        "etc/.wh.value",
        EntryType::Regular,
        None,
        &[],
        0o000,
    );
    append_entry(
        &mut top,
        "var/.wh.gone",
        EntryType::Regular,
        None,
        &[],
        0o000,
    );
    append_entry(
        &mut top,
        "var/.wh.missing",
        EntryType::Regular,
        None,
        &[],
        0o000,
    );
    let top = finish(top);
    let layers = validate(vec![
        layer(&directory, "whiteout-base", &base),
        layer(&directory, "whiteout-top", &top),
    ])
    .await;
    let destination = directory.path().join("rootfs");

    merge_validated_layers(&layers, &destination)
        .await
        .expect("apply explicit whiteouts");

    assert_eq!(
        std::fs::read(destination.join("etc/value")).unwrap(),
        b"same-layer replacement"
    );
    assert!(!destination.join("var/gone").exists());
    assert_eq!(
        std::fs::read(destination.join("var/keep")).unwrap(),
        b"keep me"
    );
    assert_eq!(
        std::fs::metadata(&destination)
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o755
    );
    assert!(!destination.join("etc/.wh.value").exists());
    assert!(!destination.join("var/.wh.gone").exists());
}

#[tokio::test]
async fn opaque_whiteouts_keep_current_layer_entries_regardless_of_tar_order() {
    let directory = tempdir().expect("create fixture directory");
    let mut base = Builder::new(Vec::new());
    append_entry(
        &mut base,
        "app/lower",
        EntryType::Regular,
        None,
        b"lower",
        0o644,
    );
    append_entry(
        &mut base,
        "app/nested/lower",
        EntryType::Regular,
        None,
        b"nested lower",
        0o644,
    );
    let base = finish(base);
    let mut top = Builder::new(Vec::new());
    append_entry(
        &mut top,
        "app/current",
        EntryType::Regular,
        None,
        b"current",
        0o644,
    );
    append_entry(
        &mut top,
        "app/nested/current",
        EntryType::Regular,
        None,
        b"nested current",
        0o644,
    );
    append_entry(
        &mut top,
        "app/.wh..wh..opq",
        EntryType::Regular,
        None,
        &[],
        0o000,
    );
    let top = finish(top);
    let layers = validate(vec![
        layer(&directory, "opaque-base", &base),
        layer(&directory, "opaque-top", &top),
    ])
    .await;
    let destination = directory.path().join("rootfs");

    merge_validated_layers(&layers, &destination)
        .await
        .expect("apply opaque whiteout");

    assert!(!destination.join("app/lower").exists());
    assert!(!destination.join("app/nested/lower").exists());
    assert_eq!(
        std::fs::read(destination.join("app/current")).unwrap(),
        b"current"
    );
    assert_eq!(
        std::fs::read(destination.join("app/nested/current")).unwrap(),
        b"nested current"
    );
    assert!(!destination.join("app/.wh..wh..opq").exists());

    let mut replacement_base = Builder::new(Vec::new());
    append_entry(
        &mut replacement_base,
        "replace",
        EntryType::Regular,
        None,
        b"lower file",
        0o644,
    );
    let replacement_base = finish(replacement_base);
    let mut replacement_top = Builder::new(Vec::new());
    append_entry(
        &mut replacement_top,
        "replace/",
        EntryType::Directory,
        None,
        &[],
        0o755,
    );
    append_entry(
        &mut replacement_top,
        "replace/.wh..wh..opq",
        EntryType::Regular,
        None,
        &[],
        0o000,
    );
    append_entry(
        &mut replacement_top,
        "replace/current",
        EntryType::Regular,
        None,
        b"current",
        0o644,
    );
    let replacement_top = finish(replacement_top);
    let replacement_layers = validate(vec![
        layer(&directory, "opaque-file-base", &replacement_base),
        layer(&directory, "opaque-file-top", &replacement_top),
    ])
    .await;
    let replacement_destination = directory.path().join("replacement-rootfs");
    merge_validated_layers(&replacement_layers, &replacement_destination)
        .await
        .expect("opaque directory may replace a lower non-directory");
    assert_eq!(
        std::fs::read(replacement_destination.join("replace/current")).unwrap(),
        b"current"
    );

    let mut root_base = Builder::new(Vec::new());
    append_entry(
        &mut root_base,
        "lower",
        EntryType::Regular,
        None,
        b"lower",
        0o644,
    );
    let root_base = finish(root_base);
    let mut root_top = Builder::new(Vec::new());
    append_entry(
        &mut root_top,
        "current",
        EntryType::Regular,
        None,
        b"current",
        0o644,
    );
    append_entry(
        &mut root_top,
        ".wh..wh..opq",
        EntryType::Regular,
        None,
        &[],
        0o000,
    );
    let root_top = finish(root_top);
    let root_layers = validate(vec![
        layer(&directory, "opaque-root-base", &root_base),
        layer(&directory, "opaque-root-top", &root_top),
    ])
    .await;
    let root_destination = directory.path().join("opaque-rootfs");
    merge_validated_layers(&root_layers, &root_destination)
        .await
        .expect("apply root opaque whiteout");
    assert!(!root_destination.join("lower").exists());
    assert_eq!(
        std::fs::read(root_destination.join("current")).unwrap(),
        b"current"
    );
}

#[tokio::test]
async fn invalid_whiteouts_and_ambiguous_paths_fail_without_publishing() {
    let directory = tempdir().expect("create fixture directory");
    let mut invalid_type = Builder::new(Vec::new());
    append_entry(
        &mut invalid_type,
        ".wh.victim",
        EntryType::Directory,
        None,
        &[],
        0o755,
    );
    let mut nonempty = Builder::new(Vec::new());
    append_entry(
        &mut nonempty,
        ".wh.victim",
        EntryType::Regular,
        None,
        b"x",
        0o000,
    );
    let mut duplicate = Builder::new(Vec::new());
    append_entry(
        &mut duplicate,
        "duplicate",
        EntryType::Regular,
        None,
        b"one",
        0o644,
    );
    append_raw_entry(
        &mut duplicate,
        "./duplicate",
        EntryType::Regular,
        b"two",
        0o644,
    );
    let mut conflict = Builder::new(Vec::new());
    append_entry(
        &mut conflict,
        "parent",
        EntryType::Regular,
        None,
        b"file",
        0o644,
    );
    let mut marker_conflict = Builder::new(Vec::new());
    append_entry(
        &mut marker_conflict,
        "parent/.wh.child",
        EntryType::Regular,
        None,
        &[],
        0o000,
    );
    append_entry(
        &mut marker_conflict,
        "parent/.wh.child/descendant",
        EntryType::Regular,
        None,
        b"ambiguous",
        0o644,
    );
    append_entry(
        &mut conflict,
        "parent/child",
        EntryType::Regular,
        None,
        b"child",
        0o644,
    );
    let mut missing_hardlink = Builder::new(Vec::new());
    append_entry(
        &mut missing_hardlink,
        "hardlink",
        EntryType::Link,
        Some("missing"),
        &[],
        0o644,
    );
    let mut directory_hardlink = Builder::new(Vec::new());
    append_entry(
        &mut directory_hardlink,
        "target/",
        EntryType::Directory,
        None,
        &[],
        0o755,
    );
    append_entry(
        &mut directory_hardlink,
        "hardlink",
        EntryType::Link,
        Some("target"),
        &[],
        0o644,
    );

    let fixtures = [
        (
            "empty-whiteout-target",
            {
                let mut builder = Builder::new(Vec::new());
                append_entry(&mut builder, ".wh.", EntryType::Regular, None, &[], 0o000);
                finish(builder)
            },
            TarMemberViolation::InvalidWhiteoutTarget,
        ),
        (
            "dot-whiteout-target",
            {
                let mut builder = Builder::new(Vec::new());
                append_entry(&mut builder, ".wh..", EntryType::Regular, None, &[], 0o000);
                finish(builder)
            },
            TarMemberViolation::InvalidWhiteoutTarget,
        ),
        (
            "dotdot-whiteout-target",
            {
                let mut builder = Builder::new(Vec::new());
                append_entry(&mut builder, ".wh...", EntryType::Regular, None, &[], 0o000);
                finish(builder)
            },
            TarMemberViolation::InvalidWhiteoutTarget,
        ),
        (
            "whiteout-type",
            finish(invalid_type),
            TarMemberViolation::InvalidWhiteoutType,
        ),
        (
            "whiteout-payload",
            finish(nonempty),
            TarMemberViolation::NonEmptyWhiteout { size: 1 },
        ),
        (
            "duplicate-path",
            finish(duplicate),
            TarMemberViolation::DuplicatePath,
        ),
        (
            "conflicting-path",
            finish(conflict),
            TarMemberViolation::ConflictingPath {
                descendant: PathBuf::from("parent/child"),
            },
        ),
        (
            "marker-conflicting-path",
            finish(marker_conflict),
            TarMemberViolation::ConflictingPath {
                descendant: PathBuf::from("parent/.wh.child/descendant"),
            },
        ),
        (
            "missing-hardlink",
            finish(missing_hardlink),
            TarMemberViolation::MissingMergedHardlinkTarget {
                target: PathBuf::from("missing"),
            },
        ),
        (
            "directory-hardlink",
            finish(directory_hardlink),
            TarMemberViolation::DirectoryHardlinkTarget {
                target: PathBuf::from("target"),
            },
        ),
    ];

    for (name, bytes, expected) in fixtures {
        let layers = validate(vec![layer(&directory, name, &bytes)]).await;
        let destination = directory.path().join(format!("{name}-rootfs"));
        let error = merge_validated_layers(&layers, &destination)
            .await
            .expect_err("invalid merge layer must fail");
        assert!(matches!(
            error,
            ResolveError::UnsafeTarMember { reason, .. } if reason == expected
        ));
        assert!(!destination.exists());
        assert_no_partial_trees(directory.path());
    }
}

#[tokio::test]
async fn lower_symlink_ancestors_cannot_write_or_whiteout_outside_the_tree() {
    let directory = tempdir().expect("create fixture directory");
    let outside = directory.path().join("outside");
    std::fs::create_dir(&outside).expect("create outside directory");
    std::fs::write(outside.join("sentinel"), b"unchanged").expect("write outside sentinel");
    let target = outside.to_string_lossy();
    let mut base = Builder::new(Vec::new());
    append_entry(
        &mut base,
        "escape",
        EntryType::Symlink,
        Some(&target),
        &[],
        0o777,
    );
    let base = finish(base);
    let upper_fixtures = [
        (
            "write-through-symlink",
            {
                let mut builder = Builder::new(Vec::new());
                append_entry(
                    &mut builder,
                    "escape/sentinel",
                    EntryType::Regular,
                    None,
                    b"changed",
                    0o644,
                );
                finish(builder)
            },
            false,
        ),
        (
            "whiteout-through-symlink",
            {
                let mut builder = Builder::new(Vec::new());
                append_entry(
                    &mut builder,
                    "escape/.wh.sentinel",
                    EntryType::Regular,
                    None,
                    &[],
                    0o000,
                );
                finish(builder)
            },
            true,
        ),
    ];

    for (name, upper, succeeds_as_noop) in upper_fixtures {
        let layers = validate(vec![
            layer(&directory, &format!("{name}-base"), &base),
            layer(&directory, &format!("{name}-top"), &upper),
        ])
        .await;
        let destination = directory.path().join(format!("{name}-rootfs"));
        let result = merge_validated_layers(&layers, &destination).await;
        if succeeds_as_noop {
            result.expect("a whiteout below a lower symlink is a lexical no-op");
            assert_eq!(
                std::fs::read_link(destination.join("escape")).unwrap(),
                Path::new(target.as_ref())
            );
        } else {
            let error = result.expect_err("ordinary writes must not traverse a lower symlink");
            assert!(matches!(
                error,
                ResolveError::UnsafeTarMember {
                    reason: TarMemberViolation::SymlinkAncestor { ancestor },
                    ..
                } if ancestor == Path::new("escape")
            ));
            assert!(!destination.exists());
        }
        assert_eq!(
            std::fs::read(outside.join("sentinel")).unwrap(),
            b"unchanged"
        );
        assert_no_partial_trees(directory.path());
    }
}

#[tokio::test]
async fn stale_validated_bytes_and_preexisting_destinations_are_not_published_over() {
    let directory = tempdir().expect("create fixture directory");
    let mut first = Builder::new(Vec::new());
    append_entry(
        &mut first,
        "value",
        EntryType::Regular,
        None,
        b"first",
        0o644,
    );
    let first = finish(first);
    let mut replacement = Builder::new(Vec::new());
    append_entry(
        &mut replacement,
        "value",
        EntryType::Regular,
        None,
        b"other",
        0o644,
    );
    let replacement = finish(replacement);
    assert_eq!(first.len(), replacement.len());
    let decompressed = layer(&directory, "stale", &first);
    let layer_path = decompressed.path.clone();
    let layers = validate(vec![decompressed]).await;
    std::fs::write(&layer_path, &replacement).expect("replace validated tar bytes");
    let destination = directory.path().join("stale-rootfs");

    let error = merge_validated_layers(&layers, &destination)
        .await
        .expect_err("changed validated bytes must be rehashed");
    assert!(matches!(error, ResolveError::DiffIdMismatch { .. }));
    assert!(!destination.exists());
    assert_no_partial_trees(directory.path());

    let existing = directory.path().join("existing-rootfs");
    std::fs::create_dir(&existing).expect("create existing destination");
    std::fs::write(existing.join("sentinel"), b"unchanged").unwrap();
    let error = merge_validated_layers(&[], &existing)
        .await
        .expect_err("existing destination must not be replaced");
    assert!(matches!(error, ResolveError::MergeDestinationExists { path } if path == existing));
    assert_eq!(
        std::fs::read(existing.join("sentinel")).unwrap(),
        b"unchanged"
    );
    assert_no_partial_trees(directory.path());
}

#[tokio::test]
async fn only_directory_entries_may_name_the_archive_root() {
    let directory = tempdir().expect("create fixture directory");
    let mut builder = Builder::new(Vec::new());
    append_raw_entry(
        &mut builder,
        "./",
        EntryType::Regular,
        b"not a directory",
        0o644,
    );
    let bytes = finish(builder);
    let error = validate_decompressed_layers(vec![layer(&directory, "root-file", &bytes)])
        .await
        .expect_err("an archive-root regular file must fail preflight");

    assert!(matches!(
        error,
        ResolveError::UnsafeTarMember {
            path,
            reason: TarMemberViolation::Path,
            ..
        } if path == Path::new(".")
    ));
}

#[tokio::test]
async fn merge_destinations_must_name_a_directory_below_a_parent() {
    let directory = tempdir().expect("create fixture directory");

    let error = merge_validated_layers(&[], Path::new("/"))
        .await
        .expect_err("a destination without a parent must fail");
    assert!(matches!(
        error,
        ResolveError::MergeIo { operation, path, .. }
            if operation == "resolve destination parent" && path == Path::new("/")
    ));

    let unnamed = directory.path().join("..");
    let error = merge_validated_layers(&[], &unnamed)
        .await
        .expect_err("a destination without a final component must fail");
    assert!(matches!(
        error,
        ResolveError::MergeIo { operation, path, .. }
            if operation == "resolve destination name" && path == unnamed
    ));
    assert_no_partial_trees(directory.path());
}

#[tokio::test]
async fn hard_link_targets_must_resolve_inside_the_merged_tree() {
    let directory = tempdir().expect("create fixture directory");
    let outside = directory.path().join("hardlink-outside");
    std::fs::create_dir(&outside).expect("create outside directory");
    std::fs::write(outside.join("sentinel"), b"unchanged").expect("write outside sentinel");

    let mut symlink_base = Builder::new(Vec::new());
    append_entry(
        &mut symlink_base,
        "escape",
        EntryType::Symlink,
        Some(&outside.to_string_lossy()),
        &[],
        0o777,
    );
    let mut symlink_top = Builder::new(Vec::new());
    append_entry(
        &mut symlink_top,
        "link",
        EntryType::Link,
        Some("escape/sentinel"),
        &[],
        0o644,
    );

    let mut file_base = Builder::new(Vec::new());
    append_entry(
        &mut file_base,
        "blocker",
        EntryType::Regular,
        None,
        b"file",
        0o644,
    );
    let mut file_top = Builder::new(Vec::new());
    append_entry(
        &mut file_top,
        "link",
        EntryType::Link,
        Some("blocker/inner"),
        &[],
        0o644,
    );

    let mut missing_parent = Builder::new(Vec::new());
    append_entry(
        &mut missing_parent,
        "link",
        EntryType::Link,
        Some("absent/target"),
        &[],
        0o644,
    );

    let fixtures = [
        (
            "hardlink-symlink-ancestor",
            Some(finish(symlink_base)),
            finish(symlink_top),
            TarMemberViolation::SymlinkAncestor {
                ancestor: PathBuf::from("escape"),
            },
        ),
        (
            "hardlink-file-ancestor",
            Some(finish(file_base)),
            finish(file_top),
            TarMemberViolation::NonDirectoryAncestor {
                ancestor: PathBuf::from("blocker"),
            },
        ),
        (
            "hardlink-missing-parent",
            None,
            finish(missing_parent),
            TarMemberViolation::MissingMergedHardlinkTarget {
                target: PathBuf::from("absent/target"),
            },
        ),
    ];

    for (name, base, top, expected) in fixtures {
        let mut decompressed = Vec::new();
        if let Some(base) = &base {
            decompressed.push(layer(&directory, &format!("{name}-base"), base));
        }
        decompressed.push(layer(&directory, &format!("{name}-top"), &top));
        let layers = validate(decompressed).await;
        let destination = directory.path().join(format!("{name}-rootfs"));
        let error = merge_validated_layers(&layers, &destination)
            .await
            .expect_err("an unresolvable hard link target must fail");
        assert!(matches!(
            error,
            ResolveError::UnsafeTarMember { reason, .. } if reason == expected
        ));
        assert!(!destination.exists());
        assert_no_partial_trees(directory.path());
    }

    assert_eq!(
        std::fs::read(outside.join("sentinel")).unwrap(),
        b"unchanged"
    );
}

#[tokio::test]
async fn whiteouts_below_missing_directories_are_lexical_no_ops() {
    let directory = tempdir().expect("create fixture directory");
    let mut base = Builder::new(Vec::new());
    append_entry(
        &mut base,
        "present/",
        EntryType::Directory,
        None,
        &[],
        0o755,
    );
    let base = finish(base);
    let mut removals = Builder::new(Vec::new());
    append_entry(
        &mut removals,
        "absent/gone/.wh.victim",
        EntryType::Regular,
        None,
        &[],
        0o000,
    );
    append_entry(
        &mut removals,
        "absent/hidden/.wh..wh..opq",
        EntryType::Regular,
        None,
        &[],
        0o000,
    );
    append_entry(
        &mut removals,
        "present/hidden/.wh..wh..opq",
        EntryType::Regular,
        None,
        &[],
        0o000,
    );
    let removals = finish(removals);

    let layers = validate(vec![
        layer(&directory, "whiteout-missing-base", &base),
        layer(&directory, "whiteout-missing-top", &removals),
    ])
    .await;
    let destination = directory.path().join("whiteout-missing-rootfs");
    let merged = merge_validated_layers(&layers, &destination)
        .await
        .expect("whiteouts without a lower directory must not fail the merge");

    assert_eq!(merged.path(), destination);
    assert!(destination.join("present").is_dir());
    assert!(!destination.join("absent").exists());
    assert!(!destination.join("present/hidden").exists());
    assert_no_partial_trees(directory.path());
}

#[tokio::test]
async fn members_may_not_be_written_beneath_a_lower_layer_file() {
    let directory = tempdir().expect("create fixture directory");
    let mut base = Builder::new(Vec::new());
    append_entry(
        &mut base,
        "blocker",
        EntryType::Regular,
        None,
        b"file",
        0o644,
    );
    let base = finish(base);
    let mut top = Builder::new(Vec::new());
    append_entry(
        &mut top,
        "blocker/child",
        EntryType::Regular,
        None,
        b"child",
        0o644,
    );
    let top = finish(top);

    let layers = validate(vec![
        layer(&directory, "nondirectory-ancestor-base", &base),
        layer(&directory, "nondirectory-ancestor-top", &top),
    ])
    .await;
    let destination = directory.path().join("nondirectory-ancestor-rootfs");
    let error = merge_validated_layers(&layers, &destination)
        .await
        .expect_err("a lower-layer file must not silently become a directory");

    assert!(matches!(
        error,
        ResolveError::UnsafeTarMember {
            reason: TarMemberViolation::NonDirectoryAncestor { ancestor },
            ..
        } if ancestor == Path::new("blocker")
    ));
    assert!(!destination.exists());
    assert_no_partial_trees(directory.path());
}

#[tokio::test]
async fn cache_entries_swapped_after_validation_are_rejected() {
    let directory = tempdir().expect("create fixture directory");
    let mut builder = Builder::new(Vec::new());
    append_entry(
        &mut builder,
        "value",
        EntryType::Regular,
        None,
        b"first",
        0o644,
    );
    let bytes = finish(builder);

    let decompressed = layer(&directory, "swapped-directory", &bytes);
    let layer_path = decompressed.path.clone();
    let layers = validate(vec![decompressed]).await;
    std::fs::remove_file(&layer_path).expect("remove validated tar");
    std::fs::create_dir(&layer_path).expect("replace validated tar with a directory");
    let destination = directory.path().join("swapped-directory-rootfs");
    let error = merge_validated_layers(&layers, &destination)
        .await
        .expect_err("a cache entry that is no longer a file must fail");
    assert!(matches!(
        error,
        ResolveError::MergeIo { operation, .. } if operation == "inspect validated layer"
    ));
    assert!(!destination.exists());
    assert_no_partial_trees(directory.path());

    let decompressed = layer(&directory, "swapped-size", &bytes);
    let layer_path = decompressed.path.clone();
    let declared_size = decompressed.size;
    let layers = validate(vec![decompressed]).await;
    std::fs::write(&layer_path, &bytes[..bytes.len() - 512]).expect("shrink validated tar");
    let destination = directory.path().join("swapped-size-rootfs");
    let error = merge_validated_layers(&layers, &destination)
        .await
        .expect_err("a resized cache entry must fail");
    assert!(matches!(
        error,
        ResolveError::SizeMismatch { expected, actual, .. }
            if expected == declared_size && actual == declared_size - 512
    ));
    assert!(!destination.exists());
    assert_no_partial_trees(directory.path());
}
