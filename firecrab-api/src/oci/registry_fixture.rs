//! Isolated loopback OCI registry used only by tests.
//!
//! Serves one tiny Linux image for this host (`amd64` or `arm64`) over plain
//! HTTP on `127.0.0.1`. Testers and the browser E2E type
//! `127.0.0.1:<port>/firecrab/e2e:ready`. After import the guest service
//! prints [`READY_SENTINEL`] to the console. Nothing here is compiled into
//! the production binary, and Drop aborts the listener and deletes scratch.

use super::*;

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;

use async_compression::tokio::bufread::GzipEncoder;
use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{Response, StatusCode, header::CONTENT_TYPE};
use axum::routing::get;
use tar::{Builder, EntryType, Header};
use tempfile::TempDir;
use tokio::task::JoinHandle;

/// Console line the imported guest service prints after boot.
pub(crate) const READY_SENTINEL: &str = "FIRECRAB_OCI_E2E_READY";
/// Repository path under the loopback registry.
pub(crate) const REPOSITORY: &str = "firecrab/e2e";
/// Tag testers type after the repository.
pub(crate) const TAG: &str = "ready";

/// Entrypoint that becomes `/etc/firecrab/services.d/app` after import.
///
/// `/etc/firecrab/busybox` is injected during provisioning, so this image
/// does not ship a shell of its own and never pulls from Docker Hub.
const ENTRYPOINT: &[&str] = &[provision::GUEST_TOOLBOX, "sh", "-c"];

#[derive(Clone)]
struct FixtureState {
    manifest: Arc<Vec<u8>>,
    blobs: Arc<BTreeMap<String, Arc<Vec<u8>>>>,
}

/// In-process OCI distribution server bound to `127.0.0.1:0`.
pub(crate) struct LocalOciRegistry {
    registry: String,
    architecture: &'static str,
    task: JoinHandle<()>,
    scratch: Option<TempDir>,
}

impl LocalOciRegistry {
    /// Builds the tiny image, writes scratch blobs, and serves them.
    pub(crate) async fn start() -> Self {
        let scratch = tempfile::tempdir().expect("create OCI fixture scratch");
        let (manifest, blobs) = write_fixture_image(scratch.path()).await;
        let state = FixtureState {
            manifest: Arc::new(manifest),
            blobs: Arc::new(blobs),
        };
        let app = axum::Router::new()
            .route("/v2/", get(serve_api_version))
            .route("/v2/firecrab/e2e/manifests/{selector}", get(serve_manifest))
            .route("/v2/firecrab/e2e/blobs/{digest}", get(serve_blob))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind isolated OCI fixture");
        let registry = listener
            .local_addr()
            .expect("OCI fixture address")
            .to_string();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve isolated OCI fixture");
        });
        Self {
            registry,
            architecture: oci_platform(Architecture::HOST),
            task,
            scratch: Some(scratch),
        }
    }

    /// `host:port` the fixture is listening on.
    pub(crate) fn registry(&self) -> &str {
        &self.registry
    }

    /// Reference testers type: `127.0.0.1:<port>/firecrab/e2e:ready`.
    pub(crate) fn reference(&self) -> String {
        format!("{}/{REPOSITORY}:{TAG}", self.registry)
    }

    /// Parsed form of [`Self::reference`].
    pub(crate) fn parsed_reference(&self) -> ImageReference {
        ImageReference::parse(&self.reference()).expect("parse fixture reference")
    }

    /// Template alias `POST /api/oci/import` will claim for this listener.
    pub(crate) fn alias(&self) -> String {
        template_name_from_reference(&self.parsed_reference())
            .expect("derive fixture alias")
            .alias
    }

    /// OCI platform name baked into the image config (`amd64` or `arm64`).
    pub(crate) fn architecture(&self) -> &'static str {
        self.architecture
    }

    /// Stops the listener and deletes scratch blobs. Also run from [`Drop`].
    pub(crate) fn shutdown(&mut self) {
        self.task.abort();
        if let Some(scratch) = self.scratch.take() {
            let _ = scratch.close();
        }
    }
}

impl Drop for LocalOciRegistry {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn write_fixture_image(scratch: &Path) -> (Vec<u8>, BTreeMap<String, Arc<Vec<u8>>>) {
    let tar = layer_tar();
    tokio::fs::write(scratch.join("layer.tar"), &tar)
        .await
        .expect("write fixture layer tar");
    let diff_id = Sha256Digest::of_bytes(&tar);
    let compressed = gzip_bytes(&tar).await;
    tokio::fs::write(scratch.join("layer.tar.gz"), &compressed)
        .await
        .expect("write fixture compressed layer");
    let layer_digest = Sha256Digest::of_bytes(&compressed);

    let config = serde_json::to_vec(&serde_json::json!({
        "architecture": oci_platform(Architecture::HOST),
        "os": "linux",
        "config": {
            "Entrypoint": ENTRYPOINT,
            "Cmd": [ready_command()],
        },
        "rootfs": {
            "type": "layers",
            "diff_ids": [diff_id.to_string()],
        },
    }))
    .expect("serialize fixture image config");
    tokio::fs::write(scratch.join("config.json"), &config)
        .await
        .expect("write fixture config");
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
    .expect("serialize fixture image manifest");
    tokio::fs::write(scratch.join("manifest.json"), &manifest)
        .await
        .expect("write fixture manifest");

