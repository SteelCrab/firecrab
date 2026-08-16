use super::*;
use core::assert_matches;

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_compression::tokio::bufread::GzipEncoder;
use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{Response, StatusCode, header::CONTENT_TYPE};
use axum::routing::get;
use tar::{Builder, EntryType, Header};
use tempfile::{TempDir, tempdir};
use tokio::task::JoinHandle;

/// `e_machine` for the architecture that is not this host's.
const FOREIGN_MACHINE: u16 = 0x00f3;

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

fn finish(builder: Builder<Vec<u8>>) -> Vec<u8> {
    builder.into_inner().expect("finish fixture tar")
}

/// Builds a 64-bit little-endian ELF image with the given program headers.
///
/// Real busybox is 1 MiB of machine code; every rule this stage enforces lives
/// in the header, so a hand-built one exercises the verifier exactly.
fn elf(machine: u16, e_type: u16, program_headers: &[[u8; 56]], trailer: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0_u8; 64];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2; // ELFCLASS64
    bytes[5] = 1; // ELFDATA2LSB
    bytes[6] = 1; // EI_VERSION
    bytes[16..18].copy_from_slice(&e_type.to_le_bytes());
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes[32..40].copy_from_slice(&64_u64.to_le_bytes()); // e_phoff
    bytes[52..54].copy_from_slice(&64_u16.to_le_bytes()); // e_ehsize
    bytes[54..56].copy_from_slice(&56_u16.to_le_bytes()); // e_phentsize
    bytes[56..58].copy_from_slice(&(program_headers.len() as u16).to_le_bytes());
    for entry in program_headers {
        bytes.extend_from_slice(entry);
    }
    bytes.extend_from_slice(trailer);
    bytes
}

/// One program header entry.
fn program_header(p_type: u32, p_offset: u64, p_filesz: u64) -> [u8; 56] {
    let mut entry = [0_u8; 56];
    entry[..4].copy_from_slice(&p_type.to_le_bytes());
    entry[8..16].copy_from_slice(&p_offset.to_le_bytes());
    entry[32..40].copy_from_slice(&p_filesz.to_le_bytes());
    entry
}

/// A static program for this host, carrying the applet names the guest calls.
fn static_program() -> Vec<u8> {
    let mut applets = vec![0_u8];
    for applet in ["sh", "ip", "udhcpc", "awk", "mount", "sleep", "cut"] {
        applets.extend_from_slice(applet.as_bytes());
        applets.push(0);
    }
    let machine = match Architecture::HOST {
        Architecture::X86_64 => 62,
        Architecture::Aarch64 => 183,
    };
    elf(machine, 2, &[program_header(1, 0, 0)], &applets)
}

/// Writes a program to disk and verifies it the way the pull path would.
async fn toolbox(directory: &TempDir, name: &str, bytes: &[u8]) -> ToolboxProgram {
    let path: PathBuf = directory.path().join(name);
    std::fs::write(&path, bytes).expect("write toolbox fixture");
    busybox::inspect_toolbox(&path, Architecture::HOST)
        .await
        .expect("verify toolbox fixture")
}

/// Rejection reason for a program that should not pass verification.
async fn refuse_toolbox(directory: &TempDir, name: &str, bytes: &[u8]) -> ToolboxViolation {
    let path = directory.path().join(name);
    std::fs::write(&path, bytes).expect("write toolbox fixture");
    match busybox::inspect_toolbox(&path, Architecture::HOST).await {
        Err(ResolveError::ToolboxUnusable { reason, .. }) => reason,
        other => panic!("expected a toolbox rejection, got {other:?}"),
    }
}

#[tokio::test]
async fn a_dynamically_linked_toolbox_program_is_refused() {
    let directory = tempdir().expect("create fixture directory");
    let interpreter = b"/lib64/ld-linux-x86-64.so.2\0";
    let machine = match Architecture::HOST {
        Architecture::X86_64 => 62,
        Architecture::Aarch64 => 183,
    };
    let program = elf(
        machine,
        2,
        &[program_header(3, 120, interpreter.len() as u64)],
        interpreter,
    );

    // A merged container tree has no loader, so this would exec into a panic.
    assert_matches!(refuse_toolbox(&directory, "dynamic", &program).await,
        ToolboxViolation::DynamicallyLinked { interpreter }
            if interpreter == "/lib64/ld-linux-x86-64.so.2");
}

