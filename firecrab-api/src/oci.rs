//! OCI registry access for image import (`public-docs/images.md`).

use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read as _, Seek as _};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::task::{Context, Poll};
use std::time::Duration;

use async_compression::tokio::bufread::{GzipDecoder, ZstdDecoder};
use futures_util::{StreamExt, TryStreamExt, stream};
use serde::{Deserialize, Deserializer};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader, ReadBuf};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use uuid::Uuid;

use crate::image_install::{Architecture, ImageInstallTracker};
use crate::templates::TemplateRegistry;

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

/// A validated OCI SHA-256 digest safe to use in registry URLs and cache paths.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    /// Parses `sha256:<64 hex characters>` and normalizes the hex to lowercase.
    pub fn parse(value: &str) -> Result<Self, DigestError> {
        let Some((algorithm, encoded)) = value.split_once(':') else {
            return Err(DigestError::MissingAlgorithm(value.to_owned()));
        };
        if algorithm != "sha256" {
            return Err(DigestError::UnsupportedAlgorithm(algorithm.to_owned()));
        }
        if encoded.len() != SHA256_HEX_LENGTH {
            return Err(DigestError::InvalidLength {
                value: value.to_owned(),
                expected: SHA256_HEX_LENGTH,
                actual: encoded.len(),
            });
        }
        if !encoded.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(DigestError::InvalidEncoding(value.to_owned()));
        }
        Ok(Self(format!("sha256:{}", encoded.to_ascii_lowercase())))
    }

    /// Complete descriptor spelling (`sha256:<hex>`).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Hash body alone, used as the filename under `blobs/sha256/`.
    pub fn encoded(&self) -> &str {
        &self.0["sha256:".len()..]
    }

    /// Computes the digest of an in-memory document.
    fn of_bytes(bytes: &[u8]) -> Self {
        Self(format!("sha256:{:x}", Sha256::digest(bytes)))
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Sha256Digest {
    type Err = DigestError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Why an OCI content digest is unsafe or unsupported.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DigestError {
    /// A descriptor did not contain the required `algorithm:encoded` separator.
    #[error("digest is missing its algorithm: {0}")]
    MissingAlgorithm(String),
    /// The MVP accepts only SHA-256 content addresses.
    #[error("unsupported digest algorithm {0:?}; only sha256 is supported")]
    UnsupportedAlgorithm(String),
    /// A SHA-256 digest had the wrong encoded length.
    #[error("sha256 digest must have {expected} hex characters, got {actual}: {value}")]
    InvalidLength {
        /// Original digest text.
        value: String,
        /// Required SHA-256 hex length.
        expected: usize,
        /// Supplied hex length.
        actual: usize,
    },
    /// The encoded digest used a character outside hexadecimal.
    #[error("sha256 digest contains non-hex characters: {0}")]
    InvalidEncoding(String),
}

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
    Digest(Sha256Digest),
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
            Self::Digest(digest) => digest.as_str(),
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
        let digest = Sha256Digest::parse(digest)
            .map_err(|_| ReferenceError::InvalidDigest(reference.to_owned()))?;
        return Ok((name, ImageVersion::Digest(digest)));
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

/// OCI metadata common to manifests, configuration documents, and layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    /// MIME type describing the referenced content.
    pub media_type: String,
    /// Cryptographic address of the exact registry bytes.
    pub digest: Sha256Digest,
    /// Exact byte length of the referenced content.
    pub size: u64,
}

/// One manifest inside an index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestDescriptor {
    /// Content metadata for the selected image manifest.
    pub descriptor: Descriptor,
    /// Absent on a single-platform entry, which cannot be selected by
    /// architecture and is therefore never a candidate.
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
#[derive(Debug, Clone)]
pub struct ImageIndex {
    /// OCI/Docker schema revision; only schema 2 is understood.
    pub schema_version: u32,
    /// Declared index media type. The HTTP content type is used when omitted.
    pub media_type: String,
    /// The per-platform manifests this index offers.
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

/// A resolved single-platform OCI or Docker image manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageManifest {
    /// OCI/Docker schema revision; only schema 2 is understood.
    pub schema_version: u32,
    /// Declared manifest media type. The HTTP content type is used when omitted.
    pub media_type: String,
    /// Runtime configuration document for the image.
    pub config: Descriptor,
    /// Filesystem changesets in the order they must later be applied.
    pub layers: Vec<Descriptor>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDescriptor {
    media_type: String,
    digest: String,
    size: u64,
}

impl TryFrom<RawDescriptor> for Descriptor {
    type Error = DigestError;

    fn try_from(raw: RawDescriptor) -> Result<Self, Self::Error> {
        Ok(Self {
            media_type: raw.media_type,
            digest: Sha256Digest::parse(&raw.digest)?,
            size: raw.size,
        })
    }
}

#[derive(Debug, Deserialize)]
struct RawManifestDescriptor {
    #[serde(flatten)]
    descriptor: RawDescriptor,
    #[serde(default)]
    platform: Option<Platform>,
}

#[derive(Debug, Deserialize)]
struct RawImageIndex {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    #[serde(default, rename = "mediaType")]
    media_type: String,
    manifests: Vec<RawManifestDescriptor>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawImageManifest {
    schema_version: u32,
    #[serde(default)]
    media_type: String,
    config: RawDescriptor,
    layers: Vec<RawDescriptor>,
}

#[derive(Debug, Deserialize)]
struct RawImageConfiguration {
    rootfs: RawRootFilesystem,
}

#[derive(Debug, Deserialize)]
struct RawRootFilesystem {
    #[serde(rename = "type")]
    kind: String,
    diff_ids: Vec<Sha256Digest>,
}

const OCI_INDEX_MEDIA_TYPE: &str = "application/vnd.oci.image.index.v1+json";
const DOCKER_INDEX_MEDIA_TYPE: &str = "application/vnd.docker.distribution.manifest.list.v2+json";
const OCI_MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
const DOCKER_MANIFEST_MEDIA_TYPE: &str = "application/vnd.docker.distribution.manifest.v2+json";
const OCI_CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.image.config.v1+json";
const DOCKER_CONFIG_MEDIA_TYPE: &str = "application/vnd.docker.container.image.v1+json";
const OCI_LAYER_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar";
const OCI_LAYER_GZIP_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
const OCI_LAYER_ZSTD_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar+zstd";
const OCI_NONDISTRIBUTABLE_LAYER_MEDIA_TYPE: &str =
    "application/vnd.oci.image.layer.nondistributable.v1.tar";
const OCI_NONDISTRIBUTABLE_LAYER_GZIP_MEDIA_TYPE: &str =
    "application/vnd.oci.image.layer.nondistributable.v1.tar+gzip";
const OCI_NONDISTRIBUTABLE_LAYER_ZSTD_MEDIA_TYPE: &str =
    "application/vnd.oci.image.layer.nondistributable.v1.tar+zstd";
const DOCKER_LAYER_MEDIA_TYPE: &str = "application/vnd.docker.image.rootfs.diff.tar";
const DOCKER_LAYER_GZIP_MEDIA_TYPE: &str = "application/vnd.docker.image.rootfs.diff.tar.gzip";
const DOCKER_FOREIGN_LAYER_GZIP_MEDIA_TYPE: &str =
    "application/vnd.docker.image.rootfs.foreign.diff.tar.gzip";

/// Whether a descriptor points at an image manifest rather than an artifact.
fn is_manifest_media_type(media_type: &str) -> bool {
    matches!(
        media_type,
        OCI_MANIFEST_MEDIA_TYPE | DOCKER_MANIFEST_MEDIA_TYPE
    )
}

/// Whether a config descriptor can be translated by a later import stage.
fn is_config_media_type(media_type: &str) -> bool {
    matches!(media_type, OCI_CONFIG_MEDIA_TYPE | DOCKER_CONFIG_MEDIA_TYPE)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum LayerCompression {
    Identity,
    Gzip,
    Zstd,
}

impl LayerCompression {
    fn cache_name(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Gzip => "gzip",
            Self::Zstd => "zstd",
        }
    }
}

fn layer_compression(media_type: &str) -> Option<LayerCompression> {
    match media_type {
        OCI_LAYER_MEDIA_TYPE | OCI_NONDISTRIBUTABLE_LAYER_MEDIA_TYPE | DOCKER_LAYER_MEDIA_TYPE => {
            Some(LayerCompression::Identity)
        }
        OCI_LAYER_GZIP_MEDIA_TYPE
        | OCI_NONDISTRIBUTABLE_LAYER_GZIP_MEDIA_TYPE
        | DOCKER_LAYER_GZIP_MEDIA_TYPE
        | DOCKER_FOREIGN_LAYER_GZIP_MEDIA_TYPE => Some(LayerCompression::Gzip),
        OCI_LAYER_ZSTD_MEDIA_TYPE | OCI_NONDISTRIBUTABLE_LAYER_ZSTD_MEDIA_TYPE => {
            Some(LayerCompression::Zstd)
        }
        _ => None,
    }
}

/// Layer encodings whose raw bytes Firecrab can safely cache and unpack.
fn is_layer_media_type(media_type: &str) -> bool {
    layer_compression(media_type).is_some()
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
/// Token documents should contain only a short bearer credential and optional
/// metadata. Capping them prevents an authentication service from buffering an
/// arbitrary response before JSON decoding.
const TOKEN_MAX_BYTES: usize = 64 * 1024;
/// Default per-blob download ceiling. Operators with unusually large layers
/// can raise it through `FIRECRAB_OCI_MAX_BLOB_BYTES` without weakening the
/// descriptor and streamed-body checks below.
const DEFAULT_MAX_BLOB_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_BLOB_BYTES_ENV: &str = "FIRECRAB_OCI_MAX_BLOB_BYTES";
/// Image configuration is metadata and is parsed in memory before unpacking.
const CONFIG_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// A separate ceiling for decoder output prevents a small compressed layer
/// from expanding until it fills the image volume.
const DEFAULT_MAX_UNCOMPRESSED_LAYER_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_UNCOMPRESSED_LAYER_BYTES_ENV: &str = "FIRECRAB_OCI_MAX_UNCOMPRESSED_LAYER_BYTES";
/// libzstd's normal maximum window is 128 MiB. Setting it explicitly keeps a
/// hostile frame header from requesting an unexpectedly large decoder window.
const ZSTD_MAX_WINDOW_LOG: u32 = 27;
/// Decompression has a separate concurrency ceiling because each zstd decoder
/// may retain a 128 MiB window. Keeping at most two active bounds those windows
/// to 256 MiB instead of multiplying them by a many-core host's CPU count.
const MAX_PARALLEL_DECOMPRESSIONS: usize = 2;
/// GNU long-name and PAX records are buffered by the tar parser. Real
/// filesystem metadata is far smaller; this ceiling prevents a crafted layer
/// from turning an otherwise streamed preflight into an unbounded allocation.
const TAR_METADATA_MAX_BYTES: u64 = 1024 * 1024;

/// What a reference resolved to on this host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedImage {
    /// Digest of the manifest to pull next.
    pub digest: String,
    /// The architecture that manifest runs.
    pub architecture: Architecture,
    /// True when the registry answered with a manifest instead of an index,
    /// so no platform selection happened. The digest is still calculated from
    /// the response body when the original reference used a mutable tag.
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
    /// The registry's anonymous bearer-token exchange could not be completed.
    #[error("registry authentication failed: {0}")]
    Authentication(String),
    /// The manifest document did not parse.
    #[error("registry returned an unreadable manifest: {0}")]
    Malformed(String),
    /// A document or descriptor used a kind this importer cannot consume.
    #[error("unsupported OCI media type {0:?}")]
    UnsupportedMediaType(String),
    /// A descriptor or digest-pinned request named different content.
    #[error("digest mismatch for {subject}: expected {expected}, got {actual}")]
    DigestMismatch {
        /// Registry object being verified.
        subject: String,
        /// Digest named by the request or descriptor.
        expected: Sha256Digest,
        /// Digest calculated from the response or cache file.
        actual: Sha256Digest,
    },
    /// A registry object did not have the exact descriptor length.
    #[error("size mismatch for {subject}: expected {expected} bytes, got {actual}")]
    SizeMismatch {
        /// Registry object being verified.
        subject: String,
        /// Descriptor length.
        expected: u64,
        /// Actual or HTTP-advertised length.
        actual: u64,
    },
    /// A descriptor exceeds the operator's per-blob download policy.
    #[error("OCI blob {digest} declares {size} bytes, exceeding the {limit}-byte limit")]
    BlobTooLarge {
        /// Content address of the rejected blob.
        digest: Sha256Digest,
        /// Size declared by the manifest descriptor.
        size: u64,
        /// Configured maximum size of one downloaded blob.
        limit: u64,
    },
    /// The verified image configuration could not be interpreted.
    #[error("OCI image configuration is invalid: {0}")]
    MalformedConfig(String),
    /// OCI defines only the `layers` root filesystem model.
    #[error("unsupported OCI rootfs type {0:?}; expected \"layers\"")]
    UnsupportedRootfsType(String),
    /// The config must name one uncompressed digest for every manifest layer.
    #[error("OCI config has {actual} rootfs diff_ids but the manifest has {expected} layers")]
    DiffIdCountMismatch {
        /// Number of layer descriptors in the manifest.
        expected: usize,
        /// Number of uncompressed digests in the configuration.
        actual: usize,
    },
    /// A compressed layer stream could not be decoded completely.
    #[error("could not decompress OCI layer {digest} ({media_type}): {message}")]
    Decompression {
        /// Digest of the exact registry bytes being decoded.
        digest: Sha256Digest,
        /// Layer media type that selected the decoder.
        media_type: String,
        /// Codec or source-read failure.
        message: String,
    },
    /// Decoder output did not match the config's uncompressed digest.
    #[error(
        "diff ID mismatch for OCI layer {compressed_digest}: expected {expected}, got {actual}"
    )]
    DiffIdMismatch {
        /// Manifest digest of the compressed registry blob.
        compressed_digest: Sha256Digest,
        /// Uncompressed digest declared by the image configuration.
        expected: Sha256Digest,
        /// Digest calculated from the decoded tar stream.
        actual: Sha256Digest,
    },
    /// Decoder output exceeded the operator's per-layer policy.
    #[error(
        "OCI layer {compressed_digest} decompressed to more than the {limit}-byte limit ({actual} bytes observed)"
    )]
    UncompressedLayerTooLarge {
        /// Manifest digest of the compressed registry blob.
        compressed_digest: Sha256Digest,
        /// Configured maximum uncompressed size of one layer.
        limit: u64,
        /// Size observed from metadata or while streaming.
        actual: u64,
    },
    /// A verified layer is not a structurally readable tar stream.
    #[error("OCI layer {compressed_digest} is not a readable tar archive: {message}")]
    MalformedLayerArchive {
        /// Manifest digest of the compressed registry blob.
        compressed_digest: Sha256Digest,
        /// Tar parser or truncated-payload failure.
        message: String,
    },
    /// A tar entry could write outside the future extraction root or create an
    /// unsupported filesystem object.
    #[error("unsafe tar member {path:?} in OCI layer {compressed_digest}: {reason}")]
    UnsafeTarMember {
        /// Manifest digest of the compressed registry blob.
        compressed_digest: Sha256Digest,
        /// Effective entry path after GNU and PAX metadata is applied.
        path: PathBuf,
        /// Safety rule rejected by the entry.
        reason: TarMemberViolation,
    },
    /// A merge destination already exists and must not be replaced.
    #[error("OCI layer merge destination already exists at {path}")]
    MergeDestinationExists {
        /// Caller-selected final staging tree path.
        path: PathBuf,
    },
    /// A filesystem operation failed while constructing the merged tree.
    #[error("OCI layer merge {operation} failed at {path}: {source}")]
    MergeIo {
        /// Operation being attempted.
        operation: &'static str,
        /// Staging or published path involved.
        path: PathBuf,
        /// Operating-system failure.
        #[source]
        source: io::Error,
    },
    /// The caller stopped waiting before the private tree could be published.
    #[error("OCI layer merge was cancelled before publishing {path}")]
    MergeCancelled {
        /// Caller-selected final staging tree path.
        path: PathBuf,
    },
    /// The pinned guest toolbox image does not carry the program the guest
    /// runtime is built from.
    #[error("pinned guest toolbox {reference} has no {member} member")]
    ToolboxMissing {
        /// Reference the toolbox was pulled from.
        reference: String,
        /// Archive path expected to hold the program.
        member: &'static str,
    },
    /// The lifted toolbox program cannot serve as a guest init.
    #[error("guest toolbox program at {path} is unusable: {reason}")]
    ToolboxUnusable {
        /// Host path of the rejected program.
        path: PathBuf,
        /// Rule the program failed.
        reason: ToolboxViolation,
    },
    /// A guest path could not be resolved safely inside the merged tree.
    #[error("guest path {path} cannot be provisioned: {reason}")]
    GuestPathUnusable {
        /// Absolute guest path being provisioned.
        path: String,
        /// Rule the merged tree's shape broke.
        reason: GuestPathViolation,
    },
    /// A filesystem operation failed while injecting the guest runtime.
    #[error("OCI guest injection {operation} failed at {path}: {source}")]
    GuestInjectionIo {
        /// Operation being attempted.
        operation: &'static str,
        /// Tree or cache path involved.
        path: PathBuf,
        /// Operating-system failure.
        #[source]
        source: io::Error,
    },
    /// The caller stopped waiting before the guest runtime was complete.
    #[error("OCI guest injection was cancelled before provisioning {path}")]
    GuestInjectionCancelled {
        /// Merged tree that was left unprovisioned.
        path: PathBuf,
    },
    /// A filesystem operation failed while sizing or writing the ext4 image.
    #[error("OCI ext4 {operation} failed at {path}: {source}")]
    Ext4Io {
        /// Operation being attempted.
        operation: &'static str,
        /// Tree or image path involved.
        path: PathBuf,
        /// Operating-system failure.
        #[source]
        source: io::Error,
    },
    /// The ext4 destination already exists and must not be replaced.
    #[error("OCI ext4 destination already exists at {path}")]
    Ext4DestinationExists {
        /// Caller-selected final image path.
        path: PathBuf,
    },
    /// `mkfs.ext4` or `tune2fs` ran but did not produce a usable image.
    #[error("OCI ext4 image at {path} could not be built: {detail}")]
    Ext4Build {
        /// Image path being built.
        path: PathBuf,
        /// Tool diagnostic.
        detail: String,
    },
    /// The packed image has less free space than the required headroom.
    #[error(
        "OCI ext4 image at {path} is full after packing ({free_bytes} bytes free of {size_bytes}; {required_bytes} required)"
    )]
    Ext4Full {
        /// Image path that was rejected.
        path: PathBuf,
        /// Planned image length.
        size_bytes: u64,
        /// Free space reported after packing.
        free_bytes: u64,
        /// Headroom the image must keep.
        required_bytes: u64,
    },
    /// The planned image exceeds the operator ceiling.
    #[error(
        "OCI ext4 image at {path} would be {size_bytes} bytes, exceeding the {limit}-byte limit"
    )]
    Ext4TooLarge {
        /// Image path that was not written.
        path: PathBuf,
        /// Planned image length.
        size_bytes: u64,
        /// Configured maximum size.
        limit: u64,
    },
    /// The caller stopped waiting before the ext4 image was published.
    #[error("OCI ext4 write was cancelled before publishing {path}")]
    Ext4Cancelled {
        /// Destination that was left unpublished.
        path: PathBuf,
    },
    /// The compiled-in catalog has no kernel that can boot a module-less
    /// OCI rootfs on this architecture.
    #[error("no architecture-matched kernel without an initrd is published for {architecture}")]
    NoHostKernel {
        /// Architecture a later TemplateSpec would have to boot.
        architecture: Architecture,
    },
    /// The catalog kernel for this host is not installed under the image root.
    #[error("no architecture-matched kernel at {path}; install {hint} for {architecture}")]
    KernelMissing {
        /// Catalog-relative kernel path that was required.
        path: PathBuf,
        /// Template alias that publishes that kernel.
        hint: String,
        /// Architecture the missing kernel must match.
        architecture: Architecture,
    },
    /// The catalog kernel exists but is built for another architecture.
    #[error("kernel {path} is built for {found}, but this host is {host}")]
    KernelArchitectureMismatch {
        /// Catalog-relative kernel path that was inspected.
        path: PathBuf,
        /// Architecture the kernel header declares.
        found: Architecture,
        /// Architecture this build of the API runs on.
        host: Architecture,
    },
    /// The catalog kernel is a well-formed ELF Firecracker cannot boot.
    #[error(
        "kernel {path} targets an architecture firecrab does not support (ELF machine {machine:#06x})"
    )]
    UnsupportedKernelArchitecture {
        /// Catalog-relative kernel path that was inspected.
        path: PathBuf,
        /// ELF `e_machine` value that identified it.
        machine: u16,
    },
    /// The file at the catalog kernel path is not a classifiable kernel.
    #[error("kernel {path} is not a classifiable Firecracker kernel")]
    KernelUnrecognized {
        /// Catalog-relative kernel path that was inspected.
        path: PathBuf,
    },
    /// No source could supply the kernel an imported tree boots under.
    #[error("no OCI kernel is available for {architecture}: {reason}")]
    KernelUnavailable {
        /// Architecture a later TemplateSpec would have to boot.
        architecture: Architecture,
        /// What every configured source had to say.
        reason: String,
    },
    /// The published kernel package could not be downloaded.
    #[error("OCI kernel download from {url} failed: {message}")]
    KernelDownloadFailed {
        /// Object URL that was requested.
        url: String,
        /// Transport or filesystem diagnostic.
        message: String,
    },
    /// The verified package does not carry the kernel it is pinned for.
    #[error("OCI kernel package {package} has no member {member}")]
    KernelPackageMemberMissing {
        /// Registry object key of the package.
        package: String,
        /// Archive member the pin named.
        member: String,
    },
    /// A filesystem operation failed while inspecting the paired kernel.
    #[error("OCI kernel pairing {operation} failed at {path}: {source}")]
    KernelIo {
        /// Operation being attempted.
        operation: &'static str,
        /// Kernel path involved.
        path: PathBuf,
        /// Operating-system failure.
        #[source]
        source: io::Error,
    },
    /// The derived alias is already an installed or catalog image.
    #[error("OCI import alias {alias} collides with installed image {occupant}")]
    AliasCollision {
        /// Alias derived from the reference.
        alias: String,
        /// Alias already claimed by the registry or catalog.
        occupant: String,
    },
    /// The reference cannot be turned into a safe template alias.
    #[error("OCI reference {reference} cannot be turned into a template alias")]
    AliasUnusable {
        /// Original reference text, for the operator.
        reference: String,
    },
    /// The published rootfs path already exists and must not be replaced.
    #[error("OCI rootfs destination already exists at {path}")]
    RegisterDestinationExists {
        /// Intended published rootfs path.
        path: PathBuf,
    },
    /// A filesystem operation failed while publishing the rootfs.
    #[error("OCI registration {operation} failed at {path}: {source}")]
    RegisterIo {
        /// Operation being attempted.
        operation: &'static str,
        /// Path involved.
        path: PathBuf,
        /// Operating-system failure.
        #[source]
        source: io::Error,
    },
    /// Publishing succeeded but the template registry refused the spec.
    #[error("OCI image {alias} could not be registered: {detail}")]
    RegisterFailed {
        /// Alias that was not registered.
        alias: String,
        /// Registry diagnostic.
        detail: String,
    },
    /// An image Env entry was not `KEY=value`.
    #[error("OCI image Env entry {entry:?} is not KEY=value")]
    ServiceEnvInvalid {
        /// The rejected environment entry.
        entry: String,
    },
    /// A filesystem operation failed while writing the image service.
    #[error("OCI service {operation} failed at {path}: {source}")]
    ServiceIo {
        /// Operation being attempted.
        operation: &'static str,
        /// Guest or host path involved.
        path: PathBuf,
        /// Operating-system failure.
        #[source]
        source: io::Error,
    },
    /// One manifest contradicted itself about a shared content address.
    #[error("manifest declares {digest} with conflicting sizes {first} and {second}")]
    ConflictingDescriptorSize {
        /// Reused digest.
        digest: Sha256Digest,
        /// First declared size.
        first: u64,
        /// Conflicting declared size.
        second: u64,
    },
    /// A validated digest could not be read from or written to the local cache.
    #[error("OCI cache {operation} failed at {path}: {source}")]
    CacheIo {
        /// Filesystem operation being performed.
        operation: &'static str,
        /// Cache or work path involved.
        path: PathBuf,
        /// Operating-system failure.
        #[source]
        source: io::Error,
    },
    /// A registry supplied an unsafe or unsupported content digest.
    #[error(transparent)]
    Digest(#[from] DigestError),
    /// The index parsed but offers nothing this host can run.
    #[error(transparent)]
    Index(#[from] IndexError),
}

/// A `WWW-Authenticate: Bearer` challenge's token endpoint.
fn token_request(challenge: &str, base: &str) -> Option<String> {
    let (scheme, parameters) = challenge.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let mut realm = None;
    let mut query = Vec::new();
    for parameter in parameters.split(',') {
        let (key, value) = parameter.trim().split_once('=')?;
        let value = value.trim_matches('"');
        match key {
            "realm" => realm = reqwest::Url::parse(value).ok(),
            "service" | "scope" => query.push((key, value)),
            _ => {}
        }
    }
    let mut realm = realm?;
    let base = reqwest::Url::parse(base).ok()?;
    if !realm.username().is_empty() || realm.password().is_some() || realm.fragment().is_some() {
        return None;
    }
    // Public registries commonly delegate anonymous tokens to a different
    // HTTPS authority (Docker Hub uses auth.docker.io). Never follow a
    // challenge that downgrades to plaintext. Plain HTTP remains restricted
    // to the same loopback authority as the explicitly insecure registry.
    match realm.scheme() {
        "https" => {}
        "http"
            if base.scheme() == "http"
                && realm.host_str() == base.host_str()
                && realm.port_or_known_default() == base.port_or_known_default() => {}
        _ => return None,
    }
    {
        let mut pairs = realm.query_pairs_mut();
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
    }
    Some(realm.into())
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocumentKind {
    Index,
    Manifest,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocumentProbe {
    #[serde(default)]
    media_type: String,
}

#[derive(Debug)]
struct FetchedManifest {
    body: Vec<u8>,
    digest: Sha256Digest,
    kind: DocumentKind,
    media_type: String,
}

#[derive(Debug)]
struct ResolvedManifest {
    resolved: ResolvedImage,
    manifest: ImageManifest,
}

/// What to say when the kernel, not the network, refused the socket.
///
/// The three places named here are the ones that produce `EACCES` on a host
/// where an ordinary shell on the same account reaches the registry fine.
const LOCAL_POLICY_HINT: &str = "; this host refused the connection before it \
     left the machine (EACCES) — check SELinux (ausearch -m avc), the service \
     sandbox (systemctl show firecrab-api -p IPAddressDeny \
     -p RestrictAddressFamilies), and firewall rules matching the API user, \
     or run firecrab doctor";

/// Whether a transport failure was a local policy refusal rather than a
/// network one.
///
/// reqwest wraps hyper wraps the `io::Error` that `connect(2)` returned, so
/// the kind is only visible by walking to the bottom of the chain.
fn refused_by_local_policy(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(io) = error.downcast_ref::<io::Error>()
            && io.kind() == io::ErrorKind::PermissionDenied
        {
            return true;
        }
        current = error.source();
    }
    false
}

/// reqwest's Display is only "error sending request for url (…)". The
/// useful cause (DNS, TLS, timeout) is in the source chain.
///
/// A permission denial gets a hint appended: it is the one transport failure
/// whose cause is on this host rather than at the other end, and the bare
/// message sends operators looking for a proxy or a missing login instead.
fn format_error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut parts = vec![error.to_string()];
    let mut source = error.source();
    while let Some(err) = source {
        let text = err.to_string();
        if parts.iter().all(|part| !part.contains(&text)) {
            parts.push(text);
        }
        source = err.source();
    }
    let mut message = parts.join(": ");
    if refused_by_local_policy(error) {
        message.push_str(LOCAL_POLICY_HINT);
    }
    message
}

/// Username and token sent as HTTP Basic on the registry token endpoint.
///
/// The registry belongs to the credential rather than to the call site: a
/// session attaches a login only while talking to the registry it was saved
/// for, so a Docker Hub token cannot follow a reference to another host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryCredential {
    /// Registry host this login belongs to.
    pub registry: String,
    /// Registry account name.
    pub username: String,
    /// Password or personal access token.
    pub secret: String,
}

