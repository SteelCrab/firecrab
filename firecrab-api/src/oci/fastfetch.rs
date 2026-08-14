//! Supplies a working `fastfetch` for imported guests whose package
//! repositories do not ship it.
//!
//! Debian bookworm (`nginx:1.27` and friends) has `apt-get` but no `fastfetch`
//! package — that arrives in Debian 13. A silent `apt-get install` after DHCP
//! therefore leaves the console without a banner. The official polyfilled
//! glibc build needs only GLIBC_2.17, which bookworm, Ubuntu, and Rocky all
//! have, so the host downloads that binary once, caches it, and copies it
//! into the guest. Alpine and other musl trees keep the package-manager
//! fallback: a glibc binary would be `-x` and then fail at exec.

use super::*;

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek};
use std::os::unix::fs::OpenOptionsExt as _;

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Guest path the console wrapper and `/etc/profile.d` already invoke.
pub(crate) const GUEST_PATH: &str = "/usr/bin/fastfetch";
/// Dynamic loaders that mean a glibc guest can exec the polyfilled binary.
pub(crate) const GLIBC_LOADERS: &[&str] = &[
    "/lib64/ld-linux-x86-64.so.2",
    "/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
    "/lib/ld-linux-x86-64.so.2",
    "/lib/ld-linux-aarch64.so.1",
    "/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1",
    "/lib64/ld-linux-aarch64.so.1",
];

/// Pinned official release. Bump the hashes together when moving this.
const FASTFETCH_VERSION: &str = "2.67.1";
/// Operator override naming a program already on this host.
const FASTFETCH_PATH_ENV: &str = "FIRECRAB_OCI_FASTFETCH_PATH";
/// Ceiling on the downloaded archive and on the lifted program.
const FASTFETCH_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// ELF magic every candidate program must start with.
const ELF_MAGIC: &[u8; 4] = b"\x7fELF";
/// `EI_CLASS` value for a 64-bit image.
const ELFCLASS64: u8 = 2;
/// `EI_DATA` value for a little-endian image.
const ELFDATA2LSB: u8 = 1;
/// `e_type` for a fixed-position executable.
const ET_EXEC: u16 = 2;
/// `e_type` for a position-independent executable or shared object.
const ET_DYN: u16 = 3;
/// `e_machine` for 64-bit x86.
const EM_X86_64: u16 = 62;
/// `e_machine` for 64-bit ARM.
const EM_AARCH64: u16 = 183;
/// Size of the 64-bit ELF header.
const ELF64_HEADER_BYTES: usize = 64;

/// One architecture's digest-pinned GitHub release.
#[derive(Clone, Copy)]
struct ReleasePin {
    /// HTTPS URL of the official polyfilled tarball.
    url: &'static str,
    /// SHA-256 of the tarball bytes, hex only.
    archive_sha256: &'static str,
    /// Archive member that holds the program.
    member: &'static str,
    /// SHA-256 of the extracted program, hex only.
    binary_sha256: &'static str,
}

