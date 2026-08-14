use super::*;

use std::io::Cursor;

use tar::{Builder, EntryType, Header};
use tempfile::{TempDir, tempdir};

fn header(entry_type: EntryType, size: u64) -> Header {
    let mut header = Header::new_gnu();
    header.set_entry_type(entry_type);
    header.set_mode(0o644);
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
) {
    let mut header = header(entry_type, data.len() as u64);
    if let Some(target) = target {
        header
            .set_link_name_literal(target.as_bytes())
            .expect("set fixture link target");
    }
    builder
        .append_data(&mut header, path, Cursor::new(data))
        .expect("append fixture tar entry");
}

fn finish(builder: Builder<Vec<u8>>) -> Vec<u8> {
    builder.into_inner().expect("finish fixture tar")
}

fn one_entry_tar(path: &str, entry_type: EntryType, target: Option<&str>, data: &[u8]) -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());
    append_entry(&mut builder, path, entry_type, target, data);
    finish(builder)
}

fn raw_path_tar(path: &str) -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());
    append_raw_entry(&mut builder, path, EntryType::Regular, &[]);
    finish(builder)
}

fn append_raw_path(builder: &mut Builder<Vec<u8>>, path: &str) {
    append_raw_entry(builder, path, EntryType::Regular, &[]);
}

fn append_raw_entry(
    builder: &mut Builder<Vec<u8>>,
    path: &str,
    entry_type: EntryType,
    data: &[u8],
) {
    let mut raw_header = header(entry_type, data.len() as u64);
    let name = &mut raw_header.as_old_mut().name;
    assert!(path.len() < name.len(), "fixture path fits the raw header");
    name[..path.len()].copy_from_slice(path.as_bytes());
    raw_header.set_cksum();
    builder
        .append(&raw_header, Cursor::new(data))
        .expect("append raw fixture path");
}

fn pax_record(key: &str, value: &str) -> Vec<u8> {
    let value = format!(" {key}={value}\n");
    let mut length = value.len() + 1;
    loop {
        let actual = length.to_string().len() + value.len();
        if actual == length {
            return format!("{length}{value}").into_bytes();
        }
        length = actual;
    }
}

fn append_pax_metadata(builder: &mut Builder<Vec<u8>>, bytes: &[u8]) {
    append_entry(
        builder,
        "PaxHeaders.X/member",
        EntryType::XHeader,
        None,
        bytes,
    );
}

fn pax_override_tar(
    key: &str,
    value: &str,
    entry_type: EntryType,
    target: Option<&str>,
) -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());
    append_pax_metadata(&mut builder, &pax_record(key, value));
    append_entry(&mut builder, "safe/member", entry_type, target, &[]);
    finish(builder)
}

fn gnu_override_tar(
    extension_type: EntryType,
    value: &str,
    entry_type: EntryType,
    target: Option<&str>,
) -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());
    let mut metadata = value.as_bytes().to_vec();
    metadata.push(0);
    append_entry(
        &mut builder,
        "././@LongLink",
        extension_type,
        None,
        &metadata,
    );
    append_entry(&mut builder, "safe/member", entry_type, target, &[]);
    finish(builder)
}

fn gnu_and_pax_linkpath_tar(gnu_target: &str, pax_target: &str) -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());
    let mut metadata = gnu_target.as_bytes().to_vec();
    metadata.push(0);
    append_entry(
        &mut builder,
        "././@LongLink",
        EntryType::GNULongLink,
        None,
        &metadata,
    );
    append_pax_metadata(&mut builder, &pax_record("linkpath", pax_target));
    append_entry(
        &mut builder,
        "safe/member",
        EntryType::Symlink,
        Some("header-safe"),
        &[],
    );
    finish(builder)
}

fn oversized_metadata_tar() -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());
    let metadata = vec![b'a'; (TAR_METADATA_MAX_BYTES + 1) as usize];
    append_pax_metadata(&mut builder, &metadata);
    finish(builder)
}

