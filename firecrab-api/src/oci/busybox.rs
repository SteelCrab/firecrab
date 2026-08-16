//! Supplies the static program an imported container tree boots as PID 1.
//!
//! A container image carries an application, not an operating system: no init,
//! no shell in a distroless tree, and never a DHCP client. The missing pieces
//! come from a busybox image pulled through the very pipeline that imports the
//! user's image, so no second downloader, digest verifier, or cache exists to
//! keep correct. Only the first import on a host reaches the registry; the
//! lifted program is cached under the image root afterwards.

use super::*;

use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt as _;

/// Repository the guest init, shell, and DHCP client come from.
///
/// Pinned to an image *index* digest rather than a per-architecture manifest:
/// the index digest covers both children, so one immutable constant pins amd64
/// and arm64 at once and [`ImageIndex::select`] still picks the right one. A
/// tag here would let a registry repoint PID 1 under a running deployment.
///
/// `uclibc` is the statically linked variant. A merged container tree has no
/// dynamic loader, so a glibc build would exec straight into a panic.
const TOOLBOX_IMAGE: &str =
    "busybox@sha256:8d7b1636e974e0adfd8d945955fca609304f0a56c18799dfd032d6e661382d84";
/// Operator override for air-gapped hosts, private mirrors, and rate limits.
const TOOLBOX_IMAGE_ENV: &str = "FIRECRAB_OCI_TOOLBOX_IMAGE";
/// Operator override naming a static program already on this host.
const TOOLBOX_PATH_ENV: &str = "FIRECRAB_OCI_TOOLBOX_PATH";
/// Archive path the static program is lifted from.
const TOOLBOX_MEMBER: &str = "bin/busybox";
/// Ceiling on the lifted program, so a mirrored override cannot copy an
/// unbounded file into every imported rootfs.
const TOOLBOX_MAX_BYTES: u64 = 32 * 1024 * 1024;
/// Applet names the injected guest scripts invoke.
///
/// busybox stores its applet table as NUL-separated names in the binary, so
/// their absence is detectable without running anything. A trimmed build is
/// reported and still used: refusing on a false positive would break every
/// boot, while a genuinely missing applet surfaces as a readiness failure.
const REQUIRED_APPLETS: &[&str] = &["sh", "ip", "udhcpc", "awk", "mount", "sleep", "cut"];

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
/// Program header type naming a dynamic loader.
const PT_INTERP: u32 = 3;
/// Size of the 64-bit ELF header.
const ELF64_HEADER_BYTES: usize = 64;
/// Size of one 64-bit program header entry.
const ELF64_PROGRAM_HEADER_BYTES: u16 = 56;
/// Refuse absurd program header counts before allocating for them.
const ELF64_MAX_PROGRAM_HEADERS: u16 = 128;
/// Longest interpreter path reported back in a rejection.
const ELF64_MAX_INTERPRETER_BYTES: u64 = 4096;

/// Acquires the toolbox program, pulling it only when the cache cannot serve it.
pub(super) async fn ensure_toolbox(
    options: &GuestRuntimeOptions<'_>,
) -> Result<ToolboxProgram, ResolveError> {
    if let Some(path) = configured_toolbox_override() {
        return inspect_toolbox(&path, options.architecture).await;
    }

    let reference = ImageReference::parse(configured_toolbox_image().as_str())
        .map_err(|error| ResolveError::Malformed(error.to_string()))?;
    ensure_toolbox_from(options, &reference).await
}