/// Why the host could not produce a guest fastfetch.
#[derive(Debug, Error)]
pub(super) enum FastfetchError {
    /// A filesystem operation around the cache or override failed.
    #[error("guest fastfetch {operation} failed at {path}: {source}")]
    Io {
        /// Operation being attempted.
        operation: &'static str,
        /// Path involved.
        path: PathBuf,
        /// Operating-system failure.
        #[source]
        source: io::Error,
    },
    /// The file is not a usable 64-bit executable for this guest.
    #[error("guest fastfetch at {path} is unusable: {reason}")]
    Unusable {
        /// Host path of the rejected program.
        path: PathBuf,
        /// Rule the program failed.
        reason: &'static str,
    },
    /// The downloaded archive did not match the pin.
    #[error("guest fastfetch archive digest mismatch: expected {expected}, got {actual}")]
    ArchiveDigest {
        /// Pinned hex digest.
        expected: String,
        /// Digest of the downloaded bytes.
        actual: String,
    },
    /// The extracted program did not match the pin.
    #[error("guest fastfetch binary digest mismatch: expected {expected}, got {actual}")]
    BinaryDigest {
        /// Pinned hex digest.
        expected: String,
        /// Digest of the extracted bytes.
        actual: String,
    },
    /// The tarball did not contain the expected member.
    #[error("guest fastfetch archive is missing {member}")]
    MissingMember {
        /// Archive path expected to hold the program.
        member: &'static str,
    },
    /// GitHub (or a mirror) rejected or stalled the download.
    #[error("guest fastfetch download of {url} failed: {message}")]
    Download {
        /// Release URL that was requested.
        url: &'static str,
        /// Transport or HTTP failure.
        message: String,
    },
    /// The program targets another machine.
    #[error("guest fastfetch at {path} targets ELF machine {actual}, expected {expected}")]
    ForeignArch {
        /// Host path of the rejected program.
        path: PathBuf,
        /// ELF machine this guest requires.
        expected: u16,
        /// ELF machine the program declares.
        actual: u16,
    },
    /// A mirrored override or a corrupt download exceeded the ceiling.
    #[error("guest fastfetch is {size} bytes, over the {limit}-byte limit")]
    TooLarge {
        /// Observed size.
        size: u64,
        /// Configured ceiling.
        limit: u64,
    },
}

/// Acquires the program, pulling it only when the cache cannot serve it.
///
/// Failures are logged and become `None` so an air-gapped host still imports.
pub(super) async fn ensure_fastfetch(
    image_root: &Path,
    architecture: Architecture,
) -> Option<FastfetchProgram> {
    match acquire(image_root, architecture).await {
        Ok(program) => Some(program),
        Err(error) => {
            tracing::warn!(error = %error, "guest fastfetch is unavailable; console will skip it");
            None
        }
    }
}

/// Reads the operator's program override, if one is set.
pub(super) fn configured_override() -> Option<PathBuf> {
    let value = std::env::var(FASTFETCH_PATH_ENV).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| PathBuf::from(value))
}

/// Verifies a cached or operator-supplied program.
pub(super) async fn inspect_fastfetch(
    path: &Path,
    architecture: Architecture,
    expected_sha256: Option<&str>,
) -> Result<FastfetchProgram, FastfetchError> {
    let path = path.to_owned();
    let expected = expected_sha256.map(str::to_owned);
    tokio::task::spawn_blocking(move || inspect_blocking(&path, architecture, expected.as_deref()))
        .await
        .map_err(|error| FastfetchError::Io {
            operation: "join fastfetch worker",
            path: PathBuf::from("fastfetch"),
            source: io::Error::other(error),
        })?
}

/// Pulls, extracts, and verifies, or returns a warm cache / override.
async fn acquire(
    image_root: &Path,
    architecture: Architecture,
) -> Result<FastfetchProgram, FastfetchError> {
    if let Some(path) = configured_override() {
        return inspect_fastfetch(&path, architecture, None).await;
    }

    let pin = release_pin(architecture);
    let cached = cache_path(image_root, architecture);
    if let Some(parent) = cached.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|source| io_err("create fastfetch cache", parent.to_owned(), source))?;
    }
    let lock = cache_path_lock(
        cached.parent().unwrap_or(image_root),
        Path::new("fastfetch"),
    )
    .await
    .map_err(|error| FastfetchError::Io {
        operation: "lock fastfetch cache",
        path: cached.clone(),
        source: io::Error::other(error.to_string()),
    })?;
    let _guard = lock.lock().await;

    if let Ok(program) = inspect_fastfetch(&cached, architecture, Some(pin.binary_sha256)).await {
        return Ok(program);
    }
    let _ = tokio::fs::remove_file(&cached).await;
    pull_fastfetch(pin, &cached).await?;
    inspect_fastfetch(&cached, architecture, Some(pin.binary_sha256)).await
}