impl RegistryCredential {
    /// A Docker Hub login. `docker.io` and `registry-1.docker.io` name the
    /// same account, so the API host is stored.
    pub fn docker_hub(username: impl Into<String>, secret: impl Into<String>) -> Self {
        Self {
            registry: DOCKER_HUB_REGISTRY.to_owned(),
            username: username.into(),
            secret: secret.into(),
        }
    }

    /// Whether this login may be sent while fetching from `registry`.
    fn covers(&self, registry: &str) -> bool {
        canonical_registry(&self.registry) == canonical_registry(registry)
    }
}

impl From<crate::persistence::DockerHubCredential> for RegistryCredential {
    fn from(stored: crate::persistence::DockerHubCredential) -> Self {
        Self::docker_hub(stored.username, stored.secret)
    }
}

/// The one spelling of a registry host used when comparing two of them.
fn canonical_registry(registry: &str) -> String {
    let registry = registry.to_ascii_lowercase();
    if is_docker_hub_registry(&registry) {
        return DOCKER_HUB_REGISTRY.to_owned();
    }
    registry
}

/// Docker Hub's registry host and the name operators type.
pub fn is_docker_hub_registry(registry: &str) -> bool {
    let host = registry.rsplit_once(':').map_or(registry, |(host, _)| host);
    matches!(host, DOCKER_HUB_REGISTRY | DOCKER_HUB_ALIAS)
}

/// One authenticated conversation with a registry. The bearer token is kept
/// across the selected-manifest and blob requests and refreshed only once when
/// concurrent requests all encounter the same initial challenge.
#[derive(Debug, Clone)]
struct RegistrySession {
    client: reqwest::Client,
    base: String,
    token: Arc<AsyncMutex<Option<String>>>,
    basic: Option<RegistryCredential>,
}

impl RegistrySession {
    fn new(
        reference: &ImageReference,
        insecure: bool,
        credential: Option<RegistryCredential>,
    ) -> Result<Self, ResolveError> {
        // A stored login is sent only to the registry it was saved for. Any
        // other reference — a private mirror, a toolbox override — is fetched
        // anonymously rather than leaking the secret to that host.
        let basic = credential.filter(|credential| credential.covers(&reference.registry));
        let scheme = if insecure { "http" } else { "https" };
        let client = reqwest::Client::builder()
            // A registry response must not redirect an authenticated manifest
            // or blob request from HTTPS to plaintext. Local registries opt in
            // to HTTP explicitly through `insecure`.
            .https_only(!insecure)
            // rustls HTTP/2 to some registry fronts fails at send() with no
            // HTTP status. Docker Hub and GHCR answer HTTP/1.1 fine.
            .http1_only()
            .connect_timeout(Duration::from_secs(15))
            // Unlike a total request timeout, this resets whenever another
            // chunk arrives, so a large healthy layer may take as long as it
            // needs while a stalled registry still fails predictably.
            .read_timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| ResolveError::Transport(error.to_string()))?;
        Ok(Self {
            client,
            base: format!("{scheme}://{}", reference.registry),
            token: Arc::new(AsyncMutex::new(None)),
            basic,
        })
    }

    async fn send_once(
        &self,
        url: &str,
        accept: Option<&str>,
        token: Option<&str>,
        timeout: Option<Duration>,
    ) -> Result<reqwest::Response, ResolveError> {
        let mut request = self.client.get(url);
        if let Some(accept) = accept {
            request = request.header("accept", accept);
        }
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some(timeout) = timeout {
            request = request.timeout(timeout);
        }
        request.send().await.map_err(|error| {
            ResolveError::Transport(format!("GET {url}: {}", format_error_chain(&error)))
        })
    }

    async fn get(
        &self,
        url: &str,
        accept: Option<&str>,
        timeout: Option<Duration>,
    ) -> Result<reqwest::Response, ResolveError> {
        let attempted = self.token.lock().await.clone();
        let response = self
            .send_once(url, accept, attempted.as_deref(), timeout)
            .await?;
        if response.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Ok(response);
        }

        let challenge = response
            .headers()
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let mut stored = self.token.lock().await;
        let token = if stored.is_some() && *stored != attempted {
            stored.clone().expect("token checked as present")
        } else {
            let token_url = token_request(&challenge, &self.base).ok_or_else(|| {
                ResolveError::Authentication(
                    "registry sent an unusable bearer challenge".to_owned(),
                )
            })?;
            let mut token_request = self.client.get(&token_url).timeout(Duration::from_secs(20));
            if let Some(basic) = &self.basic {
                token_request = token_request.basic_auth(&basic.username, Some(&basic.secret));
            }
            let response = token_request.send().await.map_err(|error| {
                ResolveError::Transport(format!("GET {token_url}: {}", format_error_chain(&error)))
            })?;
            if !response.status().is_success() {
                return Err(ResolveError::Authentication(format!(
                    "token endpoint answered {}",
                    response.status().as_u16()
                )));
            }
            let issued = read_token_body(response).await?;
            let issued: TokenResponse = serde_json::from_slice(&issued)
                .map_err(|error| ResolveError::Authentication(error.to_string()))?;
            let Some(token) = issued.issued().map(str::to_owned) else {
                return Err(ResolveError::Authentication(
                    "token endpoint returned an empty token".to_owned(),
                ));
            };
            *stored = Some(token.clone());
            token
        };
        drop(stored);

        let response = self.send_once(url, accept, Some(&token), timeout).await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ResolveError::Authentication(
                "registry rejected the issued bearer token".to_owned(),
            ));
        }
        Ok(response)
    }

    async fn fetch_manifest(
        &self,
        repository: &str,
        selector: &str,
        expected: Option<&Descriptor>,
        pinned: Option<&Sha256Digest>,
    ) -> Result<FetchedManifest, ResolveError> {
        if let Some(descriptor) = expected
            && descriptor.size > MANIFEST_MAX_BYTES as u64
        {
            return Err(ResolveError::Malformed(format!(
                "manifest {} exceeds limit",
                descriptor.digest
            )));
        }
        let url = format!("{}/v2/{repository}/manifests/{selector}", self.base);
        let response = self
            .get(&url, Some(MANIFEST_ACCEPT), Some(Duration::from_secs(20)))
            .await?;
        if !response.status().is_success() {
            return Err(ResolveError::Status {
                status: response.status().as_u16(),
                reference: format!("{repository}:{selector}"),
            });
        }

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let header_digest = response_digest(response.headers())?;
        if let Some(descriptor) = expected
            && let Some(length) = response.content_length()
            && length != descriptor.size
        {
            return Err(ResolveError::SizeMismatch {
                subject: descriptor.digest.to_string(),
                expected: descriptor.size,
                actual: length,
            });
        }

        let body = read_manifest_body(response).await?;
        if let Some(descriptor) = expected
            && body.len() as u64 != descriptor.size
        {
            return Err(ResolveError::SizeMismatch {
                subject: descriptor.digest.to_string(),
                expected: descriptor.size,
                actual: body.len() as u64,
            });
        }
        let digest = Sha256Digest::of_bytes(&body);
        if let Some(expected) = expected.map(|descriptor| &descriptor.digest).or(pinned)
            && &digest != expected
        {
            return Err(ResolveError::DigestMismatch {
                subject: format!("manifest {selector}"),
                expected: expected.clone(),
                actual: digest,
            });
        }
        if let Some(header_digest) = header_digest
            && header_digest != digest
        {
            return Err(ResolveError::DigestMismatch {
                subject: format!("Docker-Content-Digest for manifest {selector}"),
                expected: header_digest,
                actual: digest,
            });
        }

        let (kind, media_type) = classify_document(content_type.as_deref(), &body)?;
        Ok(FetchedManifest {
            body,
            digest,
            kind,
            media_type,
        })
    }
}

