//! The shared builder-VM source for from-scratch distro bootstraps
//! (`handlers::bootstrap`). Boots off Alpine's own official minimal
//! kernel+initrd instead of any installed template, so a bootstrap can run
//! on a machine with zero templates installed. Registered into
//! `TemplateRegistry` under an alias no `/api/images` consumer ever
//! surfaces, purely so the existing `create_vm` disk-provisioning and
//! artifact-verification machinery works unchanged. See
//! `public-docs/images.md`.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::state::AppState;
use crate::templates::TemplateSpec;

/// Internal-only alias this registers under. `handlers::images::list_images`
/// keeps it out of `/api/images` by comparing against this exact constant
/// (not by any `__` prefix rule), and `handlers::vms::validate_create`
/// rejects it by the same comparison so it can't be driven as if it were a
/// user-facing template.
pub(crate) const MICROBOOT_ALIAS: &str = "__microboot";

/// Bumped whenever anything this module pins changes — the artifact URLs,
/// the placeholder's structure, or `boot_args`. `ensure_registered`'s fast
/// path requires an exact match, so an older persisted registration in
/// `images/.templates.json` is re-derived rather than silently reused: that
/// file is replayed at every startup, and without this check a host that
/// bootstrapped once would keep booting builders off the stale spec forever.
const MICROBOOT_VERSION: &str = "v4";

fn netboot_url(filename: &str) -> String {
    netboot_url_for_arch(crate::image_install::host_architecture(), filename)
}

fn netboot_url_for_arch(architecture: &str, filename: &str) -> String {
    format!(
        "https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/{architecture}/netboot/{filename}"
    )
}

fn microboot_boot_args() -> String {
    if crate::image_install::host_architecture() == "aarch64" {
        "keep_bootcon console=ttyS0 reboot=k".to_owned()
    } else {
        "console=ttyS0 reboot=k".to_owned()
    }
}

/// Kept as its own subdirectory of the image root (parallel to `kernel/`,
/// `rootfs/`, `.packages/`) so this cache is trivially distinguishable from
/// user-visible template artifacts on disk.
const CACHE_DIR: &str = ".microboot";

/// Relative (to the image root) paths `register()` pins as this alias's
/// `TemplateSpec`.
fn kernel_relative() -> PathBuf {
    kernel_relative_for_arch(crate::image_install::host_architecture())
}
fn kernel_relative_for_arch(architecture: &str) -> PathBuf {
    let filename = if architecture == "aarch64" {
        "Image-virt"
    } else {
        "vmlinux-virt"
    };
    Path::new(CACHE_DIR).join(filename)
}
fn initrd_relative() -> PathBuf {
    Path::new(CACHE_DIR).join("initramfs-virt")
}
fn rootfs_placeholder_relative() -> PathBuf {
    Path::new(CACHE_DIR).join("placeholder.ext4")
}