    let blobs = BTreeMap::from([
        (config_digest.to_string(), Arc::new(config)),
        (layer_digest.to_string(), Arc::new(compressed)),
    ]);
    (manifest, blobs)
}

fn layer_tar() -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());
    append_entry(&mut builder, "etc/", EntryType::Directory, &[], 0o755);
    append_entry(
        &mut builder,
        "etc/firecrab-e2e",
        EntryType::Regular,
        format!("{READY_SENTINEL}\n").as_bytes(),
        0o644,
    );
    builder.into_inner().expect("finish fixture layer tar")
}

fn append_entry(
    builder: &mut Builder<Vec<u8>>,
    path: &str,
    entry_type: EntryType,
    data: &[u8],
    mode: u32,
) {
    let mut header = Header::new_gnu();
    header.set_entry_type(entry_type);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(data.len() as u64);
    builder
        .append_data(&mut header, path, Cursor::new(data))
        .expect("append fixture tar entry");
}

async fn gzip_bytes(bytes: &[u8]) -> Vec<u8> {
    let input = tokio::io::BufReader::new(bytes);
    let mut encoder = GzipEncoder::new(input);
    let mut output = Vec::new();
    encoder
        .read_to_end(&mut output)
        .await
        .expect("gzip fixture layer");
    output
}

fn ready_command() -> String {
    format!(
        "while true; do echo {READY_SENTINEL}; {} sleep 2; done",
        provision::GUEST_TOOLBOX
    )
}

async fn serve_api_version() -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header("docker-distribution-api-version", "registry/2.0")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .expect("build API version response")
}

async fn serve_manifest(
    State(state): State<FixtureState>,
    AxumPath(selector): AxumPath<String>,
) -> Response<Body> {
    let digest = Sha256Digest::of_bytes(&state.manifest);
    if selector != TAG && selector != digest.as_str() {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .expect("build missing manifest response");
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, OCI_MANIFEST_MEDIA_TYPE)
        .header("docker-content-digest", digest.as_str())
        .body(Body::from(state.manifest.to_vec()))
        .expect("build manifest response")
}

async fn serve_blob(
    State(state): State<FixtureState>,
    AxumPath(digest): AxumPath<String>,
) -> Response<Body> {
    match state.blobs.get(&digest) {
        Some(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/octet-stream")
            .header("docker-content-digest", digest)
            .body(Body::from(bytes.to_vec()))
            .expect("build blob response"),
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .expect("build missing blob response"),
    }
}

#[tokio::test]
async fn the_fixture_serves_a_host_architecture_linux_image() {
    let registry = LocalOciRegistry::start().await;
    let reference = registry.parsed_reference();
    let directory = tempfile::tempdir().expect("create image root");
    let blobs = BlobCache::new(directory.path());
    let layers = LayerCache::new(directory.path());

    let resolved = resolve(&reference, Architecture::HOST, true)
        .await
        .expect("inspect the fixture over loopback HTTP");
    assert_eq!(resolved.architecture, Architecture::HOST);
    assert!(resolved.single_platform);
    assert_eq!(registry.architecture(), oci_platform(Architecture::HOST));

    let cached = cache_image_blobs(&reference, Architecture::HOST, true, &blobs)
        .await
        .expect("pull fixture config and layer");
    let process = OciProcessConfig::from_image_config(
        &tokio::fs::read(&cached.config.path)
            .await
            .expect("read cached fixture config"),
    )
    .expect("parse fixture process config");
    assert_eq!(
        process
            .entrypoint()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ENTRYPOINT
    );
    assert!(
        process.cmd().iter().any(|arg| arg.contains(READY_SENTINEL)),
        "fixture Cmd must print {READY_SENTINEL}: {:?}",
        process.cmd()
    );

    let decompressed = decompress_cached_layers(&cached, &layers)
        .await
        .expect("decompress fixture layer");
    let validated = validate_decompressed_layers(decompressed)
        .await
        .expect("fixture layer must pass archive safety");
    let merged = merge_validated_layers(&validated, &directory.path().join("rootfs"))
        .await
        .expect("merge fixture layer");
    let marker = tokio::fs::read_to_string(merged.path().join("etc/firecrab-e2e"))
        .await
        .expect("read fixture marker");
    assert_eq!(marker, format!("{READY_SENTINEL}\n"));
}

#[tokio::test]
async fn the_fixture_reference_is_what_testers_type() {
    let registry = LocalOciRegistry::start().await;
    let reference = registry.reference();
    assert!(
        reference.starts_with("127.0.0.1:") && reference.ends_with("/firecrab/e2e:ready"),
        "unexpected tester reference {reference}"
    );
    assert_eq!(
        registry.alias(),
        format!(
            "{}-firecrab-e2e-ready",
            registry.registry().replace(':', "-")
        )
    );
}

#[tokio::test]
async fn dropping_the_fixture_stops_the_listener_and_deletes_scratch() {
    let mut registry = LocalOciRegistry::start().await;
    let address = registry.registry().to_owned();
    let scratch = registry
        .scratch
        .as_ref()
        .expect("scratch lives until shutdown")
        .path()
        .to_owned();
    assert!(scratch.join("manifest.json").exists());

    registry.shutdown();

    assert!(
        tokio::net::TcpStream::connect(&address).await.is_err(),
        "listener must not survive shutdown"
    );
    assert!(
        !scratch.exists(),
        "scratch blobs must be deleted on shutdown, including after failure"
    );
}