async fn read_token_body(response: reqwest::Response) -> Result<Vec<u8>, ResolveError> {
    if response
        .content_length()
        .is_some_and(|length| length > TOKEN_MAX_BYTES as u64)
    {
        return Err(ResolveError::Authentication(
            "token response exceeds limit".to_owned(),
        ));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|error| ResolveError::Transport(format!("read token response: {error}")))?;
        if body.len().saturating_add(chunk.len()) > TOKEN_MAX_BYTES {
            return Err(ResolveError::Authentication(
                "token response exceeds limit".to_owned(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn read_manifest_body(response: reqwest::Response) -> Result<Vec<u8>, ResolveError> {
    if response
        .content_length()
        .is_some_and(|length| length > MANIFEST_MAX_BYTES as u64)
    {
        return Err(ResolveError::Malformed("manifest exceeds limit".to_owned()));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| ResolveError::Transport(error.to_string()))?;
        if body.len().saturating_add(chunk.len()) > MANIFEST_MAX_BYTES {
            return Err(ResolveError::Malformed("manifest exceeds limit".to_owned()));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn response_digest(
    headers: &reqwest::header::HeaderMap,
) -> Result<Option<Sha256Digest>, ResolveError> {
    let Some(value) = headers.get("docker-content-digest") else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|_| ResolveError::Malformed("non-UTF-8 Docker-Content-Digest".to_owned()))?;
    Ok(Some(Sha256Digest::parse(value)?))
}

fn media_kind(media_type: &str) -> Option<DocumentKind> {
    match media_type {
        OCI_INDEX_MEDIA_TYPE | DOCKER_INDEX_MEDIA_TYPE => Some(DocumentKind::Index),
        OCI_MANIFEST_MEDIA_TYPE | DOCKER_MANIFEST_MEDIA_TYPE => Some(DocumentKind::Manifest),
        _ => None,
    }
}

fn classify_document(
    content_type: Option<&str>,
    body: &[u8],
) -> Result<(DocumentKind, String), ResolveError> {
    let probe: DocumentProbe =
        serde_json::from_slice(body).map_err(|error| ResolveError::Malformed(error.to_string()))?;
    let body_type = (!probe.media_type.is_empty()).then(|| probe.media_type.to_ascii_lowercase());
    let header_type = content_type.map(|value| {
        value
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
    });

    let body_kind = body_type.as_deref().and_then(media_kind);
    if let Some(body_type) = body_type.as_deref()
        && body_kind.is_none()
    {
        return Err(ResolveError::UnsupportedMediaType(body_type.to_owned()));
    }
    let header_kind = header_type.as_deref().and_then(media_kind);
    if let Some(header_type) = header_type.as_deref()
        && header_kind.is_none()
    {
        return Err(ResolveError::UnsupportedMediaType(header_type.to_owned()));
    }
    if let (Some(_), Some(_)) = (body_kind, header_kind)
        && body_type != header_type
    {
        return Err(ResolveError::Malformed(format!(
            "manifest media type disagrees with HTTP content type {}",
            header_type.as_deref().unwrap_or_default()
        )));
    }

    let kind = body_kind.or(header_kind).ok_or_else(|| {
        ResolveError::UnsupportedMediaType(
            body_type
                .clone()
                .or(header_type.clone())
                .unwrap_or_else(|| "<missing>".to_owned()),
        )
    })?;
    let media_type = body_type
        .or_else(|| header_kind.and(header_type))
        .expect("a classified document has a media type");
    Ok((kind, media_type))
}

fn parse_index(document: &FetchedManifest) -> Result<ImageIndex, ResolveError> {
    let raw: RawImageIndex = serde_json::from_slice(&document.body)
        .map_err(|error| ResolveError::Malformed(error.to_string()))?;
    let mut index = ImageIndex {
        schema_version: raw.schema_version,
        media_type: raw.media_type,
        manifests: raw
            .manifests
            .into_iter()
            .map(|descriptor| {
                Ok(ManifestDescriptor {
                    descriptor: descriptor.descriptor.try_into()?,
                    platform: descriptor.platform,
                })
            })
            .collect::<Result<_, DigestError>>()?,
    };
    if index.schema_version != 2 {
        return Err(ResolveError::Malformed(format!(
            "unsupported manifest schema version {}",
            index.schema_version
        )));
    }
    if index.media_type.is_empty() {
        index.media_type.clone_from(&document.media_type);
    }
    Ok(index)
}

fn parse_image_manifest(document: &FetchedManifest) -> Result<ImageManifest, ResolveError> {
    let raw: RawImageManifest = serde_json::from_slice(&document.body)
        .map_err(|error| ResolveError::Malformed(error.to_string()))?;
    let mut manifest = ImageManifest {
        schema_version: raw.schema_version,
        media_type: raw.media_type,
        config: raw.config.try_into()?,
        layers: raw
            .layers
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<_, DigestError>>()?,
    };
    if manifest.schema_version != 2 {
        return Err(ResolveError::Malformed(format!(
            "unsupported manifest schema version {}",
            manifest.schema_version
        )));
    }
    if manifest.media_type.is_empty() {
        manifest.media_type.clone_from(&document.media_type);
    }
    if !is_config_media_type(&manifest.config.media_type) {
        return Err(ResolveError::UnsupportedMediaType(
            manifest.config.media_type.clone(),
        ));
    }
    if let Some(layer) = manifest
        .layers
        .iter()
        .find(|layer| !is_layer_media_type(&layer.media_type))
    {
        return Err(ResolveError::UnsupportedMediaType(layer.media_type.clone()));
    }
    validate_descriptor_sizes(&manifest)?;
    Ok(manifest)
}

fn validate_descriptor_sizes(manifest: &ImageManifest) -> Result<(), ResolveError> {
    let mut sizes = HashMap::new();
    for descriptor in std::iter::once(&manifest.config).chain(&manifest.layers) {
        if let Some(first) = sizes.insert(descriptor.digest.clone(), descriptor.size)
            && first != descriptor.size
        {
            return Err(ResolveError::ConflictingDescriptorSize {
                digest: descriptor.digest.clone(),
                first,
                second: descriptor.size,
            });
        }
    }
    Ok(())
}

async fn resolve_manifest(
    session: &RegistrySession,
    reference: &ImageReference,
    architecture: Architecture,
) -> Result<ResolvedManifest, ResolveError> {
    let pinned = match &reference.version {
        ImageVersion::Digest(digest) => Some(digest),
        ImageVersion::Tag(_) => None,
    };
    let first = session
        .fetch_manifest(
            &reference.repository,
            reference.version.as_str(),
            None,
            pinned,
        )
        .await?;

    match first.kind {
        DocumentKind::Manifest => {
            let manifest = parse_image_manifest(&first)?;
            Ok(ResolvedManifest {
                resolved: ResolvedImage {
                    digest: first.digest.to_string(),
                    architecture,
                    single_platform: true,
                },
                manifest,
            })
        }
        DocumentKind::Index => {
            let index = parse_index(&first)?;
            let selected = index.select(architecture)?;
            if !is_manifest_media_type(&selected.descriptor.media_type) {
                return Err(ResolveError::UnsupportedMediaType(
                    selected.descriptor.media_type.clone(),
                ));
            }
            let selected_document = session
                .fetch_manifest(
                    &reference.repository,
                    selected.descriptor.digest.as_str(),
                    Some(&selected.descriptor),
                    None,
                )
                .await?;
            if selected_document.kind != DocumentKind::Manifest {
                return Err(ResolveError::Malformed(
                    "selected index entry did not resolve to an image manifest".to_owned(),
                ));
            }
            if selected_document.media_type != selected.descriptor.media_type {
                return Err(ResolveError::Malformed(format!(
                    "selected manifest media type is {}, descriptor declared {}",
                    selected_document.media_type, selected.descriptor.media_type
                )));
            }
            let manifest = parse_image_manifest(&selected_document)?;
            Ok(ResolvedManifest {
                resolved: ResolvedImage {
                    digest: selected_document.digest.to_string(),
                    architecture,
                    single_platform: false,
                },
                manifest,
            })
        }
    }
}

/// Resolves a reference to the verified image manifest this host should pull.
///
/// The anonymous bearer-token flow is shared across the initial document and
/// a selected platform manifest. No config or layer blob is downloaded here,
/// so `GET /api/oci/inspect` remains a metadata-only operation.
pub async fn resolve(
    reference: &ImageReference,
    architecture: Architecture,
    insecure: bool,
    credential: Option<RegistryCredential>,
) -> Result<ResolvedImage, ResolveError> {
    let session = RegistrySession::new(reference, insecure, credential)?;
    Ok(resolve_manifest(&session, reference, architecture)
        .await?
        .resolved)
}

/// Content-addressed storage for verified OCI config and layer blobs.
#[derive(Debug, Clone)]
pub struct BlobCache {
    root: PathBuf,
    max_blob_bytes: u64,
}

type CacheLockMap = HashMap<PathBuf, Weak<AsyncMutex<()>>>;
static CACHE_LOCKS: OnceLock<StdMutex<CacheLockMap>> = OnceLock::new();
static DECOMPRESSION_PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();

fn shared_decompression_permits() -> Arc<Semaphore> {
    Arc::clone(
        DECOMPRESSION_PERMITS.get_or_init(|| Arc::new(Semaphore::new(MAX_PARALLEL_DECOMPRESSIONS))),
    )
}

async fn cache_path_lock(
    root: &Path,
    relative_path: &Path,
) -> Result<Arc<AsyncMutex<()>>, ResolveError> {
    let canonical_root = tokio::fs::canonicalize(root)
        .await
        .map_err(|source| cache_io("canonicalize directory", root.to_owned(), source))?;
    let key = canonical_root.join(relative_path);
    let mut locks = CACHE_LOCKS
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    let lock = Arc::new(AsyncMutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    Ok(lock)
}

impl BlobCache {
    /// Creates a cache rooted at `<image-root>/.oci/blobs/sha256/`.
    pub fn new(image_root: impl AsRef<Path>) -> Self {
        Self::with_max_blob_bytes(image_root, configured_max_blob_bytes())
    }

    /// Creates a cache with an explicit per-blob download ceiling.
    ///
    /// This is useful for embedders and deterministic tests; normal service
    /// startup reads `FIRECRAB_OCI_MAX_BLOB_BYTES` through [`Self::new`].
    pub fn with_max_blob_bytes(image_root: impl AsRef<Path>, max_blob_bytes: u64) -> Self {
        Self {
            root: image_root.as_ref().join(".oci/blobs/sha256"),
            max_blob_bytes,
        }
    }

    /// Final path for a digest. The digest type prevents path traversal.
    pub fn path_for(&self, digest: &Sha256Digest) -> PathBuf {
        self.root.join(digest.encoded())
    }

    async fn digest_lock(
        &self,
        digest: &Sha256Digest,
    ) -> Result<Arc<AsyncMutex<()>>, ResolveError> {
        cache_path_lock(&self.root, Path::new(digest.encoded())).await
    }

    async fn cache_descriptor(
        &self,
        session: &RegistrySession,
        repository: &str,
        descriptor: &Descriptor,
    ) -> Result<PathBuf, ResolveError> {
        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(|source| cache_io("create directory", self.root.clone(), source))?;
        let lock = self.digest_lock(&descriptor.digest).await?;
        let _guard = lock.lock().await;
        let path = self.path_for(&descriptor.digest);

        match verify_cache_file(&path, descriptor).await {
            Ok(CacheFileState::Valid) => return Ok(path),
            Ok(CacheFileState::Missing) => {}
            Ok(CacheFileState::Corrupt) => remove_cache_file(&path).await?,
            Err(error) => return Err(error),
        }

        self.download_blob(session, repository, descriptor, &path)
            .await?;
        Ok(path)
    }

    async fn download_blob(
        &self,
        session: &RegistrySession,
        repository: &str,
        descriptor: &Descriptor,
        destination: &Path,
    ) -> Result<(), ResolveError> {
        if descriptor.size > self.max_blob_bytes {
            return Err(ResolveError::BlobTooLarge {
                digest: descriptor.digest.clone(),
                size: descriptor.size,
                limit: self.max_blob_bytes,
            });
        }
        let url = format!(
            "{}/v2/{repository}/blobs/{}",
            session.base, descriptor.digest
        );
        let response = session.get(&url, None, None).await?;
        if !response.status().is_success() {
            return Err(ResolveError::Status {
                status: response.status().as_u16(),
                reference: format!("{repository}@{}", descriptor.digest),
            });
        }
        if let Some(header_digest) = response_digest(response.headers())?
            && header_digest != descriptor.digest
        {
            return Err(ResolveError::DigestMismatch {
                subject: format!("Docker-Content-Digest for blob {}", descriptor.digest),
                expected: descriptor.digest.clone(),
                actual: header_digest,
            });
        }
        if let Some(length) = response.content_length()
            && length != descriptor.size
        {
            return Err(ResolveError::SizeMismatch {
                subject: descriptor.digest.to_string(),
                expected: descriptor.size,
                actual: length,
            });
        }

        let (temporary, mut file) = create_partial_file(&self.root, &descriptor.digest).await?;
        let mut cleanup = PartialCleanup::new(temporary.clone());
        let mut hasher = Sha256::new();
        let mut downloaded = 0_u64;
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|error| {
                ResolveError::Transport(format!("read blob {}: {error}", descriptor.digest))
            })?;
            downloaded = downloaded.saturating_add(chunk.len() as u64);
            if downloaded > descriptor.size {
                return Err(ResolveError::SizeMismatch {
                    subject: descriptor.digest.to_string(),
                    expected: descriptor.size,
                    actual: downloaded,
                });
            }
            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|source| cache_io("write", temporary.clone(), source))?;
        }
        if downloaded != descriptor.size {
            return Err(ResolveError::SizeMismatch {
                subject: descriptor.digest.to_string(),
                expected: descriptor.size,
                actual: downloaded,
            });
        }
        let actual = Sha256Digest(format!("sha256:{:x}", hasher.finalize()));
        if actual != descriptor.digest {
            return Err(ResolveError::DigestMismatch {
                subject: format!("blob {}", descriptor.digest),
                expected: descriptor.digest.clone(),
                actual,
            });
        }
        file.flush()
            .await
            .map_err(|source| cache_io("flush", temporary.clone(), source))?;
        file.sync_all()
            .await
            .map_err(|source| cache_io("sync", temporary.clone(), source))?;
        drop(file);
        tokio::fs::rename(&temporary, destination)
            .await
            .map_err(|source| cache_io("publish", destination.to_owned(), source))?;
        cleanup.published = true;
        Ok(())
    }
}

fn configured_max_blob_bytes() -> u64 {
    match std::env::var(MAX_BLOB_BYTES_ENV) {
        Ok(value) => match value.trim().parse::<u64>() {
            Ok(limit) if limit > 0 => limit,
            _ => {
                tracing::warn!(
                    variable = MAX_BLOB_BYTES_ENV,
                    value,
                    default = DEFAULT_MAX_BLOB_BYTES,
                    "invalid OCI blob limit; using default"
                );
                DEFAULT_MAX_BLOB_BYTES
            }
        },
        Err(std::env::VarError::NotPresent) => DEFAULT_MAX_BLOB_BYTES,
        Err(std::env::VarError::NotUnicode(_)) => {
            tracing::warn!(
                variable = MAX_BLOB_BYTES_ENV,
                default = DEFAULT_MAX_BLOB_BYTES,
                "non-Unicode OCI blob limit; using default"
            );
            DEFAULT_MAX_BLOB_BYTES
        }
    }
}

fn configured_max_uncompressed_layer_bytes() -> u64 {
    match std::env::var(MAX_UNCOMPRESSED_LAYER_BYTES_ENV) {
        Ok(value) => match value.trim().parse::<u64>() {
            Ok(limit) if limit > 0 => limit,
            _ => {
                tracing::warn!(
                    variable = MAX_UNCOMPRESSED_LAYER_BYTES_ENV,
                    value,
                    default = DEFAULT_MAX_UNCOMPRESSED_LAYER_BYTES,
                    "invalid OCI uncompressed layer limit; using default"
                );
                DEFAULT_MAX_UNCOMPRESSED_LAYER_BYTES
            }
        },
        Err(std::env::VarError::NotPresent) => DEFAULT_MAX_UNCOMPRESSED_LAYER_BYTES,
        Err(std::env::VarError::NotUnicode(_)) => {
            tracing::warn!(
                variable = MAX_UNCOMPRESSED_LAYER_BYTES_ENV,
                default = DEFAULT_MAX_UNCOMPRESSED_LAYER_BYTES,
                "non-Unicode OCI uncompressed layer limit; using default"
            );
            DEFAULT_MAX_UNCOMPRESSED_LAYER_BYTES
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheFileState {
    Missing,
    Valid,
    Corrupt,
}

async fn verify_cache_file(
    path: &Path,
    descriptor: &Descriptor,
) -> Result<CacheFileState, ResolveError> {
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(CacheFileState::Missing);
        }
        Err(source) => return Err(cache_io("inspect", path.to_owned(), source)),
    };
    if !metadata.file_type().is_file() {
        return Ok(CacheFileState::Corrupt);
    }

    let (size, actual) = match hash_file(path).await {
        Ok(result) => result,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(CacheFileState::Missing);
        }
        Err(source) => return Err(cache_io("verify", path.to_owned(), source)),
    };
    if actual != descriptor.digest {
        return Ok(CacheFileState::Corrupt);
    }
    if size != descriptor.size {
        return Err(ResolveError::SizeMismatch {
            subject: descriptor.digest.to_string(),
            expected: descriptor.size,
            actual: size,
        });
    }
    Ok(CacheFileState::Valid)
}

async fn hash_file(path: &Path) -> io::Result<(u64, Sha256Digest)> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        size = size.saturating_add(read as u64);
        hasher.update(&buffer[..read]);
    }
    Ok((
        size,
        Sha256Digest(format!("sha256:{:x}", hasher.finalize())),
    ))
}

async fn remove_cache_file(path: &Path) -> Result<(), ResolveError> {
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(cache_io("inspect corrupt entry", path.to_owned(), source)),
    };
    let result = if metadata.file_type().is_dir() {
        // Never recursively remove an unexpected tree at a content-addressed
        // filename. An empty directory is safe to replace; a non-empty one is
        // surfaced as cache I/O damage for an operator to inspect.
        tokio::fs::remove_dir(path).await
    } else {
        tokio::fs::remove_file(path).await
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(cache_io("remove corrupt entry", path.to_owned(), source)),
    }
}

async fn create_partial_file(
    root: &Path,
    digest: &Sha256Digest,
) -> Result<(PathBuf, tokio::fs::File), ResolveError> {
    for _ in 0..8 {
        let path = root.join(format!(".{}.{}.partial", digest.encoded(), Uuid::new_v4()));
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(cache_io("create partial", path, source)),
        }
    }
    Err(cache_io(
        "create partial",
        root.to_owned(),
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique work file",
        ),
    ))
}

