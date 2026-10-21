use super::*;
use core::assert_matches;

use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{Response, StatusCode};
use axum::routing::get;
use tar::{Builder, EntryType, Header};
use tempfile::tempdir;
use tokio::task::JoinHandle;

/// Alias, version, and image name the fixture package is published under.
const FIXTURE_ALIAS: &str = "vmlinux-9.9.9";
const FIXTURE_VERSION: &str = "9.9.9";

fn fixture_image_name(architecture: Architecture) -> String {
    match architecture {
        Architecture::X86_64 => format!("vmlinux-{FIXTURE_VERSION}-x86_64"),
        Architecture::Aarch64 => format!("Image-{FIXTURE_VERSION}-aarch64"),
    }
}

/// A 64-bit little-endian ELF header Firecracker classifies as `machine`.
fn elf_kernel(machine: u16) -> Vec<u8> {
    let mut bytes = vec![0_u8; 4096];
    bytes[0..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    bytes[16..18].copy_from_slice(&2_u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes
}

/// A bare `Linux/arm64 Image` header.
fn arm64_image_kernel() -> Vec<u8> {
    let mut bytes = vec![0_u8; 4096];
    bytes[0..2].copy_from_slice(b"MZ");
    bytes[56..60].copy_from_slice(&0x644d_5241_u32.to_le_bytes());
    bytes
}

fn kernel_bytes_for(architecture: Architecture) -> Vec<u8> {
    match architecture {
        Architecture::Aarch64 => arm64_image_kernel(),
        Architecture::X86_64 => elf_kernel(0x3e),
    }
}

fn tar_entry(builder: &mut Builder<Vec<u8>>, path: &str, data: &[u8]) {
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Regular);
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(data.len() as u64);
    builder
        .append_data(&mut header, path, Cursor::new(data))
        .expect("append fixture tar entry");
}

/// Builds the `.tar.zst` the registry publishes: `kernel/<image>` beside the
/// config it was built from, exactly as `package-vmlinux.sh` writes it.
fn kernel_package(member: &str, kernel: &[u8]) -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());
    tar_entry(&mut builder, member, kernel);
    tar_entry(
        &mut builder,
        "config/fixture.config",
        b"CONFIG_VIRTIO_BLK=y\n",
    );
    let tar = builder.into_inner().expect("finish fixture tar");
    zstd::stream::encode_all(tar.as_slice(), 0).expect("compress fixture package")
}

#[derive(Clone)]
struct PackageState {
    package: Arc<Vec<u8>>,
    key: Arc<String>,
    requests: Arc<AtomicUsize>,
}

/// Static object host standing in for `registry.firecrab.dev`.
struct KernelRegistry {
    base_url: String,
    state: PackageState,
    task: JoinHandle<()>,
}

impl Drop for KernelRegistry {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl KernelRegistry {
    async fn start(package: Vec<u8>, key: String) -> Self {
        let state = PackageState {
            package: Arc::new(package),
            key: Arc::new(key),
            requests: Arc::new(AtomicUsize::new(0)),
        };
        let app = axum::Router::new()
            .route("/{*key}", get(serve_package))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind kernel registry");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("kernel registry address")
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve kernel registry");
        });
        Self {
            base_url,
            state,
            task,
        }
    }

    fn requests(&self) -> usize {
        self.state.requests.load(Ordering::SeqCst)
    }
}

async fn serve_package(
    State(state): State<PackageState>,
    AxumPath(key): AxumPath<String>,
) -> Response<Body> {
    state.requests.fetch_add(1, Ordering::SeqCst);
    if key != *state.key {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .expect("build missing package response");
    }
    Response::builder()
        .status(StatusCode::OK)
        .body(Body::from(state.package.to_vec()))
        .expect("build package response")
}

/// One published kernel plus the pin a build would carry for it.
struct PublishedKernel {
    registry: KernelRegistry,
    kernel: Vec<u8>,
    package_digest: String,
    image_digest: String,
    image: String,
}

impl PublishedKernel {
    /// Publishes `kernel` under the layout the real registry serves.
    async fn publish(architecture: Architecture, kernel: Vec<u8>, member: Option<&str>) -> Self {
        let image = fixture_image_name(architecture);
        let member = member.map_or_else(|| format!("kernel/{image}"), str::to_owned);
        let package = kernel_package(&member, &kernel);
        let key = format!(
            "kernel/{FIXTURE_VERSION}/{}/{FIXTURE_ALIAS}.tar.zst",
            architecture.as_str()
        );
        Self {
            package_digest: Sha256Digest::of_bytes(&package).to_string(),
            image_digest: Sha256Digest::of_bytes(&kernel).to_string(),
            registry: KernelRegistry::start(package, key).await,
            kernel,
            image,
        }
    }