/// Official polyfilled tarball for one architecture.
fn release_pin(architecture: Architecture) -> ReleasePin {
    match architecture {
        Architecture::X86_64 => ReleasePin {
            url: "https://github.com/fastfetch-cli/fastfetch/releases/download/2.67.1/fastfetch-linux-amd64-polyfilled.tar.gz",
            archive_sha256: "ccb0b144d845880692750831a53334029a54ea6ac66b2af40549c7bfad04e250",
            member: "fastfetch-linux-amd64-polyfilled/usr/bin/fastfetch",
            binary_sha256: "bc1c99972be290e929534224136a45f55d33d62d56574e1cab3f64355c9cecb7",
        },
        Architecture::Aarch64 => ReleasePin {
            url: "https://github.com/fastfetch-cli/fastfetch/releases/download/2.67.1/fastfetch-linux-aarch64-polyfilled.tar.gz",
            archive_sha256: "c9a112f10ffbea3a7dc664a9c88893cee8fbe85d9f0de16b5192bf68ea500c66",
            member: "fastfetch-linux-aarch64-polyfilled/usr/bin/fastfetch",
            binary_sha256: "94a2e5b92c9907ce1aa21a4c8a78aad4e154011e2d1417b8454cd9563b565266",
        },
    }
}

/// Cache path for one pinned version and architecture.
fn cache_path(image_root: &Path, architecture: Architecture) -> PathBuf {
    image_root
        .join(".oci/fastfetch")
        .join(FASTFETCH_VERSION)
        .join(oci_platform(architecture))
        .join("fastfetch")
}

/// Downloads the pinned archive and lifts its program into `destination`.
async fn pull_fastfetch(pin: ReleasePin, destination: &Path) -> Result<(), FastfetchError> {
    let parent = destination
        .parent()
        .expect("fastfetch cache paths are built with a parent");
    let archive = parent.join(format!("archive-{}.tgz", Uuid::new_v4()));
    let result = async {
        download_archive(pin.url, &archive, pin.archive_sha256).await?;
        extract_member(&archive, pin.member, destination).await
    }
    .await;
    let _ = tokio::fs::remove_file(&archive).await;
    result
}

/// Streams `url` to `destination` and checks the archive digest.
async fn download_archive(
    url: &'static str,
    destination: &Path,
    expected_sha256: &str,
) -> Result<(), FastfetchError> {
    let client = reqwest::Client::builder()
        .user_agent("firecrab-oci")
        .https_only(true)
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| FastfetchError::Download {
            url,
            message: error.to_string(),
        })?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| FastfetchError::Download {
            url,
            message: error.to_string(),
        })?;
    if !response.status().is_success() {
        return Err(FastfetchError::Download {
            url,
            message: format!("HTTP {}", response.status()),
        });
    }

    let partial = destination.with_extension(format!("{}.partial", Uuid::new_v4()));
    let mut file = tokio::fs::File::create(&partial)
        .await
        .map_err(|source| io_err("create fastfetch archive", partial.clone(), source))?;
    let mut stream = response.bytes_stream();
    let mut hasher = Sha256::new();
    let mut written = 0_u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| FastfetchError::Download {
            url,
            message: error.to_string(),
        })?;
        written = written.saturating_add(chunk.len() as u64);
        if written > FASTFETCH_MAX_BYTES {
            let _ = tokio::fs::remove_file(&partial).await;
            return Err(FastfetchError::TooLarge {
                size: written,
                limit: FASTFETCH_MAX_BYTES,
            });
        }
        hasher.update(&chunk);
        if let Err(source) = file.write_all(&chunk).await {
            let _ = tokio::fs::remove_file(&partial).await;
            return Err(io_err("write fastfetch archive", partial, source));
        }
    }
    if let Err(source) = file.sync_all().await {
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(io_err("sync fastfetch archive", partial, source));
    }
    drop(file);
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected_sha256 {
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(FastfetchError::ArchiveDigest {
            expected: expected_sha256.to_owned(),
            actual,
        });
    }
    tokio::fs::rename(&partial, destination)
        .await
        .map_err(|source| {
            let _ = std::fs::remove_file(&partial);
            io_err("publish fastfetch archive", destination.to_owned(), source)
        })
}