struct PartialCleanup {
    path: PathBuf,
    published: bool,
}

impl PartialCleanup {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            published: false,
        }
    }
}

impl Drop for PartialCleanup {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn cache_io(operation: &'static str, path: PathBuf, source: io::Error) -> ResolveError {
    ResolveError::CacheIo {
        operation,
        path,
        source,
    }
}

mod busybox;

#[cfg(test)]
mod busybox_tests;

pub(crate) mod fastfetch;

#[cfg(test)]
mod fastfetch_tests;

#[cfg(test)]
mod cache_tests;

#[cfg(test)]
mod tar_tests;

mod merge;

#[cfg(test)]
mod merge_tests;

pub(crate) mod provision;

#[cfg(test)]
mod provision_tests;

mod ext4;

#[cfg(test)]
mod ext4_tests;

mod boot;

#[cfg(test)]
mod boot_tests;

pub(crate) mod kernel;

#[cfg(test)]
mod kernel_tests;

mod name;

#[cfg(test)]
mod name_tests;

mod register;

#[cfg(test)]
mod register_tests;

pub(crate) mod service;

#[cfg(test)]
mod service_tests;

#[cfg(test)]
pub(crate) mod registry_fixture;

/// One verified cache entry together with the descriptor that named it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedBlob {
    /// Manifest metadata for the raw cached bytes.
    pub descriptor: Descriptor,
    /// Content-addressed path below `.oci/blobs/sha256/`.
    pub path: PathBuf,
}

/// Verified config and ordered layer paths for a resolved image manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedImageBlobs {
    /// Actual manifest digest and selected host architecture.
    pub resolved: ResolvedImage,
    /// Parsed, verified manifest that supplied the descriptors.
    pub manifest: ImageManifest,
    /// Cached image configuration document.
    pub config: CachedBlob,
    /// Cached filesystem layers in manifest application order.
    pub layers: Vec<CachedBlob>,
}

/// Content-addressed storage for verified, uncompressed layer tar streams.
///
/// A cache filename records both identities involved in unpacking: the
/// config's digest of the uncompressed tar and the manifest's digest of the
/// exact registry blob. This prevents a cached tar from making a new, false
/// compressed-digest-to-diff-ID relationship appear verified.
#[derive(Debug, Clone)]
pub struct LayerCache {
    root: PathBuf,
    max_uncompressed_layer_bytes: u64,
    decompression_permits: Arc<Semaphore>,
}

struct CompressedReadState {
    size: u64,
    hasher: Sha256,
}

impl CompressedReadState {
    fn finish(&self) -> (u64, Sha256Digest) {
        (
            self.size,
            Sha256Digest(format!("sha256:{:x}", self.hasher.clone().finalize())),
        )
    }
}

struct VerifyingReader<R> {
    inner: R,
    state: Arc<StdMutex<CompressedReadState>>,
}

impl<R> VerifyingReader<R> {
    fn new(inner: R) -> (Self, Arc<StdMutex<CompressedReadState>>) {
        let state = Arc::new(StdMutex::new(CompressedReadState {
            size: 0,
            hasher: Sha256::new(),
        }));
        (
            Self {
                inner,
                state: Arc::clone(&state),
            },
            state,
        )
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for VerifyingReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let filled_before = buffer.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(context, buffer);
        if let Poll::Ready(Ok(())) = &result {
            let bytes = &buffer.filled()[filled_before..];
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let Some(size) = state.size.checked_add(bytes.len() as u64) else {
                return Poll::Ready(Err(io::Error::other(
                    "compressed layer byte count overflow",
                )));
            };
            state.size = size;
            state.hasher.update(bytes);
        }
        result
    }
}

impl LayerCache {
    /// Creates a cache rooted at `<image-root>/.oci/layers/sha256/`.
    pub fn new(image_root: impl AsRef<Path>) -> Self {
        Self::with_max_uncompressed_layer_bytes(
            image_root,
            configured_max_uncompressed_layer_bytes(),
        )
    }

    /// Creates a layer cache with an explicit decoded-output ceiling.
    pub fn with_max_uncompressed_layer_bytes(
        image_root: impl AsRef<Path>,
        max_uncompressed_layer_bytes: u64,
    ) -> Self {
        Self {
            root: image_root.as_ref().join(".oci/layers/sha256"),
            max_uncompressed_layer_bytes,
            decompression_permits: shared_decompression_permits(),
        }
    }

    /// Returns the verified tar path for one compressed descriptor/diff-ID pair.
    pub fn path_for(
        &self,
        descriptor: &Descriptor,
        diff_id: &Sha256Digest,
    ) -> Result<PathBuf, ResolveError> {
        let compression = layer_compression(&descriptor.media_type)
            .ok_or_else(|| ResolveError::UnsupportedMediaType(descriptor.media_type.clone()))?;
        Ok(self.root.join(layer_relative_path(
            &descriptor.digest,
            diff_id,
            compression,
        )))
    }

    async fn cache_layer(
        &self,
        source: &CachedBlob,
        diff_id: &Sha256Digest,
        compression: LayerCompression,
    ) -> Result<(PathBuf, u64), ResolveError> {
        let relative = layer_relative_path(&source.descriptor.digest, diff_id, compression);
        let path = self.root.join(&relative);
        let parent = path
            .parent()
            .expect("a layer cache path always has a digest directory");
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|source| cache_io("create directory", parent.to_owned(), source))?;
        let lock = cache_path_lock(&self.root, &relative).await?;
        let _guard = lock.lock().await;

        match verify_uncompressed_layer_file(
            &path,
            source,
            diff_id,
            self.max_uncompressed_layer_bytes,
        )
        .await?
        {
            LayerFileState::Valid(size) => return Ok((path, size)),
            LayerFileState::Missing => {}
            LayerFileState::Corrupt => remove_cache_file(&path).await?,
        }

        let size = self
            .decode_layer(source, diff_id, compression, &path)
            .await?;
        Ok((path, size))
    }

    async fn decode_layer(
        &self,
        source: &CachedBlob,
        diff_id: &Sha256Digest,
        compression: LayerCompression,
        destination: &Path,
    ) -> Result<u64, ResolveError> {
        let _decompression_permit = self
            .decompression_permits
            .acquire()
            .await
            .expect("the shared decompression semaphore is never closed");
        let input = tokio::fs::File::open(&source.path)
            .await
            .map_err(|error| cache_io("open compressed layer", source.path.clone(), error))?;
        // Hash the exact opened file stream underneath the decoder. A separate
        // preflight check would leave a path-replacement race between blob
        // verification and decompression.
        let (input, compressed_state) = VerifyingReader::new(input);
        let input = BufReader::new(input);
        let mut reader: Box<dyn AsyncRead + Send + Unpin> = match compression {
            LayerCompression::Identity => Box::new(input),
            LayerCompression::Gzip => {
                let mut decoder = GzipDecoder::new(input);
                decoder.multiple_members(true);
                Box::new(decoder)
            }
            LayerCompression::Zstd => {
                let mut decoder = ZstdDecoder::with_params(
                    input,
                    &[async_compression::zstd::DParameter::window_log_max(
                        ZSTD_MAX_WINDOW_LOG,
                    )],
                );
                decoder.multiple_members(true);
                Box::new(decoder)
            }
        };

        let parent = destination
            .parent()
            .expect("a layer cache path always has a digest directory");
        let (temporary, mut output) =
            create_partial_file(parent, &source.descriptor.digest).await?;
        let mut cleanup = PartialCleanup::new(temporary.clone());
        let mut hasher = Sha256::new();
        let mut size = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read =
                reader
                    .read(&mut buffer)
                    .await
                    .map_err(|error| ResolveError::Decompression {
                        digest: source.descriptor.digest.clone(),
                        media_type: source.descriptor.media_type.clone(),
                        message: error.to_string(),
                    })?;
            if read == 0 {
                break;
            }
            let next_size = size.checked_add(read as u64).ok_or_else(|| {
                ResolveError::UncompressedLayerTooLarge {
                    compressed_digest: source.descriptor.digest.clone(),
                    limit: self.max_uncompressed_layer_bytes,
                    actual: u64::MAX,
                }
            })?;
            if next_size > self.max_uncompressed_layer_bytes {
                return Err(ResolveError::UncompressedLayerTooLarge {
                    compressed_digest: source.descriptor.digest.clone(),
                    limit: self.max_uncompressed_layer_bytes,
                    actual: next_size,
                });
            }
            hasher.update(&buffer[..read]);
            output
                .write_all(&buffer[..read])
                .await
                .map_err(|error| cache_io("write", temporary.clone(), error))?;
            size = next_size;
            // Codec reads may remain immediately ready while input is
            // buffered. Yield between chunks so a large layer cannot occupy a
            // Tokio worker for the duration of its entire decompression.
            tokio::task::yield_now().await;
        }

        let (compressed_size, compressed_digest) = compressed_state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .finish();
        if compressed_size != source.descriptor.size {
            return Err(ResolveError::SizeMismatch {
                subject: source.descriptor.digest.to_string(),
                expected: source.descriptor.size,
                actual: compressed_size,
            });
        }
        if compressed_digest != source.descriptor.digest {
            return Err(ResolveError::DigestMismatch {
                subject: format!("compressed layer {}", source.descriptor.digest),
                expected: source.descriptor.digest.clone(),
                actual: compressed_digest,
            });
        }

        let actual = Sha256Digest(format!("sha256:{:x}", hasher.finalize()));
        if actual != *diff_id {
            return Err(ResolveError::DiffIdMismatch {
                compressed_digest: source.descriptor.digest.clone(),
                expected: diff_id.clone(),
                actual,
            });
        }
        output
            .flush()
            .await
            .map_err(|error| cache_io("flush", temporary.clone(), error))?;
        output
            .sync_all()
            .await
            .map_err(|error| cache_io("sync", temporary.clone(), error))?;
        drop(output);
        tokio::fs::rename(&temporary, destination)
            .await
            .map_err(|error| cache_io("publish", destination.to_owned(), error))?;
        cleanup.published = true;
        Ok(size)
    }
}

fn layer_relative_path(
    compressed_digest: &Sha256Digest,
    diff_id: &Sha256Digest,
    compression: LayerCompression,
) -> PathBuf {
    PathBuf::from(diff_id.encoded()).join(format!(
        "{}.{}.tar",
        compressed_digest.encoded(),
        compression.cache_name()
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayerFileState {
    Missing,
    Valid(u64),
    Corrupt,
}

async fn verify_uncompressed_layer_file(
    path: &Path,
    source: &CachedBlob,
    diff_id: &Sha256Digest,
    max_uncompressed_layer_bytes: u64,
) -> Result<LayerFileState, ResolveError> {
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(LayerFileState::Missing);
        }
        Err(error) => return Err(cache_io("inspect", path.to_owned(), error)),
    };
    if !metadata.file_type().is_file() {
        return Ok(LayerFileState::Corrupt);
    }
    if metadata.len() > max_uncompressed_layer_bytes {
        return Err(ResolveError::UncompressedLayerTooLarge {
            compressed_digest: source.descriptor.digest.clone(),
            limit: max_uncompressed_layer_bytes,
            actual: metadata.len(),
        });
    }

    let (size, actual) = match hash_file(path).await {
        Ok(result) => result,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(LayerFileState::Missing);
        }
        Err(error) => return Err(cache_io("verify", path.to_owned(), error)),
    };
    if actual != *diff_id {
        return Ok(LayerFileState::Corrupt);
    }
    Ok(LayerFileState::Valid(size))
}

/// One verified uncompressed layer, retaining both OCI content identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecompressedLayer {
    /// Original cached registry bytes and their manifest descriptor.
    pub source: CachedBlob,
    /// Digest of the uncompressed tar stream from config `rootfs.diff_ids`.
    pub diff_id: Sha256Digest,
    /// Verified uncompressed tar path below `.oci/layers/sha256/`.
    pub path: PathBuf,
    /// Number of bytes in the uncompressed tar stream.
    pub size: u64,
}

