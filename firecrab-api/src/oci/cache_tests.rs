use super::*;

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

#[test]
fn sha256_digests_are_canonical_and_path_safe() {
    let uppercase = format!("sha256:{}", "A".repeat(SHA256_HEX_LENGTH));
    let parsed = Sha256Digest::parse(&uppercase).expect("uppercase hex is valid");
    assert_eq!(
        parsed.as_str(),
        format!("sha256:{}", "a".repeat(SHA256_HEX_LENGTH))
    );
    assert_eq!(parsed.encoded(), "a".repeat(SHA256_HEX_LENGTH));

    assert!(matches!(
        Sha256Digest::parse(&format!("sha512:{}", "a".repeat(SHA256_HEX_LENGTH))),
        Err(DigestError::UnsupportedAlgorithm(algorithm)) if algorithm == "sha512"
    ));
    assert!(matches!(
        Sha256Digest::parse("sha256:abcd"),
        Err(DigestError::InvalidLength { .. })
    ));
    assert!(matches!(
        Sha256Digest::parse(&format!("sha256:{}g", "a".repeat(SHA256_HEX_LENGTH - 1))),
        Err(DigestError::InvalidEncoding(_))
    ));
    let path_characters = format!("sha256:{}/.", "a".repeat(SHA256_HEX_LENGTH - 2));
    assert!(matches!(
        Sha256Digest::parse(&path_characters),
        Err(DigestError::InvalidEncoding(_))
    ));
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

    let cached = cache_image_blobs(&registry.reference(), Architecture::X86_64, true, &cache)
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

    cache_image_blobs(&reference, Architecture::X86_64, true, &cache)
        .await
        .expect("populate cache");
    cache_image_blobs(&reference, Architecture::X86_64, true, &cache)
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

    cache_image_blobs(&reference, Architecture::X86_64, true, &cache)
        .await
        .expect("populate cache");
    tokio::fs::write(cache.path_for(&config.digest), b"corrupt config")
        .await
        .expect("corrupt cached config");
    cache_image_blobs(&reference, Architecture::X86_64, true, &cache)
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

    cache_image_blobs(&registry.reference(), Architecture::X86_64, true, &cache)
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

    let cached = cache_image_blobs(&registry.reference(), Architecture::X86_64, true, &cache)
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
        cache_image_blobs(&reference, Architecture::X86_64, true, &first_cache),
        cache_image_blobs(&reference, Architecture::X86_64, true, &second_cache),
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

    let error = cache_image_blobs(&registry.reference(), Architecture::X86_64, true, &cache)
        .await
        .expect_err("wrong blob bytes must fail verification");

    assert!(matches!(error, ResolveError::DigestMismatch { .. }));
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

    let error = cache_image_blobs(&registry.reference(), Architecture::X86_64, true, &cache)
        .await
        .expect_err("a contradictory blob digest header must fail");

    assert!(matches!(
        error,
        ResolveError::DigestMismatch {
            expected,
            actual,
            ..
        } if expected == config.digest && actual == wrong_header
    ));
    assert_eq!(registry.blob_requests(&config.digest), 1);
    assert_no_cache_artifacts(&cache, &config.digest).await;
}

#[tokio::test]
async fn a_missing_blob_leaves_no_cache_artifact() {
    let config = FixtureBlob::new(OCI_CONFIG_MEDIA_TYPE, b"missing config".to_vec());
    let registry = TestRegistry::start(ordinary_manifest(&config, &[]), []).await;
    let directory = tempdir().expect("create image root");
    let cache = BlobCache::new(directory.path());

    let error = cache_image_blobs(&registry.reference(), Architecture::X86_64, true, &cache)
        .await
        .expect_err("a missing blob must fail");

    assert!(matches!(error, ResolveError::Status { status: 404, .. }));
    assert_eq!(registry.blob_requests(&config.digest), 1);
    assert_no_cache_artifacts(&cache, &config.digest).await;
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

        let error = cache_image_blobs(&registry.reference(), Architecture::X86_64, true, &cache)
            .await
            .expect_err("wrong blob length must fail verification");

        assert!(matches!(
            error,
            ResolveError::SizeMismatch {
                expected: 6,
                actual: received,
                ..
            } if received == actual.len() as u64
        ));
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

    let error = cache_image_blobs(&registry.reference(), Architecture::X86_64, true, &cache)
        .await
        .expect_err("conflicting descriptor sizes must fail");

    assert!(matches!(
        error,
        ResolveError::ConflictingDescriptorSize {
            digest,
            first,
            second,
        } if digest == shared.digest
            && first == shared.bytes.len() as u64
            && second == shared.bytes.len() as u64 + 1
    ));
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

    let error = cache_image_blobs(&registry.reference(), Architecture::X86_64, true, &cache)
        .await
        .expect_err("conflicting descriptor sizes must fail before cache validation");

    assert!(matches!(
        error,
        ResolveError::ConflictingDescriptorSize { digest, .. } if digest == shared.digest
    ));
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

    let error = cache_image_blobs(&registry.reference(), Architecture::X86_64, true, &cache)
        .await
        .expect_err("sha512 descriptors are unsupported");

    assert!(matches!(
        error,
        ResolveError::Digest(DigestError::UnsupportedAlgorithm(algorithm))
            if algorithm == "sha512"
    ));
    assert_eq!(registry.total_blob_requests(), 0);
}