/// Ensures `MICROBOOT_ALIAS` is registered and ready to hand to `create_vm`,
/// downloading and registering it on first use and reusing the existing
/// registration (and its on-disk cache) on every call after that — the
/// registration is persisted by `register_spec` itself exactly like any
/// other runtime registration, so a restarted `firecrab-api` replays it
/// without re-downloading anything.
///
/// Returns the alias to pass as `CreateVmRequest.template` — always
/// `MICROBOOT_ALIAS` on success.
///
/// Called from `handlers::bootstrap::pick_builder_source`, which is now the
/// only thing that decides a bootstrap's builder-VM source.
pub(crate) async fn ensure_registered(state: &AppState) -> Result<String, String> {
    if is_current(state) {
        return Ok(MICROBOOT_ALIAS.to_owned());
    }

    // Only one caller may run the slow path at a time. The single-session
    // gate in `start_bootstrap` is explicitly TOCTOU-prone (its own comment
    // says a double-click clears it), and it sits *after* this call anyway,
    // so without this two requests would race on the very same files:
    // one's `mkfs.ext4` would wipe the `/etc` the other's `debugfs` just
    // created, and `extract_vmlinux`'s non-atomic `fs::write` could be
    // hashed mid-write by `verify_artifact`, pinning a corrupt length that
    // then fails `open_verified` forever.
    let _guard = SLOW_PATH.lock().await;
    // Re-check under the lock: whoever we queued behind has very likely
    // just finished the whole registration for us.
    if is_current(state) {
        return Ok(MICROBOOT_ALIAS.to_owned());
    }

    let image_root = state.templates.image_root_path().to_path_buf();
    let cache_dir = image_root.join(CACHE_DIR);
    tokio::fs::create_dir_all(&cache_dir)
        .await
        .map_err(|error| format!("mkdir {}: {error}", cache_dir.display()))?;

    let client = reqwest::Client::builder()
        .user_agent(concat!("firecrab-api/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| format!("http client: {error}"))?;

    let raw_kernel = cache_dir.join("vmlinuz-virt.raw");
    let initrd_dest = image_root.join(initrd_relative());
    // Fetched as a pair, never individually. Alpine republishes both files
    // at these same two URLs on every 3.24.x point release, so a cache left
    // half-populated by an earlier failure would otherwise pair a stale
    // kernel with a fresh initrd — whose `/lib/modules/$(uname -r)` then
    // doesn't match, so virtio never loads and the guest script fails at
    // `ip link set eth0 up` or on `/dev/vda` with a symptom that points
    // nowhere near the real cause.
    let kernel_cached = tokio::fs::try_exists(&raw_kernel).await.unwrap_or(false);
    let initrd_cached = tokio::fs::try_exists(&initrd_dest).await.unwrap_or(false);
    if !kernel_cached || !initrd_cached {
        crate::image_install::download_to(&client, &netboot_url("vmlinuz-virt"), &raw_kernel)
            .await?;
        crate::image_install::download_to(&client, &netboot_url("initramfs-virt"), &initrd_dest)
            .await?;
    }

    let templates = state.templates.clone();
    tokio::task::spawn_blocking(move || register_blocking(&templates, &image_root, &raw_kernel))
        .await
        .map_err(|error| format!("microboot registration task panicked: {error}"))??;

    Ok(MICROBOOT_ALIAS.to_owned())
}

/// Serializes the download-and-register path — see `ensure_registered`.
static SLOW_PATH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The version tag a test fixture must register under for
/// `ensure_registered`'s fast path to accept it. Exposed so those fixtures
/// track `MICROBOOT_VERSION` automatically instead of hardcoding a tag that
/// silently rots into a real network download.
#[cfg(test)]
pub(crate) fn microboot_version_for_test() -> &'static str {
    MICROBOOT_VERSION
}

/// Whether the alias is registered *at the version this build pins*. A
/// registration from an older build is deliberately not reused: see
/// `MICROBOOT_VERSION`.
fn is_current(state: &AppState) -> bool {
    state
        .templates
        .resolve_alias(MICROBOOT_ALIAS)
        .is_some_and(|template| template.version == MICROBOOT_VERSION)
}

/// Downloads and registers MicroBoot ahead of any request needing it.
///
/// `ensure_registered` moves ~22 MB over the network plus an
/// `extract-vmlinux` decompression and an `mkfs.ext4`, but its only caller
/// is inside an HTTP handler that `server::enforce_limits` caps at 10
/// seconds — on a slow link the very first bootstrap on a fresh machine
/// would 504 every time, and worse, dropping that handler future partway
/// can strand an already-started builder VM that nothing tracks. Doing it
/// once at startup means the request path only ever takes the fast path.
///
/// Best-effort by design: a failure here is logged and left for
/// `ensure_registered` to retry, since a machine that never bootstraps
/// shouldn't fail to boot over an unreachable mirror.
pub(crate) fn spawn_warmup(state: AppState) {
    tokio::spawn(async move {
        match ensure_registered(&state).await {
            Ok(_) => tracing::debug!("microboot builder source ready"),
            Err(error) => {
                tracing::warn!(%error, "could not prepare the microboot builder source at startup; the first bootstrap will retry")
            }
        }
    });
}

/// The blocking half: prepare the downloaded `vmlinuz-virt` as an x86_64
/// ELF vmlinux or ARM64 PE Image, create a small
/// placeholder rootfs artifact (its content is irrelevant — the guest
/// overwrites the real disk it grows into via `mkfs.ext4 -F`, see the
/// design doc's "스크래치 디스크" section), and register the spec.
fn register_blocking(
    templates: &crate::templates::TemplateRegistry,
    image_root: &Path,
    raw_kernel: &Path,
) -> Result<(), String> {
    let kernel_dest = image_root.join(kernel_relative());
    prepare_kernel_image(raw_kernel, &kernel_dest)?;

    let rootfs_dest = image_root.join(rootfs_placeholder_relative());
    create_placeholder_rootfs(&rootfs_dest)?;

    templates
        .register_spec(TemplateSpec {
            alias: MICROBOOT_ALIAS.to_owned(),
            version: MICROBOOT_VERSION.to_owned(),
            kernel: kernel_relative(),
            initrd: Some(initrd_relative()),
            rootfs: rootfs_placeholder_relative(),
            // No panic= — this is the whole mechanism: Alpine's own /init
            // (mkinitfs-generated) fails to find real boot media and falls
            // into its own recovery_shell() instead of a hard kernel panic
            // (verified live: /proc, /sys, /dev, PATH already set up there).
            boot_args: microboot_boot_args(),
        })
        .map_err(|error| format!("register microboot template: {error}"))?;
    Ok(())
}

/// Writes the stand-in rootfs artifact the MicroBoot builder VM is
/// provisioned from. Its *contents* are irrelevant — the guest's own
/// `mkfs.ext4 -F -d "$out" /dev/vda` overwrites the disk entirely — but its
/// structure is not, because every VM start puts this image through the
/// same host-side machinery as any real template. Both requirements below
/// were found by booting it for real, not by reading the code.
fn create_placeholder_rootfs(rootfs_dest: &Path) -> Result<(), String> {
    if let Some(parent) = rootfs_dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("mkdir {}: {error}", parent.display()))?;
    }
    // Must be a genuinely valid ext4 filesystem, not just arbitrary bytes:
    // `rootfs::prepare_rootfs`'s `grow()` step runs `e2fsck -f -y` on the
    // copied template before `resize2fs`, for every VM (including this
    // builder), before the guest ever gets a chance to run its own
    // `mkfs.ext4 -F` — a placeholder that isn't real ext4 fails e2fsck with
    // "Bad magic number in super-block" and the VM never boots (found live
    // while running Task 8's manual verification). Its *contents* still
    // don't matter — the guest's own `mkfs.ext4 -F -d "$out" /dev/vda`
    // overwrites this completely — only its structure has to be valid.
    std::process::Command::new("mkfs.ext4")
        .args(["-q", "-F"])
        .arg(&rootfs_dest)
        .arg("16M")
        .status()
        .map_err(|error| format!("run mkfs.ext4 for {}: {error}", rootfs_dest.display()))
        .and_then(|status| {
            if status.success() {
                Ok(())
            } else {
                Err(format!(
                    "mkfs.ext4 {} failed: {status}",
                    rootfs_dest.display()
                ))
            }
        })?;
    // Every VM start runs `rootfs::specialize_guest` against its own disk
    // copy, which writes `/etc/hostname` unconditionally (found live in the
    // same manual verification pass) — `debugfs`'s `write` doesn't create
    // parent directories, so a freshly `mkfs.ext4`'d image (no directories
    // at all beyond `/`) fails that write with "File not found". Same
    // one-directory fix this crate's own `real_rootfs_with_guest_dirs` test
    // helper (`rootfs.rs`) already applies for the identical reason.
    let mkdir_etc = std::process::Command::new("debugfs")
        .args(["-w", "-R", "mkdir /etc"])
        .arg(rootfs_dest)
        .output()
        .map_err(|error| {
            format!(
                "run debugfs mkdir /etc for {}: {error}",
                rootfs_dest.display()
            )
        })?;
    if String::from_utf8_lossy(&mkdir_etc.stderr).contains("File not found") {
        return Err(format!(
            "debugfs mkdir /etc failed for {}: {}",
            rootfs_dest.display(),
            String::from_utf8_lossy(&mkdir_etc.stderr)
        ));
    }
    Ok(())
}