#[tokio::test]
async fn a_toolbox_program_for_another_machine_is_refused() {
    let directory = tempdir().expect("create fixture directory");
    let program = elf(FOREIGN_MACHINE, 2, &[program_header(1, 0, 0)], &[]);
    assert_matches!(refuse_toolbox(&directory, "foreign", &program).await,
        ToolboxViolation::ForeignArchitecture { actual, .. } if actual == FOREIGN_MACHINE);
}

#[tokio::test]
async fn programs_that_are_not_static_executables_are_refused() {
    let directory = tempdir().expect("create fixture directory");
    let machine = match Architecture::HOST {
        Architecture::X86_64 => 62,
        Architecture::Aarch64 => 183,
    };

    assert_matches!(
        refuse_toolbox(&directory, "text", b"#!/bin/sh\necho hello\n").await,
        ToolboxViolation::NotElf
    );
    assert_matches!(
        refuse_toolbox(&directory, "empty", b"").await,
        ToolboxViolation::Empty
    );
    // ET_REL: an object file, not something the kernel can exec.
    assert_matches!(
        refuse_toolbox(&directory, "relocatable", &elf(machine, 1, &[], &[])).await,
        ToolboxViolation::NotExecutable
    );
    // A header table that claims more entries than the file can hold.
    let mut truncated = elf(machine, 2, &[program_header(1, 0, 0)], &[]);
    truncated[56..58].copy_from_slice(&64_u16.to_le_bytes());
    assert_matches!(
        refuse_toolbox(&directory, "truncated", &truncated).await,
        ToolboxViolation::MalformedProgramHeaders
    );
    // No program headers at all leaves nothing to prove staticness with.
    assert_matches!(
        refuse_toolbox(&directory, "headerless", &elf(machine, 2, &[], &[])).await,
        ToolboxViolation::MalformedProgramHeaders
    );
}

#[tokio::test]
async fn an_oversized_toolbox_program_is_refused_before_it_is_copied() {
    let directory = tempdir().expect("create fixture directory");
    let mut program = static_program();
    program.resize(33 * 1024 * 1024, 0);
    assert_matches!(refuse_toolbox(&directory, "huge", &program).await,
        ToolboxViolation::TooLarge { limit, .. } if limit == 32 * 1024 * 1024);
}

#[tokio::test]
async fn a_toolbox_directory_is_refused_rather_than_copied() {
    let directory = tempdir().expect("create fixture directory");
    let path = directory.path().join("a-directory");
    std::fs::create_dir(&path).expect("create fixture directory entry");
    assert_matches!(
        busybox::inspect_toolbox(&path, Architecture::HOST).await,
        Err(ResolveError::ToolboxUnusable {
            reason: ToolboxViolation::NotRegularFile,
            ..
        })
    );
}

#[tokio::test]
async fn toolbox_refusals_render_operator_readable_messages() {
    let rendered = [
        ResolveError::ToolboxMissing {
            reference: "registry/busybox@sha256:0".to_owned(),
            member: "bin/busybox",
        },
        ResolveError::ToolboxUnusable {
            path: PathBuf::from("/images/.oci/toolbox/busybox"),
            reason: ToolboxViolation::NotElf,
        },
    ]
    .map(|error| error.to_string());

    assert!(rendered[0].contains("bin/busybox"));
    assert!(rendered[1].contains("not a 64-bit little-endian ELF"));
}

// ---------------------------------------------------------------------------
// Toolbox acquisition against a local registry. Nothing here reaches Docker Hub.
// ---------------------------------------------------------------------------

const TOOLBOX_REPOSITORY: &str = "firecrab/toolbox";

#[derive(Clone)]
struct ToolboxRegistry {
    manifest: Arc<Vec<u8>>,
    blobs: Arc<BTreeMap<String, Arc<Vec<u8>>>>,
    requests: Arc<AtomicUsize>,
}

struct TestRegistry {
    registry: String,
    state: ToolboxRegistry,
    task: JoinHandle<()>,
}

impl Drop for TestRegistry {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl TestRegistry {
    /// Serves a one-layer image whose tar holds `member` at `bin/busybox`.
    async fn start(member: Option<&[u8]>) -> Self {
        let mut builder = Builder::new(Vec::new());
        append_entry(&mut builder, "bin/", EntryType::Directory, None, &[], 0o755);
        match member {
            Some(bytes) => append_entry(
                &mut builder,
                "bin/busybox",
                EntryType::Regular,
                None,
                bytes,
                0o755,
            ),
            None => append_entry(
                &mut builder,
                "bin/other",
                EntryType::Regular,
                None,
                b"not the program",
                0o755,
            ),
        }
        Self::start_from(finish(builder)).await
    }