    fn pinned(&self) -> kernel::PinnedKernel<'_> {
        kernel::PinnedKernel {
            alias: FIXTURE_ALIAS,
            version: FIXTURE_VERSION,
            image: &self.image,
            package_digest: &self.package_digest,
            image_digest: &self.image_digest,
            boot_args: "console=ttyS0 root=/dev/vda rw",
        }
    }

    /// Publishes the pinned digest of a kernel the registry does not serve.
    fn with_foreign_pin(mut self, digest: &str) -> Self {
        self.package_digest = digest.to_owned();
        self
    }
}

async fn ensure(
    published: &PublishedKernel,
    image_root: &std::path::Path,
) -> Result<boot::KernelBootPair, ResolveError> {
    kernel::ensure_pinned_kernel(
        image_root,
        Architecture::HOST,
        &published.pinned(),
        None,
        Some(&published.registry.base_url),
    )
    .await
}

#[tokio::test]
async fn a_fetched_kernel_is_cached_under_the_image_root_and_never_downloaded_twice() {
    let directory = tempdir().expect("create fixture directory");
    let image_root = directory.path();
    let published = PublishedKernel::publish(
        Architecture::HOST,
        kernel_bytes_for(Architecture::HOST),
        None,
    )
    .await;

    let first = ensure(&published, image_root).await.expect("first fetch");
    let second = ensure(&published, image_root).await.expect("cached reuse");

    assert_eq!(first, second);
    assert_eq!(first.architecture, Architecture::HOST);
    assert_eq!(first.source_alias, FIXTURE_ALIAS);
    assert_eq!(first.initrd, None);
    assert_eq!(first.boot_args, "console=ttyS0 root=/dev/vda rw");
    assert_eq!(
        first.kernel,
        std::path::PathBuf::from(format!(
            ".oci/kernel/{}/{}",
            Architecture::HOST.as_str(),
            published.image
        )),
        "the pair must name an image-root-relative path a TemplateSpec can record"
    );
    assert_eq!(
        std::fs::read(image_root.join(&first.kernel)).expect("cached kernel"),
        published.kernel
    );
    assert_eq!(
        published.registry.requests(),
        1,
        "a warm host must not contact the registry again"
    );
}

#[tokio::test]
async fn a_corrupt_cached_kernel_is_refetched() {
    let directory = tempdir().expect("create fixture directory");
    let image_root = directory.path();
    let published = PublishedKernel::publish(
        Architecture::HOST,
        kernel_bytes_for(Architecture::HOST),
        None,
    )
    .await;

    let first = ensure(&published, image_root).await.expect("first fetch");
    std::fs::write(image_root.join(&first.kernel), b"truncated").expect("corrupt the cache");
    let second = ensure(&published, image_root).await.expect("refetch");

    assert_eq!(first, second);
    assert_eq!(
        std::fs::read(image_root.join(&second.kernel)).expect("restored kernel"),
        published.kernel
    );
    assert_eq!(published.registry.requests(), 2);
}

#[tokio::test]
async fn a_package_that_does_not_match_the_pinned_digest_is_refused() {
    let directory = tempdir().expect("create fixture directory");
    let image_root = directory.path();
    let published = PublishedKernel::publish(
        Architecture::HOST,
        kernel_bytes_for(Architecture::HOST),
        None,
    )
    .await
    .with_foreign_pin(&Sha256Digest::of_bytes(b"another package").to_string());

    let error = ensure(&published, image_root)
        .await
        .expect_err("pinned digest mismatch");

    assert_matches!(
        error,
        ResolveError::DigestMismatch { .. },
        "expected DigestMismatch for a repointed package, got {error}"
    );
    assert!(
        !image_root
            .join(format!(
                ".oci/kernel/{}/{}",
                Architecture::HOST.as_str(),
                published.image
            ))
            .exists(),
        "a refused package must publish nothing"
    );
}