/// Resolves the `extract-vmlinux` script path. Checks next to the running
/// binary first (installed layout), then falls back to the compile-time
/// repo-relative path (dev layout).
pub(crate) fn resolve_extract_vmlinux() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        let candidate = exe
            .parent()
            .unwrap_or(Path::new(""))
            .join("extract-vmlinux");
        if candidate.exists() {
            return candidate;
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/firecracker-menual/extract-vmlinux")
}

fn extract_vmlinux(raw_kernel: &Path, dest: &Path) -> Result<(), String> {
    let extract_vmlinux = resolve_extract_vmlinux();
    let output = std::process::Command::new(&extract_vmlinux)
        .arg(raw_kernel)
        .output()
        .map_err(|error| {
            format!(
                "run extract-vmlinux ({}): {error}",
                extract_vmlinux.display()
            )
        })?;
    // extract-vmlinux's own exit code doesn't reliably reflect success (same
    // caveat handlers::bootstrap's own copy of this check documents) — a
    // real extraction always produces non-empty stdout.
    if !output.status.success() || output.stdout.is_empty() {
        return Err(format!(
            "extract-vmlinux failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("mkdir {}: {error}", parent.display()))?;
    }
    std::fs::write(dest, &output.stdout)
        .map_err(|error| format!("write {}: {error}", dest.display()))
}

pub(crate) fn prepare_kernel_image(raw_kernel: &Path, dest: &Path) -> Result<(), String> {
    prepare_kernel_image_for_arch(raw_kernel, dest, crate::image_install::host_architecture())
}

fn prepare_kernel_image_for_arch(
    raw_kernel: &Path,
    dest: &Path,
    architecture: &str,
) -> Result<(), String> {
    if architecture != "aarch64" {
        return extract_vmlinux(raw_kernel, dest);
    }

    let mut file = std::fs::File::open(raw_kernel)
        .map_err(|error| format!("open {}: {error}", raw_kernel.display()))?;
    let mut magic = [0_u8; 2];
    file.read_exact(&mut magic)
        .map_err(|error| format!("read ARM64 kernel {}: {error}", raw_kernel.display()))?;
    if magic != *b"MZ" {
        return Err(format!(
            "ARM64 kernel is not a PE Image: {}",
            raw_kernel.display()
        ));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("mkdir {}: {error}", parent.display()))?;
    }
    std::fs::copy(raw_kernel, dest)
        .map_err(|error| {
            format!(
                "copy {} to {}: {error}",
                raw_kernel.display(),
                dest.display()
            )
        })?;
    restore_arm64_image_magic(dest)
}

/// Offset of the Linux/arm64 "Image" boot header's `magic` field, per
/// `Documentation/arch/arm64/booting.rst`.
const ARM64_IMAGE_MAGIC_OFFSET: u64 = 0x38;
/// `ARM64_IMAGE_MAGIC` (`0x644d5241`), little-endian.
const ARM64_IMAGE_MAGIC: [u8; 4] = *b"ARMd";

/// Alpine's official netboot `vmlinuz-virt` for aarch64 (what `ensure_registered`
/// downloads this builder kernel from) ships with this field zeroed out, even
/// though the PE/EFI wrapper checked above is intact and would pass a plain
/// `file`-style PE/ARM64 sniff. Firecracker's aarch64 kernel loader (rust-vmm
/// linux-loader) validates this field independently of the PE header and
/// refuses to boot with "invalid Image magic number" if it doesn't match —
/// confirmed live against a production build (see the M2Images magic-number
/// fix in `scripts/firecracker-menual/install-*-rootfs.sh`, same defect, same
/// upstream cause), so repair it here too.
fn restore_arm64_image_magic(dest: &Path) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(dest)
        .map_err(|error| format!("open {} for magic repair: {error}", dest.display()))?;
    file.seek(SeekFrom::Start(ARM64_IMAGE_MAGIC_OFFSET))
        .map_err(|error| format!("seek {} for magic repair: {error}", dest.display()))?;
    file.write_all(&ARM64_IMAGE_MAGIC)
        .map_err(|error| format!("write ARM64 Image magic into {}: {error}", dest.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::TemplateRegistry;

    fn temp_image_root() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn extract_vmlinux_rejects_a_file_it_cannot_recognize() {
        let dir = temp_image_root();
        let raw = dir.path().join("not-a-kernel");
        std::fs::write(&raw, b"plainly not an ELF or a compressed kernel").unwrap();
        let dest = dir.path().join("vmlinux-out");
        let result = extract_vmlinux(&raw, &dest);
        assert!(
            result.is_err(),
            "expected extract_vmlinux to reject garbage input"
        );
        assert!(!dest.exists());
    }

    #[test]
    fn arm64_kernel_keeps_the_pe_image_instead_of_extracting_elf() {
        let dir = temp_image_root();
        let raw = dir.path().join("Image.raw");
        let mut raw_bytes = vec![0_u8; 64];
        raw_bytes[0] = b'M';
        raw_bytes[1] = b'Z';
        raw_bytes.extend_from_slice(b"firecracker-arm64-image");
        std::fs::write(&raw, &raw_bytes).unwrap();
        let dest = dir.path().join("Image-virt");

        prepare_kernel_image_for_arch(&raw, &dest, "aarch64").unwrap();

        let mut expected = raw_bytes;
        expected[0x38..0x3c].copy_from_slice(&ARM64_IMAGE_MAGIC);
        assert_eq!(std::fs::read(dest).unwrap(), expected);
    }

    #[test]
    fn arm64_kernel_gets_its_boot_magic_repaired() {
        // Reproduces the exact production defect: Alpine's official
        // netboot vmlinuz-virt for aarch64 has this field zeroed, which
        // Firecracker's kernel loader rejects with "invalid Image magic
        // number" even though the PE/EFI wrapper around it is intact.
        let dir = temp_image_root();
        let raw = dir.path().join("Image.raw");
        let mut raw_bytes = vec![0_u8; 64];
        raw_bytes[0] = b'M';
        raw_bytes[1] = b'Z';
        raw_bytes.extend_from_slice(b"payload");
        std::fs::write(&raw, &raw_bytes).unwrap();
        let dest = dir.path().join("Image-virt");

        prepare_kernel_image_for_arch(&raw, &dest, "aarch64").unwrap();

        let dest_bytes = std::fs::read(dest).unwrap();
        assert_eq!(&dest_bytes[0x38..0x3c], &ARM64_IMAGE_MAGIC);
    }

    #[test]
    fn arm64_microboot_uses_architecture_specific_urls_and_kernel_name() {
        assert_eq!(
            netboot_url_for_arch("aarch64", "vmlinuz-virt"),
            "https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/aarch64/netboot/vmlinuz-virt"
        );
        assert_eq!(
            kernel_relative_for_arch("aarch64"),
            PathBuf::from(".microboot/Image-virt")
        );
    }

    #[test]
    fn register_blocking_builds_and_registers_a_complete_microboot_spec() {
        let dir = temp_image_root();
        let registry =
            TemplateRegistry::from_specs(dir.path(), std::iter::empty()).expect("empty registry");
        let raw_kernel = dir.path().join("vmlinuz-virt.raw");
        // `/usr/bin/true` is a small real ELF binary on the Linux hosts
        // Firecracker supports, so extract-vmlinux takes its pass-through
        // path without scanning the much larger test executable.
        if crate::image_install::host_architecture() == "aarch64" {
            // Magic already correct at 0x38 so the repair step is a no-op
            // and the byte-equality assertion below stays meaningful for
            // what this test actually covers (registration, not repair —
            // see `arm64_kernel_gets_its_boot_magic_repaired` for that).
            let mut fake = vec![0_u8; 64];
            fake[0] = b'M';
            fake[1] = b'Z';
            fake[0x38..0x3c].copy_from_slice(&ARM64_IMAGE_MAGIC);
            fake.extend_from_slice(b"fake ARM64 PE Image");
            std::fs::write(&raw_kernel, &fake).unwrap();
        } else {
            std::fs::copy("/usr/bin/true", &raw_kernel).unwrap();
        }

        // `register_spec` verifies every referenced artifact, including the
        // initrd that the asynchronous download path normally creates.
        let initrd = dir.path().join(initrd_relative());
        std::fs::create_dir_all(initrd.parent().unwrap()).unwrap();
        std::fs::write(&initrd, b"fake initrd").unwrap();

        register_blocking(&registry, dir.path(), &raw_kernel).expect("register microboot");

        let spec = registry
            .resolve_alias(MICROBOOT_ALIAS)
            .expect("registered MicroBoot spec");
        assert_eq!(spec.version, MICROBOOT_VERSION);
        assert_eq!(spec.kernel.relative_path(), kernel_relative());
        assert_eq!(
            spec.initrd
                .as_ref()
                .map(|artifact| artifact.relative_path()),
            Some(initrd_relative().as_path())
        );
        assert_eq!(spec.rootfs.relative_path(), rootfs_placeholder_relative());
        assert_eq!(
            std::fs::read(dir.path().join(kernel_relative())).unwrap(),
            std::fs::read(&raw_kernel).unwrap()
        );
        assert!(dir.path().join(rootfs_placeholder_relative()).is_file());
    }

    /// Both assertions here stand in for a live boot: the first is what
    /// `rootfs::prepare_rootfs`'s `grow()` does to every VM's disk copy
    /// before Firecracker ever starts, the second is what
    /// `rootfs::specialize_guest` needs in order to write `/etc/hostname`
    /// through `debugfs`. Reverting either half of
    /// `create_placeholder_rootfs` used to leave the whole suite green
    /// while making every bootstrap fail on a real machine.
    #[test]
    fn the_placeholder_rootfs_survives_the_host_side_machinery_every_vm_start_runs() {
        let dir = temp_image_root();
        let placeholder = dir.path().join(rootfs_placeholder_relative());
        create_placeholder_rootfs(&placeholder).expect("create placeholder");

        // e2fsck exit codes are a bitmask; 0 = clean, 1 = errors corrected.
        // Anything above that (notably 8, "operational error", which is what
        // a non-ext4 file produces alongside "Bad magic number in
        // super-block") is what grow() surfaces as a failed start.
        let fsck = std::process::Command::new("e2fsck")
            .args(["-f", "-n"])
            .arg(&placeholder)
            .output()
            .expect("run e2fsck");
        let code = fsck.status.code().unwrap_or(-1);
        assert!(
            code <= 1,
            "e2fsck rejected the placeholder (exit {code}): {}{}",
            String::from_utf8_lossy(&fsck.stdout),
            String::from_utf8_lossy(&fsck.stderr)
        );

        let stat = std::process::Command::new("debugfs")
            .args(["-R", "stat /etc"])
            .arg(&placeholder)
            .output()
            .expect("run debugfs stat /etc");
        let stderr = String::from_utf8_lossy(&stat.stderr);
        assert!(
            !stderr.contains("File not found"),
            "placeholder has no /etc directory, so specialize_guest's \
             hostname write would fail: {stderr}"
        );
    }

    #[tokio::test]
    async fn ensure_registered_is_a_no_op_once_already_registered() {
        let dir = temp_image_root();
        let registry =
            TemplateRegistry::from_specs(dir.path(), std::iter::empty()).expect("empty registry");
        // Fabricate a pre-registered microboot spec directly (bypassing the
        // real network download) to test the fast path in isolation.
        let kernel_path = dir.path().join(kernel_relative());
        std::fs::create_dir_all(kernel_path.parent().unwrap()).unwrap();
        std::fs::write(&kernel_path, b"fake-elf-content").unwrap();
        let initrd_path = dir.path().join(initrd_relative());
        std::fs::write(&initrd_path, b"fake-initrd").unwrap();
        let rootfs_path = dir.path().join(rootfs_placeholder_relative());
        std::fs::write(&rootfs_path, [0u8; 4096]).unwrap();
        registry
            .register_spec(TemplateSpec {
                alias: MICROBOOT_ALIAS.to_owned(),
                version: MICROBOOT_VERSION.to_owned(),
                kernel: kernel_relative(),
                initrd: Some(initrd_relative()),
                rootfs: rootfs_placeholder_relative(),
                boot_args: "console=ttyS0 reboot=k".to_owned(),
            })
            .expect("register fixture spec");

        let state = crate::state::AppState::with_db_file(registry, dir.path().join("state.db"))
            .await
            .expect("build minimal AppState");

        // The fast path returns before any network call, so this exercises
        // ensure_registered itself with zero real downloads.
        let result = ensure_registered(&state).await;

        assert_eq!(result, Ok(MICROBOOT_ALIAS.to_owned()));
    }
}
