use super::*;
use core::assert_matches;

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_compression::tokio::bufread::{GzipEncoder, ZstdEncoder};
use axum::body::{Body, Bytes};
use axum::extract::{Path as AxumPath, State};
use axum::http::{Response, StatusCode, header::CONTENT_TYPE};
use axum::routing::get;
use tempfile::tempdir;
use tokio::task::JoinHandle;

const REPOSITORY: &str = "team/app";
const LAYER_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar+gzip";

#[derive(Clone)]
struct FixtureBlob {
    media_type: &'static str,
    digest: Sha256Digest,
    bytes: Vec<u8>,
}

impl FixtureBlob {
    fn new(media_type: &'static str, bytes: impl Into<Vec<u8>>) -> Self {
        let bytes = bytes.into();
        Self {
            media_type,
            digest: Sha256Digest::of_bytes(&bytes),
            bytes,
        }
    }

    fn descriptor(&self) -> serde_json::Value {
        descriptor(
            self.media_type,
            self.digest.as_str(),
            self.bytes.len() as u64,
        )
    }
}

fn config_blob(diff_ids: &[Sha256Digest]) -> FixtureBlob {
    FixtureBlob::new(
        OCI_CONFIG_MEDIA_TYPE,
        serde_json::to_vec(&serde_json::json!({
            "architecture": "amd64",
            "os": "linux",
            "rootfs": {
                "type": "layers",
                "diff_ids": diff_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
            },
        }))
        .expect("serialize image configuration"),
    )
}

async fn gzip(bytes: &[u8]) -> Vec<u8> {
    let input = tokio::io::BufReader::new(bytes);
    let mut encoder = GzipEncoder::new(input);
    let mut output = Vec::new();
    encoder
        .read_to_end(&mut output)
        .await
        .expect("gzip fixture bytes");
    output
}

async fn zstd(bytes: &[u8]) -> Vec<u8> {
    let input = tokio::io::BufReader::new(bytes);
    let mut encoder = ZstdEncoder::new(input);
    let mut output = Vec::new();
    encoder
        .read_to_end(&mut output)
        .await
        .expect("zstd fixture bytes");
    output
}

fn descriptor(media_type: &str, digest: &str, size: u64) -> serde_json::Value {
    serde_json::json!({
        "mediaType": media_type,
        "digest": digest,
        "size": size,
    })
}

fn image_manifest(
    config: serde_json::Value,
    layers: impl IntoIterator<Item = serde_json::Value>,
) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": OCI_MANIFEST_MEDIA_TYPE,
        "config": config,
        "layers": layers.into_iter().collect::<Vec<_>>(),
    }))
    .expect("serialize OCI image manifest")
}

fn ordinary_manifest(config: &FixtureBlob, layers: &[FixtureBlob]) -> Vec<u8> {
    image_manifest(
        config.descriptor(),
        layers.iter().map(FixtureBlob::descriptor),
    )
}

#[derive(Clone)]
struct BlobReply {
    bytes: Arc<Vec<u8>>,
    digest_header: Option<String>,
    delay: Duration,
}

impl BlobReply {
    fn for_descriptor(descriptor: &FixtureBlob) -> Self {
        Self {
            bytes: Arc::new(descriptor.bytes.clone()),
            digest_header: Some(descriptor.digest.to_string()),
            delay: Duration::ZERO,
        }
    }

    fn with_bytes(digest: &Sha256Digest, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            bytes: Arc::new(bytes.into()),
            digest_header: Some(digest.to_string()),
            delay: Duration::ZERO,
        }
    }

    fn delayed(mut self) -> Self {
        self.delay = Duration::from_millis(40);
        self
    }

    fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

#[derive(Clone)]
struct RegistryState {
    manifest: Arc<Vec<u8>>,
    blobs: Arc<HashMap<String, BlobReply>>,
    manifest_requests: Arc<AtomicUsize>,
    blob_requests: Arc<Mutex<HashMap<String, usize>>>,
    active_blob_requests: Arc<AtomicUsize>,
    max_active_blob_requests: Arc<AtomicUsize>,
    blob_completions: Arc<Mutex<Vec<String>>>,
}

struct TestRegistry {
    registry: String,
    state: RegistryState,
    task: JoinHandle<()>,
}

