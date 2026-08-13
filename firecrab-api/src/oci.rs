//! OCI registry access for image import (`public-docs/images.md`).

use serde::Deserialize;
use thiserror::Error;

use crate::image_install::Architecture;

/// The OS every Firecrab guest runs. Windows and BSD entries in a multi-OS
/// index are never candidates.
const LINUX: &str = "linux";
/// Buildx marks SBOM and signature attachments with this placeholder
/// platform instead of omitting them from the index.
const ATTESTATION_PLATFORM: &str = "unknown";

/// Docker Hub's registry host. A bare `nginx` resolves here.
const DOCKER_HUB_REGISTRY: &str = "registry-1.docker.io";
/// The namespace Docker Hub gives its own official images.
const DOCKER_HUB_LIBRARY: &str = "library";
/// `docker.io` is the name users type; it is not the host that serves the
/// registry API, so it is rewritten rather than used directly.
const DOCKER_HUB_ALIAS: &str = "docker.io";
/// Length of a `sha256:` digest's hex body.
const SHA256_HEX_LENGTH: usize = 64;

/// Why an image reference could not be resolved to something pullable.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ReferenceError {
    /// The reference was empty or only whitespace.
    #[error("image reference is empty")]
    Empty,
    /// A path component between slashes was empty.
    #[error("image reference has an empty path component: {0}")]
    EmptyComponent(String),
    /// A repository component used characters the distribution spec forbids.
    #[error("image repository must be lowercase alphanumeric with . _ - separators: {0}")]
    InvalidRepository(String),
    /// The tag after `:` was empty.
    #[error("image reference has an empty tag: {0}")]
    EmptyTag(String),
    /// The digest after `@` was empty, not `sha256:`, or the wrong length.
    #[error("image digest must be sha256 with {SHA256_HEX_LENGTH} hex characters: {0}")]
    InvalidDigest(String),
    /// Both a tag and a digest were given.
    #[error("image reference cannot carry both a tag and a digest: {0}")]
    TagAndDigest(String),
}

/// Which revision of a repository to pull.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageVersion {
    /// A mutable tag. The registry may repoint it at any time.
    Tag(String),
    /// A content digest, which always names the same bytes.
    Digest(String),
}

impl ImageVersion {
    /// Whether this version can never be repointed at different content.
    /// Only a digest gives that; a tag is a moving target.
    pub fn is_immutable(&self) -> bool {
        matches!(self, Self::Digest(_))
    }

    /// The form that goes in a registry manifest URL path.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Tag(tag) => tag,
            Self::Digest(digest) => digest,
        }
    }
}

/// A parsed `[registry/]repository[:tag|@digest]` image reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageReference {
    /// Registry host, already rewritten to the one serving the registry API.
    pub registry: String,
    /// Repository path, with Docker Hub's implicit `library/` filled in.
    pub repository: String,
    /// The tag or digest to pull.
    pub version: ImageVersion,
}

impl ImageReference {
    /// Parses a reference the way `docker pull` does.
    pub fn parse(reference: &str) -> Result<Self, ReferenceError> {
        let reference = reference.trim();
        if reference.is_empty() {
            return Err(ReferenceError::Empty);
        }

        let (name, version) = split_version(reference)?;
        let (registry, path) = split_registry(name);
        let repository = qualify(&registry, path)?;

        Ok(Self {
            registry,
            repository,
            version,
        })
    }
}