fn extended_sparse_tar() -> Vec<u8> {
    let mut header = header(EntryType::GNUSparse, 0);
    header.set_path("var/sparse").expect("set sparse path");
    header
        .as_gnu_mut()
        .expect("fixture uses a GNU header")
        .set_is_extended(true);
    header.set_cksum();
    let mut builder = Builder::new(Vec::new());
    builder
        .append(&header, Cursor::new(Vec::<u8>::new()))
        .expect("append sparse header");
    finish(builder)
}

fn pax_records_tar(records: &[(&str, &str)], entry_type: EntryType) -> Vec<u8> {
    let mut metadata = Vec::new();
    for (key, value) in records {
        metadata.extend(pax_record(key, value));
    }
    let mut builder = Builder::new(Vec::new());
    append_pax_metadata(&mut builder, &metadata);
    append_entry(&mut builder, "safe/member", entry_type, None, &[]);
    finish(builder)
}

fn global_pax_tar(key: &str, value: &str) -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());
    append_entry(
        &mut builder,
        "GlobalHead.0",
        EntryType::XGlobalHeader,
        None,
        &pax_record(key, value),
    );
    append_entry(&mut builder, "safe/member", EntryType::Regular, None, &[]);
    finish(builder)
}

fn fixture_layer(directory: &TempDir, name: &str, bytes: &[u8]) -> DecompressedLayer {
    let compressed_bytes = format!("compressed fixture {name}").into_bytes();
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

#[tokio::test]
async fn regular_entries_links_and_whiteouts_pass_preflight_in_manifest_order() {
    let mut first_builder = Builder::new(Vec::new());
    append_raw_entry(
        &mut first_builder,
        "/tmp/GlobalHead.0",
        EntryType::XGlobalHeader,
        &pax_record("mtime", "1.0"),
    );
    append_entry(&mut first_builder, "etc/", EntryType::Directory, None, &[]);
    append_entry(
        &mut first_builder,
        "bin/busybox",
        EntryType::Regular,
        None,
        b"binary",
    );
    append_entry(
        &mut first_builder,
        "bin/sh",
        EntryType::Symlink,
        Some("/bin/busybox"),
        &[],
    );
    append_entry(
        &mut first_builder,
        "usr/bin/tool",
        EntryType::Symlink,
        Some("../../bin/busybox"),
        &[],
    );
    append_entry(
        &mut first_builder,
        "bin/busybox-copy",
        EntryType::Link,
        Some("bin/busybox"),
        &[],
    );
    append_entry(
        &mut first_builder,
        "etc/.wh.removed",
        EntryType::Regular,
        None,
        &[],
    );
    append_entry(
        &mut first_builder,
        "var/lib/data/.wh..wh..opq",
        EntryType::Regular,
        None,
        &[],
    );
    let first_tar = finish(first_builder);
    let second_tar = one_entry_tar("./usr/share/message", EntryType::Regular, None, b"hello");
    let directory = tempdir().expect("create fixture directory");
    let first = fixture_layer(&directory, "first", &first_tar);
    let second = fixture_layer(&directory, "second", &second_tar);

    let validated =
        validate_decompressed_layers(vec![first.clone(), second.clone(), first.clone()])
            .await
            .expect("validate ordinary OCI layer members");

    assert_eq!(validated.len(), 3);
    assert_eq!(validated[0].source(), &first.source);
    assert_eq!(validated[1].diff_id(), &second.diff_id);
    assert_eq!(validated[2].path(), first.path.as_path());
    assert_eq!(validated[2].size(), first.size);
}

#[tokio::test]
async fn traversal_absolute_and_pax_overridden_paths_are_rejected() {
    let directory = tempdir().expect("create fixture directory");
    let mut late_traversal_builder = Builder::new(Vec::new());
    append_entry(
        &mut late_traversal_builder,
        "safe/first",
        EntryType::Regular,
        None,
        b"safe",
    );
    append_raw_path(&mut late_traversal_builder, "../../late-escape");
    let late_traversal = finish(late_traversal_builder);
    let fixtures = [
        (
            "parent",
            raw_path_tar("../escape"),
            PathBuf::from("../escape"),
        ),
        (
            "nested-parent",
            raw_path_tar("etc/../../escape"),
            PathBuf::from("etc/../../escape"),
        ),
        (
            "absolute",
            raw_path_tar("/absolute/path"),
            PathBuf::from("/absolute/path"),
        ),
        (
            "pax-parent",
            pax_override_tar("path", "../pax-escape", EntryType::Regular, None),
            PathBuf::from("../pax-escape"),
        ),
        (
            "gnu-parent",
            gnu_override_tar(
                EntryType::GNULongName,
                "../../gnu-escape",
                EntryType::Regular,
                None,
            ),
            PathBuf::from("../../gnu-escape"),
        ),
        (
            "late-parent",
            late_traversal,
            PathBuf::from("../../late-escape"),
        ),
    ];

    for (name, bytes, expected_path) in fixtures {
        let layer = fixture_layer(&directory, name, &bytes);
        let expected_digest = layer.source.descriptor.digest.clone();
        let error = validate_decompressed_layers(vec![layer])
            .await
            .expect_err("unsafe member path must fail preflight");
        assert!(matches!(
            error,
            ResolveError::UnsafeTarMember {
                compressed_digest,
                path,
                reason: TarMemberViolation::Path,
            } if compressed_digest == expected_digest && path == expected_path
        ));
    }

    let safe = fixture_layer(
        &directory,
        "safe-layer",
        &one_entry_tar("safe/first", EntryType::Regular, None, b"safe"),
    );
    let unsafe_layer = fixture_layer(
        &directory,
        "unsafe-second-layer",
        &raw_path_tar("../../second-layer-escape"),
    );
    let expected_digest = unsafe_layer.source.descriptor.digest.clone();
    let error = validate_decompressed_layers(vec![safe, unsafe_layer])
        .await
        .expect_err("a later unsafe layer must fail the complete image preflight");
    assert!(matches!(
        error,
        ResolveError::UnsafeTarMember { compressed_digest, path, .. }
            if compressed_digest == expected_digest && path == Path::new("../../second-layer-escape")
    ));
}

#[tokio::test]
async fn parser_differential_pax_overrides_are_explicitly_rejected() {
    let directory = tempdir().expect("create fixture directory");
    for (name, bytes, expected_key) in [
        (
            "local-size",
            pax_override_tar("size", "512", EntryType::Regular, None),
            "size",
        ),
        (
            "global-path",
            global_pax_tar("path", "../../global-escape"),
            "path",
        ),
        (
            "global-linkpath",
            global_pax_tar("linkpath", "../../global-link"),
            "linkpath",
        ),
        ("global-size", global_pax_tar("size", "512"), "size"),
    ] {
        let layer = fixture_layer(&directory, name, &bytes);
        let error = validate_decompressed_layers(vec![layer])
            .await
            .expect_err("ambiguous PAX parser override must fail preflight");
        assert!(matches!(
            error,
            ResolveError::UnsafeTarMember {
                reason: TarMemberViolation::UnsupportedPaxAttribute { key },
                ..
            } if key == expected_key
        ));
    }
}

#[tokio::test]
async fn device_and_other_special_nodes_are_rejected() {
    let directory = tempdir().expect("create fixture directory");
    for (name, entry_type, expected_reason) in [
        (
            "fifo",
            EntryType::Fifo,
            TarMemberViolation::UnsupportedEntryType { entry_type: b'6' },
        ),
        (
            "unknown",
            EntryType::new(b'9'),
            TarMemberViolation::UnsupportedEntryType { entry_type: b'9' },
        ),
    ] {
        let bytes = one_entry_tar("dev/unsafe", entry_type, None, &[]);
        let layer = fixture_layer(&directory, name, &bytes);
        let error = validate_decompressed_layers(vec![layer])
            .await
            .expect_err("special filesystem node must fail preflight");
        assert!(matches!(
            error,
            ResolveError::UnsafeTarMember { path, reason, .. }
                if path == Path::new("dev/unsafe") && reason == expected_reason
        ));
    }

    let sparse = fixture_layer(&directory, "sparse", &extended_sparse_tar());
    let error = validate_decompressed_layers(vec![sparse])
        .await
        .expect_err("GNU sparse parsing must be rejected in the raw preflight");
    assert!(matches!(
        error,
        ResolveError::UnsafeTarMember {
            path,
            reason: TarMemberViolation::UnsupportedEntryType { entry_type: b'S' },
            ..
        } if path == Path::new("var/sparse")
    ));

    let sparse_pax = pax_override_tar("GNU.sparse.name", "../../outside", EntryType::Regular, None);
    let sparse_pax = fixture_layer(&directory, "sparse-pax", &sparse_pax);
    let error = validate_decompressed_layers(vec![sparse_pax])
        .await
        .expect_err("GNU sparse PAX metadata must be rejected");
    assert!(matches!(
        error,
        ResolveError::UnsafeTarMember {
            reason: TarMemberViolation::UnsupportedPaxAttribute { key },
            ..
        } if key == "GNU.sparse.name"
    ));
}

#[tokio::test]
async fn character_and_block_devices_are_skipped_and_keep_later_members_aligned() {
    let directory = tempdir().expect("create fixture directory");
    let mut builder = Builder::new(Vec::new());
    append_entry(
        &mut builder,
        "dev/console",
        EntryType::Char,
        None,
        b"ignored-char-payload",
    );
    append_entry(
        &mut builder,
        "dev/sda",
        EntryType::Block,
        None,
        b"ignored-block-payload",
    );
    append_entry(
        &mut builder,
        "etc/os-release",
        EntryType::Regular,
        None,
        b"ID=linux\n",
    );
    let layer = fixture_layer(&directory, "skip-devices", &finish(builder));
    let validated = validate_decompressed_layers(vec![layer])
        .await
        .expect("character and block devices must be skipped during preflight");
    assert_eq!(validated.len(), 1);
}

#[tokio::test]
async fn invalid_link_targets_are_rejected() {
    let directory = tempdir().expect("create fixture directory");
    let fixtures = [
        (
            "missing",
            one_entry_tar("usr/bin/hard", EntryType::Link, None, &[]),
            PathBuf::from("usr/bin/hard"),
            TarMemberViolation::MissingHardlinkTarget,
        ),
        (
            "missing-symlink",
            one_entry_tar("usr/bin/symlink", EntryType::Symlink, None, &[]),
            PathBuf::from("usr/bin/symlink"),
            TarMemberViolation::MissingSymlinkTarget,
        ),
        (
            "empty-pax-symlink",
            pax_override_tar("linkpath", "", EntryType::Symlink, Some("safe")),
            PathBuf::from("safe/member"),
            TarMemberViolation::MissingSymlinkTarget,
        ),
        (
            "nul-symlink",
            pax_override_tar("linkpath", "safe\0suffix", EntryType::Symlink, Some("safe")),
            PathBuf::from("safe/member"),
            TarMemberViolation::InvalidSymlinkTarget,
        ),
        (
            "gnu-pax-nul-symlink",
            gnu_and_pax_linkpath_tar("gnu-safe", "safe\0suffix"),
            PathBuf::from("safe/member"),
            TarMemberViolation::InvalidSymlinkTarget,
        ),
        (
            "parent",
            one_entry_tar("usr/bin/hard", EntryType::Link, Some("../outside"), &[]),
            PathBuf::from("usr/bin/hard"),
            TarMemberViolation::HardlinkTarget {
                target: PathBuf::from("../outside"),
            },
        ),
        (
            "absolute",
            one_entry_tar("usr/bin/hard", EntryType::Link, Some("/etc/shadow"), &[]),
            PathBuf::from("usr/bin/hard"),
            TarMemberViolation::HardlinkTarget {
                target: PathBuf::from("/etc/shadow"),
            },
        ),
        (
            "pax-linkpath",
            pax_override_tar(
                "linkpath",
                "../../outside",
                EntryType::Link,
                Some("safe/target"),
            ),
            PathBuf::from("safe/member"),
            TarMemberViolation::HardlinkTarget {
                target: PathBuf::from("../../outside"),
            },
        ),
        (
            "gnu-longlink",
            gnu_override_tar(
                EntryType::GNULongLink,
                "../../gnu-outside",
                EntryType::Link,
                Some("safe/target"),
            ),
            PathBuf::from("safe/member"),
            TarMemberViolation::HardlinkTarget {
                target: PathBuf::from("../../gnu-outside"),
            },
        ),
    ];

    for (name, bytes, expected_path, expected_reason) in fixtures {
        let layer = fixture_layer(&directory, name, &bytes);
        let error = validate_decompressed_layers(vec![layer])
            .await
            .expect_err("unsafe hard link must fail preflight");
        match error {
            ResolveError::UnsafeTarMember { path, reason, .. } => {
                assert_eq!(path, expected_path);
                assert_eq!(reason, expected_reason);
            }
            other => panic!("unexpected hard link error: {other}"),
        }
    }
}

#[tokio::test]
async fn malformed_pax_checksum_and_truncated_payload_are_rejected() {
    let directory = tempdir().expect("create fixture directory");

    let mut malformed_pax_builder = Builder::new(Vec::new());
    append_pax_metadata(&mut malformed_pax_builder, b"99 path=../escape\n");
    append_entry(
        &mut malformed_pax_builder,
        "safe/member",
        EntryType::Regular,
        None,
        &[],
    );
    let malformed_pax = finish(malformed_pax_builder);

    let mut bad_checksum = one_entry_tar("file", EntryType::Regular, None, b"data");
    bad_checksum[0] ^= 1;

    let mut truncated = one_entry_tar("file", EntryType::Regular, None, b"payload");
    truncated.truncate(512 + 3);

    let mut missing_terminator = one_entry_tar("file", EntryType::Regular, None, b"payload");
    missing_terminator.truncate(1024);

    let mut concatenated = one_entry_tar("safe", EntryType::Regular, None, &[]);
    concatenated.extend(raw_path_tar("../../hidden-after-zero"));

    let duplicate_path = pax_records_tar(
        &[("path", "safe/member"), ("path", "../../last-wins")],
        EntryType::Regular,
    );
    for (name, bytes) in [
        ("pax", malformed_pax),
        ("checksum", bad_checksum),
        ("truncated", truncated),
        ("missing-terminator", missing_terminator),
        ("concatenated", concatenated),
        ("duplicate-path", duplicate_path),
    ] {
        let layer = fixture_layer(&directory, name, &bytes);
        let expected_digest = layer.source.descriptor.digest.clone();
        let error = validate_decompressed_layers(vec![layer])
            .await
            .expect_err("malformed tar must fail preflight");
        assert!(matches!(
            error,
            ResolveError::MalformedLayerArchive { compressed_digest, .. }
                if compressed_digest == expected_digest
        ));
    }

    let oversized = fixture_layer(&directory, "oversized-metadata", &oversized_metadata_tar());
    let error = validate_decompressed_layers(vec![oversized])
        .await
        .expect_err("oversized metadata must fail before the parser buffers it");
    assert!(matches!(
        error,
        ResolveError::MalformedLayerArchive { message, .. }
            if message.contains("exceeding")
    ));
}

#[tokio::test]
async fn rejected_member_keeps_verified_blob_and_layer_cache_bytes() {
    let directory = tempdir().expect("create fixture directory");
    let bytes = raw_path_tar("../../outside");
    let layer = fixture_layer(&directory, "retained", &bytes);
    let compressed_before = std::fs::read(&layer.source.path).expect("read compressed fixture");
    let tar_before = std::fs::read(&layer.path).expect("read layer fixture");
    let compressed_path = layer.source.path.clone();
    let tar_path = layer.path.clone();

    validate_decompressed_layers(vec![layer])
        .await
        .expect_err("unsafe tar must fail preflight");

    assert_eq!(
        std::fs::read(compressed_path).expect("compressed cache remains"),
        compressed_before
    );
    assert_eq!(
        std::fs::read(tar_path).expect("layer cache remains"),
        tar_before
    );
    for entry in std::fs::read_dir(directory.path()).expect("read fixture directory") {
        let name = entry
            .expect("read fixture entry")
            .file_name()
            .to_string_lossy()
            .into_owned();
        assert!(!name.ends_with(".partial"), "unexpected partial {name}");
    }
}