/// A decompressed layer whose complete tar metadata passed the import safety
/// preflight.
///
/// The wrapper is deliberately distinct from [`DecompressedLayer`] so future
/// extraction and merge stages can require proof that validation ran first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedLayer {
    layer: DecompressedLayer,
}

impl ValidatedLayer {
    /// Original cached registry bytes and their manifest descriptor.
    pub fn source(&self) -> &CachedBlob {
        &self.layer.source
    }

    /// Digest of the uncompressed tar stream from config `rootfs.diff_ids`.
    pub fn diff_id(&self) -> &Sha256Digest {
        &self.layer.diff_id
    }

    /// Path of the validated, uncompressed layer tar.
    pub fn path(&self) -> &Path {
        &self.layer.path
    }

    /// Number of bytes in the validated tar stream.
    pub fn size(&self) -> u64 {
        self.layer.size
    }
}

/// A fully merged OCI filesystem tree published after every layer succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedRootfs {
    path: PathBuf,
}

impl MergedRootfs {
    /// Path to the merged staging tree.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// A merged tree that has been given a bootable Firecrab guest runtime.
///
/// Deliberately distinct from [`MergedRootfs`] so the later ext4 stage can
/// require proof that a PID 1, a DHCP client, the readiness sentinel, and the
/// metrics agent are present before it builds something meant to boot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionedRootfs {
    path: PathBuf,
    toolbox: Sha256Digest,
}

impl ProvisionedRootfs {
    /// Path to the provisioned staging tree.
    ///
    /// Injection edits the merged tree in place, so this is the same path
    /// [`MergedRootfs::path`] reported.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Digest of the toolbox program installed as the guest's PID 1.
    pub fn toolbox_digest(&self) -> &Sha256Digest {
        &self.toolbox
    }
}

/// A provisioned tree packed into a sized ext4 image that still has headroom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciExt4Image {
    path: PathBuf,
    size_bytes: u64,
    payload_bytes: u64,
    free_bytes: u64,
    toolbox: Sha256Digest,
}

impl OciExt4Image {
    /// Host path of the published ext4 image.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Length of the image file in bytes.
    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// Measured payload of the provisioned tree packed into the image.
    pub fn payload_bytes(&self) -> u64 {
        self.payload_bytes
    }

    /// Free space remaining after packing, from `tune2fs`.
    pub fn free_bytes(&self) -> u64 {
        self.free_bytes
    }

    /// Digest of the toolbox program the provisioned tree will boot.
    pub fn toolbox_digest(&self) -> &Sha256Digest {
        &self.toolbox
    }
}

/// An ext4 image paired with the kernel and boot args a TemplateSpec needs.
///
/// Deliberately distinct from [`OciExt4Image`] so registration can require
/// proof that an architecture-matched kernel was chosen. An OCI image
/// supplies neither field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciBootableImage {
    rootfs: OciExt4Image,
    kernel: PathBuf,
    initrd: Option<PathBuf>,
    boot_args: String,
    architecture: Architecture,
}

impl OciBootableImage {
    /// Packed ext4 this pair will boot.
    pub fn rootfs(&self) -> &OciExt4Image {
        &self.rootfs
    }

    /// Kernel path relative to the image root.
    pub fn kernel(&self) -> &Path {
        &self.kernel
    }

    /// Initrd path relative to the image root, if the paired kernel needs one.
    ///
    /// The current catalog pair does not: a module-less OCI tree cannot use
    /// a distro initrd without losing the injected guest init.
    pub fn initrd(&self) -> Option<&Path> {
        self.initrd.as_deref()
    }

    /// Firecracker kernel command line recorded from the paired kernel.
    pub fn boot_args(&self) -> &str {
        &self.boot_args
    }

    /// Architecture the paired kernel was classified as.
    pub fn architecture(&self) -> Architecture {
        self.architecture
    }
}

/// Alias and version later registration can copy into a [`crate::templates::TemplateSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciTemplateName {
    /// User-facing alias derived from the reference.
    pub alias: String,
    /// Version tag or digest pin from the reference.
    pub version: String,
}

/// A bootable OCI image that has a unique alias and version.
///
/// Deliberately distinct from [`OciBootableImage`] so registration can require
/// proof that the name was derived and does not collide with an installed
/// image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedOciImage {
    image: OciBootableImage,
    alias: String,
    version: String,
}

impl NamedOciImage {
    /// Kernel, boot args, and packed ext4 this name will register.
    pub fn image(&self) -> &OciBootableImage {
        &self.image
    }

    /// Unique alias derived from the image reference.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// Version derived from the image reference's tag or digest.
    pub fn version(&self) -> &str {
        &self.version
    }
}

/// A named OCI image that has been published and registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredOciImage {
    alias: String,
    version: String,
    rootfs: PathBuf,
}

impl RegisteredOciImage {
    /// Registered alias.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// Registered version.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Rootfs path relative to the image root.
    pub fn rootfs(&self) -> &Path {
        &self.rootfs
    }
}

/// Process fields from an OCI image configuration.
///
/// These become a service under the injected init, never PID 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciProcessConfig {
    entrypoint: Vec<String>,
    cmd: Vec<String>,
    env: Vec<String>,
    working_dir: String,
}

impl OciProcessConfig {
    /// Reads Entrypoint, Cmd, Env, and WorkingDir from an image config blob.
    pub fn from_image_config(bytes: &[u8]) -> Result<Self, ResolveError> {
        service::process_config_from_image_config(bytes)
    }

    /// Config `Entrypoint`.
    pub fn entrypoint(&self) -> &[String] {
        &self.entrypoint
    }

    /// Config `Cmd`.
    pub fn cmd(&self) -> &[String] {
        &self.cmd
    }

    /// Config `Env` entries, each `KEY=value`.
    pub fn env(&self) -> &[String] {
        &self.env
    }

    /// Config `WorkingDir`, empty when the image did not set one.
    pub fn working_dir(&self) -> &str {
        &self.working_dir
    }

    /// The argv the service will exec: Entrypoint followed by Cmd.
    pub fn argv(&self) -> Vec<&str> {
        self.entrypoint
            .iter()
            .chain(self.cmd.iter())
            .map(String::as_str)
            .collect()
    }
}

/// A verified static program cached on the host to become a guest's init.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolboxProgram {
    path: PathBuf,
    digest: Sha256Digest,
    size: u64,
}

impl ToolboxProgram {
    /// Host path of the cached, verified program.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// SHA-256 of the program bytes, recorded as guest provenance.
    pub fn digest(&self) -> &Sha256Digest {
        &self.digest
    }

    /// Program size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }
}

/// A verified fastfetch program cached on the host for glibc guests.
///
/// Unlike the toolbox, this binary is dynamically linked: official polyfilled
/// builds need only GLIBC_2.17, which Debian bookworm, Ubuntu, and Rocky all
/// satisfy. It is never PID 1, so a missing copy must not fail an import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FastfetchProgram {
    /// Host path of the cached, verified program.
    path: PathBuf,
    /// SHA-256 of the program bytes.
    digest: Sha256Digest,
    /// Program size in bytes.
    size: u64,
}

impl FastfetchProgram {
    /// Host path of the cached, verified program.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// SHA-256 of the program bytes.
    pub fn digest(&self) -> &Sha256Digest {
        &self.digest
    }

    /// Program size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }
}

/// Host-side inputs the guest runtime stage cannot derive from a merged tree.
///
/// The toolbox program is pulled through the same verified pipeline as the
/// image being imported, so it needs the same two caches. Grouping them means a
/// later ext4 or registration stage adds a field instead of an argument.
#[derive(Debug, Clone, Copy)]
pub struct GuestRuntimeOptions<'a> {
    /// Image root whose `.oci/` subtree backs every cache below.
    pub image_root: &'a Path,
    /// Verified raw blob cache the toolbox pull may fill.
    pub blobs: &'a BlobCache,
    /// Verified uncompressed-layer cache the toolbox pull may fill.
    pub layers: &'a LayerCache,
    /// Architecture the merged tree targets; the toolbox ELF must match it.
    pub architecture: Architecture,
    /// Stored login for the toolbox pull. The toolbox is a Docker Hub image
    /// by default, so an operator whose anonymous quota is exhausted must be
    /// able to authenticate this pull too, not only the image being imported.
    pub credential: Option<&'a RegistryCredential>,
}

/// Why an otherwise valid layer tar entry is unsafe for later extraction.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TarMemberViolation {
    /// The member name is empty, absolute, contains a parent/root/prefix
    /// component, or names the archive root with a non-directory entry.
    #[error(
        "member path must be relative beneath the extraction root; only a directory may name the root itself"
    )]
    Path,
    /// Only regular files, directories, symbolic links, and hard links are
    /// supported by the MVP extraction contract.
    #[error("tar entry type 0x{entry_type:02x} is not permitted")]
    UnsupportedEntryType {
        /// Raw tar typeflag byte.
        entry_type: u8,
    },
    /// A hard link did not identify the archive member it aliases.
    #[error("hard link target is missing")]
    MissingHardlinkTarget,
    /// A symbolic link did not identify the path stored in the link.
    #[error("symbolic link target is missing")]
    MissingSymlinkTarget,
    /// A symbolic link target cannot be represented by filesystem APIs.
    #[error("symbolic link target contains a NUL byte")]
    InvalidSymlinkTarget,
    /// Two tar entries resolve to the same normalized path.
    #[error("layer contains a duplicate normalized path")]
    DuplicatePath,
    /// A non-directory entry also has entries beneath it in the same layer.
    #[error("non-directory entry conflicts with descendant {descendant:?}")]
    ConflictingPath {
        /// Descendant that makes the final tree ambiguous.
        descendant: PathBuf,
    },
    /// A whiteout marker has no valid sibling basename to remove.
    #[error("whiteout marker has an invalid target basename")]
    InvalidWhiteoutTarget,
    /// Whiteout markers must be regular files.
    #[error("whiteout marker must be a regular file")]
    InvalidWhiteoutType,
    /// Whiteout markers must have an empty payload.
    #[error("whiteout marker must be empty, but declares {size} bytes")]
    NonEmptyWhiteout {
        /// Marker payload length.
        size: u64,
    },
    /// Applying the current layer would traverse a symlink from an older one.
    #[error("operation would traverse symlink ancestor {ancestor:?}")]
    SymlinkAncestor {
        /// Relative path of the unsafe ancestor.
        ancestor: PathBuf,
    },
    /// A path component required as a directory is another filesystem type.
    #[error("path ancestor {ancestor:?} is not a directory")]
    NonDirectoryAncestor {
        /// Relative path of the conflicting ancestor.
        ancestor: PathBuf,
    },
    /// A hard-link target was not produced by a lower or current layer.
    #[error("hard link target {target:?} does not exist")]
    MissingMergedHardlinkTarget {
        /// Archive-root-relative target.
        target: PathBuf,
    },
    /// Hard links to directories are unsupported and unsafe.
    #[error("hard link target {target:?} is a directory")]
    DirectoryHardlinkTarget {
        /// Archive-root-relative directory target.
        target: PathBuf,
    },
    /// Tar hard-link names are archive-root-relative and cannot leave that
    /// root.
    #[error("hard link target {target:?} is outside the archive root")]
    HardlinkTarget {
        /// Unsafe target recorded in the tar header or extension metadata.
        target: PathBuf,
    },
    /// A PAX attribute is unsupported because different tar parsers can apply
    /// it to a different path, link, or payload boundary.
    #[error("unsupported PAX attribute {key:?}")]
    UnsupportedPaxAttribute {
        /// Attribute name supplied by the archive.
        key: String,
    },
}

/// Why a toolbox program cannot serve as an imported guest's init.
///
/// The host never runs the program to find out. Every rule below is decided
/// from the bytes on disk, because executing an unverified registry payload to
/// test it would be the exact thing this stage exists to avoid.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ToolboxViolation {
    /// Only a plain file can be copied into a guest tree.
    #[error("the member is not a regular file")]
    NotRegularFile,
    /// An empty file would leave the guest without a PID 1.
    #[error("the program is empty")]
    Empty,
    /// A mirrored override must not copy an unbounded file into every rootfs.
    #[error("the program is {size} bytes, over the {limit}-byte limit")]
    TooLarge {
        /// Observed program size.
        size: u64,
        /// Configured ceiling.
        limit: u64,
    },
    /// Firecracker boots 64-bit little-endian guests only.
    #[error("the program is not a 64-bit little-endian ELF image")]
    NotElf,
    /// A program built for another machine cannot be this guest's PID 1.
    #[error("the program targets ELF machine {actual}, but this host needs {expected}")]
    ForeignArchitecture {
        /// ELF machine this host requires.
        expected: u16,
        /// ELF machine the program declares.
        actual: u16,
    },
    /// Shared objects and relocatable files are not programs.
    #[error("the ELF image is not an executable")]
    NotExecutable,
    /// A merged container tree has no dynamic loader to satisfy the request.
    #[error("the program needs the dynamic loader {interpreter:?}; only a static program can boot")]
    DynamicallyLinked {
        /// Interpreter path recorded in the `PT_INTERP` segment.
        interpreter: String,
    },
    /// The program header table did not fit inside the file it describes.
    #[error("the ELF program header table is malformed")]
    MalformedProgramHeaders,
}