/// Splits the trailing `:tag` or `@digest` off the name.
///
/// The tag search starts after the last `/` so a registry port (`host:5000`)
/// is never mistaken for one.
fn split_version(reference: &str) -> Result<(&str, ImageVersion), ReferenceError> {
    if let Some((name, digest)) = reference.split_once('@') {
        if name.contains(':') && name.rfind(':') > name.rfind('/') {
            return Err(ReferenceError::TagAndDigest(reference.to_owned()));
        }
        let hex = digest
            .strip_prefix("sha256:")
            .ok_or_else(|| ReferenceError::InvalidDigest(reference.to_owned()))?;
        if hex.len() != SHA256_HEX_LENGTH || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ReferenceError::InvalidDigest(reference.to_owned()));
        }
        return Ok((name, ImageVersion::Digest(digest.to_owned())));
    }

    let last_slash = reference.rfind('/');
    if let Some(colon) = reference.rfind(':')
        && last_slash.is_none_or(|slash| colon > slash)
    {
        let (name, tag) = reference.split_at(colon);
        let tag = &tag[1..];
        if tag.is_empty() {
            return Err(ReferenceError::EmptyTag(reference.to_owned()));
        }
        return Ok((name, ImageVersion::Tag(tag.to_owned())));
    }

    Ok((reference, ImageVersion::Tag("latest".to_owned())))
}

/// Splits an optional registry host off the front.
///
/// A first component counts as a host only when it carries a dot or a port,
/// or is `localhost`; otherwise `myuser/app` would read as host `myuser`.
fn split_registry(name: &str) -> (String, &str) {
    if let Some((head, rest)) = name.split_once('/')
        && (head.contains('.') || head.contains(':') || head == "localhost")
    {
        let registry = if head == DOCKER_HUB_ALIAS {
            DOCKER_HUB_REGISTRY.to_owned()
        } else {
            head.to_owned()
        };
        return (registry, rest);
    }
    (DOCKER_HUB_REGISTRY.to_owned(), name)
}

/// Validates the repository path and fills in Docker Hub's implicit
/// `library/` namespace for single-component names.
fn qualify(registry: &str, path: &str) -> Result<String, ReferenceError> {
    if path.is_empty() {
        return Err(ReferenceError::EmptyComponent(path.to_owned()));
    }
    for component in path.split('/') {
        if component.is_empty() {
            return Err(ReferenceError::EmptyComponent(path.to_owned()));
        }
        if !is_valid_component(component) {
            return Err(ReferenceError::InvalidRepository(path.to_owned()));
        }
    }

    if registry == DOCKER_HUB_REGISTRY && !path.contains('/') {
        return Ok(format!("{DOCKER_HUB_LIBRARY}/{path}"));
    }
    Ok(path.to_owned())
}

/// Why no manifest in an index could be pulled.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum IndexError {
    /// The index has Linux manifests, but none for the wanted architecture.
    #[error("image has no {wanted} manifest; it offers {}", available.join(", "))]
    NoMatchingArchitecture {
        /// The OCI platform name that was searched for.
        wanted: &'static str,
        /// The OCI platform names the index does carry, for the operator.
        available: Vec<String>,
    },
    /// The index carries nothing bootable — empty, or attestations only.
    #[error("image index contains no Linux manifests")]
    NoLinuxManifests {
        /// How many entries were skipped, to distinguish empty from filtered.
        skipped: usize,
    },
}

/// The OCI platform name for an architecture.
///
/// This is a *third* architecture vocabulary, after the registry labels
/// [`Architecture::as_str`] returns and the Debian names in rootfs filenames.
/// OCI uses Go's `GOARCH`, so x86_64 is `amd64` here and `x86_64` nowhere.
const fn oci_platform(architecture: Architecture) -> &'static str {
    match architecture {
        Architecture::X86_64 => "amd64",
        Architecture::Aarch64 => "arm64",
    }
}

/// The platform an index entry declares.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Platform {
    /// Go's `GOARCH` name, e.g. `amd64`.
    pub architecture: String,
    /// Go's `GOOS` name, e.g. `linux`.
    pub os: String,
}

/// One manifest inside an index.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManifestDescriptor {
    /// Content digest of the manifest this entry points at.
    pub digest: String,
    /// Size of that manifest in bytes.
    #[serde(default)]
    pub size: u64,
    /// Absent on a single-platform entry, which cannot be selected by
    /// architecture and is therefore never a candidate.
    #[serde(default)]
    pub platform: Option<Platform>,
}

impl ManifestDescriptor {
    /// Whether this entry is a real Linux image manifest rather than an
    /// attestation attachment or another OS.
    fn is_linux_image(&self) -> bool {
        self.platform.as_ref().is_some_and(|platform| {
            platform.os == LINUX && platform.architecture != ATTESTATION_PLATFORM
        })
    }
}