/// Acquires the toolbox from one explicit reference.
///
/// Split out so tests can point at a local registry without mutating the
/// process environment, which `ensure_toolbox` reads and parallel tests share.
pub(super) async fn ensure_toolbox_from(
    options: &GuestRuntimeOptions<'_>,
    reference: &ImageReference,
) -> Result<ToolboxProgram, ResolveError> {
    let cached = toolbox_cache_path(options, reference);
    if let Some(parent) = cached.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|source| cache_io("create toolbox cache", parent.to_owned(), source))?;
    }
    let lock = cache_path_lock(
        cached.parent().unwrap_or(options.image_root),
        Path::new(TOOLBOX_MEMBER),
    )
    .await?;
    let _guard = lock.lock().await;

    if let Ok(program) = inspect_toolbox(&cached, options.architecture).await {
        return Ok(program);
    }
    // A cache entry that fails verification is stale or corrupt, never trusted.
    let _ = tokio::fs::remove_file(&cached).await;
    pull_toolbox(options, reference, &cached).await?;
    inspect_toolbox(&cached, options.architecture).await
}

/// Reads the operator's program override, if one is set.
pub(super) fn configured_toolbox_override() -> Option<PathBuf> {
    let value = std::env::var(TOOLBOX_PATH_ENV).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| PathBuf::from(value))
}

/// Reads the toolbox reference, falling back to the pinned digest.
///
/// An override may carry a tag: a private mirror is trusted operator input.
/// The built-in default never does.
pub(super) fn configured_toolbox_image() -> String {
    match std::env::var(TOOLBOX_IMAGE_ENV) {
        Ok(value) if !value.trim().is_empty() => value.trim().to_owned(),
        Ok(_) | Err(std::env::VarError::NotPresent) => TOOLBOX_IMAGE.to_owned(),
        Err(std::env::VarError::NotUnicode(_)) => {
            tracing::warn!(
                variable = TOOLBOX_IMAGE_ENV,
                "non-Unicode OCI toolbox image; using the pinned default"
            );
            TOOLBOX_IMAGE.to_owned()
        }
    }
}

/// Cache path for one reference and architecture.
///
/// The name is derived only from constants and the override, so a warm host
/// finds its program without resolving anything over the network.
fn toolbox_cache_path(options: &GuestRuntimeOptions<'_>, reference: &ImageReference) -> PathBuf {
    let canonical = format!(
        "{}/{}@{}",
        reference.registry,
        reference.repository,
        reference.version.as_str()
    );
    let key = Sha256Digest::of_bytes(canonical.as_bytes());
    options
        .image_root
        .join(".oci/toolbox")
        .join(key.encoded())
        .join(oci_platform(options.architecture))
        .join("busybox")
}

/// Pulls the pinned image and lifts its program into the cache.
async fn pull_toolbox(
    options: &GuestRuntimeOptions<'_>,
    reference: &ImageReference,
    destination: &Path,
) -> Result<(), ResolveError> {
    let insecure = is_loopback_registry(&reference.registry);
    let blobs = cache_image_blobs(
        reference,
        options.architecture,
        insecure,
        options.blobs,
        options.credential.cloned(),
    )
    .await?;
    let layers = decompress_cached_layers(&blobs, options.layers).await?;
    let layers = validate_decompressed_layers(layers).await?;

    let parent = destination
        .parent()
        .expect("toolbox cache paths are built with a parent");
    let scratch = parent.join(format!(".merge-{}", Uuid::new_v4()));
    let merged = merge_validated_layers(&layers, &scratch).await?;
    let mut cleanup = ScratchTree::new(scratch);
    let member = merged.path().join(TOOLBOX_MEMBER);
    let result = publish_toolbox(&member, destination, reference);
    cleanup.remove();
    result
}