/// Why a guest path in a merged tree cannot be written safely.
///
/// Ancestor symbolic links are followed rather than refused: usr-merged images
/// ship `/sbin` as a link to `usr/sbin`, so refusing them would reject Ubuntu,
/// Debian, and Fedora outright. Resolution is clamped to the tree instead.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum GuestPathViolation {
    /// A path component required as a directory is another filesystem type.
    #[error("ancestor {ancestor:?} is not a directory")]
    NonDirectoryAncestor {
        /// Tree-relative path of the conflicting ancestor.
        ancestor: PathBuf,
    },
    /// Following the image's symbolic links never reached a real directory.
    #[error("resolving it followed more than {limit} symbolic links")]
    SymlinkLoop {
        /// Traversal budget the tree exhausted.
        limit: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LayerWorkKey {
    compressed_digest: Sha256Digest,
    diff_id: Sha256Digest,
    compression: LayerCompression,
}

/// Decompresses and verifies every cached layer against config `diff_ids`.
///
/// Decoder work is bounded by the host's logical CPU count and the returned
/// vector always retains manifest order. This stage produces tar streams only;
/// member validation, extraction, whiteouts, and layer merging happen later.
pub async fn decompress_cached_layers(
    image: &CachedImageBlobs,
    cache: &LayerCache,
) -> Result<Vec<DecompressedLayer>, ResolveError> {
    let available = std::thread::available_parallelism()
        .map(std::num::NonZero::get)
        .unwrap_or(1);
    let parallelism = decompression_parallelism(available);
    decompress_cached_layers_with_parallelism(image, cache, parallelism).await
}

fn decompression_parallelism(available: usize) -> usize {
    available.clamp(1, MAX_PARALLEL_DECOMPRESSIONS)
}

async fn decompress_cached_layers_with_parallelism(
    image: &CachedImageBlobs,
    cache: &LayerCache,
    parallelism: usize,
) -> Result<Vec<DecompressedLayer>, ResolveError> {
    let diff_ids = read_config_diff_ids(image).await?;
    let layers = image
        .layers
        .iter()
        .cloned()
        .zip(diff_ids)
        .map(|(source, diff_id)| {
            let compression =
                layer_compression(&source.descriptor.media_type).ok_or_else(|| {
                    ResolveError::UnsupportedMediaType(source.descriptor.media_type.clone())
                })?;
            let key = LayerWorkKey {
                compressed_digest: source.descriptor.digest.clone(),
                diff_id,
                compression,
            };
            Ok((key, source))
        })
        .collect::<Result<Vec<_>, ResolveError>>()?;

    let mut seen = std::collections::HashSet::new();
    let work: Vec<(LayerWorkKey, CachedBlob)> = layers
        .iter()
        .filter(|(key, _)| seen.insert(key.clone()))
        .cloned()
        .collect();
    let completed: Vec<(LayerWorkKey, PathBuf, u64)> = stream::iter(work)
        .map(|(key, source)| {
            let cache = cache.clone();
            async move {
                let (path, size) = cache
                    .cache_layer(&source, &key.diff_id, key.compression)
                    .await?;
                Ok::<_, ResolveError>((key, path, size))
            }
        })
        .buffer_unordered(parallelism.max(1))
        .try_collect()
        .await?;
    let completed: HashMap<LayerWorkKey, (PathBuf, u64)> = completed
        .into_iter()
        .map(|(key, path, size)| (key, (path, size)))
        .collect();

    Ok(layers
        .into_iter()
        .map(|(key, source)| {
            let (path, size) = completed
                .get(&key)
                .expect("every unique layer relationship produces one cache path");
            DecompressedLayer {
                source,
                diff_id: key.diff_id,
                path: path.clone(),
                size: *size,
            }
        })
        .collect())
}

/// Validates every decompressed layer tar before any extraction may begin.
///
/// Validation reads the effective GNU/PAX member paths, rejects member names
/// and hard-link targets that leave an archive root, skips character devices,
/// block devices, and FIFOs, and permits only regular files, directories,
/// symbolic links, and archive-root-confined hard links.
/// Symbolic links remain inert at this stage; the later extractor must not
/// follow one while creating another member. The input content-addressed tar
/// remains cached when validation fails: unsafe contents do not make a
/// correctly hashed cache entry corrupt.
///
/// Repeated manifest entries that resolve to the same cache path are scanned
/// once, while the returned wrappers retain manifest order and multiplicity.
pub async fn validate_decompressed_layers(
    layers: Vec<DecompressedLayer>,
) -> Result<Vec<ValidatedLayer>, ResolveError> {
    let mut validated_paths = std::collections::HashSet::new();
    for layer in &layers {
        if validated_paths.insert(layer.path.clone()) {
            validate_layer_archive(layer).await?;
        }
    }
    Ok(layers
        .into_iter()
        .map(|layer| ValidatedLayer { layer })
        .collect())
}

/// Applies validated OCI layers in manifest order to a new filesystem tree.
///
/// Each layer's whiteouts are applied before that layer's ordinary members,
/// regardless of marker order in the tar. Work happens in a private sibling
/// directory and becomes visible at `destination` only after every layer and
/// final directory attribute succeeds. The destination must not already
/// exist. Verified blob and decompressed-layer caches are never removed when
/// merging fails.
pub async fn merge_validated_layers(
    layers: &[ValidatedLayer],
    destination: &Path,
) -> Result<MergedRootfs, ResolveError> {
    merge::merge_validated_layers(layers, destination).await
}

/// Pulls (or reuses) the pinned toolbox image and returns its static program.
///
/// The program is fetched through the same verified pipeline as any other
/// image and cached under the image root, so only the first import on a host
/// contacts the registry. Operators can point
/// `FIRECRAB_OCI_TOOLBOX_IMAGE` at a mirror.
pub async fn provision_toolbox(
    options: &GuestRuntimeOptions<'_>,
) -> Result<ToolboxProgram, ResolveError> {
    busybox::ensure_toolbox(options).await
}

/// Pulls (or reuses) the pinned fastfetch program for glibc guests.
///
/// A missing or unverifiable program is `None`: the console still boots, and
/// the injected boot script may try the guest package manager as a fallback.
/// Operators can name a host binary with `FIRECRAB_OCI_FASTFETCH_PATH`.
pub async fn ensure_guest_fastfetch(image_root: &Path) -> Option<FastfetchProgram> {
    fastfetch::ensure_fastfetch(image_root, Architecture::HOST).await
}

/// Installs a bootable Firecrab guest runtime into a merged OCI tree.
///
/// A container tree has no PID 1, no DHCP client, and nothing that reports
/// readiness, so it cannot boot as a MicroVM. This stage adds an init, a
/// DHCP client, the readiness sentinel, and the metrics agent, editing the
/// merged tree in place. The merged handle is consumed because injection is
/// not repeatable, and a failure restores exactly the paths it touched.
pub async fn provision_merged_rootfs(
    rootfs: MergedRootfs,
    options: &GuestRuntimeOptions<'_>,
) -> Result<ProvisionedRootfs, ResolveError> {
    provision::provision_merged_rootfs(rootfs, options).await
}

/// Packs a provisioned tree into a new ext4 image sized from that tree.
///
/// The image is large enough for the payload plus headroom. A result that
/// lands full is deleted and returned as an error rather than registered
/// later as a bootable template. The destination must not already exist.
/// The source tree is left in place. This stage does not pair a kernel or
/// register a template.
pub async fn write_provisioned_ext4(
    rootfs: &ProvisionedRootfs,
    destination: &Path,
) -> Result<OciExt4Image, ResolveError> {
    ext4::write_provisioned_ext4(rootfs, destination).await
}

/// Pairs a packed ext4 with this host's architecture-matched kernel and
/// boot args.
///
/// `TemplateSpec` requires both; an OCI image supplies neither. The kernel is
/// the digest-pinned artifact Firecrab publishes for this architecture,
/// fetched once and cached under `image_root`; a host that cannot reach the
/// registry falls back to an installed catalog kernel. The ext4 is left in
/// place. This stage does not register a template.
pub async fn pair_ext4_with_host_kernel(
    image: OciExt4Image,
    image_root: &Path,
) -> Result<OciBootableImage, ResolveError> {
    let pair = boot::host_kernel_pair(image_root).await?;
    boot::pair_ext4_with_kernel(image, image_root, &pair)
}

/// Derives a unique alias and version from the reference and attaches them
/// to a paired image.
///
/// An installed alias or a catalog alias is a collision and is refused.
/// This stage does not register a template.
pub fn name_oci_image(
    image: OciBootableImage,
    reference: &ImageReference,
    templates: &crate::templates::TemplateRegistry,
) -> Result<NamedOciImage, ResolveError> {
    name::name_oci_image(image, reference, templates)
}

/// Publishes the packed ext4 under the image root and registers it.
///
/// A failed publish or registration removes the partial rootfs and leaves
/// the source ext4 in place. The shared kernel is not copied.
pub fn register_named_oci_image(
    named: NamedOciImage,
    templates: &crate::templates::TemplateRegistry,
) -> Result<RegisteredOciImage, ResolveError> {
    register::register_named_oci_image(named, templates)
}

/// Translates the image process config into a service under the injected init.
///
/// The script lands in `/etc/firecrab/services.d` and is started after the
/// readiness sentinel. It is never PID 1. An image with no command is a
/// no-op.
pub fn install_oci_service(
    rootfs: &ProvisionedRootfs,
    process: &OciProcessConfig,
) -> Result<(), ResolveError> {
    service::install_oci_service(rootfs, process)
}

/// Builds the candidate alias and version without consulting the registry.
pub fn template_name_from_reference(
    reference: &ImageReference,
) -> Result<OciTemplateName, ResolveError> {
    name::template_name_from_reference(reference)
}

/// Derives the name and refuses it when an installed or reserved alias exists.
pub fn claim_template_name(
    reference: &ImageReference,
    templates: &TemplateRegistry,
) -> Result<OciTemplateName, ResolveError> {
    name::claim_template_name(reference, templates)
}

/// Runs the already-landed import stages as one background job.
///
/// Scratch lives at `{image_root}/.oci/import/{alias}/` and is removed when
/// the job finishes, whether it succeeded or failed. Verified blob and
/// decompressed-layer caches stay in place.
pub async fn run_oci_import(
    tracker: ImageInstallTracker,
    templates: TemplateRegistry,
    reference: ImageReference,
    alias: String,
    credential: Option<RegistryCredential>,
) {
    if let Err(error) = import_oci_image(
        &tracker,
        &templates,
        &reference,
        &alias,
        credential.as_ref(),
    )
    .await
    {
        tracker.finish_err_with(&alias, format!("import failed: {error}"));
        return;
    }
    tracker.finish_ok_with(&alias, "import succeeded — template registered");
}

async fn import_oci_image(
    tracker: &ImageInstallTracker,
    templates: &TemplateRegistry,
    reference: &ImageReference,
    alias: &str,
    credential: Option<&RegistryCredential>,
) -> Result<(), ResolveError> {
    let image_root = templates.image_root_path();
    let scratch = image_root.join(".oci/import").join(alias);
    reset_import_scratch(&scratch).await?;

    let result = import_oci_image_in_scratch(
        tracker, templates, reference, alias, image_root, &scratch, credential,
    )
    .await;
    if let Err(error) = tokio::fs::remove_dir_all(&scratch).await
        && error.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(
            alias,
            path = %scratch.display(),
            error = %error,
            "failed to remove OCI import scratch"
        );
    }
    result
}

async fn reset_import_scratch(scratch: &Path) -> Result<(), ResolveError> {
    match tokio::fs::remove_dir_all(scratch).await {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(cache_io("remove import scratch", scratch.to_owned(), error));
        }
    }
    tokio::fs::create_dir_all(scratch)
        .await
        .map_err(|error| cache_io("create import scratch", scratch.to_owned(), error))
}

async fn import_oci_image_in_scratch(
    tracker: &ImageInstallTracker,
    templates: &TemplateRegistry,
    reference: &ImageReference,
    alias: &str,
    image_root: &Path,
    scratch: &Path,
    credential: Option<&RegistryCredential>,
) -> Result<(), ResolveError> {
    let insecure = is_loopback_registry(&reference.registry);
    let blobs = BlobCache::new(image_root);
    let layers = LayerCache::new(image_root);

    tracker.append_log(alias, "caching image blobs");
    let cached = cache_image_blobs(
        reference,
        Architecture::HOST,
        insecure,
        &blobs,
        credential.cloned(),
    )
    .await?;

    tracker.append_log(alias, "reading image process config");
    let process = read_cached_process_config(&cached).await?;

    tracker.append_log(alias, "decompressing layers");
    let decompressed = decompress_cached_layers(&cached, &layers).await?;

    tracker.append_log(alias, "validating layer archives");
    let validated = validate_decompressed_layers(decompressed).await?;

    tracker.append_log(alias, "merging layers");
    let merged = merge_validated_layers(&validated, &scratch.join("rootfs")).await?;

    tracker.append_log(alias, "provisioning guest runtime");
    let options = GuestRuntimeOptions {
        image_root,
        blobs: &blobs,
        layers: &layers,
        architecture: cached.resolved.architecture,
        credential,
    };
    let provisioned = provision_merged_rootfs(merged, &options).await?;

    tracker.append_log(alias, "installing image service");
    install_oci_service(&provisioned, &process)?;

    tracker.append_log(alias, "writing ext4 image");
    let ext4 = write_provisioned_ext4(&provisioned, &scratch.join("rootfs.ext4")).await?;

    tracker.append_log(alias, "pairing host kernel");
    let bootable = pair_ext4_with_host_kernel(ext4, image_root).await?;

    tracker.append_log(alias, "naming and registering template");
    let named = name_oci_image(bootable, reference, templates)?;
    register_named_oci_image(named, templates)?;
    Ok(())
}

async fn read_cached_process_config(
    cached: &CachedImageBlobs,
) -> Result<OciProcessConfig, ResolveError> {
    let bytes = tokio::fs::read(&cached.config.path)
        .await
        .map_err(|error| cache_io("read config", cached.config.path.clone(), error))?;
    OciProcessConfig::from_image_config(&bytes)
}

async fn validate_layer_archive(layer: &DecompressedLayer) -> Result<(), ResolveError> {
    let path = layer.path.clone();
    let compressed_digest = layer.source.descriptor.digest.clone();
    let task_digest = compressed_digest.clone();
    tokio::task::spawn_blocking(move || validate_layer_archive_blocking(&path, &task_digest))
        .await
        .map_err(|error| ResolveError::MalformedLayerArchive {
            compressed_digest,
            message: format!("validation task failed: {error}"),
        })?
}

fn validate_layer_archive_blocking(
    path: &Path,
    compressed_digest: &Sha256Digest,
) -> Result<(), ResolveError> {
    let mut file = std::fs::File::open(path)
        .map_err(|error| cache_io("open layer tar", path.to_owned(), error))?;
    validate_layer_archive_file(&mut file, path, compressed_digest)
}

fn validate_layer_archive_file(
    file: &mut std::fs::File,
    path: &Path,
    compressed_digest: &Sha256Digest,
) -> Result<(), ResolveError> {
    let file_len = file
        .metadata()
        .map_err(|error| cache_io("inspect layer tar", path.to_owned(), error))?
        .len();
    if file_len % 512 != 0 {
        return Err(malformed_layer_archive(
            compressed_digest,
            format!("archive length {file_len} is not a multiple of the 512-byte tar block size"),
        ));
    }
    file.seek(io::SeekFrom::Start(0))
        .map_err(|error| cache_io("rewind layer tar", path.to_owned(), error))?;
    let mut archive = tar::Archive::new(&mut *file);
    let raw_last_end = validate_raw_tar_metadata(&mut archive, file_len, compressed_digest)?;
    let file = archive.into_inner();
    validate_tar_terminator(file, raw_last_end, file_len, compressed_digest)?;
    file.seek(io::SeekFrom::Start(0))
        .map_err(|error| cache_io("rewind layer tar", path.to_owned(), error))?;
    let mut archive = tar::Archive::new(&mut *file);
    let entries = archive
        .entries()
        .map_err(|error| malformed_layer_archive(compressed_digest, error))?;

    let mut semantic_last_end = 0;
    for (index, entry) in entries.enumerate() {
        let mut entry = entry.map_err(|error| {
            malformed_layer_archive(
                compressed_digest,
                format!("could not read member {}: {error}", index + 1),
            )
        })?;
        let entry_type = entry.header().entry_type();
        let mut pax_linkpaths = Vec::new();
        if let Some(extensions) = entry.pax_extensions().map_err(|error| {
            malformed_layer_archive(
                compressed_digest,
                format!("could not read member {} PAX metadata: {error}", index + 1),
            )
        })? {
            for extension in extensions {
                let extension = extension.map_err(|error| {
                    malformed_layer_archive(
                        compressed_digest,
                        format!("member {} has malformed PAX metadata: {error}", index + 1),
                    )
                })?;
                if extension.key_bytes() == b"linkpath" {
                    pax_linkpaths.push(extension.value_bytes().to_vec());
                }
            }
        }

        semantic_last_end = tar_entry_end(&entry, file_len, compressed_digest, index + 1)?;
        if entry_type.is_pax_global_extensions() {
            // The raw pass already parsed and restricted the global records.
            // tar-rs deliberately does not apply them; the remaining allowed
            // attributes cannot affect path, link, size, or entry type.
            continue;
        }
        let member_path = entry
            .path()
            .map_err(|error| {
                malformed_layer_archive(
                    compressed_digest,
                    format!("could not decode member {} path: {error}", index + 1),
                )
            })?
            .into_owned();
        if !is_safe_layer_entry_path(&member_path, entry_type) {
            return Err(unsafe_tar_member(
                compressed_digest,
                member_path,
                TarMemberViolation::Path,
            ));
        }
        if is_skipped_special_layer_member(entry_type) {
            // Distro root layers ship /dev/console and /dev/initctl. Skip
            // them; drain below so a truncated payload is still malformed
            // and later members stay aligned. Merge must not unpack them.
        } else if entry_type.is_hard_link() {
            for target in pax_linkpaths {
                if !is_safe_layer_member_bytes(&target) {
                    return Err(unsafe_tar_member(
                        compressed_digest,
                        member_path,
                        TarMemberViolation::HardlinkTarget {
                            target: PathBuf::from(String::from_utf8_lossy(&target).into_owned()),
                        },
                    ));
                }
            }
            let target = entry
                .link_name()
                .map_err(|error| {
                    malformed_layer_archive(
                        compressed_digest,
                        format!(
                            "could not decode hard link target for member {}: {error}",
                            index + 1
                        ),
                    )
                })?
                .map(|target| target.into_owned())
                .ok_or_else(|| {
                    unsafe_tar_member(
                        compressed_digest,
                        member_path.clone(),
                        TarMemberViolation::MissingHardlinkTarget,
                    )
                })?;
            if target.as_os_str().is_empty() {
                return Err(unsafe_tar_member(
                    compressed_digest,
                    member_path,
                    TarMemberViolation::MissingHardlinkTarget,
                ));
            }
            if !is_safe_layer_member_path(&target) {
                return Err(unsafe_tar_member(
                    compressed_digest,
                    member_path,
                    TarMemberViolation::HardlinkTarget { target },
                ));
            }
        } else if entry_type.is_symlink() {
            for target in pax_linkpaths {
                if target.is_empty() {
                    return Err(unsafe_tar_member(
                        compressed_digest,
                        member_path,
                        TarMemberViolation::MissingSymlinkTarget,
                    ));
                }
                if target.contains(&0) {
                    return Err(unsafe_tar_member(
                        compressed_digest,
                        member_path,
                        TarMemberViolation::InvalidSymlinkTarget,
                    ));
                }
            }
            let target = entry
                .link_name()
                .map_err(|error| {
                    malformed_layer_archive(
                        compressed_digest,
                        format!(
                            "could not decode symbolic link target for member {}: {error}",
                            index + 1
                        ),
                    )
                })?
                .map(|target| target.into_owned())
                .ok_or_else(|| {
                    unsafe_tar_member(
                        compressed_digest,
                        member_path.clone(),
                        TarMemberViolation::MissingSymlinkTarget,
                    )
                })?;
            if target.as_os_str().is_empty() {
                return Err(unsafe_tar_member(
                    compressed_digest,
                    member_path,
                    TarMemberViolation::MissingSymlinkTarget,
                ));
            }
            if target.as_os_str().as_encoded_bytes().contains(&0) {
                return Err(unsafe_tar_member(
                    compressed_digest,
                    member_path,
                    TarMemberViolation::InvalidSymlinkTarget,
                ));
            }
            // Absolute and parent-relative symbolic links are ordinary inside
            // container root filesystems. They are safe only while inert; the
            // later extractor must never follow a link while creating another
            // member beneath the staging root.
            drop(target);
        } else if !entry_type.is_file() && !entry_type.is_dir() && !entry_type.is_symlink() {
            return Err(unsafe_tar_member(
                compressed_digest,
                member_path,
                TarMemberViolation::UnsupportedEntryType {
                    entry_type: entry_type.as_byte(),
                },
            ));
        }

        let expected = entry.size();
        let actual = io::copy(&mut entry, &mut io::sink()).map_err(|error| {
            malformed_layer_archive(
                compressed_digest,
                format!("could not read member {} payload: {error}", index + 1),
            )
        })?;
        if actual != expected {
            return Err(malformed_layer_archive(
                compressed_digest,
                format!(
                    "member {} payload is truncated: expected {expected} bytes, got {actual}",
                    index + 1
                ),
            ));
        }
    }
    let file = archive.into_inner();
    validate_tar_terminator(file, semantic_last_end, file_len, compressed_digest)?;
    file.seek(io::SeekFrom::Start(0))
        .map_err(|error| cache_io("rewind layer tar", path.to_owned(), error))?;
    Ok(())
}