/// A parsed OCI image index (or Docker manifest list — same shape).
#[derive(Debug, Clone, Deserialize)]
pub struct ImageIndex {
    /// The per-platform manifests this index offers.
    #[serde(default)]
    pub manifests: Vec<ManifestDescriptor>,
}

impl ImageIndex {
    /// Picks the manifest for `architecture`, or explains what the image has
    /// instead. Firecracker cannot emulate, so a near miss is still a miss.
    pub fn select(&self, architecture: Architecture) -> Result<&ManifestDescriptor, IndexError> {
        let wanted = oci_platform(architecture);
        let linux: Vec<&ManifestDescriptor> = self
            .manifests
            .iter()
            .filter(|descriptor| descriptor.is_linux_image())
            .collect();

        if linux.is_empty() {
            return Err(IndexError::NoLinuxManifests {
                skipped: self.manifests.len(),
            });
        }
        linux
            .iter()
            .find(|descriptor| {
                descriptor
                    .platform
                    .as_ref()
                    .is_some_and(|platform| platform.architecture == wanted)
            })
            .copied()
            .ok_or_else(|| IndexError::NoMatchingArchitecture {
                wanted,
                available: linux
                    .iter()
                    .filter_map(|descriptor| descriptor.platform.as_ref())
                    .map(|platform| platform.architecture.clone())
                    .collect(),
            })
    }
}

/// Media types a manifest request accepts. Both index forms are listed so a
/// Docker-native registry answers with its manifest list rather than picking
/// a platform for us.
const MANIFEST_ACCEPT: &str = "application/vnd.oci.image.index.v1+json, \
     application/vnd.docker.distribution.manifest.list.v2+json, \
     application/vnd.oci.image.manifest.v1+json, \
     application/vnd.docker.distribution.manifest.v2+json";
/// Cap on a manifest document, which is metadata and never large.
const MANIFEST_MAX_BYTES: usize = 4 * 1024 * 1024;

/// What a reference resolved to on this host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedImage {
    /// Digest of the manifest to pull next.
    pub digest: String,
    /// The architecture that manifest runs.
    pub architecture: Architecture,
    /// True when the registry answered with a manifest instead of an index,
    /// so no platform selection happened and the digest is the reference's own.
    pub single_platform: bool,
}

/// Why a reference could not be resolved against its registry.
#[derive(Debug, Error)]
pub enum ResolveError {
    /// The registry could not be reached or answered malformed bytes.
    #[error("registry request failed: {0}")]
    Transport(String),
    /// The registry answered with a status the pull flow cannot use.
    #[error("registry answered {status} for {reference}")]
    Status {
        /// The HTTP status.
        status: u16,
        /// The reference being resolved, for the operator.
        reference: String,
    },
    /// The manifest document did not parse.
    #[error("registry returned an unreadable manifest: {0}")]
    Malformed(String),
    /// The index parsed but offers nothing this host can run.
    #[error(transparent)]
    Index(#[from] IndexError),
}

/// A `WWW-Authenticate: Bearer` challenge's token endpoint.
fn token_request(challenge: &str, base: &str) -> Option<String> {
    let parameters = challenge.strip_prefix("Bearer ")?;
    let mut realm = None;
    let mut query = Vec::new();
    for parameter in parameters.split(',') {
        let (key, value) = parameter.trim().split_once('=')?;
        let value = value.trim_matches('"');
        match key {
            // The challenge's own realm host is not trusted: a registry under
            // test answers on a caller-chosen port, and a public one always
            // names itself. Only the path is taken.
            "realm" => realm = value.rsplit_once("/token").map(|_| format!("{base}/token")),
            "service" | "scope" => query.push(format!("{key}={value}")),
            _ => {}
        }
    }
    Some(format!("{}?{}", realm?, query.join("&")))
}

/// Whether a registry host is on this machine's loopback interface.
///
/// A local development registry is normally run without TLS, so loopback is
/// the one place plain HTTP is used. Any other host must present a
/// certificate — a registry reached over the network decides which bytes end
/// up inside a VM's root filesystem.
pub fn is_loopback_registry(registry: &str) -> bool {
    let host = registry.rsplit_once(':').map_or(registry, |(host, _)| host);
    matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1")
}

/// Resolves a reference to the manifest digest this host should pull.
///
/// Performs the anonymous token dance every public registry uses: the first
/// request is unauthenticated, a `401` carries the challenge, and the reissued
/// request carries the bearer token.
pub async fn resolve(
    reference: &ImageReference,
    architecture: Architecture,
    insecure: bool,
) -> Result<ResolvedImage, ResolveError> {
    let scheme = if insecure { "http" } else { "https" };
    let base = format!("{scheme}://{}", reference.registry);
    let url = format!(
        "{base}/v2/{}/manifests/{}",
        reference.repository,
        reference.version.as_str()
    );
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|error| ResolveError::Transport(error.to_string()))?;