/// Decompresses the gzip tarball and copies the pinned member to `destination`.
pub(super) async fn extract_member(
    archive: &Path,
    member: &'static str,
    destination: &Path,
) -> Result<(), FastfetchError> {
    let file = tokio::fs::File::open(archive)
        .await
        .map_err(|source| io_err("open fastfetch archive", archive.to_owned(), source))?;
    let mut decoder =
        async_compression::tokio::bufread::GzipDecoder::new(tokio::io::BufReader::new(file));
    let mut decoded = Vec::new();
    decoder
        .read_to_end(&mut decoded)
        .await
        .map_err(|source| io_err("decompress fastfetch archive", archive.to_owned(), source))?;
    if decoded.len() as u64 > FASTFETCH_MAX_BYTES.saturating_mul(4) {
        return Err(FastfetchError::TooLarge {
            size: decoded.len() as u64,
            limit: FASTFETCH_MAX_BYTES.saturating_mul(4),
        });
    }
    let destination = destination.to_owned();
    let reported = destination.clone();
    tokio::task::spawn_blocking(move || extract_tar_member(&decoded, member, &destination))
        .await
        .map_err(|error| FastfetchError::Io {
            operation: "join extract worker",
            path: reported,
            source: io::Error::other(error),
        })?
}

/// Walks a decoded tar and publishes the pinned program member.
fn extract_tar_member(
    decoded: &[u8],
    member: &'static str,
    destination: &Path,
) -> Result<(), FastfetchError> {
    let mut tarball = tar::Archive::new(std::io::Cursor::new(decoded));
    let entries = tarball
        .entries()
        .map_err(|source| io_err("read fastfetch tar", destination.to_owned(), source))?;
    for entry in entries {
        let mut entry = entry.map_err(|source| {
            io_err("read fastfetch tar member", destination.to_owned(), source)
        })?;
        let name = entry
            .path()
            .map_err(|source| io_err("read fastfetch tar path", destination.to_owned(), source))?;
        let name = name.to_string_lossy().replace('\\', "/");
        let name = name.trim_start_matches("./");
        if !member_matches(name, member) {
            continue;
        }
        if name.split('/').any(|component| component == "..") {
            return Err(FastfetchError::Unusable {
                path: destination.to_owned(),
                reason: "archive member path escapes the extraction root",
            });
        }
        if !entry.header().entry_type().is_file() {
            return Err(FastfetchError::Unusable {
                path: destination.to_owned(),
                reason: "archive member is not a regular file",
            });
        }
        let size = entry.header().size().unwrap_or(0);
        if size > FASTFETCH_MAX_BYTES {
            return Err(FastfetchError::TooLarge {
                size,
                limit: FASTFETCH_MAX_BYTES,
            });
        }
        return publish_program(&mut entry, destination);
    }
    Err(FastfetchError::MissingMember { member })
}

/// Whether this tar member is the pinned program.
fn member_matches(name: &str, expected: &str) -> bool {
    name == expected || name.ends_with("/usr/bin/fastfetch")
}

/// Copies one tar member to its cache path without publishing a partial.
fn publish_program(
    source: &mut tar::Entry<'_, impl Read>,
    destination: &Path,
) -> Result<(), FastfetchError> {
    let partial = destination.with_extension(format!("{}.partial", Uuid::new_v4()));
    let mut target = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o755)
        .custom_flags(libc::O_CLOEXEC)
        .open(&partial)
        .map_err(|source| io_err("stage fastfetch program", partial.clone(), source))?;
    let copy = std::io::copy(source, &mut target)
        .and_then(|_| target.sync_all())
        .map_err(|source| io_err("write fastfetch program", partial.clone(), source));
    if let Err(error) = copy {
        let _ = std::fs::remove_file(&partial);
        return Err(error);
    }
    std::fs::rename(&partial, destination).map_err(|source| {
        let _ = std::fs::remove_file(&partial);
        io_err("publish fastfetch program", destination.to_owned(), source)
    })
}

