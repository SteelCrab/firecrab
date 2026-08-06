//! The shared builder-VM source for from-scratch distro bootstraps
//! (`handlers::bootstrap`). Boots off Alpine's own official minimal
//! kernel+initrd instead of any installed template, so a bootstrap can run
//! on a machine with zero templates installed. Registered into
//! `TemplateRegistry` under an alias no `/api/images` consumer ever
//! surfaces, purely so the existing `create_vm` disk-provisioning and
//! artifact-verification machinery works unchanged. See
//! `docs/superpowers/specs/2026-08-05-m2image-microboot-design.md`.

use std::path::{Path, PathBuf};

use crate::state::AppState;
use crate::templates::TemplateSpec;

/// Internal-only alias `__microboot` registers under. The leading `__` is
/// also what `handlers::images::list_images` filters on to keep this out of
/// `/api/images` — see that module's own doc comment.
pub(crate) const MICROBOOT_ALIAS: &str = "__microboot";
const MICROBOOT_VERSION: &str = "v1";

const KERNEL_URL: &str =
    "https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/netboot/vmlinuz-virt";
const INITRD_URL: &str =
    "https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/netboot/initramfs-virt";

/// Kept as its own subdirectory of the image root (parallel to `kernel/`,
/// `rootfs/`, `.packages/`) so this cache is trivially distinguishable from
/// user-visible template artifacts on disk.
const CACHE_DIR: &str = ".microboot";

/// Relative (to the image root) paths `register()` pins as this alias's
/// `TemplateSpec`.
fn kernel_relative() -> PathBuf {
    Path::new(CACHE_DIR).join("vmlinux-virt")
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
    if state.templates.resolve_alias(MICROBOOT_ALIAS).is_some() {
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
    if !tokio::fs::try_exists(&raw_kernel).await.unwrap_or(false) {
        crate::image_install::download_to(&client, KERNEL_URL, &raw_kernel).await?;
    }
    let initrd_dest = image_root.join(initrd_relative());
    if !tokio::fs::try_exists(&initrd_dest).await.unwrap_or(false) {
        crate::image_install::download_to(&client, INITRD_URL, &initrd_dest).await?;
    }

    let templates = state.templates.clone();
    tokio::task::spawn_blocking(move || register_blocking(&templates, &image_root, &raw_kernel))
        .await
        .map_err(|error| format!("microboot registration task panicked: {error}"))??;

    Ok(MICROBOOT_ALIAS.to_owned())
}

/// The blocking half: convert the downloaded `vmlinuz-virt` (a compressed
/// bzImage) to the ELF `vmlinux` Firecracker needs, create a small
/// placeholder rootfs artifact (its content is irrelevant — the guest
/// overwrites the real disk it grows into via `mkfs.ext4 -F`, see the
/// design doc's "스크래치 디스크" section), and register the spec.
fn register_blocking(
    templates: &crate::templates::TemplateRegistry,
    image_root: &Path,
    raw_kernel: &Path,
) -> Result<(), String> {
    let kernel_dest = image_root.join(kernel_relative());
    extract_vmlinux(raw_kernel, &kernel_dest)?;

    let rootfs_dest = image_root.join(rootfs_placeholder_relative());
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
        .arg(&rootfs_dest)
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
            boot_args: "console=ttyS0 reboot=k".to_owned(),
        })
        .map_err(|error| format!("register microboot template: {error}"))?;
    Ok(())
}

/// Same `extract-vmlinux` invocation `handlers::bootstrap`'s packaging step
/// already uses (compile-time repo-relative path — see that call site's own
/// doc comment for why `env!("CARGO_MANIFEST_DIR")` and not `current_dir()`).
fn extract_vmlinux(raw_kernel: &Path, dest: &Path) -> Result<(), String> {
    let extract_vmlinux =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/firecracker-menual/extract-vmlinux");
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