    /// Serves a one-layer image from an explicit tar.
    async fn start_from(tar: Vec<u8>) -> Self {
        let diff_id = Sha256Digest::of_bytes(&tar);
        let compressed = {
            let input = tokio::io::BufReader::new(tar.as_slice());
            let mut encoder = GzipEncoder::new(input);
            let mut output = Vec::new();
            encoder
                .read_to_end(&mut output)
                .await
                .expect("gzip fixture layer");
            output
        };
        // The manifest digest covers the compressed blob while the config's
        // diff_id covers the tar, so the two are deliberately different here.
        let layer_digest = Sha256Digest::of_bytes(&compressed);
        let config = serde_json::to_vec(&serde_json::json!({
            "architecture": "amd64",
            "os": "linux",
            "rootfs": { "type": "layers", "diff_ids": [diff_id.to_string()] },
        }))
        .expect("serialize toolbox config");
        let config_digest = Sha256Digest::of_bytes(&config);
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": OCI_MANIFEST_MEDIA_TYPE,
            "config": {
                "mediaType": OCI_CONFIG_MEDIA_TYPE,
                "digest": config_digest.to_string(),
                "size": config.len(),
            },
            "layers": [{
                "mediaType": OCI_LAYER_GZIP_MEDIA_TYPE,
                "digest": layer_digest.to_string(),
                "size": compressed.len(),
            }],
        }))
        .expect("serialize toolbox manifest");

        let state = ToolboxRegistry {
            manifest: Arc::new(manifest),
            blobs: Arc::new(BTreeMap::from([
                (config_digest.to_string(), Arc::new(config)),
                (layer_digest.to_string(), Arc::new(compressed)),
            ])),
            requests: Arc::new(AtomicUsize::new(0)),
        };
        let app = axum::Router::new()
            .route(
                "/v2/firecrab/toolbox/manifests/{selector}",
                get(serve_manifest),
            )
            .route("/v2/firecrab/toolbox/blobs/{digest}", get(serve_blob))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind toolbox registry");
        let registry = listener
            .local_addr()
            .expect("toolbox registry address")
            .to_string();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve toolbox registry");
        });
        Self {
            registry,
            state,
            task,
        }
    }

    fn reference(&self) -> ImageReference {
        ImageReference::parse(&format!("{}/{TOOLBOX_REPOSITORY}:latest", self.registry))
            .expect("parse toolbox reference")
    }

    fn requests(&self) -> usize {
        self.state.requests.load(Ordering::SeqCst)
    }
}

async fn serve_manifest(
    State(state): State<ToolboxRegistry>,
    AxumPath(_selector): AxumPath<String>,
) -> Response<Body> {
    state.requests.fetch_add(1, Ordering::SeqCst);
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, OCI_MANIFEST_MEDIA_TYPE)
        .body(Body::from(state.manifest.to_vec()))
        .expect("build manifest response")
}

async fn serve_blob(
    State(state): State<ToolboxRegistry>,
    AxumPath(digest): AxumPath<String>,
) -> Response<Body> {
    state.requests.fetch_add(1, Ordering::SeqCst);
    match state.blobs.get(&digest) {
        Some(bytes) => Response::builder()
            .status(StatusCode::OK)
            .body(Body::from(bytes.to_vec()))
            .expect("build blob response"),
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .expect("build missing blob response"),
    }
}

fn options<'a>(
    image_root: &'a Path,
    blobs: &'a BlobCache,
    layers: &'a LayerCache,
) -> GuestRuntimeOptions<'a> {
    GuestRuntimeOptions {
        image_root,
        blobs,
        layers,
        architecture: Architecture::HOST,
    }
}

#[tokio::test]
async fn a_pulled_toolbox_is_cached_and_never_requested_twice() {
    let directory = tempdir().expect("create fixture directory");
    let image_root = directory.path();
    let blobs = BlobCache::new(image_root);
    let layers = LayerCache::new(image_root);
    let options = options(image_root, &blobs, &layers);
    let program = static_program();
    let registry = TestRegistry::start(Some(&program)).await;

    let first = busybox::ensure_toolbox_from(&options, &registry.reference())
        .await
        .expect("pull the toolbox");
    assert_eq!(std::fs::read(first.path()).unwrap(), program);
    assert_eq!(first.size(), program.len() as u64);
    let pulled = registry.requests();
    assert!(pulled > 0, "the first call reaches the registry");

    let second = busybox::ensure_toolbox_from(&options, &registry.reference())
        .await
        .expect("reuse the cached toolbox");
    assert_eq!(second, first);
    // Only the first import on a host may contact the registry; this is what
    // keeps an anonymous pull limit from bounding how many images can be
    // imported.
    assert_eq!(registry.requests(), pulled);
    // No scratch merge tree survives the pull.
    let toolbox_root = image_root.join(".oci/toolbox");
    assert!(
        !scratch_trees_remain(&toolbox_root),
        "no scratch trees left"
    );
}