    let send = async |token: Option<&str>| {
        let mut request = client.get(&url).header("accept", MANIFEST_ACCEPT);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request.send().await
    };

    let mut response = send(None::<&str>)
        .await
        .map_err(|error| ResolveError::Transport(error.to_string()))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        let challenge = response
            .headers()
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let token_url = token_request(&challenge, &base).ok_or_else(|| {
            ResolveError::Transport("registry sent an unusable auth challenge".to_owned())
        })?;
        // reqwest is built without its `json` feature here, so the token
        // document is decoded from bytes like every other body in this file.
        let issued = client
            .get(&token_url)
            .send()
            .await
            .map_err(|error| ResolveError::Transport(error.to_string()))?
            .bytes()
            .await
            .map_err(|error| ResolveError::Transport(error.to_string()))?;
        let token: TokenResponse = serde_json::from_slice(&issued)
            .map_err(|error| ResolveError::Transport(error.to_string()))?;
        response = send(Some(token.token.as_str()))
            .await
            .map_err(|error| ResolveError::Transport(error.to_string()))?;
    }

    if !response.status().is_success() {
        return Err(ResolveError::Status {
            status: response.status().as_u16(),
            reference: format!("{}:{}", reference.repository, reference.version.as_str()),
        });
    }

    let body = response
        .bytes()
        .await
        .map_err(|error| ResolveError::Transport(error.to_string()))?;
    if body.len() > MANIFEST_MAX_BYTES {
        return Err(ResolveError::Malformed("manifest exceeds limit".to_owned()));
    }

    let index: ImageIndex = serde_json::from_slice(&body)
        .map_err(|error| ResolveError::Malformed(error.to_string()))?;
    // A single-platform repository answers with the manifest itself, which
    // carries no `manifests` array. There is nothing to select there.
    if index.manifests.is_empty() {
        return Ok(ResolvedImage {
            digest: reference.version.as_str().to_owned(),
            architecture,
            single_platform: true,
        });
    }

    let selected = index.select(architecture)?;
    Ok(ResolvedImage {
        digest: selected.digest.clone(),
        architecture,
        single_platform: false,
    })
}

/// A registry token endpoint's answer.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    token: String,
}