#[tokio::test]
async fn a_package_without_the_pinned_kernel_member_is_refused() {
    let directory = tempdir().expect("create fixture directory");
    let image_root = directory.path();
    let published = PublishedKernel::publish(
        Architecture::HOST,
        kernel_bytes_for(Architecture::HOST),
        Some("kernel/some-other-build"),
    )
    .await;

    let error = ensure(&published, image_root)
        .await
        .expect_err("missing member");

    match error {
        ResolveError::KernelPackageMemberMissing { member, .. } => {
            assert_eq!(member, format!("kernel/{}", published.image));
        }
        other => panic!("expected KernelPackageMemberMissing, got {other}"),
    }
}

#[tokio::test]
async fn a_kernel_built_for_another_architecture_is_refused() {
    let directory = tempdir().expect("create fixture directory");
    let image_root = directory.path();
    let published = PublishedKernel::publish(
        Architecture::HOST,
        kernel_bytes_for(Architecture::HOST.other()),
        None,
    )
    .await;

    let error = ensure(&published, image_root)
        .await
        .expect_err("foreign architecture");

    match error {
        ResolveError::KernelArchitectureMismatch { found, host, .. } => {
            assert_eq!(found, Architecture::HOST.other());
            assert_eq!(host, Architecture::HOST);
        }
        other => panic!("expected KernelArchitectureMismatch, got {other}"),
    }
    assert!(
        !image_root
            .join(format!(
                ".oci/kernel/{}/{}",
                Architecture::HOST.as_str(),
                published.image
            ))
            .exists(),
        "a foreign kernel must publish nothing"
    );
}

#[tokio::test]
async fn an_operator_supplied_kernel_is_published_without_contacting_the_registry() {
    let directory = tempdir().expect("create fixture directory");
    let image_root = directory.path();
    let published = PublishedKernel::publish(
        Architecture::HOST,
        kernel_bytes_for(Architecture::HOST),
        None,
    )
    .await;
    let supplied = directory.path().join("operator-vmlinux");
    std::fs::write(&supplied, &published.kernel).expect("write operator kernel");

    let pair = kernel::ensure_pinned_kernel(
        image_root,
        Architecture::HOST,
        &published.pinned(),
        Some(&supplied),
        Some(&published.registry.base_url),
    )
    .await
    .expect("operator override");

    assert_eq!(
        std::fs::read(image_root.join(&pair.kernel)).expect("published kernel"),
        published.kernel
    );
    assert_eq!(
        published.registry.requests(),
        0,
        "an operator-supplied kernel must not reach the network"
    );
}

#[tokio::test]
async fn an_operator_supplied_kernel_must_still_match_the_pinned_digest() {
    let directory = tempdir().expect("create fixture directory");
    let image_root = directory.path();
    let published = PublishedKernel::publish(
        Architecture::HOST,
        kernel_bytes_for(Architecture::HOST),
        None,
    )
    .await;
    let supplied = directory.path().join("operator-vmlinux");
    std::fs::write(&supplied, kernel_bytes_for(Architecture::HOST.other()))
        .expect("write operator kernel");

    let error = kernel::ensure_pinned_kernel(
        image_root,
        Architecture::HOST,
        &published.pinned(),
        Some(&supplied),
        None,
    )
    .await
    .expect_err("operator kernel is not the pinned one");

    assert_matches!(
        error,
        ResolveError::DigestMismatch { .. },
        "expected DigestMismatch for an operator file that is not the pin, got {error}"
    );
}

/// Accepts a connection and answers nothing, holding it until dropped.
///
/// A registry that stops mid-transfer looks exactly like this to the client:
/// the socket stays open and no byte ever arrives.
struct StalledRegistry {
    base_url: String,
    task: JoinHandle<()>,
}

impl Drop for StalledRegistry {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl StalledRegistry {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stalled registry");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("stalled registry address")
        );
        let task = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });
        Self { base_url, task }
    }
}

#[tokio::test(start_paused = true)]
async fn a_registry_that_answers_nothing_fails_instead_of_hanging() {
    let directory = tempdir().expect("create fixture directory");
    let image_root = directory.path();
    let published = PublishedKernel::publish(
        Architecture::HOST,
        kernel_bytes_for(Architecture::HOST),
        None,
    )
    .await;
    let stalled = StalledRegistry::start().await;

    let attempt = tokio::time::timeout(
        std::time::Duration::from_secs(600),
        kernel::ensure_pinned_kernel(
            image_root,
            Architecture::HOST,
            &published.pinned(),
            None,
            Some(&stalled.base_url),
        ),
    )
    .await
    .expect("an import must not wait on a stalled registry forever");

    assert_matches!(
        attempt.expect_err("a stalled registry cannot supply a kernel"),
        ResolveError::KernelDownloadFailed { .. },
        "expected KernelDownloadFailed once the read timeout elapses"
    );
}