/// Copies a lifted program to its cache path without ever publishing a partial.
fn publish_toolbox(
    member: &Path,
    destination: &Path,
    reference: &ImageReference,
) -> Result<(), ResolveError> {
    match std::fs::symlink_metadata(member) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(ResolveError::ToolboxUnusable {
                path: member.to_owned(),
                reason: ToolboxViolation::NotRegularFile,
            });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ResolveError::ToolboxMissing {
                reference: format!(
                    "{}/{}@{}",
                    reference.registry,
                    reference.repository,
                    reference.version.as_str()
                ),
                member: TOOLBOX_MEMBER,
            });
        }
        Err(source) => {
            return Err(cache_io(
                "inspect toolbox member",
                member.to_owned(),
                source,
            ));
        }
    }

    let partial = destination.with_extension(format!("{}.partial", Uuid::new_v4()));
    let mut source = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(member)
        .map_err(|source| cache_io("open toolbox member", member.to_owned(), source))?;
    let mut target = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o755)
        .custom_flags(libc::O_CLOEXEC)
        .open(&partial)
        .map_err(|source| cache_io("stage toolbox program", partial.clone(), source))?;
    let copy = io::copy(&mut source, &mut target)
        .and_then(|_| target.sync_all())
        .map_err(|source| cache_io("write toolbox program", partial.clone(), source));
    if let Err(error) = copy {
        let _ = std::fs::remove_file(&partial);
        return Err(error);
    }
    std::fs::rename(&partial, destination).map_err(|source| {
        let _ = std::fs::remove_file(&partial);
        cache_io("publish toolbox program", destination.to_owned(), source)
    })
}

/// Verifies a cached or operator-supplied program and describes it.
pub(super) async fn inspect_toolbox(
    path: &Path,
    architecture: Architecture,
) -> Result<ToolboxProgram, ResolveError> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || inspect_toolbox_blocking(&path, architecture))
        .await
        .map_err(|error| {
            cache_io(
                "join toolbox worker",
                PathBuf::from("toolbox"),
                io::Error::other(error),
            )
        })?
}

/// Blocking half of [`inspect_toolbox`].
fn inspect_toolbox_blocking(
    path: &Path,
    architecture: Architecture,
) -> Result<ToolboxProgram, ResolveError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| cache_io("open toolbox program", path.to_owned(), source))?;
    let metadata = file
        .metadata()
        .map_err(|source| cache_io("inspect toolbox program", path.to_owned(), source))?;
    if !metadata.file_type().is_file() {
        return Err(unusable(path, ToolboxViolation::NotRegularFile));
    }
    let size = metadata.len();
    if size == 0 {
        return Err(unusable(path, ToolboxViolation::Empty));
    }
    if size > TOOLBOX_MAX_BYTES {
        return Err(unusable(
            path,
            ToolboxViolation::TooLarge {
                size,
                limit: TOOLBOX_MAX_BYTES,
            },
        ));
    }
    verify_static_elf(&mut file, path, architecture)?;

    let mut bytes = Vec::with_capacity(size as usize);
    file.seek(io::SeekFrom::Start(0))
        .and_then(|_| file.read_to_end(&mut bytes))
        .map_err(|source| cache_io("read toolbox program", path.to_owned(), source))?;
    report_missing_applets(&bytes, path);

    Ok(ToolboxProgram {
        path: path.to_owned(),
        digest: Sha256Digest::of_bytes(&bytes),
        size,
    })
}

/// Wraps a rejection reason with the program it applies to.
fn unusable(path: &Path, reason: ToolboxViolation) -> ResolveError {
    ResolveError::ToolboxUnusable {
        path: path.to_owned(),
        reason,
    }
}