fn validate_raw_tar_metadata<R: io::Read + io::Seek>(
    archive: &mut tar::Archive<R>,
    file_len: u64,
    compressed_digest: &Sha256Digest,
) -> Result<u64, ResolveError> {
    let entries = archive
        .entries_with_seek()
        .map_err(|error| malformed_layer_archive(compressed_digest, error))?
        .raw(true);
    let mut last_end = 0;
    for (index, entry) in entries.enumerate() {
        let mut entry = entry.map_err(|error| {
            malformed_layer_archive(
                compressed_digest,
                format!("could not inspect raw member {}: {error}", index + 1),
            )
        })?;
        let entry_type = entry.header().entry_type();
        last_end = tar_entry_end(&entry, file_len, compressed_digest, index + 1)?;
        if entry_type.is_gnu_sparse() {
            let path = entry
                .path()
                .map(|path| path.into_owned())
                .unwrap_or_else(|_| PathBuf::from("<GNU sparse member>"));
            return Err(unsafe_tar_member(
                compressed_digest,
                path,
                TarMemberViolation::UnsupportedEntryType { entry_type: b'S' },
            ));
        }
        let buffers_metadata = entry_type.is_gnu_longname()
            || entry_type.is_gnu_longlink()
            || entry_type.is_pax_local_extensions()
            || entry_type.is_pax_global_extensions();
        if buffers_metadata && entry.size() > TAR_METADATA_MAX_BYTES {
            return Err(malformed_layer_archive(
                compressed_digest,
                format!(
                    "member {} declares {} bytes of tar metadata, exceeding the {}-byte limit",
                    index + 1,
                    entry.size(),
                    TAR_METADATA_MAX_BYTES
                ),
            ));
        }
        if entry_type.is_pax_local_extensions() || entry_type.is_pax_global_extensions() {
            validate_pax_metadata(
                &mut entry,
                entry_type.is_pax_global_extensions(),
                compressed_digest,
                index + 1,
            )?;
        }
        if buffers_metadata {
            continue;
        }

        let member_path = entry
            .path()
            .map_err(|error| {
                malformed_layer_archive(
                    compressed_digest,
                    format!("could not decode raw member {} path: {error}", index + 1),
                )
            })?
            .into_owned();
        if !is_safe_layer_entry_path(&member_path, entry_type) {
            return Err(unsafe_tar_member(
                compressed_digest,
                member_path,
                TarMemberViolation::Path,
            ));
        }
        if is_skipped_special_layer_member(entry_type) {
            continue;
        }
        if entry_type.is_hard_link() {
            if let Some(target) = entry
                .link_name()
                .map_err(|error| {
                    malformed_layer_archive(
                        compressed_digest,
                        format!(
                            "could not decode raw hard link target for member {}: {error}",
                            index + 1
                        ),
                    )
                })?
                .map(|target| target.into_owned())
                && !is_safe_layer_member_path(&target)
            {
                return Err(unsafe_tar_member(
                    compressed_digest,
                    member_path,
                    TarMemberViolation::HardlinkTarget { target },
                ));
            }
        } else if !entry_type.is_file() && !entry_type.is_dir() && !entry_type.is_symlink() {
            return Err(unsafe_tar_member(
                compressed_digest,
                member_path,
                TarMemberViolation::UnsupportedEntryType {
                    entry_type: entry_type.as_byte(),
                },
            ));
        }
    }
    Ok(last_end)
}