/// Whether any `.merge-*` scratch tree survived a toolbox pull.
fn scratch_trees_remain(root: &Path) -> bool {
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(".merge-"))
                {
                    return true;
                }
                pending.push(path);
            }
        }
    }
    false
}

#[tokio::test]
async fn a_toolbox_image_without_the_program_member_is_refused() {
    let directory = tempdir().expect("create fixture directory");
    let image_root = directory.path();
    let blobs = BlobCache::new(image_root);
    let layers = LayerCache::new(image_root);
    let options = options(image_root, &blobs, &layers);
    let registry = TestRegistry::start(None).await;

    let error = busybox::ensure_toolbox_from(&options, &registry.reference())
        .await
        .expect_err("an image without bin/busybox cannot supply an init");
    assert_matches!(error,
        ResolveError::ToolboxMissing { member, .. } if member == "bin/busybox");
    assert!(!scratch_trees_remain(&image_root.join(".oci/toolbox")));
}

#[tokio::test]
async fn a_corrupt_cached_toolbox_is_rebuilt_instead_of_trusted() {
    let directory = tempdir().expect("create fixture directory");
    let image_root = directory.path();
    let blobs = BlobCache::new(image_root);
    let layers = LayerCache::new(image_root);
    let options = options(image_root, &blobs, &layers);
    let program = static_program();
    let registry = TestRegistry::start(Some(&program)).await;

    let first = busybox::ensure_toolbox_from(&options, &registry.reference())
        .await
        .expect("pull the toolbox");
    std::fs::write(first.path(), b"not an ELF image at all").expect("corrupt the cache entry");

    let rebuilt = busybox::ensure_toolbox_from(&options, &registry.reference())
        .await
        .expect("a corrupt entry is re-pulled, not trusted by path");
    assert_eq!(rebuilt, first);
    assert_eq!(std::fs::read(rebuilt.path()).unwrap(), program);
}

#[tokio::test]
async fn the_built_in_toolbox_reference_can_never_be_repointed() {
    // No override is set in this process, so this is the pinned default.
    assert!(busybox::configured_toolbox_override().is_none());
    let reference = ImageReference::parse(&busybox::configured_toolbox_image())
        .expect("the built-in toolbox reference parses");
    // A tag would let a registry swap PID 1 under a running deployment.
    assert!(
        reference.version.is_immutable(),
        "the built-in toolbox must be pinned by digest, not by tag"
    );
    assert_eq!(reference.repository, "library/busybox");
}

#[tokio::test]
async fn a_toolbox_member_that_is_not_a_regular_file_is_refused() {
    let directory = tempdir().expect("create fixture directory");
    let image_root = directory.path();
    let blobs = BlobCache::new(image_root);
    let layers = LayerCache::new(image_root);
    let options = options(image_root, &blobs, &layers);

    let mut builder = Builder::new(Vec::new());
    append_entry(&mut builder, "bin/", EntryType::Directory, None, &[], 0o755);
    append_entry(
        &mut builder,
        "bin/real",
        EntryType::Regular,
        None,
        b"elsewhere",
        0o755,
    );
    // A symlink here would make the copy read through whatever it names.
    append_entry(
        &mut builder,
        "bin/busybox",
        EntryType::Symlink,
        Some("real"),
        &[],
        0o777,
    );
    let registry = TestRegistry::start_from(finish(builder)).await;

    let error = busybox::ensure_toolbox_from(&options, &registry.reference())
        .await
        .expect_err("a symlinked member must not be copied");
    assert_matches!(
        error,
        ResolveError::ToolboxUnusable {
            reason: ToolboxViolation::NotRegularFile,
            ..
        }
    );
}

#[tokio::test]
async fn a_toolbox_missing_applet_names_is_reported_but_still_used() {
    let directory = tempdir().expect("create fixture directory");
    let machine = match Architecture::HOST {
        Architecture::X86_64 => 62,
        Architecture::Aarch64 => 183,
    };
    // No applet table at all. Refusing on this would break every boot the
    // moment the scan produced a false positive, so it only warns.
    let trimmed = elf(machine, 2, &[program_header(1, 0, 0)], &[]);
    let program = toolbox(&directory, "trimmed", &trimmed).await;
    assert_eq!(program.size(), trimmed.len() as u64);
}