#[tokio::test]
async fn an_operator_path_that_is_not_a_regular_file_is_refused() {
    let directory = tempdir().expect("create fixture directory");
    let image_root = directory.path();
    let published = PublishedKernel::publish(
        Architecture::HOST,
        kernel_bytes_for(Architecture::HOST),
        None,
    )
    .await;
    let supplied = directory.path().join("kernel-directory");
    std::fs::create_dir(&supplied).expect("create operator directory");

    let error = kernel::ensure_pinned_kernel(
        image_root,
        Architecture::HOST,
        &published.pinned(),
        Some(&supplied),
        None,
    )
    .await
    .expect_err("a directory is not a kernel");

    assert_matches!(
        error,
        ResolveError::KernelIo { .. },
        "expected KernelIo for an operator path that is not a file, got {error}"
    );
}

#[tokio::test]
async fn a_registry_that_does_not_publish_the_pinned_version_is_reported() {
    let directory = tempdir().expect("create fixture directory");
    let image_root = directory.path();
    let published = PublishedKernel::publish(
        Architecture::HOST,
        kernel_bytes_for(Architecture::HOST),
        None,
    )
    .await;
    let mut pinned = published.pinned();
    pinned.version = "0.0.0";

    let error = kernel::ensure_pinned_kernel(
        image_root,
        Architecture::HOST,
        &pinned,
        None,
        Some(&published.registry.base_url),
    )
    .await
    .expect_err("the pinned version is not published");

    match error {
        ResolveError::KernelDownloadFailed { url, message } => {
            assert!(
                url.ends_with("kernel/0.0.0/x86_64/vmlinux-9.9.9.tar.zst")
                    || url.ends_with("kernel/0.0.0/aarch64/vmlinux-9.9.9.tar.zst")
            );
            assert_eq!(message, "HTTP 404");
        }
        other => panic!("expected KernelDownloadFailed, got {other}"),
    }
}

#[test]
fn the_kernel_source_defaults_to_the_public_registry_and_no_override() {
    assert_eq!(
        kernel::configured_base_url().as_deref(),
        Some(crate::image_install::DEFAULT_IMAGE_BASE_URL),
        "an unconfigured host fetches the kernel from the public MicroRegistry"
    );
    assert!(kernel::configured_kernel_override().is_none());
}

#[tokio::test]
async fn a_host_with_no_kernel_source_is_told_so() {
    let directory = tempdir().expect("create fixture directory");
    let image_root = directory.path();
    let published = PublishedKernel::publish(
        Architecture::HOST,
        kernel_bytes_for(Architecture::HOST),
        None,
    )
    .await;

    let error = kernel::ensure_pinned_kernel(
        image_root,
        Architecture::HOST,
        &published.pinned(),
        None,
        None,
    )
    .await
    .expect_err("no configured source");

    assert_matches!(
        error,
        ResolveError::KernelUnavailable { .. },
        "expected KernelUnavailable when remote install is disabled, got {error}"
    );
}

#[test]
fn the_compiled_pins_name_the_latest_stable_registry_artifacts() {
    let expected = [
        (
            Architecture::X86_64,
            "vmlinux-7.2.2-x86_64",
            "kernel/7.2.2/x86_64/vmlinux-7.2.2.tar.zst",
            false,
        ),
        (
            Architecture::Aarch64,
            "Image-7.2.2-aarch64",
            "kernel/7.2.2/aarch64/vmlinux-7.2.2.tar.zst",
            true,
        ),
    ];

    for (architecture, image, package_key, needs_keep_bootcon) in expected {
        let pinned = kernel::pinned_kernel(architecture)
            .unwrap_or_else(|| panic!("{architecture} kernel is published"));

        assert_eq!(pinned.alias, "vmlinux-7.2.2");
        assert_eq!(pinned.version, "7.2.2");
        assert_eq!(pinned.image, image);
        assert_eq!(kernel::package_key(architecture, &pinned), package_key);
        Sha256Digest::parse(pinned.package_digest).expect("package digest is pinned, not a tag");
        Sha256Digest::parse(pinned.image_digest).expect("kernel digest is pinned, not a tag");
        assert!(pinned.boot_args.contains("root=/dev/vda"));
        assert!(pinned.boot_args.contains("console=ttyS0"));
        assert_eq!(
            pinned.boot_args.contains("keep_bootcon"),
            needs_keep_bootcon
        );
    }
}