/// Proves a program is a static 64-bit executable for this architecture.
///
/// The host never executes the program to find out. `PT_INTERP` is the only
/// on-disk marker separating a static build from a dynamic one, and a dynamic
/// build would exec into a panic inside a tree that has no loader.
fn verify_static_elf(
    file: &mut File,
    path: &Path,
    architecture: Architecture,
) -> Result<(), ResolveError> {
    let mut header = [0_u8; ELF64_HEADER_BYTES];
    file.seek(io::SeekFrom::Start(0))
        .and_then(|_| file.read_exact(&mut header))
        .map_err(|_| unusable(path, ToolboxViolation::NotElf))?;
    if &header[..4] != ELF_MAGIC || header[4] != ELFCLASS64 || header[5] != ELFDATA2LSB {
        return Err(unusable(path, ToolboxViolation::NotElf));
    }
    let e_type = u16::from_le_bytes([header[16], header[17]]);
    if e_type != ET_EXEC && e_type != ET_DYN {
        return Err(unusable(path, ToolboxViolation::NotExecutable));
    }
    let e_machine = u16::from_le_bytes([header[18], header[19]]);
    let expected = match architecture {
        Architecture::X86_64 => EM_X86_64,
        Architecture::Aarch64 => EM_AARCH64,
    };
    if e_machine != expected {
        return Err(unusable(
            path,
            ToolboxViolation::ForeignArchitecture {
                expected,
                actual: e_machine,
            },
        ));
    }

    let e_phoff = u64::from_le_bytes(header[32..40].try_into().expect("8 bytes"));
    let e_phentsize = u16::from_le_bytes([header[54], header[55]]);
    let e_phnum = u16::from_le_bytes([header[56], header[57]]);
    if e_phnum == 0
        || e_phnum > ELF64_MAX_PROGRAM_HEADERS
        || e_phentsize != ELF64_PROGRAM_HEADER_BYTES
    {
        return Err(unusable(path, ToolboxViolation::MalformedProgramHeaders));
    }
    let mut table = vec![0_u8; usize::from(e_phnum) * usize::from(e_phentsize)];
    file.seek(io::SeekFrom::Start(e_phoff))
        .and_then(|_| file.read_exact(&mut table))
        .map_err(|_| unusable(path, ToolboxViolation::MalformedProgramHeaders))?;

    for entry in table.chunks_exact(usize::from(e_phentsize)) {
        if u32::from_le_bytes(entry[..4].try_into().expect("4 bytes")) != PT_INTERP {
            continue;
        }
        let p_offset = u64::from_le_bytes(entry[8..16].try_into().expect("8 bytes"));
        let p_filesz = u64::from_le_bytes(entry[32..40].try_into().expect("8 bytes"));
        let interpreter = read_interpreter(file, p_offset, p_filesz).unwrap_or_default();
        return Err(unusable(
            path,
            ToolboxViolation::DynamicallyLinked { interpreter },
        ));
    }
    Ok(())
}

/// Reads the interpreter path so a rejection can name it.
fn read_interpreter(file: &mut File, offset: u64, length: u64) -> Option<String> {
    if length == 0 || length > ELF64_MAX_INTERPRETER_BYTES {
        return None;
    }
    let mut bytes = vec![0_u8; length as usize];
    file.seek(io::SeekFrom::Start(offset)).ok()?;
    file.read_exact(&mut bytes).ok()?;
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
}

/// Warns when the applet table does not mention an applet the guest needs.
///
/// A trimmed build is used anyway: refusing on a false positive would break
/// every boot, while a genuinely absent applet fails the readiness sentinel
/// with a message an operator can act on.
fn report_missing_applets(bytes: &[u8], path: &Path) {
    let missing: Vec<&str> = REQUIRED_APPLETS
        .iter()
        .copied()
        .filter(|applet| !contains_applet(bytes, applet))
        .collect();
    if !missing.is_empty() {
        tracing::warn!(
            path = %path.display(),
            missing = ?missing,
            "guest toolbox may be missing applets the injected runtime calls"
        );
    }
}

/// Whether the program mentions an applet as a NUL-delimited name.
fn contains_applet(bytes: &[u8], applet: &str) -> bool {
    let needle: Vec<u8> = std::iter::once(0)
        .chain(applet.bytes())
        .chain(std::iter::once(0))
        .collect();
    bytes.windows(needle.len()).any(|window| window == needle)
}

/// Removes the scratch merge tree a toolbox pull built.
struct ScratchTree {
    /// Tree to remove.
    path: PathBuf,
    /// Cleared once the tree has been removed.
    armed: bool,
}

impl ScratchTree {
    /// Arms cleanup for a freshly merged scratch tree.
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    /// Removes the tree now, reporting nothing: the caller's error wins.
    fn remove(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        if let Err(error) = std::fs::remove_dir_all(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(
                error = %error,
                path = %self.path.display(),
                "failed to remove the OCI toolbox scratch tree"
            );
        }
    }
}

impl Drop for ScratchTree {
    fn drop(&mut self) {
        self.remove();
    }
}