impl TestRegistry {
    async fn start(
        manifest: Vec<u8>,
        blobs: impl IntoIterator<Item = (Sha256Digest, BlobReply)>,
    ) -> Self {
        let state = RegistryState {
            manifest: Arc::new(manifest),
            blobs: Arc::new(
                blobs
                    .into_iter()
                    .map(|(digest, reply)| (digest.to_string(), reply))
                    .collect(),
            ),
            manifest_requests: Arc::new(AtomicUsize::new(0)),
            blob_requests: Arc::new(Mutex::new(HashMap::new())),
            active_blob_requests: Arc::new(AtomicUsize::new(0)),
            max_active_blob_requests: Arc::new(AtomicUsize::new(0)),
            blob_completions: Arc::new(Mutex::new(Vec::new())),
        };
        let app = axum::Router::new()
            .route("/v2/team/app/manifests/{selector}", get(serve_manifest))
            .route("/v2/team/app/blobs/{digest}", get(serve_blob))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test registry");
        let registry = listener
            .local_addr()
            .expect("test registry address")
            .to_string();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve test registry");
        });
        Self {
            registry,
            state,
            task,
        }
    }

    fn reference(&self) -> ImageReference {
        ImageReference::parse(&format!("{}/{REPOSITORY}:latest", self.registry))
            .expect("parse test registry reference")
    }

    fn manifest_requests(&self) -> usize {
        self.state.manifest_requests.load(Ordering::SeqCst)
    }

    fn blob_requests(&self, digest: &Sha256Digest) -> usize {
        self.state
            .blob_requests
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(digest.as_str())
            .copied()
            .unwrap_or(0)
    }

    fn total_blob_requests(&self) -> usize {
        self.state
            .blob_requests
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .values()
            .sum()
    }

    fn max_active_blob_requests(&self) -> usize {
        self.state.max_active_blob_requests.load(Ordering::SeqCst)
    }

    fn blob_completions(&self) -> Vec<String> {
        self.state
            .blob_completions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

impl Drop for TestRegistry {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct ActiveBlobRequest(Arc<AtomicUsize>);

impl Drop for ActiveBlobRequest {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn serve_manifest(
    State(state): State<RegistryState>,
    AxumPath(_selector): AxumPath<String>,
) -> Response<Body> {
    state.manifest_requests.fetch_add(1, Ordering::SeqCst);
    let digest = Sha256Digest::of_bytes(&state.manifest);
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, OCI_MANIFEST_MEDIA_TYPE)
        .header("docker-content-digest", digest.as_str())
        .body(Body::from(state.manifest.as_ref().clone()))
        .expect("build manifest response")
}

async fn serve_blob(
    State(state): State<RegistryState>,
    AxumPath(digest): AxumPath<String>,
) -> Response<Body> {
    {
        let mut requests = state
            .blob_requests
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *requests.entry(digest.clone()).or_default() += 1;
    }
    let Some(reply) = state.blobs.get(&digest).cloned() else {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .expect("build missing blob response");
    };
    let active = state.active_blob_requests.fetch_add(1, Ordering::SeqCst) + 1;
    state
        .max_active_blob_requests
        .fetch_max(active, Ordering::SeqCst);
    let _active_request = ActiveBlobRequest(Arc::clone(&state.active_blob_requests));
    if !reply.delay.is_zero() {
        tokio::time::sleep(reply.delay).await;
    }
    state
        .blob_completions
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .push(digest);

    // A one-item stream deliberately carries no Content-Length. This forces
    // the cache to discover short/long bodies while streaming to a partial.
    let bytes = reply.bytes.as_ref().clone();
    let body = Body::from_stream(futures_util::stream::once(async move {
        Ok::<Bytes, Infallible>(Bytes::from(bytes))
    }));
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/octet-stream");
    if let Some(digest_header) = reply.digest_header {
        response = response.header("docker-content-digest", digest_header);
    }
    response.body(body).expect("build blob response")
}

fn replies(blobs: &[FixtureBlob]) -> Vec<(Sha256Digest, BlobReply)> {
    blobs
        .iter()
        .map(|blob| (blob.digest.clone(), BlobReply::for_descriptor(blob)))
        .collect()
}

async fn assert_no_cache_artifacts(cache: &BlobCache, digest: &Sha256Digest) {
    assert!(
        tokio::fs::metadata(cache.path_for(digest)).await.is_err(),
        "failed blob unexpectedly has a final cache entry"
    );
    let mut entries = match tokio::fs::read_dir(&cache.root).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => panic!("read cache directory: {error}"),
    };
    while let Some(entry) = entries.next_entry().await.expect("read cache entry") {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        assert!(
            !name.ends_with(".partial"),
            "failed download left partial file {name}"
        );
    }
}

async fn local_cached_image(
    image_root: &Path,
    config: FixtureBlob,
    layers: Vec<FixtureBlob>,
) -> CachedImageBlobs {
    let blob_cache = BlobCache::new(image_root);
    tokio::fs::create_dir_all(&blob_cache.root)
        .await
        .expect("create raw blob cache");
    for blob in std::iter::once(&config).chain(&layers) {
        tokio::fs::write(blob_cache.path_for(&blob.digest), &blob.bytes)
            .await
            .expect("write cached fixture blob");
    }
    let manifest = ImageManifest {
        schema_version: 2,
        media_type: OCI_MANIFEST_MEDIA_TYPE.to_owned(),
        config: Descriptor {
            media_type: config.media_type.to_owned(),
            digest: config.digest.clone(),
            size: config.bytes.len() as u64,
        },
        layers: layers
            .iter()
            .map(|layer| Descriptor {
                media_type: layer.media_type.to_owned(),
                digest: layer.digest.clone(),
                size: layer.bytes.len() as u64,
            })
            .collect(),
    };
    CachedImageBlobs {
        resolved: ResolvedImage {
            digest: Sha256Digest::of_bytes(b"manifest").to_string(),
            architecture: Architecture::X86_64,
            single_platform: true,
        },
        config: CachedBlob {
            descriptor: manifest.config.clone(),
            path: blob_cache.path_for(&config.digest),
        },
        layers: manifest
            .layers
            .iter()
            .cloned()
            .map(|descriptor| CachedBlob {
                path: blob_cache.path_for(&descriptor.digest),
                descriptor,
            })
            .collect(),
        manifest,
    }
}

async fn assert_no_layer_artifacts(
    cache: &LayerCache,
    descriptor: &Descriptor,
    diff_id: &Sha256Digest,
) {
    let path = cache
        .path_for(descriptor, diff_id)
        .expect("supported fixture media type");
    assert!(
        tokio::fs::metadata(&path).await.is_err(),
        "failed decompression unexpectedly has a final cache entry"
    );
    let Some(parent) = path.parent() else {
        return;
    };
    let mut entries = match tokio::fs::read_dir(parent).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => panic!("read layer cache directory: {error}"),
    };
    while let Some(entry) = entries.next_entry().await.expect("read layer cache entry") {
        assert!(
            !entry.file_name().to_string_lossy().ends_with(".partial"),
            "failed decompression left a partial file"
        );
    }
}

#[test]
fn sha256_digests_are_canonical_and_path_safe() {
    let uppercase = format!("sha256:{}", "A".repeat(SHA256_HEX_LENGTH));
    let parsed = Sha256Digest::parse(&uppercase).expect("uppercase hex is valid");
    assert_eq!(
        parsed.as_str(),
        format!("sha256:{}", "a".repeat(SHA256_HEX_LENGTH))
    );
    assert_eq!(parsed.encoded(), "a".repeat(SHA256_HEX_LENGTH));

    assert_matches!(Sha256Digest::parse(&format!("sha512:{}", "a".repeat(SHA256_HEX_LENGTH))),
        Err(DigestError::UnsupportedAlgorithm(algorithm)) if algorithm == "sha512");
    assert_matches!(
        Sha256Digest::parse("sha256:abcd"),
        Err(DigestError::InvalidLength { .. })
    );
    assert_matches!(
        Sha256Digest::parse(&format!("sha256:{}g", "a".repeat(SHA256_HEX_LENGTH - 1))),
        Err(DigestError::InvalidEncoding(_))
    );
    let path_characters = format!("sha256:{}/.", "a".repeat(SHA256_HEX_LENGTH - 2));
    assert_matches!(
        Sha256Digest::parse(&path_characters),
        Err(DigestError::InvalidEncoding(_))
    );
}