/// Blocking half of [`inspect_fastfetch`].
fn inspect_blocking(
    path: &Path,
    architecture: Architecture,
    expected_sha256: Option<&str>,
) -> Result<FastfetchProgram, FastfetchError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| io_err("open fastfetch program", path.to_owned(), source))?;
    let metadata = file
        .metadata()
        .map_err(|source| io_err("inspect fastfetch program", path.to_owned(), source))?;
    if !metadata.file_type().is_file() {
        return Err(FastfetchError::Unusable {
            path: path.to_owned(),
            reason: "the member is not a regular file",
        });
    }
    let size = metadata.len();
    if size == 0 {
        return Err(FastfetchError::Unusable {
            path: path.to_owned(),
            reason: "the program is empty",
        });
    }
    if size > FASTFETCH_MAX_BYTES {
        return Err(FastfetchError::TooLarge {
            size,
            limit: FASTFETCH_MAX_BYTES,
        });
    }
    verify_guest_elf(&mut file, path, architecture)?;

    let mut bytes = Vec::with_capacity(size as usize);
    file.seek(io::SeekFrom::Start(0))
        .and_then(|_| file.read_to_end(&mut bytes))
        .map_err(|source| io_err("read fastfetch program", path.to_owned(), source))?;
    let digest = Sha256Digest::of_bytes(&bytes);
    if let Some(expected) = expected_sha256
        && digest.encoded() != expected
    {
        return Err(FastfetchError::BinaryDigest {
            expected: expected.to_owned(),
            actual: digest.encoded().to_owned(),
        });
    }
    Ok(FastfetchProgram {
        path: path.to_owned(),
        digest,
        size,
    })
}

/// Proves a program is a 64-bit executable for this architecture.
///
/// Dynamic linking is required: the official polyfilled build uses the
/// guest's glibc. A static check would reject the only binary that works
/// on Debian bookworm.
fn verify_guest_elf(
    file: &mut File,
    path: &Path,
    architecture: Architecture,
) -> Result<(), FastfetchError> {
    let mut header = [0_u8; ELF64_HEADER_BYTES];
    file.seek(io::SeekFrom::Start(0))
        .and_then(|_| file.read_exact(&mut header))
        .map_err(|_| FastfetchError::Unusable {
            path: path.to_owned(),
            reason: "the program is not a 64-bit little-endian ELF image",
        })?;
    if &header[..4] != ELF_MAGIC || header[4] != ELFCLASS64 || header[5] != ELFDATA2LSB {
        return Err(FastfetchError::Unusable {
            path: path.to_owned(),
            reason: "the program is not a 64-bit little-endian ELF image",
        });
    }
    let e_type = u16::from_le_bytes([header[16], header[17]]);
    if e_type != ET_EXEC && e_type != ET_DYN {
        return Err(FastfetchError::Unusable {
            path: path.to_owned(),
            reason: "the ELF image is not an executable",
        });
    }
    let e_machine = u16::from_le_bytes([header[18], header[19]]);
    let expected = match architecture {
        Architecture::X86_64 => EM_X86_64,
        Architecture::Aarch64 => EM_AARCH64,
    };
    if e_machine != expected {
        return Err(FastfetchError::ForeignArch {
            path: path.to_owned(),
            expected,
            actual: e_machine,
        });
    }
    Ok(())
}

/// Wraps a filesystem failure.
fn io_err(operation: &'static str, path: PathBuf, source: io::Error) -> FastfetchError {
    FastfetchError::Io {
        operation,
        path,
        source,
    }
}