/// One path component, per the distribution spec: lowercase alphanumerics
/// with `.`, `_`, `-` as separators between them.
fn is_valid_component(component: &str) -> bool {
    let bytes = component.as_bytes();
    let alphanumeric = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    if !alphanumeric(bytes[0]) || !alphanumeric(bytes[bytes.len() - 1]) {
        return false;
    }
    bytes
        .iter()
        .all(|&byte| alphanumeric(byte) || matches!(byte, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(reference: &str) -> ImageReference {
        ImageReference::parse(reference).expect(reference)
    }

    /// A bare name is Docker Hub's `library/` namespace at `latest`. Getting
    /// this wrong sends every unqualified pull to the wrong repository.
    #[test]
    fn parse_resolves_docker_hub_shorthand() {
        let cases: [(&str, &str, &str, ImageVersion); 4] = [
            (
                "nginx",
                "registry-1.docker.io",
                "library/nginx",
                ImageVersion::Tag("latest".to_owned()),
            ),
            (
                "nginx:1.27",
                "registry-1.docker.io",
                "library/nginx",
                ImageVersion::Tag("1.27".to_owned()),
            ),
            (
                "myuser/app",
                "registry-1.docker.io",
                "myuser/app",
                ImageVersion::Tag("latest".to_owned()),
            ),
            (
                "docker.io/library/alpine:3.24",
                "registry-1.docker.io",
                "library/alpine",
                ImageVersion::Tag("3.24".to_owned()),
            ),
        ];

        for (input, registry, repository, version) in cases {
            let parsed = parse(input);
            assert_eq!(parsed.registry, registry, "{input}");
            assert_eq!(parsed.repository, repository, "{input}");
            assert_eq!(parsed.version, version, "{input}");
        }
    }

    /// A first component is a registry host only when it looks like one — it
    /// carries a dot or a port, or is `localhost`. Otherwise `myuser/app`
    /// would be read as host `myuser`.
    #[test]
    fn parse_tells_a_registry_host_from_a_namespace() {
        let cases: [(&str, &str, &str); 3] = [
            ("ghcr.io/owner/repo", "ghcr.io", "owner/repo"),
            ("localhost:5000/app", "localhost:5000", "app"),
            (
                "registry.example.com/team/app",
                "registry.example.com",
                "team/app",
            ),
        ];

        for (input, registry, repository) in cases {
            let parsed = parse(input);
            assert_eq!(parsed.registry, registry, "{input}");
            assert_eq!(parsed.repository, repository, "{input}");
        }
    }

    #[test]
    fn parse_reads_a_digest_pin() {
        let digest = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let parsed = parse(&format!("ghcr.io/owner/repo@{digest}"));

        assert_eq!(parsed.repository, "owner/repo");
        assert_eq!(parsed.version, ImageVersion::Digest(digest.to_owned()));
    }

    /// A digest pin is the only form that survives a mutable tag being moved,
    /// so the caller has to be able to ask which one it got.
    #[test]
    fn parse_marks_whether_the_reference_is_immutable() {
        let digest = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

        assert!(parse(&format!("nginx@{digest}")).version.is_immutable());
        assert!(!parse("nginx:1.27").version.is_immutable());
    }

    fn index(manifests: serde_json::Value) -> ImageIndex {
        serde_json::from_value(serde_json::json!({
            "schemaVersion": 2,
            "manifests": manifests
        }))
        .expect("index fixture")
    }

    fn entry(digest: &str, architecture: &str, os: &str) -> serde_json::Value {
        serde_json::json!({
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": digest,
            "size": 1024,
            "platform": { "architecture": architecture, "os": os }
        })
    }

    /// OCI platforms use Go's names — `amd64`/`arm64` — which are neither the
    /// registry labels nor anything Firecracker prints. Matching them by the
    /// wrong spelling silently picks another architecture's manifest.
    #[test]
    fn select_picks_the_manifest_for_the_requested_architecture() {
        let index = index(serde_json::json!([
            entry("sha256:aa", "amd64", "linux"),
            entry("sha256:bb", "arm64", "linux"),
        ]));

        assert_eq!(
            index.select(Architecture::X86_64).unwrap().digest,
            "sha256:aa"
        );
        assert_eq!(
            index.select(Architecture::Aarch64).unwrap().digest,
            "sha256:bb"
        );
    }

    /// Buildx attaches SBOM and signature entries with a placeholder
    /// platform. Treating one as a real manifest pulls a blob that is not a
    /// root filesystem at all.
    #[test]
    fn select_skips_attestation_and_non_linux_entries() {
        let index = index(serde_json::json!([
            entry("sha256:att", "unknown", "unknown"),
            entry("sha256:win", "amd64", "windows"),
            serde_json::json!({ "digest": "sha256:bare", "size": 1 }),
            entry("sha256:real", "amd64", "linux"),
        ]));

        assert_eq!(
            index.select(Architecture::X86_64).unwrap().digest,
            "sha256:real"
        );
    }

    /// The operator needs to know what the image *does* offer, otherwise the
    /// only next step is guessing.
    #[test]
    fn select_reports_the_architectures_the_image_does_offer() {
        let index = index(serde_json::json!([
            entry("sha256:aa", "arm64", "linux"),
            entry("sha256:bb", "riscv64", "linux"),
            entry("sha256:att", "unknown", "unknown"),
        ]));

        let error = index.select(Architecture::X86_64).unwrap_err();

        let message = error.to_string();
        assert!(message.contains("arm64"), "{message}");
        assert!(message.contains("riscv64"), "{message}");
        assert!(!message.contains("unknown"), "{message}");
    }

    #[test]
    fn select_reports_an_index_with_nothing_usable() {
        let empty = index(serde_json::json!([]));

        assert!(matches!(
            empty.select(Architecture::HOST).unwrap_err(),
            IndexError::NoLinuxManifests { skipped: 0 }
        ));
    }

    /// A registry that answers `401` with a `Bearer` challenge, hands out a
    /// token, and only then serves the index — the anonymous pull flow every
    /// public registry uses.
    async fn token_guarded_registry(body: serde_json::Value) -> String {
        use axum::http::{HeaderMap, StatusCode};
        use axum::response::IntoResponse;
        use axum::routing::get;

        let app = axum::Router::new()
            .route(
                "/token",
                get(|| async { axum::Json(serde_json::json!({ "token": "issued-token" })) }),
            )
            .route(
                "/v2/library/nginx/manifests/1.27",
                get(move |headers: HeaderMap| {
                    let body = body.clone();
                    async move {
                        if headers.get("authorization").map(|value| value.as_bytes())
                            != Some(b"Bearer issued-token".as_slice())
                        {
                            return (
                                StatusCode::UNAUTHORIZED,
                                [(
                                    "www-authenticate",
                                    "Bearer realm=\"{base}/token\",service=\"registry\",\
                                     scope=\"repository:library/nginx:pull\"",
                                )],
                            )
                                .into_response();
                        }
                        axum::Json(body).into_response()
                    }
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("127.0.0.1:{}", address.port())
    }

    /// The whole point of the endpoint: say whether this host can run the
    /// image before anything is downloaded.
    #[tokio::test]
    async fn resolve_authenticates_then_selects_the_hosts_manifest() {
        let registry = token_guarded_registry(serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [
                entry("sha256:amd", "amd64", "linux"),
                entry("sha256:arm", "arm64", "linux"),
            ]
        }))
        .await;
        let reference = ImageReference {
            registry,
            repository: "library/nginx".to_owned(),
            version: ImageVersion::Tag("1.27".to_owned()),
        };

        let resolved = resolve(&reference, Architecture::X86_64, true)
            .await
            .unwrap();

        assert_eq!(resolved.digest, "sha256:amd");
        assert_eq!(resolved.architecture, Architecture::X86_64);
    }

    /// A single-platform repository answers with the manifest itself, not an
    /// index. There is nothing to select, so the registry's own choice stands.
    #[tokio::test]
    async fn resolve_accepts_a_registry_that_answers_with_one_manifest() {
        let registry = token_guarded_registry(serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": { "digest": "sha256:cfg", "size": 7 },
            "layers": [{ "digest": "sha256:layer", "size": 99 }]
        }))
        .await;
        let reference = ImageReference {
            registry,
            repository: "library/nginx".to_owned(),
            version: ImageVersion::Tag("1.27".to_owned()),
        };

        let resolved = resolve(&reference, Architecture::HOST, true).await.unwrap();

        assert!(resolved.single_platform);
    }

    #[test]
    fn parse_rejects_references_it_cannot_resolve() {
        let cases = [
            "",
            "   ",
            "nginx:",
            "nginx@",
            "nginx@sha256:short",
            "nginx@md5:0123456789abcdef0123456789abcdef",
            "ghcr.io/",
            "/nginx",
            "nginx:tag@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "NGINX",
        ];

        for input in cases {
            assert!(
                ImageReference::parse(input).is_err(),
                "{input} must not parse"
            );
        }
    }
}