#[tokio::test]
async fn a_single_platform_manifest_caches_config_and_ordered_layers() {
    let config = FixtureBlob::new(OCI_CONFIG_MEDIA_TYPE, br#"{"architecture":"amd64"}"#);
    let first = FixtureBlob::new(LAYER_MEDIA_TYPE, b"first compressed layer".to_vec());
    let second = FixtureBlob::new(LAYER_MEDIA_TYPE, b"second compressed layer".to_vec());
    let registry = TestRegistry::start(
        ordinary_manifest(&config, &[first.clone(), second.clone()]),
        replies(&[config.clone(), first.clone(), second.clone()]),
    )
    .await;
    let directory = tempdir().expect("create image root");
    let cache = BlobCache::new(directory.path());

    let cached = cache_image_blobs(
        &registry.reference(),
        Architecture::X86_64,
        true,
        &cache,
        None,
    )
    .await
    .expect("cache a single-platform image");

    assert!(cached.resolved.single_platform);
    assert_eq!(cached.resolved.architecture, Architecture::X86_64);
    assert_eq!(
        cached.resolved.digest,
        Sha256Digest::of_bytes(&registry.state.manifest).as_str()
    );
    assert_eq!(cached.config.descriptor.digest, config.digest);
    assert_eq!(cached.config.path, cache.path_for(&config.digest));
    assert_eq!(
        tokio::fs::read(&cached.config.path)
            .await
            .expect("read cached config"),
        config.bytes
    );
    assert_eq!(cached.layers.len(), 2);
    assert_eq!(cached.layers[0].descriptor.digest, first.digest);
    assert_eq!(cached.layers[1].descriptor.digest, second.digest);
    assert_eq!(cached.layers[0].descriptor.media_type, LAYER_MEDIA_TYPE);
    assert_eq!(cached.layers[0].descriptor.size, first.bytes.len() as u64);
    assert_eq!(
        tokio::fs::read(&cached.layers[0].path)
            .await
            .expect("read first cached layer"),
        first.bytes
    );
    assert_eq!(
        tokio::fs::read(&cached.layers[1].path)
            .await
            .expect("read second cached layer"),
        second.bytes
    );
    assert_eq!(registry.manifest_requests(), 1);
    assert_eq!(registry.blob_requests(&config.digest), 1);
    assert_eq!(registry.blob_requests(&first.digest), 1);
    assert_eq!(registry.blob_requests(&second.digest), 1);
}

#[tokio::test]
async fn a_verified_cache_hit_avoids_another_blob_get() {
    let config = FixtureBlob::new(OCI_CONFIG_MEDIA_TYPE, b"verified config".to_vec());
    let registry = TestRegistry::start(
        ordinary_manifest(&config, &[]),
        replies(std::slice::from_ref(&config)),
    )
    .await;
    let directory = tempdir().expect("create image root");
    let cache = BlobCache::new(directory.path());
    let reference = registry.reference();

    cache_image_blobs(&reference, Architecture::X86_64, true, &cache, None)
        .await
        .expect("populate cache");
    cache_image_blobs(&reference, Architecture::X86_64, true, &cache, None)
        .await
        .expect("reuse verified cache");

    assert_eq!(registry.manifest_requests(), 2);
    assert_eq!(registry.blob_requests(&config.digest), 1);
}

#[tokio::test]
async fn a_corrupt_cache_entry_is_removed_and_refetched() {
    let config = FixtureBlob::new(OCI_CONFIG_MEDIA_TYPE, b"original config".to_vec());
    let registry = TestRegistry::start(
        ordinary_manifest(&config, &[]),
        replies(std::slice::from_ref(&config)),
    )
    .await;
    let directory = tempdir().expect("create image root");
    let cache = BlobCache::new(directory.path());
    let reference = registry.reference();

    cache_image_blobs(&reference, Architecture::X86_64, true, &cache, None)
        .await
        .expect("populate cache");
    tokio::fs::write(cache.path_for(&config.digest), b"corrupt config")
        .await
        .expect("corrupt cached config");
    cache_image_blobs(&reference, Architecture::X86_64, true, &cache, None)
        .await
        .expect("repair corrupt cache");

    assert_eq!(registry.blob_requests(&config.digest), 2);
    assert_eq!(
        tokio::fs::read(cache.path_for(&config.digest))
            .await
            .expect("read repaired config"),
        config.bytes
    );
}

#[tokio::test]
async fn an_empty_directory_at_a_digest_path_is_replaced() {
    let config = FixtureBlob::new(OCI_CONFIG_MEDIA_TYPE, b"replacement config".to_vec());
    let registry = TestRegistry::start(
        ordinary_manifest(&config, &[]),
        replies(std::slice::from_ref(&config)),
    )
    .await;
    let directory = tempdir().expect("create image root");
    let cache = BlobCache::new(directory.path());
    tokio::fs::create_dir_all(cache.path_for(&config.digest))
        .await
        .expect("seed abnormal cache directory");

    cache_image_blobs(
        &registry.reference(),
        Architecture::X86_64,
        true,
        &cache,
        None,
    )
    .await
    .expect("replace abnormal cache entry");

    assert_eq!(registry.blob_requests(&config.digest), 1);
    assert_eq!(
        tokio::fs::read(cache.path_for(&config.digest))
            .await
            .expect("read replacement config"),
        config.bytes
    );
}

#[tokio::test]
async fn duplicate_descriptors_download_the_digest_once() {
    let config = FixtureBlob::new(OCI_CONFIG_MEDIA_TYPE, b"config".to_vec());
    let layer = FixtureBlob::new(LAYER_MEDIA_TYPE, b"shared layer".to_vec());
    let registry = TestRegistry::start(
        ordinary_manifest(&config, &[layer.clone(), layer.clone()]),
        replies(&[config.clone(), layer.clone()]),
    )
    .await;
    let directory = tempdir().expect("create image root");
    let cache = BlobCache::new(directory.path());

    let cached = cache_image_blobs(
        &registry.reference(),
        Architecture::X86_64,
        true,
        &cache,
        None,
    )
    .await
    .expect("cache duplicate descriptors");

    assert_eq!(cached.layers.len(), 2);
    assert_eq!(cached.layers[0].path, cached.layers[1].path);
    assert_eq!(registry.blob_requests(&layer.digest), 1);
}

#[tokio::test]
async fn parallel_downloads_are_bounded_and_return_manifest_order() {
    let config = FixtureBlob::new(OCI_CONFIG_MEDIA_TYPE, b"parallel config".to_vec());
    let first = FixtureBlob::new(LAYER_MEDIA_TYPE, b"slow first layer".to_vec());
    let second = FixtureBlob::new(LAYER_MEDIA_TYPE, b"quick second layer".to_vec());
    let third = FixtureBlob::new(LAYER_MEDIA_TYPE, b"quickest third layer".to_vec());
    let registry = TestRegistry::start(
        ordinary_manifest(&config, &[first.clone(), second.clone(), third.clone()]),
        [
            (
                config.digest.clone(),
                BlobReply::for_descriptor(&config).with_delay(Duration::from_millis(10)),
            ),
            (
                first.digest.clone(),
                BlobReply::for_descriptor(&first).with_delay(Duration::from_millis(200)),
            ),
            (
                second.digest.clone(),
                BlobReply::for_descriptor(&second).with_delay(Duration::from_millis(25)),
            ),
            (
                third.digest.clone(),
                BlobReply::for_descriptor(&third).with_delay(Duration::from_millis(5)),
            ),
        ],
    )
    .await;
    let directory = tempdir().expect("create image root");
    let cache = BlobCache::new(directory.path());

    let cached = cache_image_blobs_with_parallelism(
        &registry.reference(),
        Architecture::X86_64,
        true,
        &cache,
        None,
        2,
    )
    .await
    .expect("cache blobs with bounded parallelism");

    assert_eq!(registry.max_active_blob_requests(), 2);
    let completions = registry.blob_completions();
    let completed_at = |digest: &Sha256Digest| {
        completions
            .iter()
            .position(|completed| completed == digest.as_str())
            .expect("every blob completion is recorded")
    };
    assert!(completed_at(&second.digest) < completed_at(&first.digest));
    assert!(completed_at(&third.digest) < completed_at(&first.digest));
    assert_eq!(
        cached
            .layers
            .iter()
            .map(|blob| &blob.descriptor.digest)
            .collect::<Vec<_>>(),
        vec![&first.digest, &second.digest, &third.digest]
    );
}

#[tokio::test]
async fn concurrent_pulls_share_the_per_digest_lock() {
    let config = FixtureBlob::new(OCI_CONFIG_MEDIA_TYPE, b"concurrent config".to_vec());
    let registry = TestRegistry::start(
        ordinary_manifest(&config, &[]),
        vec![(
            config.digest.clone(),
            BlobReply::for_descriptor(&config).delayed(),
        )],
    )
    .await;
    let directory = tempdir().expect("create image root");
    // Construct independent managers for the same root. The lock registry is
    // shared by canonical cache path, not merely by cloning one value.
    let first_cache = BlobCache::new(directory.path());
    let second_cache = BlobCache::new(directory.path());
    let reference = registry.reference();

    let (first, second) = tokio::join!(
        cache_image_blobs(&reference, Architecture::X86_64, true, &first_cache, None),
        cache_image_blobs(&reference, Architecture::X86_64, true, &second_cache, None),
    );
    first.expect("first concurrent pull");
    second.expect("second concurrent pull");

    assert_eq!(registry.manifest_requests(), 2);
    assert_eq!(registry.blob_requests(&config.digest), 1);
}

#[tokio::test]
async fn a_digest_mismatch_leaves_no_final_or_partial_file() {
    let expected = FixtureBlob::new(OCI_CONFIG_MEDIA_TYPE, b"expected bytes".to_vec());
    let wrong = b"different byte".to_vec();
    assert_eq!(wrong.len(), expected.bytes.len());
    let registry = TestRegistry::start(
        ordinary_manifest(&expected, &[]),
        vec![(
            expected.digest.clone(),
            BlobReply::with_bytes(&expected.digest, wrong),
        )],
    )
    .await;
    let directory = tempdir().expect("create image root");
    let cache = BlobCache::new(directory.path());

    let error = cache_image_blobs(
        &registry.reference(),
        Architecture::X86_64,
        true,
        &cache,
        None,
    )
    .await
    .expect_err("wrong blob bytes must fail verification");

    assert_matches!(error, ResolveError::DigestMismatch { .. });
    assert_no_cache_artifacts(&cache, &expected.digest).await;
}

#[tokio::test]
async fn a_blob_digest_header_mismatch_leaves_no_cache_artifact() {
    let config = FixtureBlob::new(OCI_CONFIG_MEDIA_TYPE, b"header checked config".to_vec());
    let wrong_header = Sha256Digest::of_bytes(b"some other content");
    let mut reply = BlobReply::for_descriptor(&config);
    reply.digest_header = Some(wrong_header.to_string());
    let registry = TestRegistry::start(
        ordinary_manifest(&config, &[]),
        [(config.digest.clone(), reply)],
    )
    .await;
    let directory = tempdir().expect("create image root");
    let cache = BlobCache::new(directory.path());

    let error = cache_image_blobs(
        &registry.reference(),
        Architecture::X86_64,
        true,
        &cache,
        None,
    )
    .await
    .expect_err("a contradictory blob digest header must fail");

    assert_matches!(error,
        ResolveError::DigestMismatch {
            expected,
            actual,
            ..
        } if expected == config.digest && actual == wrong_header);
    assert_eq!(registry.blob_requests(&config.digest), 1);
    assert_no_cache_artifacts(&cache, &config.digest).await;
}

#[tokio::test]
async fn a_missing_blob_leaves_no_cache_artifact() {
    let config = FixtureBlob::new(OCI_CONFIG_MEDIA_TYPE, b"missing config".to_vec());
    let registry = TestRegistry::start(ordinary_manifest(&config, &[]), []).await;
    let directory = tempdir().expect("create image root");
    let cache = BlobCache::new(directory.path());

    let error = cache_image_blobs(
        &registry.reference(),
        Architecture::X86_64,
        true,
        &cache,
        None,
    )
    .await
    .expect_err("a missing blob must fail");

    assert_matches!(error, ResolveError::Status { status: 404, .. });
    assert_eq!(registry.blob_requests(&config.digest), 1);
    assert_no_cache_artifacts(&cache, &config.digest).await;
}

#[tokio::test]
async fn oversized_config_and_layer_descriptors_are_rejected_before_download() {
    for media_type in [OCI_CONFIG_MEDIA_TYPE, LAYER_MEDIA_TYPE] {
        let blob = FixtureBlob::new(media_type, b"large".to_vec());
        let registry = TestRegistry::start(
            Vec::new(),
            [(blob.digest.clone(), BlobReply::for_descriptor(&blob))],
        )
        .await;
        let directory = tempdir().expect("create image root");
        let cache = BlobCache::with_max_blob_bytes(directory.path(), 4);
        let reference = registry.reference();
        let session =
            RegistrySession::new(&reference, true, None).expect("create registry session");
        let descriptor = Descriptor {
            media_type: media_type.to_owned(),
            digest: blob.digest.clone(),
            size: blob.bytes.len() as u64,
        };

        let error = cache
            .cache_descriptor(&session, &reference.repository, &descriptor)
            .await
            .expect_err("an oversized descriptor must fail before download");

        assert_matches!(error,
            ResolveError::BlobTooLarge {
                digest,
                size: 5,
                limit: 4,
            } if digest == blob.digest);
        assert_eq!(registry.total_blob_requests(), 0);
        assert_no_cache_artifacts(&cache, &blob.digest).await;
    }
}

#[tokio::test]
async fn a_blob_exactly_at_the_configured_limit_is_downloaded() {
    let config = FixtureBlob::new(OCI_CONFIG_MEDIA_TYPE, b"limit".to_vec());
    let registry = TestRegistry::start(
        Vec::new(),
        [(config.digest.clone(), BlobReply::for_descriptor(&config))],
    )
    .await;
    let directory = tempdir().expect("create image root");
    let cache = BlobCache::with_max_blob_bytes(directory.path(), config.bytes.len() as u64);
    let reference = registry.reference();
    let session = RegistrySession::new(&reference, true, None).expect("create registry session");
    let descriptor = Descriptor {
        media_type: config.media_type.to_owned(),
        digest: config.digest.clone(),
        size: config.bytes.len() as u64,
    };

    let path = cache
        .cache_descriptor(&session, &reference.repository, &descriptor)
        .await
        .expect("the configured limit is inclusive");

    assert_eq!(registry.blob_requests(&config.digest), 1);
    assert_eq!(tokio::fs::read(path).await.unwrap(), config.bytes);
}

#[tokio::test]
async fn short_and_long_bodies_leave_no_final_or_partial_file() {
    let expected = FixtureBlob::new(OCI_CONFIG_MEDIA_TYPE, b"123456".to_vec());
    for actual in [b"12345".to_vec(), b"1234567".to_vec()] {
        let registry = TestRegistry::start(
            ordinary_manifest(&expected, &[]),
            vec![(
                expected.digest.clone(),
                BlobReply::with_bytes(&expected.digest, actual.clone()),
            )],
        )
        .await;
        let directory = tempdir().expect("create image root");
        let cache = BlobCache::new(directory.path());

        let error = cache_image_blobs(
            &registry.reference(),
            Architecture::X86_64,
            true,
            &cache,
            None,
        )
        .await
        .expect_err("wrong blob length must fail verification");

        assert_matches!(error,
            ResolveError::SizeMismatch {
                expected: 6,
                actual: received,
                ..
            } if received == actual.len() as u64);
        assert_no_cache_artifacts(&cache, &expected.digest).await;
    }
}

#[tokio::test]
async fn conflicting_sizes_are_rejected_before_any_blob_request() {
    let shared = FixtureBlob::new(OCI_CONFIG_MEDIA_TYPE, b"shared bytes".to_vec());
    let manifest = image_manifest(
        shared.descriptor(),
        [descriptor(
            LAYER_MEDIA_TYPE,
            shared.digest.as_str(),
            shared.bytes.len() as u64 + 1,
        )],
    );
    let registry = TestRegistry::start(
        manifest,
        vec![(shared.digest.clone(), BlobReply::for_descriptor(&shared))],
    )
    .await;
    let directory = tempdir().expect("create image root");
    let cache = BlobCache::new(directory.path());

    let error = cache_image_blobs(
        &registry.reference(),
        Architecture::X86_64,
        true,
        &cache,
        None,
    )
    .await
    .expect_err("conflicting descriptor sizes must fail");

    assert_matches!(error,
        ResolveError::ConflictingDescriptorSize {
            digest,
            first,
            second,
        } if digest == shared.digest
            && first == shared.bytes.len() as u64
            && second == shared.bytes.len() as u64 + 1);
    assert_eq!(registry.total_blob_requests(), 0);
}

#[tokio::test]
async fn conflicting_sizes_preserve_a_preexisting_valid_cache_entry() {
    let shared = FixtureBlob::new(OCI_CONFIG_MEDIA_TYPE, b"already cached bytes".to_vec());
    let manifest = image_manifest(
        shared.descriptor(),
        [descriptor(
            LAYER_MEDIA_TYPE,
            shared.digest.as_str(),
            shared.bytes.len() as u64 + 1,
        )],
    );
    let registry = TestRegistry::start(manifest, []).await;
    let directory = tempdir().expect("create image root");
    let cache = BlobCache::new(directory.path());
    tokio::fs::create_dir_all(&cache.root)
        .await
        .expect("create cache directory");
    tokio::fs::write(cache.path_for(&shared.digest), &shared.bytes)
        .await
        .expect("seed valid shared cache entry");

    let error = cache_image_blobs(
        &registry.reference(),
        Architecture::X86_64,
        true,
        &cache,
        None,
    )
    .await
    .expect_err("conflicting descriptor sizes must fail before cache validation");

    assert_matches!(error,
        ResolveError::ConflictingDescriptorSize { digest, .. } if digest == shared.digest);
    assert_eq!(registry.total_blob_requests(), 0);
    assert_eq!(
        tokio::fs::read(cache.path_for(&shared.digest))
            .await
            .expect("read preserved cache entry"),
        shared.bytes
    );
}

#[tokio::test]
async fn an_unsupported_descriptor_digest_is_a_digest_error() {
    let unsupported = format!("sha512:{}", "b".repeat(SHA256_HEX_LENGTH));
    let manifest = image_manifest(
        descriptor(OCI_CONFIG_MEDIA_TYPE, &unsupported, 4),
        std::iter::empty(),
    );
    let registry = TestRegistry::start(manifest, []).await;
    let directory = tempdir().expect("create image root");
    let cache = BlobCache::new(directory.path());

    let error = cache_image_blobs(
        &registry.reference(),
        Architecture::X86_64,
        true,
        &cache,
        None,
    )
    .await
    .expect_err("sha512 descriptors are unsupported");

    assert_matches!(error,
        ResolveError::Digest(DigestError::UnsupportedAlgorithm(algorithm))
            if algorithm == "sha512");
    assert_eq!(registry.total_blob_requests(), 0);
}

#[tokio::test]
async fn supported_layer_media_types_produce_verified_ordered_tar_streams() {
    let media_types = [
        OCI_LAYER_MEDIA_TYPE,
        OCI_LAYER_GZIP_MEDIA_TYPE,
        OCI_LAYER_ZSTD_MEDIA_TYPE,
        OCI_NONDISTRIBUTABLE_LAYER_MEDIA_TYPE,
        OCI_NONDISTRIBUTABLE_LAYER_GZIP_MEDIA_TYPE,
        OCI_NONDISTRIBUTABLE_LAYER_ZSTD_MEDIA_TYPE,
        DOCKER_LAYER_MEDIA_TYPE,
        DOCKER_LAYER_GZIP_MEDIA_TYPE,
        DOCKER_FOREIGN_LAYER_GZIP_MEDIA_TYPE,
    ];
    let mut layers = Vec::new();
    let mut payloads = Vec::new();
    let mut diff_ids = Vec::new();
    for (index, media_type) in media_types.into_iter().enumerate() {
        let payload = format!("layer-{index}-uncompressed-tar-bytes").into_bytes();
        let bytes = match layer_compression(media_type).expect("supported fixture media type") {
            LayerCompression::Identity => payload.clone(),
            LayerCompression::Gzip => gzip(&payload).await,
            LayerCompression::Zstd => zstd(&payload).await,
        };
        layers.push(FixtureBlob::new(media_type, bytes));
        diff_ids.push(Sha256Digest::of_bytes(&payload));
        payloads.push(payload);
    }
    let config = config_blob(&diff_ids);
    let registry = TestRegistry::start(
        ordinary_manifest(&config, &layers),
        replies(
            &std::iter::once(config.clone())
                .chain(layers.iter().cloned())
                .collect::<Vec<_>>(),
        ),
    )
    .await;
    let directory = tempdir().expect("create image root");
    let blobs = cache_image_blobs(
        &registry.reference(),
        Architecture::X86_64,
        true,
        &BlobCache::new(directory.path()),
        None,
    )
    .await
    .expect("cache compressed image blobs");
    let layer_cache = LayerCache::new(directory.path());

    let unpacked = decompress_cached_layers(&blobs, &layer_cache)
        .await
        .expect("decompress every supported layer encoding");

    assert_eq!(unpacked.len(), layers.len());
    for (index, layer) in unpacked.iter().enumerate() {
        assert_eq!(layer.source.descriptor.digest, layers[index].digest);
        assert_eq!(layer.diff_id, diff_ids[index]);
        assert_eq!(layer.size, payloads[index].len() as u64);
        assert_eq!(
            layer.path,
            layer_cache
                .path_for(&layer.source.descriptor, &layer.diff_id)
                .unwrap()
        );
        assert_eq!(tokio::fs::read(&layer.path).await.unwrap(), payloads[index]);
        assert_eq!(
            tokio::fs::read(&layer.source.path).await.unwrap(),
            layers[index].bytes
        );
        if layer_compression(media_types[index]) == Some(LayerCompression::Identity) {
            assert_eq!(layer.source.descriptor.digest, layer.diff_id);
        } else {
            assert_ne!(layer.source.descriptor.digest, layer.diff_id);
        }
    }
}

#[tokio::test]
async fn concatenated_gzip_members_and_zstd_frames_are_consumed_completely() {
    let first = b"first tar segment";
    let second = b" and second tar segment";
    let expected = [first.as_slice(), second.as_slice()].concat();
    for (media_type, mut bytes) in [
        (OCI_LAYER_GZIP_MEDIA_TYPE, gzip(first).await),
        (OCI_LAYER_ZSTD_MEDIA_TYPE, zstd(first).await),
    ] {
        bytes.extend(match layer_compression(media_type).unwrap() {
            LayerCompression::Gzip => gzip(second).await,
            LayerCompression::Zstd => zstd(second).await,
            LayerCompression::Identity => unreachable!(),
        });
        let layer = FixtureBlob::new(media_type, bytes);
        let diff_id = Sha256Digest::of_bytes(&expected);
        let config = config_blob(std::slice::from_ref(&diff_id));
        let directory = tempdir().expect("create image root");
        let image = local_cached_image(directory.path(), config, vec![layer]).await;

        let unpacked = decompress_cached_layers(&image, &LayerCache::new(directory.path()))
            .await
            .expect("consume every gzip member or zstd frame");

        assert_eq!(tokio::fs::read(&unpacked[0].path).await.unwrap(), expected);
    }
}

#[tokio::test]
async fn invalid_configs_fail_before_a_layer_cache_is_created() {
    let payload = b"plain layer".to_vec();
    let layer = FixtureBlob::new(OCI_LAYER_MEDIA_TYPE, payload.clone());
    let diff_id = Sha256Digest::of_bytes(&payload);
    let cases = [
        ("malformed", b"{".to_vec()),
        (
            "rootfs type",
            serde_json::to_vec(&serde_json::json!({
                "rootfs": { "type": "unknown", "diff_ids": [diff_id.to_string()] }
            }))
            .unwrap(),
        ),
        (
            "count",
            serde_json::to_vec(&serde_json::json!({
                "rootfs": { "type": "layers", "diff_ids": [] }
            }))
            .unwrap(),
        ),
        (
            "digest",
            serde_json::to_vec(&serde_json::json!({
                "rootfs": { "type": "layers", "diff_ids": [format!("sha512:{}", "a".repeat(64))] }
            }))
            .unwrap(),
        ),
    ];

    for (kind, bytes) in cases {
        let directory = tempdir().expect("create image root");
        let config = FixtureBlob::new(OCI_CONFIG_MEDIA_TYPE, bytes);
        let image = local_cached_image(directory.path(), config, vec![layer.clone()]).await;
        let cache = LayerCache::new(directory.path());

        let error = decompress_cached_layers(&image, &cache)
            .await
            .expect_err("invalid config must fail before decompression");

        match kind {
            "rootfs type" => assert_matches!(error, ResolveError::UnsupportedRootfsType(_)),
            "count" => assert_matches!(
                error,
                ResolveError::DiffIdCountMismatch {
                    expected: 1,
                    actual: 0
                }
            ),
            _ => assert_matches!(error, ResolveError::MalformedConfig(_)),
        }
        assert!(tokio::fs::metadata(&cache.root).await.is_err());
    }
}

#[tokio::test]
async fn an_unsupported_layer_media_type_fails_before_output() {
    let payload = b"unknown encoding".to_vec();
    let layer = FixtureBlob::new("application/vnd.example.layer+unknown", payload.clone());
    let diff_id = Sha256Digest::of_bytes(&payload);
    let config = config_blob(std::slice::from_ref(&diff_id));
    let directory = tempdir().expect("create image root");
    let image = local_cached_image(directory.path(), config, vec![layer]).await;
    let cache = LayerCache::new(directory.path());

    let error = decompress_cached_layers(&image, &cache)
        .await
        .expect_err("unknown layer encoding must be rejected");

    assert_matches!(error, ResolveError::UnsupportedMediaType(_));
    assert!(tokio::fs::metadata(&cache.root).await.is_err());
}

#[tokio::test]
async fn a_diff_id_mismatch_preserves_the_compressed_blob_and_leaves_no_output() {
    let payload = b"actual uncompressed bytes".to_vec();
    let layer = FixtureBlob::new(OCI_LAYER_GZIP_MEDIA_TYPE, gzip(&payload).await);
    let wrong_diff_id = Sha256Digest::of_bytes(b"different uncompressed bytes");
    let config = config_blob(std::slice::from_ref(&wrong_diff_id));
    let directory = tempdir().expect("create image root");
    let image = local_cached_image(directory.path(), config, vec![layer.clone()]).await;
    let cache = LayerCache::new(directory.path());

    let error = decompress_cached_layers(&image, &cache)
        .await
        .expect_err("wrong config diff ID must fail");

    assert_matches!(error,
        ResolveError::DiffIdMismatch {
            compressed_digest,
            expected,
            ..
        } if compressed_digest == layer.digest && expected == wrong_diff_id);
    assert_no_layer_artifacts(&cache, &image.layers[0].descriptor, &wrong_diff_id).await;
    assert_eq!(
        tokio::fs::read(&image.layers[0].path).await.unwrap(),
        layer.bytes
    );
}

#[tokio::test]
async fn the_exact_compressed_stream_is_verified_while_it_is_decoded() {
    let payload = b"unchanged decoder output".to_vec();
    let layer = FixtureBlob::new(OCI_LAYER_GZIP_MEDIA_TYPE, gzip(&payload).await);
    let diff_id = Sha256Digest::of_bytes(&payload);
    let config = config_blob(std::slice::from_ref(&diff_id));
    let directory = tempdir().expect("create image root");
    let image = local_cached_image(directory.path(), config, vec![layer.clone()]).await;
    let mut changed_header = layer.bytes.clone();
    assert_eq!(&changed_header[..3], &[0x1f, 0x8b, 0x08]);
    changed_header[4] ^= 1; // gzip mtime: metadata that does not affect decoder output
    assert_ne!(Sha256Digest::of_bytes(&changed_header), layer.digest);
    tokio::fs::write(&image.layers[0].path, &changed_header)
        .await
        .expect("replace cached blob after its earlier verification");
    let cache = LayerCache::new(directory.path());

    let error = decompress_cached_layers(&image, &cache)
        .await
        .expect_err("the decoder must verify its exact compressed input");

    assert_matches!(error,
        ResolveError::DigestMismatch {
            expected,
            actual,
            ..
        } if expected == layer.digest && actual == Sha256Digest::of_bytes(&changed_header));
    assert_no_layer_artifacts(&cache, &image.layers[0].descriptor, &diff_id).await;
}

#[tokio::test]
async fn truncated_gzip_and_zstd_layers_leave_no_output_or_partial() {
    let payload = b"payload whose codec checksum must be verified".to_vec();
    for (media_type, mut bytes) in [
        (OCI_LAYER_GZIP_MEDIA_TYPE, gzip(&payload).await),
        (OCI_LAYER_ZSTD_MEDIA_TYPE, zstd(&payload).await),
    ] {
        bytes.truncate(bytes.len().saturating_sub(4));
        let layer = FixtureBlob::new(media_type, bytes);
        let diff_id = Sha256Digest::of_bytes(&payload);
        let config = config_blob(std::slice::from_ref(&diff_id));
        let directory = tempdir().expect("create image root");
        let image = local_cached_image(directory.path(), config, vec![layer]).await;
        let cache = LayerCache::new(directory.path());

        let error = decompress_cached_layers(&image, &cache)
            .await
            .expect_err("truncated compressed stream must fail");

        assert_matches!(error, ResolveError::Decompression { .. });
        assert_no_layer_artifacts(&cache, &image.layers[0].descriptor, &diff_id).await;
    }
}

#[tokio::test]
async fn trailing_bytes_after_gzip_and_zstd_streams_are_rejected() {
    let payload = b"complete payload".to_vec();
    for (media_type, mut bytes) in [
        (OCI_LAYER_GZIP_MEDIA_TYPE, gzip(&payload).await),
        (OCI_LAYER_ZSTD_MEDIA_TYPE, zstd(&payload).await),
    ] {
        bytes.extend_from_slice(b"not another valid member");
        let layer = FixtureBlob::new(media_type, bytes);
        let diff_id = Sha256Digest::of_bytes(&payload);
        let config = config_blob(std::slice::from_ref(&diff_id));
        let directory = tempdir().expect("create image root");
        let image = local_cached_image(directory.path(), config, vec![layer]).await;
        let cache = LayerCache::new(directory.path());

        let error = decompress_cached_layers(&image, &cache)
            .await
            .expect_err("trailing non-frame bytes must fail");

        assert_matches!(error, ResolveError::Decompression { .. });
        assert_no_layer_artifacts(&cache, &image.layers[0].descriptor, &diff_id).await;
    }
}

#[tokio::test]
async fn decoded_output_limit_is_inclusive_and_cleans_an_oversized_partial() {
    let payload = b"ten bytes!".to_vec();
    assert_eq!(payload.len(), 10);
    let layer = FixtureBlob::new(OCI_LAYER_GZIP_MEDIA_TYPE, gzip(&payload).await);
    let diff_id = Sha256Digest::of_bytes(&payload);
    let config = config_blob(std::slice::from_ref(&diff_id));
    let directory = tempdir().expect("create image root");
    let image = local_cached_image(directory.path(), config, vec![layer]).await;
    let too_small = LayerCache::with_max_uncompressed_layer_bytes(directory.path(), 9);

    let error = decompress_cached_layers(&image, &too_small)
        .await
        .expect_err("decoded output over the limit must fail");

    assert_matches!(
        error,
        ResolveError::UncompressedLayerTooLarge {
            limit: 9,
            actual: 10,
            ..
        }
    );
    assert_no_layer_artifacts(&too_small, &image.layers[0].descriptor, &diff_id).await;

    let exact = LayerCache::with_max_uncompressed_layer_bytes(directory.path(), 10);
    let unpacked = decompress_cached_layers(&image, &exact)
        .await
        .expect("the decoded output limit is inclusive");
    assert_eq!(tokio::fs::read(&unpacked[0].path).await.unwrap(), payload);
}

#[tokio::test]
async fn a_verified_layer_hit_survives_source_removal_and_corruption_is_rebuilt() {
    let payload = b"cached uncompressed tar stream".to_vec();
    let layer = FixtureBlob::new(OCI_LAYER_GZIP_MEDIA_TYPE, gzip(&payload).await);
    let diff_id = Sha256Digest::of_bytes(&payload);
    let config = config_blob(std::slice::from_ref(&diff_id));
    let directory = tempdir().expect("create image root");
    let image = local_cached_image(directory.path(), config, vec![layer.clone()]).await;
    let cache = LayerCache::new(directory.path());

    let first = decompress_cached_layers(&image, &cache)
        .await
        .expect("populate decoded layer cache");
    tokio::fs::remove_file(&image.layers[0].path)
        .await
        .expect("remove compressed source");
    let hit = decompress_cached_layers(&image, &cache)
        .await
        .expect("reuse a verified decoded cache entry");
    assert_eq!(hit[0].path, first[0].path);

    tokio::fs::write(&image.layers[0].path, &layer.bytes)
        .await
        .expect("restore compressed source");
    tokio::fs::write(&first[0].path, vec![b'x'; payload.len()])
        .await
        .expect("corrupt decoded cache entry without changing its size");
    let repaired = decompress_cached_layers(&image, &cache)
        .await
        .expect("rebuild a corrupt decoded cache entry");
    assert_eq!(tokio::fs::read(&repaired[0].path).await.unwrap(), payload);
}

#[tokio::test]
async fn identical_diff_ids_keep_distinct_compressed_relationships() {
    let payload = b"one tar stream encoded two ways".to_vec();
    let gzip_layer = FixtureBlob::new(OCI_LAYER_GZIP_MEDIA_TYPE, gzip(&payload).await);
    let zstd_layer = FixtureBlob::new(OCI_LAYER_ZSTD_MEDIA_TYPE, zstd(&payload).await);
    let diff_id = Sha256Digest::of_bytes(&payload);
    let config = config_blob(&[diff_id.clone(), diff_id.clone(), diff_id.clone()]);
    let directory = tempdir().expect("create image root");
    let image = local_cached_image(
        directory.path(),
        config,
        vec![gzip_layer.clone(), zstd_layer.clone(), gzip_layer],
    )
    .await;

    let layers = decompress_cached_layers(&image, &LayerCache::new(directory.path()))
        .await
        .expect("verify both compressed relationships");

    assert_eq!(
        layers
            .iter()
            .map(|layer| &layer.diff_id)
            .collect::<Vec<_>>(),
        vec![&diff_id; 3]
    );
    assert_eq!(layers[0].path, layers[2].path);
    assert_ne!(layers[0].path, layers[1].path);
    assert_ne!(
        layers[0].source.descriptor.digest,
        layers[1].source.descriptor.digest
    );
    for layer in layers {
        assert_eq!(tokio::fs::read(layer.path).await.unwrap(), payload);
    }
}

#[tokio::test]
async fn an_image_without_layers_accepts_an_empty_diff_id_list() {
    let config = config_blob(&[]);
    let directory = tempdir().expect("create image root");
    let image = local_cached_image(directory.path(), config, Vec::new()).await;

    let layers = decompress_cached_layers(&image, &LayerCache::new(directory.path()))
        .await
        .expect("empty images have no layer work");

    assert!(layers.is_empty());
}

#[test]
fn decompression_parallelism_has_a_separate_memory_bound() {
    assert_eq!(decompression_parallelism(0), 1);
    assert_eq!(decompression_parallelism(1), 1);
    assert_eq!(decompression_parallelism(2), 2);
    assert_eq!(decompression_parallelism(64), MAX_PARALLEL_DECOMPRESSIONS);
}

#[tokio::test]
async fn concurrent_calls_share_the_process_wide_decompression_permits() {
    let permits = shared_decompression_permits();
    let held = Arc::clone(&permits)
        .acquire_many_owned(MAX_PARALLEL_DECOMPRESSIONS as u32)
        .await
        .expect("the shared semaphore stays open");

    let first_directory = tempdir().expect("create first image root");
    let first_payload = b"first concurrent zstd layer".to_vec();
    let first_diff_id = Sha256Digest::of_bytes(&first_payload);
    let first_layer = FixtureBlob::new(OCI_LAYER_ZSTD_MEDIA_TYPE, zstd(&first_payload).await);
    let first_image = local_cached_image(
        first_directory.path(),
        config_blob(std::slice::from_ref(&first_diff_id)),
        vec![first_layer],
    )
    .await;
    let first_cache = LayerCache::new(first_directory.path());

    let second_directory = tempdir().expect("create second image root");
    let second_payload = b"second concurrent zstd layer".to_vec();
    let second_diff_id = Sha256Digest::of_bytes(&second_payload);
    let second_layer = FixtureBlob::new(OCI_LAYER_ZSTD_MEDIA_TYPE, zstd(&second_payload).await);
    let second_image = local_cached_image(
        second_directory.path(),
        config_blob(std::slice::from_ref(&second_diff_id)),
        vec![second_layer],
    )
    .await;
    let second_cache = LayerCache::new(second_directory.path());
    assert!(Arc::ptr_eq(&first_cache.decompression_permits, &permits));
    assert!(Arc::ptr_eq(&second_cache.decompression_permits, &permits));

    let mut first = Box::pin(decompress_cached_layers(&first_image, &first_cache));
    let mut second = Box::pin(decompress_cached_layers(&second_image, &second_cache));
    let early_completion = tokio::time::timeout(Duration::from_millis(20), async {
        tokio::select! {
            result = &mut first => result,
            result = &mut second => result,
        }
    })
    .await;
    assert!(
        early_completion.is_err(),
        "neither independent call may decode while all global permits are held"
    );

    drop(held);
    let (first_result, second_result) = tokio::join!(first, second);
    assert_eq!(
        tokio::fs::read(&first_result.unwrap()[0].path)
            .await
            .unwrap(),
        first_payload
    );
    assert_eq!(
        tokio::fs::read(&second_result.unwrap()[0].path)
            .await
            .unwrap(),
        second_payload
    );
}