fn validate_pax_metadata<R: io::Read>(
    entry: &mut tar::Entry<'_, R>,
    global: bool,
    compressed_digest: &Sha256Digest,
    index: usize,
) -> Result<(), ResolveError> {
    let metadata_path = entry
        .path()
        .map(|path| path.into_owned())
        .unwrap_or_else(|_| PathBuf::from("<PAX metadata>"));
    let extensions = entry
        .pax_extensions()
        .map_err(|error| {
            malformed_layer_archive(
                compressed_digest,
                format!("could not read raw member {index} PAX metadata: {error}"),
            )
        })?
        .expect("the caller passes only PAX extension entries");
    let mut security_keys = std::collections::HashSet::new();
    for extension in extensions {
        let extension = extension.map_err(|error| {
            malformed_layer_archive(
                compressed_digest,
                format!("raw member {index} has malformed PAX metadata: {error}"),
            )
        })?;
        let key = extension.key_bytes();
        let value = extension.value_bytes();
        if key.starts_with(b"GNU.sparse.") {
            return Err(unsafe_tar_member(
                compressed_digest,
                metadata_path,
                TarMemberViolation::UnsupportedPaxAttribute {
                    key: String::from_utf8_lossy(key).into_owned(),
                },
            ));
        }
        if matches!(key, b"path" | b"linkpath" | b"size") && !security_keys.insert(key.to_vec()) {
            return Err(malformed_layer_archive(
                compressed_digest,
                format!(
                    "raw member {index} repeats security-sensitive PAX key {:?}",
                    String::from_utf8_lossy(key)
                ),
            ));
        }
        match key {
            b"path" => {
                if global {
                    return Err(unsafe_tar_member(
                        compressed_digest,
                        metadata_path,
                        TarMemberViolation::UnsupportedPaxAttribute {
                            key: "path".to_owned(),
                        },
                    ));
                }
                if !is_safe_layer_member_bytes(value) && !is_layer_root_bytes(value) {
                    return Err(unsafe_tar_member(
                        compressed_digest,
                        PathBuf::from(String::from_utf8_lossy(value).into_owned()),
                        TarMemberViolation::Path,
                    ));
                }
            }
            b"linkpath" => {
                if global {
                    return Err(unsafe_tar_member(
                        compressed_digest,
                        metadata_path,
                        TarMemberViolation::UnsupportedPaxAttribute {
                            key: "linkpath".to_owned(),
                        },
                    ));
                }
            }
            b"size" => {
                // The raw safety pass must know each payload boundary before
                // the semantic parser allocates GNU/PAX buffers. tar-rs raw
                // iteration intentionally ignores PAX size overrides, so
                // accepting one would create a parser differential capable of
                // hiding a following header inside file data.
                return Err(unsafe_tar_member(
                    compressed_digest,
                    metadata_path,
                    TarMemberViolation::UnsupportedPaxAttribute {
                        key: "size".to_owned(),
                    },
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

fn tar_entry_end<R: io::Read>(
    entry: &tar::Entry<'_, R>,
    file_len: u64,
    compressed_digest: &Sha256Digest,
    index: usize,
) -> Result<u64, ResolveError> {
    let padded_size = entry
        .size()
        .checked_add(511)
        .map(|size| size / 512 * 512)
        .ok_or_else(|| {
            malformed_layer_archive(
                compressed_digest,
                format!("member {index} size overflows the tar block calculation"),
            )
        })?;
    let end = entry
        .raw_file_position()
        .checked_add(padded_size)
        .ok_or_else(|| {
            malformed_layer_archive(
                compressed_digest,
                format!("member {index} end offset overflows"),
            )
        })?;
    if end > file_len {
        return Err(malformed_layer_archive(
            compressed_digest,
            format!(
                "member {index} extends beyond the archive: padded end {end}, file length {file_len}"
            ),
        ));
    }
    Ok(end)
}

fn validate_tar_terminator(
    file: &mut std::fs::File,
    last_entry_end: u64,
    file_len: u64,
    compressed_digest: &Sha256Digest,
) -> Result<(), ResolveError> {
    let trailing = file_len.checked_sub(last_entry_end).ok_or_else(|| {
        malformed_layer_archive(compressed_digest, "last member ends beyond the archive")
    })?;
    if trailing < 1024 {
        return Err(malformed_layer_archive(
            compressed_digest,
            "archive is missing the two zero end-of-archive records",
        ));
    }
    file.seek(io::SeekFrom::Start(last_entry_end))
        .map_err(|error| malformed_layer_archive(compressed_digest, error))?;
    let mut remaining = trailing;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let chunk_len = usize::try_from(remaining.min(buffer.len() as u64))
            .expect("the read size is bounded by the fixed buffer");
        let read = file
            .read(&mut buffer[..chunk_len])
            .map_err(|error| malformed_layer_archive(compressed_digest, error))?;
        if read == 0 {
            return Err(malformed_layer_archive(
                compressed_digest,
                "archive ended while reading end-of-archive records",
            ));
        }
        if buffer[..read].iter().any(|byte| *byte != 0) {
            return Err(malformed_layer_archive(
                compressed_digest,
                "non-zero data follows the final tar member",
            ));
        }
        remaining -= read as u64;
    }
    Ok(())
}

fn is_safe_layer_member_bytes(path: &[u8]) -> bool {
    if path.is_empty() || path[0] == b'/' || path.contains(&0) {
        return false;
    }
    let mut has_name = false;
    for component in path.split(|byte| *byte == b'/') {
        match component {
            b"" | b"." => {}
            b".." => return false,
            _ => has_name = true,
        }
    }
    has_name
}

fn is_layer_root_bytes(path: &[u8]) -> bool {
    if path.is_empty() || path[0] == b'/' || path.contains(&0) {
        return false;
    }
    path.split(|byte| *byte == b'/')
        .all(|component| matches!(component, b"" | b"."))
}

fn is_safe_layer_member_path(path: &Path) -> bool {
    if path.as_os_str().is_empty()
        || path.as_os_str().as_encoded_bytes().contains(&0)
        || path.is_absolute()
    {
        return false;
    }
    let mut has_name = false;
    for component in path.components() {
        match component {
            std::path::Component::Normal(_) => has_name = true,
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => return false,
        }
    }
    has_name
}

fn is_layer_root_path(path: &Path) -> bool {
    if path.as_os_str().is_empty()
        || path.as_os_str().as_encoded_bytes().contains(&0)
        || path.is_absolute()
    {
        return false;
    }
    path.components()
        .all(|component| component == std::path::Component::CurDir)
}

fn is_safe_layer_entry_path(path: &Path, entry_type: tar::EntryType) -> bool {
    is_safe_layer_member_path(path) || (entry_type.is_dir() && is_layer_root_path(path))
}

/// Distro `/dev` nodes the guest does not need and the unprivileged host
/// must not create. Skip rather than fail-close so official images import.
fn is_skipped_special_layer_member(entry_type: tar::EntryType) -> bool {
    entry_type.is_character_special() || entry_type.is_block_special() || entry_type.is_fifo()
}

fn malformed_layer_archive(
    compressed_digest: &Sha256Digest,
    message: impl fmt::Display,
) -> ResolveError {
    ResolveError::MalformedLayerArchive {
        compressed_digest: compressed_digest.clone(),
        message: message.to_string(),
    }
}

fn unsafe_tar_member(
    compressed_digest: &Sha256Digest,
    path: PathBuf,
    reason: TarMemberViolation,
) -> ResolveError {
    ResolveError::UnsafeTarMember {
        compressed_digest: compressed_digest.clone(),
        path,
        reason,
    }
}

async fn read_config_diff_ids(image: &CachedImageBlobs) -> Result<Vec<Sha256Digest>, ResolveError> {
    if image.config.descriptor.size > CONFIG_MAX_BYTES {
        return Err(ResolveError::MalformedConfig(format!(
            "configuration {} exceeds the {CONFIG_MAX_BYTES}-byte parse limit",
            image.config.descriptor.digest
        )));
    }
    let file = tokio::fs::File::open(&image.config.path)
        .await
        .map_err(|error| cache_io("open config", image.config.path.clone(), error))?;
    let mut body = Vec::with_capacity(image.config.descriptor.size as usize);
    file.take(CONFIG_MAX_BYTES + 1)
        .read_to_end(&mut body)
        .await
        .map_err(|error| cache_io("read config", image.config.path.clone(), error))?;
    if body.len() as u64 > CONFIG_MAX_BYTES {
        return Err(ResolveError::MalformedConfig(format!(
            "configuration {} exceeds the {CONFIG_MAX_BYTES}-byte parse limit",
            image.config.descriptor.digest
        )));
    }
    if body.len() as u64 != image.config.descriptor.size {
        return Err(ResolveError::SizeMismatch {
            subject: image.config.descriptor.digest.to_string(),
            expected: image.config.descriptor.size,
            actual: body.len() as u64,
        });
    }
    let actual = Sha256Digest::of_bytes(&body);
    if actual != image.config.descriptor.digest {
        return Err(ResolveError::DigestMismatch {
            subject: format!("config blob {}", image.config.descriptor.digest),
            expected: image.config.descriptor.digest.clone(),
            actual,
        });
    }

    let config: RawImageConfiguration = serde_json::from_slice(&body)
        .map_err(|error| ResolveError::MalformedConfig(error.to_string()))?;
    if config.rootfs.kind != "layers" {
        return Err(ResolveError::UnsupportedRootfsType(config.rootfs.kind));
    }
    if config.rootfs.diff_ids.len() != image.layers.len() {
        return Err(ResolveError::DiffIdCountMismatch {
            expected: image.layers.len(),
            actual: config.rootfs.diff_ids.len(),
        });
    }
    Ok(config.rootfs.diff_ids)
}

/// Resolves an image and fills a verified content-addressed config/layer cache.
///
/// Work is bounded by the host's reported logical CPU count. Completion order
/// never affects the returned layer order, and a failed later layer leaves any
/// earlier verified blobs available for another image or retry.
pub async fn cache_image_blobs(
    reference: &ImageReference,
    architecture: Architecture,
    insecure: bool,
    cache: &BlobCache,
    credential: Option<RegistryCredential>,
) -> Result<CachedImageBlobs, ResolveError> {
    let parallelism = std::thread::available_parallelism()
        .map(std::num::NonZero::get)
        .unwrap_or(1);
    cache_image_blobs_with_parallelism(
        reference,
        architecture,
        insecure,
        cache,
        credential,
        parallelism,
    )
    .await
}

async fn cache_image_blobs_with_parallelism(
    reference: &ImageReference,
    architecture: Architecture,
    insecure: bool,
    cache: &BlobCache,
    credential: Option<RegistryCredential>,
    parallelism: usize,
) -> Result<CachedImageBlobs, ResolveError> {
    let session = RegistrySession::new(reference, insecure, credential)?;
    let resolved = resolve_manifest(&session, reference, architecture).await?;
    let mut seen = std::collections::HashSet::new();
    let work: Vec<Descriptor> = std::iter::once(&resolved.manifest.config)
        .chain(&resolved.manifest.layers)
        .filter(|descriptor| seen.insert(descriptor.digest.clone()))
        .cloned()
        .collect();
    let repository = reference.repository.clone();
    let completed: Vec<(Sha256Digest, PathBuf)> = stream::iter(work)
        .map(|descriptor| {
            let cache = cache.clone();
            let session = session.clone();
            let repository = repository.clone();
            async move {
                let path = cache
                    .cache_descriptor(&session, &repository, &descriptor)
                    .await?;
                Ok::<_, ResolveError>((descriptor.digest, path))
            }
        })
        .buffer_unordered(parallelism.max(1))
        .try_collect()
        .await?;
    let paths: HashMap<Sha256Digest, PathBuf> = completed.into_iter().collect();
    let cached = |descriptor: &Descriptor| CachedBlob {
        descriptor: descriptor.clone(),
        path: paths
            .get(&descriptor.digest)
            .expect("every unique descriptor produces exactly one cache path")
            .clone(),
    };
    let config = cached(&resolved.manifest.config);
    let layers = resolved.manifest.layers.iter().map(cached).collect();

    Ok(CachedImageBlobs {
        resolved: resolved.resolved,
        manifest: resolved.manifest,
        config,
        layers,
    })
}

/// A registry token endpoint's answer.
///
/// The distribution spec allows `token` and `access_token`. Docker Hub
/// sends both with the same value. A serde `alias` would treat that as a
/// duplicate field and reject the body.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
}

impl TokenResponse {
    /// The bearer secret to send back to the registry.
    fn issued(&self) -> Option<&str> {
        let token = self.token.as_deref().filter(|value| !value.is_empty());
        let access = self
            .access_token
            .as_deref()
            .filter(|value| !value.is_empty());
        token.or(access)
    }
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
    use core::assert_matches;

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
        assert_eq!(
            parsed.version,
            ImageVersion::Digest(Sha256Digest::parse(digest).unwrap())
        );
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
        let body = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": OCI_INDEX_MEDIA_TYPE,
            "manifests": manifests
        }))
        .expect("serialize index fixture");
        parse_index(&FetchedManifest {
            digest: Sha256Digest::of_bytes(&body),
            kind: DocumentKind::Index,
            media_type: OCI_INDEX_MEDIA_TYPE.to_owned(),
            body,
        })
        .expect("index fixture")
    }

    fn digest(character: char) -> Sha256Digest {
        Sha256Digest::parse(&format!("sha256:{}", character.to_string().repeat(64))).unwrap()
    }

    fn document(body: serde_json::Value, kind: DocumentKind, media_type: &str) -> FetchedManifest {
        let body = serde_json::to_vec(&body).unwrap();
        FetchedManifest {
            digest: Sha256Digest::of_bytes(&body),
            kind,
            media_type: media_type.to_owned(),
            body,
        }
    }

    #[test]
    fn descriptor_digest_errors_remain_structured() {
        let manifest = document(
            serde_json::json!({
                "schemaVersion": 2,
                "mediaType": OCI_MANIFEST_MEDIA_TYPE,
                "config": {
                    "mediaType": OCI_CONFIG_MEDIA_TYPE,
                    "digest": format!("sha512:{}", "a".repeat(128)),
                    "size": 1
                },
                "layers": []
            }),
            DocumentKind::Manifest,
            OCI_MANIFEST_MEDIA_TYPE,
        );

        assert_matches!(parse_image_manifest(&manifest),
            Err(ResolveError::Digest(DigestError::UnsupportedAlgorithm(algorithm)))
                if algorithm == "sha512");
    }

    #[test]
    fn a_manifest_cannot_assign_two_sizes_to_one_digest() {
        let shared = digest('a');
        let manifest = document(
            serde_json::json!({
                "schemaVersion": 2,
                "mediaType": OCI_MANIFEST_MEDIA_TYPE,
                "config": {
                    "mediaType": OCI_CONFIG_MEDIA_TYPE,
                    "digest": shared.as_str(),
                    "size": 3
                },
                "layers": [{
                    "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
                    "digest": shared.as_str(),
                    "size": 4
                }]
            }),
            DocumentKind::Manifest,
            OCI_MANIFEST_MEDIA_TYPE,
        );

        let error = parse_image_manifest(&manifest);
        assert_matches!(error, Err(ResolveError::ConflictingDescriptorSize { .. }));
    }

    #[test]
    fn response_and_body_media_types_cannot_disagree() {
        let body = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": OCI_MANIFEST_MEDIA_TYPE,
            "config": {},
            "layers": []
        }))
        .unwrap();

        let classified = classify_document(Some(DOCKER_MANIFEST_MEDIA_TYPE), &body);
        assert_matches!(classified, Err(ResolveError::Malformed(_)));
        assert_matches!(classify_document(Some("application/json"), &body),
            Err(ResolveError::UnsupportedMediaType(media_type))
                if media_type == "application/json");
    }

    #[test]
    fn token_response_accepts_the_distribution_access_token_alias() {
        let response: TokenResponse =
            serde_json::from_slice(br#"{"access_token":"issued-token"}"#).unwrap();

        assert_eq!(response.issued().unwrap(), "issued-token");
    }

    /// Docker Hub's auth.docker.io answers with both `token` and
    /// `access_token`. Treating the latter as a serde alias for the former
    /// rejects that body as a duplicate field.
    #[test]
    fn token_response_accepts_token_and_access_token_together() {
        let response: TokenResponse = serde_json::from_slice(
            br#"{"token":"issued-token","access_token":"issued-token","expires_in":300}"#,
        )
        .expect("Docker Hub sends both names");

        assert_eq!(response.issued().unwrap(), "issued-token");
    }

    #[test]
    fn format_error_chain_includes_source_causes() {
        #[derive(Debug)]
        struct Leaf;
        impl std::fmt::Display for Leaf {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("name or service not known")
            }
        }
        impl std::error::Error for Leaf {}

        #[derive(Debug)]
        struct Wrapper(Leaf);
        impl std::fmt::Display for Wrapper {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("error sending request")
            }
        }
        impl std::error::Error for Wrapper {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        assert_eq!(
            format_error_chain(&Wrapper(Leaf)),
            "error sending request: name or service not known"
        );
    }

    /// A wrapper whose whole purpose is to carry an `io::Error` the way
    /// reqwest carries hyper's connect failure.
    #[derive(Debug)]
    struct TransportChain(io::Error);

    impl fmt::Display for TransportChain {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("error sending request")
        }
    }

    impl std::error::Error for TransportChain {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    /// `EACCES` on connect is the host refusing the socket — SELinux, a unit
    /// sandbox, or a firewall rule on the service uid. The operator reads
    /// "error sending request" and goes hunting for DNS or a proxy, so the
    /// message has to name where the refusal actually came from.
    #[test]
    fn a_connect_refused_by_local_policy_says_so() {
        let denied = TransportChain(io::Error::from(io::ErrorKind::PermissionDenied));

        let message = format_error_chain(&denied);

        assert!(message.starts_with("error sending request"), "{message}");
        assert!(message.contains("this host"), "{message}");
        assert!(message.contains("SELinux"), "{message}");
        assert!(message.contains("firecrab doctor"), "{message}");
    }

    /// Every other transport failure keeps its old wording. A refused or
    /// unreachable registry is a network fact, and pointing at SELinux there
    /// would send the operator to the wrong host.
    #[test]
    fn other_transport_failures_keep_their_wording() {
        for kind in [
            io::ErrorKind::ConnectionRefused,
            io::ErrorKind::TimedOut,
            io::ErrorKind::NotFound,
        ] {
            let message = format_error_chain(&TransportChain(io::Error::from(kind)));
            assert!(!message.contains("SELinux"), "{kind:?}: {message}");
        }
    }

    #[tokio::test]
    async fn a_secure_session_refuses_plain_http_requests() {
        let reference = parse("registry.example.com/team/app:latest");
        let session = RegistrySession::new(&reference, false, None).unwrap();

        let error = session
            .send_once("http://127.0.0.1:1/v2/", None, None, None)
            .await
            .expect_err("a secure registry session must reject HTTP before connecting");

        assert_matches!(error, ResolveError::Transport(_));
    }

    #[test]
    fn token_request_accepts_https_delegation_and_rejects_http_downgrade() {
        let delegated = token_request(
            "Bearer realm=\"https://auth.docker.io/token\",service=\"registry.docker.io\",\
             scope=\"repository:library/nginx:pull\"",
            "https://registry-1.docker.io",
        )
        .expect("Docker Hub token challenge");
        let delegated = reqwest::Url::parse(&delegated).unwrap();
        assert_eq!(delegated.host_str(), Some("auth.docker.io"));
        assert_eq!(delegated.path(), "/token");
        assert!(
            delegated
                .query_pairs()
                .any(|(key, value)| key == "scope" && value == "repository:library/nginx:pull")
        );

        assert!(
            token_request(
                "Bearer realm=\"http://attacker.example/token\",service=\"registry\"",
                "https://registry.example"
            )
            .is_none()
        );
        assert!(
            token_request(
                "Bearer realm=\"http://127.0.0.1:5001/token\",service=\"registry\"",
                "http://127.0.0.1:5000"
            )
            .is_none()
        );
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
            entry(digest('a').as_str(), "amd64", "linux"),
            entry(digest('b').as_str(), "arm64", "linux"),
        ]));

        assert_eq!(
            index
                .select(Architecture::X86_64)
                .unwrap()
                .descriptor
                .digest
                .as_str(),
            digest('a').as_str()
        );
        assert_eq!(
            index
                .select(Architecture::Aarch64)
                .unwrap()
                .descriptor
                .digest
                .as_str(),
            digest('b').as_str()
        );
    }

    /// Buildx attaches SBOM and signature entries with a placeholder
    /// platform. Treating one as a real manifest pulls a blob that is not a
    /// root filesystem at all.
    #[test]
    fn select_skips_attestation_and_non_linux_entries() {
        let index = index(serde_json::json!([
            entry(digest('a').as_str(), "unknown", "unknown"),
            entry(digest('b').as_str(), "amd64", "windows"),
            serde_json::json!({
                "mediaType": OCI_MANIFEST_MEDIA_TYPE,
                "digest": digest('c').as_str(),
                "size": 1
            }),
            entry(digest('d').as_str(), "amd64", "linux"),
        ]));

        assert_eq!(
            index
                .select(Architecture::X86_64)
                .unwrap()
                .descriptor
                .digest
                .as_str(),
            digest('d').as_str()
        );
    }

    /// The operator needs to know what the image *does* offer, otherwise the
    /// only next step is guessing.
    #[test]
    fn select_reports_the_architectures_the_image_does_offer() {
        let index = index(serde_json::json!([
            entry(digest('a').as_str(), "arm64", "linux"),
            entry(digest('b').as_str(), "riscv64", "linux"),
            entry(digest('c').as_str(), "unknown", "unknown"),
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

        let error = empty.select(Architecture::HOST).unwrap_err();
        assert_matches!(error, IndexError::NoLinuxManifests { skipped: 0 });
    }

    /// A registry that answers `401` with a `Bearer` challenge, hands out a
    /// token, and only then serves the index — the anonymous pull flow every
    /// public registry uses.
    async fn token_guarded_registry(body: Vec<u8>, media_type: &'static str) -> String {
        token_guarded_registry_recording(body, media_type).await.0
    }

    /// The same registry, plus whatever the token endpoint was handed in
    /// `Authorization`. That header is the entire difference between an
    /// anonymous pull and an authenticated one.
    async fn token_guarded_registry_recording(
        body: Vec<u8>,
        media_type: &'static str,
    ) -> (String, Arc<StdMutex<Option<String>>>) {
        use axum::http::{HeaderMap, StatusCode, header::CONTENT_TYPE};
        use axum::response::IntoResponse;
        use axum::routing::get;

        let seen: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));
        let app = axum::Router::new()
            .route(
                "/token",
                get({
                    let seen = Arc::clone(&seen);
                    move |headers: HeaderMap| {
                        let seen = Arc::clone(&seen);
                        async move {
                            *seen.lock().unwrap() = headers
                                .get("authorization")
                                .and_then(|value| value.to_str().ok())
                                .map(str::to_owned);
                            axum::Json(serde_json::json!({ "token": "issued-token" }))
                        }
                    }
                }),
            )
            .route(
                "/v2/library/nginx/manifests/1.27",
                get(move |headers: HeaderMap| {
                    let body = body.clone();
                    async move {
                        if headers.get("authorization").map(|value| value.as_bytes())
                            != Some(b"Bearer issued-token".as_slice())
                        {
                            let host = headers
                                .get("host")
                                .and_then(|value| value.to_str().ok())
                                .expect("test request host");
                            let challenge = format!(
                                "Bearer realm=\"http://{host}/token\",service=\"registry\",\
                                 scope=\"repository:library/nginx:pull\""
                            );
                            return (StatusCode::UNAUTHORIZED, [("www-authenticate", challenge)])
                                .into_response();
                        }
                        ([(CONTENT_TYPE, media_type)], body).into_response()
                    }
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("127.0.0.1:{}", address.port()), seen)
    }

    fn single_platform_manifest_body() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": OCI_MANIFEST_MEDIA_TYPE,
            "config": {
                "mediaType": OCI_CONFIG_MEDIA_TYPE,
                "digest": digest('c').as_str(),
                "size": 7
            },
            "layers": [{
                "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
                "digest": digest('d').as_str(),
                "size": 99
            }]
        }))
        .unwrap()
    }

    /// The bearer token acquired for the first manifest request is accepted
    /// without weakening content verification.
    #[tokio::test]
    async fn resolve_authenticates_and_returns_the_single_manifest_digest() {
        let body = single_platform_manifest_body();
        let expected = Sha256Digest::of_bytes(&body).to_string();
        let registry = token_guarded_registry(body, OCI_MANIFEST_MEDIA_TYPE).await;
        let reference = ImageReference {
            registry,
            repository: "library/nginx".to_owned(),
            version: ImageVersion::Tag("1.27".to_owned()),
        };

        let resolved = resolve(&reference, Architecture::X86_64, true, None)
            .await
            .unwrap();

        assert_eq!(resolved.digest, expected);
        assert_eq!(resolved.architecture, Architecture::X86_64);
        assert!(resolved.single_platform);
    }

    /// The saved login only helps if the registry sees it: the token endpoint
    /// is where a pull stops being anonymous and starts counting against the
    /// operator's account instead of this host's address.
    #[tokio::test]
    async fn a_stored_login_authenticates_the_token_exchange() {
        let (registry, seen) = token_guarded_registry_recording(
            single_platform_manifest_body(),
            OCI_MANIFEST_MEDIA_TYPE,
        )
        .await;
        let reference = ImageReference {
            registry: registry.clone(),
            repository: "library/nginx".to_owned(),
            version: ImageVersion::Tag("1.27".to_owned()),
        };
        let credential = RegistryCredential {
            registry,
            username: "pista".to_owned(),
            secret: "dckr_pat_example".to_owned(),
        };

        resolve(&reference, Architecture::X86_64, true, Some(credential))
            .await
            .expect("an authenticated resolve must succeed");

        assert_eq!(
            seen.lock().unwrap().as_deref(),
            Some("Basic cGlzdGE6ZGNrcl9wYXRfZXhhbXBsZQ==")
        );
    }

    /// A Docker Hub token must not follow a reference to whatever host it
    /// names. A private mirror, a toolbox override, or a typo would otherwise
    /// collect the operator's secret, so the pull goes out anonymously.
    #[tokio::test]
    async fn a_stored_login_is_never_sent_to_another_registry() {
        let (registry, seen) = token_guarded_registry_recording(
            single_platform_manifest_body(),
            OCI_MANIFEST_MEDIA_TYPE,
        )
        .await;
        let reference = ImageReference {
            registry,
            repository: "library/nginx".to_owned(),
            version: ImageVersion::Tag("1.27".to_owned()),
        };

        resolve(
            &reference,
            Architecture::X86_64,
            true,
            Some(RegistryCredential::docker_hub("pista", "dckr_pat_example")),
        )
        .await
        .expect("an anonymous resolve must still succeed");

        assert_eq!(seen.lock().unwrap().as_deref(), None);
    }

    /// `docker.io` is what an operator types and `registry-1.docker.io` is
    /// what serves the API. A login saved for one is the same account.
    #[test]
    fn a_docker_hub_login_covers_both_spellings_of_the_host() {
        let credential = RegistryCredential::docker_hub("pista", "dckr_pat_example");

        assert!(credential.covers(DOCKER_HUB_REGISTRY));
        assert!(credential.covers(DOCKER_HUB_ALIAS));
        assert!(credential.covers("DOCKER.IO"));
        assert!(!credential.covers("ghcr.io"));
        assert!(!credential.covers("registry-1.docker.io.evil.example"));
        assert!(!credential.covers("127.0.0.1:5000"));
    }

    /// An explicitly empty index is still an index. Treating it as a manifest
    /// would turn an unusable image into a false-positive inspection result.
    #[tokio::test]
    async fn resolve_rejects_an_empty_index_as_an_index() {
        let body = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": OCI_INDEX_MEDIA_TYPE,
            "manifests": []
        }))
        .unwrap();
        let registry = token_guarded_registry(body, OCI_INDEX_MEDIA_TYPE).await;
        let reference = ImageReference {
            registry,
            repository: "library/nginx".to_owned(),
            version: ImageVersion::Tag("1.27".to_owned()),
        };

        let error = resolve(&reference, Architecture::HOST, true, None)
            .await
            .unwrap_err();

        assert_matches!(error, ResolveError::Index(_));
        assert!(error.to_string().contains("no Linux manifests"), "{error}");
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
