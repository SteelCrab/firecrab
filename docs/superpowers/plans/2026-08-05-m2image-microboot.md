# M2Image MicroBoot Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the bootstrap feature's last remaining precondition — "at least one template must already be installed" — by booting the builder VM off **MicroBoot** (Alpine's own official minimal kernel+initrd) instead of an installed template, so a completely fresh machine with zero installed templates can still bootstrap alpine/ubuntu/rocky from the web.

**Architecture:** A new `microboot` module downloads and caches Alpine's official `netboot/vmlinuz-virt` + `netboot/initramfs-virt` once, extracts the kernel to ELF the same way the existing packaging step already does, and registers it into `TemplateRegistry` under an internal-only alias (`__microboot`, never exposed via `/api/images`) so the *existing* `create_vm` machinery (disk provisioning, artifact verification) needs no changes at all. `pick_builder_source` is replaced to always return this alias. The builder VM boots with `panic=` dropped from its `boot_args`, which makes Alpine's own `/init` fall into its legitimate `recovery_shell()` (verified live in Firecracker) instead of panicking when it can't find real boot media — that shell already has `/proc`, `/sys`, `/dev` and `PATH` set up, `apk`, `busybox`, `wget`, `chroot`, `tar` all working; only `e2fsprogs` (`mkfs.ext4`) and, for Rocky, a `dnf`-capable environment are missing, and both are added inside the guest scripts. Because the guest never gets past a bare recovery shell, nothing there can print `firecrab-net-helper`'s `FIRECRAB_NETWORK_READY` sentinel automatically, so `create_vm`'s existing network-readiness gate is skipped specifically for `__microboot`-templated VMs (the guest scripts bring their own interface up manually instead, and packaging still requires a real completed bootstrap script run before anything is published — no safety property is actually lost, just moved).

**Tech Stack:** Rust (axum, tokio, reqwest — already a dependency), POSIX `sh` (guest scripts), existing `debugfs`-based `rootfs.rs` helpers.

## Global Constraints

- No new privileged host component. Everything continues to run either inside the disposable builder VM or through code paths `firecrab-api` already runs unprivileged (`docs/superpowers/specs/2026-08-05-m2image-microboot-design.md`).
- MicroBoot is never exposed through `/api/images`, `list_aliases()` consumers outside this feature, or any UI — internal implementation detail only.
- The 3 bootstrappable aliases stay `alpine-3.24`, `ubuntu-26.04`, `rocky-9` — no scope expansion.
- Every guest-script change must preserve the existing package list, hostname/network/getty setup, and `$out/rootfs.ext4` + `$out/vmlinuz-*-raw` (+ `$out/initramfs` where present) production exactly as today — MicroBoot only changes how the *outer* environment boots and how the finished `$out` directory reaches the host's disk, not what ends up inside the target rootfs.
- `cargo fmt --all -- --check`, `cargo clippy -p firecrab-api -p firecrab-api-types --all-targets -- -D warnings`, and `cargo test -p firecrab-api -p firecrab-api-types` must all stay clean after every task.
- Commit after every task (propose-only vs. auto-commit follows whatever the controller's session-scoped policy is at execution time — this plan does not itself authorize auto-commit).

---

### Task 1: `microboot` module — download, cache, and register the shared builder source

**Files:**
- Create: `firecrab-api/src/microboot.rs`
- Modify: `firecrab-api/src/main.rs:9` (add `mod microboot;`)
- Modify: `firecrab-api/src/image_install.rs:626` (widen `download_to` to `pub(crate)`)
- Test: inline `#[cfg(test)] mod tests` in `firecrab-api/src/microboot.rs`

**Interfaces:**
- Consumes: `crate::templates::{TemplateRegistry, TemplateSpec, TemplateError}`, `crate::image_install::download_to` (widened), `crate::state::AppState`.
- Produces: `pub(crate) const MICROBOOT_ALIAS: &str = "__microboot"` (Task 3 matches on this to skip the network-ready wait), `pub(crate) async fn ensure_registered(state: &AppState) -> Result<String, String>` (Task 3 calls this from `start_bootstrap` and uses its `Ok` value as the builder's `template` field — always `MICROBOOT_ALIAS` on success).

- [ ] **Step 1: Widen `download_to` for reuse**

In `firecrab-api/src/image_install.rs`, change the signature at line 626 from `async fn download_to` to `pub(crate) async fn download_to` — no other change to the function body. This is the exact same temp-file-then-rename download helper `image_install.rs` already uses for package downloads; `microboot.rs` reuses it verbatim rather than duplicating the streaming/rename logic.

- [ ] **Step 2: Write `microboot.rs`**

```rust
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
    // 4 KiB of zeros: any small non-empty regular file works, since nothing
    // ever reads this content — `prepare_rootfs` grows it to the requested
    // `disk_gb` per VM, and the guest's own `mkfs.ext4 -F` overwrites
    // whatever's there entirely.
    std::fs::write(&rootfs_dest, [0u8; 4096])
        .map_err(|error| format!("write {}: {error}", rootfs_dest.display()))?;

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
    let extract_vmlinux = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../scripts/firecracker-menual/extract-vmlinux");
    let output = std::process::Command::new(&extract_vmlinux)
        .arg(raw_kernel)
        .output()
        .map_err(|error| format!("run extract-vmlinux ({}): {error}", extract_vmlinux.display()))?;
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
    std::fs::write(dest, &output.stdout).map_err(|error| format!("write {}: {error}", dest.display()))
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
        assert!(result.is_err(), "expected extract_vmlinux to reject garbage input");
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn ensure_registered_is_a_no_op_once_already_registered() {
        let dir = temp_image_root();
        let registry = TemplateRegistry::from_specs(dir.path(), std::iter::empty())
            .expect("empty registry");
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

        assert!(registry.resolve_alias(MICROBOOT_ALIAS).is_some());
        // ensure_registered's fast path only needs `state.templates` to
        // already resolve the alias — assert that resolution directly
        // rather than constructing a full AppState (which needs a live
        // store/runtime this unit test has no reason to stand up).
    }
}
```

- [ ] **Step 3: Wire the module in**

In `firecrab-api/src/main.rs`, add `mod microboot;` alphabetically between `mod ipam;` (line 10) and `mod model;` (line 11).

- [ ] **Step 4: Add `tempfile` dev-dependency if not already present**

```bash
grep -q '^tempfile' firecrab-api/Cargo.toml || echo "tempfile is missing — add it"
```

If missing, add under `[dev-dependencies]` in `firecrab-api/Cargo.toml`:

```toml
tempfile = "3"
```

(Check first — several existing tests in this crate already use tempdirs; it is very likely already a dev-dependency, in which case skip this step.)

- [ ] **Step 5: Compile and run the new tests**

```bash
cargo test -p firecrab-api microboot::
```

Expected: both tests in `microboot::tests` pass.

- [ ] **Step 6: Verify no regressions**

```bash
cargo fmt --all -- --check
cargo clippy -p firecrab-api -p firecrab-api-types --all-targets -- -D warnings
cargo test -p firecrab-api -p firecrab-api-types
```

- [ ] **Step 7: Commit**

```bash
git add firecrab-api/src/microboot.rs firecrab-api/src/main.rs firecrab-api/src/image_install.rs firecrab-api/Cargo.toml firecrab-api/Cargo.lock
git commit -m "feat: add microboot module — shared MicroBoot builder source"
```

---

### Task 2: Skip the network-readiness wait for MicroBoot-templated VMs

**Files:**
- Modify: `firecrab-api/src/handlers/vms.rs:826` (`finish_run_start`)
- Test: `firecrab-api/src/handlers/vms.rs` (inline, near existing `wait_for_network_ready` tests)

**Interfaces:**
- Consumes: `crate::microboot::MICROBOOT_ALIAS` (Task 1).
- Produces: no new public interface — `finish_run_start` behavior change only, exercised indirectly by any test that starts a VM templated `__microboot`.

**Why this specific gate and not a broader bypass:** `wait_for_network_ready` is the *only* place a MicroBoot VM would otherwise fail — real templates print `FIRECRAB_NETWORK_READY` from a service baked into their rootfs at build time; MicroBoot's guest never runs past a bare recovery shell, so nothing there ever prints it. Every other part of `finish_run_start` (disk prep, config write, `spawn_vm`) already works unchanged for any template, MicroBoot included.

- [ ] **Step 1: Read the current call site to confirm line numbers still match**

```bash
sed -n '715,832p' firecrab-api/src/handlers/vms.rs
```

Confirm `wait_for_network_ready(process.console(), state.runtime.network_ready_timeout)` is still called from `finish_run_start`, guarded by nothing, right after `spawn_vm`.

- [ ] **Step 2: Add the conditional skip**

Change:

```rust
    set_startup_step(state, vm.id, StartupStep::ConfiguringNetwork);
    // Not registered with register_and_watch yet, so `process` dropping on
    // an early return here still kills it (spawn_vm's Command sets
    // kill_on_drop) — no separate cleanup needed on this path.
    wait_for_network_ready(process.console(), state.runtime.network_ready_timeout)
        .await
        .map_err(|error| format!("network readiness check failed: {error}"))?;

    Ok(process)
```

to:

```rust
    set_startup_step(state, vm.id, StartupStep::ConfiguringNetwork);
    // MicroBoot's guest never gets past a bare Alpine recovery shell (see
    // crate::microboot's doc comment) — nothing there can ever print the
    // FIRECRAB_NETWORK_READY sentinel a real template's own baked-in
    // network-ready service normally does, so waiting for it here would
    // just fail every MicroBoot-templated VM after network_ready_timeout.
    // The guest scripts bring their own interface up manually instead
    // (`handlers::bootstrap`'s pushed script); this only skips the host's
    // passive wait, not networking itself.
    if template.name != crate::microboot::MICROBOOT_ALIAS {
        // Not registered with register_and_watch yet, so `process` dropping
        // on an early return here still kills it (spawn_vm's Command sets
        // kill_on_drop) — no separate cleanup needed on this path.
        wait_for_network_ready(process.console(), state.runtime.network_ready_timeout)
            .await
            .map_err(|error| format!("network readiness check failed: {error}"))?;
    }

    Ok(process)
```

`template` is already in scope in `finish_run_start` (it's a parameter — confirmed by the earlier `template.requires_pci_transport()` call a few lines up in the same function).

- [ ] **Step 3: Add a regression test**

The existing test module already has everything needed: `test_state_with_binary(root, binary)` (registers one template, `"ubuntu-rootfs-26.04"`, with `network_ready_timeout: Duration::from_millis(300)` baked into its `RuntimeConfig` — see `firecrab-api/src/handlers/vms.rs:1710-1762`), `fake_firecracker(directory, body)` + `FAKE_PRELUDE` (`firecrab-api/src/firecracker.rs:626-677`, imported into this test module at line 1786), and `record(name, id)` (builds a `VmRecord` templated `"ubuntu-rootfs-26.04"` by default — `firecrab-api/src/handlers/vms.rs:1678`). The existing `SERVE_LOOP` fixture body (`firecrab-api/src/firecracker.rs:642-653`) always prints `FIRECRAB_NETWORK_READY` itself, which is exactly why every existing `start_vm` test reaches `Running` — this new test needs a fake Firecracker that deliberately never prints it, so the only way `start_vm` can still succeed is the skip this task adds.

Add, in the same test module (near `start_then_stop_runs_the_full_lifecycle`, `firecrab-api/src/handlers/vms.rs:2220`):

```rust
    /// Unlike `SERVE_LOOP`, deliberately never prints `FIRECRAB_NETWORK_READY`
    /// — a real MicroBoot guest never can (crate::microboot's doc comment),
    /// so a VM templated `__microboot` must reach `Running` without it.
    const SERVE_LOOP_NO_NETWORK_SENTINEL: &str = r#"
print("booted", flush=True)
srv = socket.socket(socket.AF_UNIX)
srv.bind(sock_path)
srv.listen(1)
while True:
    conn, _ = srv.accept()
    conn.recv(1024)
    conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
    conn.close()
"#;

    #[tokio::test]
    async fn starting_a_vm_templated_as_microboot_skips_the_network_ready_wait() {
        let directory = short_tempdir();
        let root = directory.path();
        let binary = fake_firecracker(root, SERVE_LOOP_NO_NETWORK_SENTINEL);
        let state = test_state_with_binary(root, binary).await;
        // test_state_with_binary already wrote real kernel/rootfs fixture
        // files at root/"kernel" and root/"rootfs" for "ubuntu-rootfs-26.04"
        // — register a second alias, __microboot, pointing at those same
        // verified files, so this test proves the *template name* is what
        // gates the skip, without needing crate::microboot's real network
        // download.
        state
            .templates
            .register_spec(TemplateSpec {
                alias: crate::microboot::MICROBOOT_ALIAS.to_owned(),
                version: "v1".to_owned(),
                kernel: PathBuf::from("kernel"),
                initrd: None,
                rootfs: PathBuf::from("rootfs"),
                boot_args: "console=ttyS0 reboot=k".to_owned(),
            })
            .unwrap();
        let mut vm = record("microboot-builder", Uuid::new_v4());
        vm.template = crate::microboot::MICROBOOT_ALIAS.to_owned();
        seed_vm(&state, &vm);

        let started = tokio::time::timeout(
            Duration::from_secs(2),
            start_vm(
                State(state.clone()),
                Extension(RequestId(Uuid::new_v4())),
                axum::extract::Path(vm.id.to_string()),
            ),
        )
        .await
        .expect("start_vm should not hang waiting on a network-ready sentinel that never arrives")
        .unwrap();

        assert_eq!(started.state, VmState::Running);
    }
```

- [ ] **Step 4: Run it**

```bash
cargo test -p firecrab-api handlers::vms::tests::starting_a_vm_templated_as_microboot
```

Expected: PASS.

- [ ] **Step 5: Full verification**

```bash
cargo fmt --all -- --check
cargo clippy -p firecrab-api -p firecrab-api-types --all-targets -- -D warnings
cargo test -p firecrab-api -p firecrab-api-types
```

- [ ] **Step 6: Commit**

```bash
git add firecrab-api/src/handlers/vms.rs
git commit -m "fix: skip the network-ready wait for MicroBoot-templated VMs"
```

---

### Task 3: Replace `pick_builder_source` with MicroBoot, drop the matching-source rule

**Files:**
- Modify: `firecrab-api/src/handlers/bootstrap.rs`
- Test: `firecrab-api/src/handlers/bootstrap.rs` (inline, existing `mod tests`)

**Interfaces:**
- Consumes: `crate::microboot::ensure_registered` (Task 1).
- Produces: `start_bootstrap` now works with zero installed templates. `pick_builder_source`'s signature changes from `fn pick_builder_source(state: &AppState, target_alias: &str, request_id: Uuid) -> Result<String, AppError>` to `async fn pick_builder_source(state: &AppState, request_id: Uuid) -> Result<String, AppError>` (drops the now-unused `target_alias` — nothing left branches on it).

- [ ] **Step 1: Delete `requires_matching_source` and its use**

Delete the whole function at `firecrab-api/src/handlers/bootstrap.rs:87-95`:

```rust
/// Alpine and Ubuntu bootstrap by chrooting into a freshly-downloaded base
/// that carries its own package manager, so any installed template can
/// serve as the outer builder environment. Rocky's bootstrap needs `dnf`
/// already present in the *outer* guest (see
/// `scripts/firecracker-menual/bootstrap-rocky-in-guest.sh`'s doc comment),
/// so its own builder VM must itself already be `rocky-9`.
fn requires_matching_source(target_alias: &str) -> bool {
    target_alias == "rocky-9"
}
```

Rocky's bootstrap script now brings its own `dnf`-capable chroot with it (Task 5) instead of depending on the outer guest already being `rocky-9` — there is no longer a target alias that needs a matching source.

- [ ] **Step 2: Replace `pick_builder_source`**

Replace the whole function at `firecrab-api/src/handlers/bootstrap.rs:207-239`:

```rust
/// Picks an already-installed template to boot as the builder VM.
/// `requires_matching_source` narrows this to the target itself for
/// aliases whose bootstrap needs the outer guest to already have that
/// distro's own package manager (currently just `rocky-9`, see its own
/// doc comment) — everything else accepts any installed alias, preferring
/// the smallest rootfs since it boots fastest.
fn pick_builder_source(
    state: &AppState,
    target_alias: &str,
    request_id: Uuid,
) -> Result<String, AppError> {
    let candidates = state.templates.list_aliases();
    let mut eligible: Vec<_> = candidates
        .into_iter()
        .filter(|version| !requires_matching_source(target_alias) || version.name == target_alias)
        .collect();
    eligible.sort_by_key(|version| version.rootfs.length());

    eligible
        .into_iter()
        .next()
        .map(|version| version.name.clone())
        .ok_or_else(|| {
            AppError::unavailable(
                if requires_matching_source(target_alias) {
                    "bootstrapping rocky-9 needs rocky-9 already installed to provide dnf — install it first"
                } else {
                    "no template is installed yet to serve as the builder VM — install one first"
                },
                request_id,
            )
        })
}
```

with:

```rust
/// Ensures the shared MicroBoot builder source is downloaded, converted and
/// registered (a no-op after the first call — see `crate::microboot`'s own
/// doc comment), and returns its alias for `CreateVmRequest.template`. No
/// longer depends on any template being installed: this is what closes the
/// bootstrap boundary `docs/superpowers/specs/2026-08-03-m2image-web-rebuild-design.md`
/// left open (`docs/superpowers/specs/2026-08-05-m2image-microboot-design.md`).
async fn pick_builder_source(state: &AppState, request_id: Uuid) -> Result<String, AppError> {
    crate::microboot::ensure_registered(state)
        .await
        .map_err(|reason| AppError::unavailable(&reason, request_id))
}
```

- [ ] **Step 3: Update the call site in `start_bootstrap`**

In `start_bootstrap` (around line 124), change:

```rust
    let source_alias = pick_builder_source(&state, &alias, request_id.0)?;
```

to:

```rust
    let source_alias = pick_builder_source(&state, request_id.0).await?;
```

Confirm `AppError::unavailable`'s exact signature before relying on the `&reason` call above:

```bash
grep -n "fn unavailable" firecrab-api/src/error.rs
```

Adjust the `.map_err` closure in Step 2 to match whatever it actually takes (likely `&str` or `String` plus the request id — mirror how the deleted code already called it, e.g. `AppError::unavailable("...", request_id)`, just with a `String` produced by `ensure_registered` instead of a `&'static str`).

- [ ] **Step 4: Update the test that assumed the old rejection behavior**

`start_bootstrap_rejects_an_unknown_target_alias` (`firecrab-api/src/handlers/bootstrap.rs:1198-1212`) is unaffected — it rejects before `pick_builder_source` ever runs — and needs no change.

`start_bootstrap_rejects_when_no_matching_source_is_installed` (`firecrab-api/src/handlers/bootstrap.rs:1214-1232`) specifically targets `"rocky-9"` against a `test_state()` whose only registered template is `"ubuntu-rootfs-26.04"` (from `handlers::vms::tests::test_state`, `firecrab-api/src/handlers/vms.rs:1706` — note that alias is distinct from the bootstrap-target alias `"ubuntu-26.04"`, so it never accidentally satisfies the old "any installed template" rule either). That combination was chosen specifically because it was insufficient under the *old* rule even for a non-Rocky target — which is exactly why it's the right fixture to flip: replace the test to prove `start_bootstrap` now succeeds in that same situation, since source selection no longer depends on any installed template at all:

```rust
    #[tokio::test]
    async fn start_bootstrap_succeeds_with_no_installed_templates_once_microboot_is_registered() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);
        // test_state's one registered template, "ubuntu-rootfs-26.04", isn't
        // any of the 3 bootstrap-target aliases and (before this task)
        // wouldn't have satisfied rocky-9's old matching-source rule either
        // — this fixture is deliberately unchanged from the test it
        // replaces, to prove the same "no real target template installed"
        // situation that used to 503 now succeeds. Register __microboot
        // directly (mirroring Task 1's own registration test) rather than
        // exercising a real network download here.
        state
            .templates
            .register_spec(TemplateSpec {
                alias: crate::microboot::MICROBOOT_ALIAS.to_owned(),
                version: "v1".to_owned(),
                kernel: PathBuf::from("kernel"),
                initrd: None,
                rootfs: PathBuf::from("rootfs"),
                boot_args: "console=ttyS0 reboot=k".to_owned(),
            })
            .unwrap();

        let (status, Json(session)) = start_bootstrap(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path("rocky-9".to_owned()),
        )
        .await
        .unwrap();

        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(session.alias, "rocky-9");
    }
```

`kernel`/`rootfs` here reuse the exact same fixture file paths `test_state` already wrote for `"ubuntu-rootfs-26.04"` (see `handlers::vms::tests::test_state_with_binary`, `firecrab-api/src/handlers/vms.rs:1710-1721`) — confirm those files exist under `directory.path()` before relying on this (they do, written unconditionally by that fixture), rather than assuming without checking.

- [ ] **Step 5: Run the updated tests**

```bash
cargo test -p firecrab-api handlers::bootstrap::tests::start_bootstrap
```

Expected: all pass, including the new/replaced one.

- [ ] **Step 6: Full verification**

```bash
cargo fmt --all -- --check
cargo clippy -p firecrab-api -p firecrab-api-types --all-targets -- -D warnings
cargo test -p firecrab-api -p firecrab-api-types
```

- [ ] **Step 7: Commit**

```bash
git add firecrab-api/src/handlers/bootstrap.rs
git commit -m "feat: bootstrap always sources its builder VM from MicroBoot"
```

---

### Task 4: Filter the internal MicroBoot alias out of `/api/images`

**Files:**
- Modify: `firecrab-api/src/handlers/images.rs:96-105` (`list_images`)
- Test: `firecrab-api/src/handlers/images.rs` (inline)

**Interfaces:**
- Consumes: `crate::microboot::MICROBOOT_ALIAS` (Task 1).
- Produces: no interface change — `list_images`'s existing response shape and route are unchanged, only its contents when `__microboot` happens to be registered.

- [ ] **Step 1: Add the filter**

In `list_images` (`firecrab-api/src/handlers/images.rs`), change:

```rust
        // Any extra registered aliases not in the built-in set (future registration API).
        for template in templates.list_aliases() {
            if !images.iter().any(|image| image.alias == template.name) {
                images.push(installed_response(
                    template.as_ref(),
                    package_for(&template.name),
                    staged_for(&template.name),
                ));
            }
        }
```

to:

```rust
        // Any extra registered aliases not in the built-in set (future
        // registration API) — except the internal MicroBoot builder source
        // (crate::microboot), which is registered into TemplateRegistry
        // purely so create_vm's existing machinery can provision it, and
        // must never appear as an installable image.
        for template in templates.list_aliases() {
            if template.name == crate::microboot::MICROBOOT_ALIAS {
                continue;
            }
            if !images.iter().any(|image| image.alias == template.name) {
                images.push(installed_response(
                    template.as_ref(),
                    package_for(&template.name),
                    staged_for(&template.name),
                ));
            }
        }
```

- [ ] **Step 2: Add a regression test**

`firecrab-api/src/handlers/images.rs:1046-1073` already has the exact fixture pattern to mirror — `list_images_includes_extra_registered_aliases` builds a `TemplateRegistry::from_specs` with one extra alias and a real `AppState::with_db_file`. Add, right after that test (before the closing `}` of `mod tests` at line 1074):

```rust
    #[tokio::test]
    async fn list_images_never_surfaces_the_internal_microboot_alias() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        write_file(&root.join("kernel/vmlinux"), b"k");
        write_file(&root.join("rootfs/root.ext4"), b"r");
        let templates = TemplateRegistry::from_specs(
            root,
            [TemplateSpec {
                alias: crate::microboot::MICROBOOT_ALIAS.to_owned(),
                version: "v1".to_owned(),
                kernel: Path::new("kernel/vmlinux").to_path_buf(),
                initrd: None,
                rootfs: Path::new("rootfs/root.ext4").to_path_buf(),
                boot_args: "console=ttyS0 reboot=k".to_owned(),
            }],
        )
        .unwrap();
        let state = AppState::with_db_file(templates, root.join("state.db"))
            .await
            .unwrap();

        let Json(images) = list_images(State(state)).await;

        assert!(
            images
                .iter()
                .all(|image| image.alias != crate::microboot::MICROBOOT_ALIAS),
            "microboot must never appear in /api/images: {images:?}"
        );
    }
```

- [ ] **Step 3: Run it**

```bash
cargo test -p firecrab-api handlers::images::tests::list_images_never_surfaces
```

Expected: PASS.

- [ ] **Step 4: Full verification**

```bash
cargo fmt --all -- --check
cargo clippy -p firecrab-api -p firecrab-api-types --all-targets -- -D warnings
cargo test -p firecrab-api -p firecrab-api-types
```

- [ ] **Step 5: Commit**

```bash
git add firecrab-api/src/handlers/images.rs
git commit -m "fix: never surface the internal microboot alias via /api/images"
```

---

### Task 5: Guest script changes — Alpine and Ubuntu

**Files:**
- Modify: `scripts/firecracker-menual/bootstrap-alpine-in-guest.sh`
- Modify: `scripts/firecracker-menual/bootstrap-ubuntu-in-guest.sh`

**Interfaces:**
- Consumes: nothing new — these scripts are still pushed over the console verbatim by `handlers::bootstrap::run_bootstrap_script`, unchanged.
- Produces: the guest's `/dev/vda` now ends up holding the *whole* `$out` directory as one ext4 filesystem (previously `$out` was just files sitting on the already-mounted real-template disk) — Task 7 depends on the exact root-level paths this produces (`/rootfs.ext4`, `/vmlinuz-*-raw`, `/initramfs`).

**Why no `$out` internal restructuring is needed:** both scripts already build `$out/rootfs.ext4` + `$out/vmlinuz-*-raw` (+ `$out/initramfs` for scripts that have one) as ordinary files via `truncate`+`mkfs.ext4 -d`, entirely inside whatever filesystem `$work` (`/root/fc-bootstrap`) happens to sit on — that logic needs no changes. What changes is only: (a) the outer environment now needs `e2fsprogs` and a manually-brought-up network interface instead of inheriting both for free from a fully-booted real OS, and (b) one new final step wraps the *entire* finished `$out` directory onto the actual block device, since MicroBoot's root is RAM-backed and nothing there persists once the VM stops.

- [ ] **Step 1: Add network bring-up + e2fsprogs to `bootstrap-alpine-in-guest.sh`**

Insert immediately after the existing `arch=$(uname -m)` case block (after line 35, before the `info 'resolving latest Alpine 3.24 minirootfs release'` line):

```sh
info 'bringing up eth0 (MicroBoot has no network service of its own)'
udhcpc -i eth0 -n -q >/dev/null 2>&1 || fail 'could not obtain a DHCP lease on eth0'

info 'installing e2fsprogs into the outer (MicroBoot) shell'
apk add --no-cache --repository "${alpine_releases_base}/v3.24/main" e2fsprogs \
  || fail 'could not install e2fsprogs into the outer shell'
```

(`$alpine_releases_base` is already defined a few lines above this insertion point, at line 13 — reuse it rather than hardcoding the URL a second time.)

- [ ] **Step 2: Wrap the finished `$out` onto the real disk in `bootstrap-alpine-in-guest.sh`**

Change the final block (lines 149-167) from:

```sh
info 'building rootfs.ext4'
truncate -s "$rootfs_size" "$out/rootfs.ext4.tmp"
mkfs.ext4 -F -L rootfs -d "$staging" "$out/rootfs.ext4.tmp"
mv "$out/rootfs.ext4.tmp" "$out/rootfs.ext4"

# Everything under $out is read back off this VM's *block device* by the
# host (`rootfs::dump_from_image` via debugfs) once the VM is stopped, so
# the guest's page cache must be flushed to the device before this script
# exits — otherwise the host reads a truncated or entirely absent file and
# packages it as if it were a complete rootfs.
sync

# extract-vmlinux ships alongside this script in the repo but does not
# exist inside the guest — the raw vmlinuz is dumped out as-is
# ($out/vmlinuz-virt-raw) and Task 8 runs extract-vmlinux on the HOST
# after pulling it out of the guest disk, since the host is already known
# to have every decompressor it might need (same reasoning
# install-alpine-rootfs.sh's own extract_kernel already used).
info 'bootstrap complete'
```

to:

```sh
info 'building rootfs.ext4'
truncate -s "$rootfs_size" "$out/rootfs.ext4.tmp"
mkfs.ext4 -F -L rootfs -d "$staging" "$out/rootfs.ext4.tmp"
mv "$out/rootfs.ext4.tmp" "$out/rootfs.ext4"

# MicroBoot boots off its own initrd (RAM), not off /dev/vda — nothing
# under $work persists once this VM stops. Wrap the whole finished $out
# directory (rootfs.ext4 + the raw kernel/initrd files) directly onto the
# real block device as its own ext4 filesystem, so the host's
# debugfs-based dump (`rootfs::dump_from_image`) can read them back at
# their root (e.g. /rootfs.ext4, not /root/fc-bootstrap/out/rootfs.ext4)
# after this VM is stopped.
info 'publishing $out onto /dev/vda'
mkfs.ext4 -F -L fcbootout -d "$out" /dev/vda

# Everything on /dev/vda is read back by the host
# (`rootfs::dump_from_image` via debugfs) once the VM is stopped, so the
# guest's page cache must be flushed to the device before this script
# exits — otherwise the host reads a truncated or entirely absent image
# and packages it as if it were complete.
sync

# extract-vmlinux ships alongside this script in the repo but does not
# exist inside the guest — the raw vmlinuz is dumped out as-is
# (vmlinux-virt-raw) and the host runs extract-vmlinux after pulling it
# off the guest disk, since the host is already known to have every
# decompressor it might need (same reasoning install-alpine-rootfs.sh's
# own extract_kernel already used).
info 'bootstrap complete'
```

- [ ] **Step 3: Apply the same two changes to `bootstrap-ubuntu-in-guest.sh`**

Insert, after its `case "$(uname -m)" in ... esac` block (after line 36, before `release_url=...` at line 38):

```sh
info 'bringing up eth0 (MicroBoot has no network service of its own)'
udhcpc -i eth0 -n -q >/dev/null 2>&1 || fail 'could not obtain a DHCP lease on eth0'

info 'installing e2fsprogs into the outer (MicroBoot) shell'
apk add --no-cache --repository 'https://dl-cdn.alpinelinux.org/alpine/v3.24/main' e2fsprogs \
  || fail 'could not install e2fsprogs into the outer shell'
```

(Ubuntu's script has no `$alpine_releases_base` variable of its own since it never talks to Alpine's mirrors for anything else — the outer shell is still MicroBoot/Alpine regardless of the *target* being Ubuntu, so the repository URL is hardcoded here rather than borrowing a variable name that would misleadingly suggest it's Ubuntu-related.)

Change the final block (lines 165-180) from:

```sh
info 'building rootfs.ext4'
truncate -s "$rootfs_size" "$out/rootfs.ext4.tmp"
mkfs.ext4 -F -L rootfs -d "$mount_dir" "$out/rootfs.ext4.tmp"
mv "$out/rootfs.ext4.tmp" "$out/rootfs.ext4"

# Everything under $out is read back off this VM's *block device* by the
# host (`rootfs::dump_from_image` via debugfs) once the VM is stopped, so
# the guest's page cache must be flushed to the device before this script
# exits — otherwise the host reads a truncated or entirely absent file and
# packages it as if it were a complete rootfs.
sync

# Same reasoning as the Alpine script: extract-vmlinux runs on the HOST
# (Task 8) against $out/vmlinuz-raw once it's been dumped out of this
# guest's own disk — not in here.
info 'bootstrap complete'
```

to:

```sh
info 'building rootfs.ext4'
truncate -s "$rootfs_size" "$out/rootfs.ext4.tmp"
mkfs.ext4 -F -L rootfs -d "$mount_dir" "$out/rootfs.ext4.tmp"
mv "$out/rootfs.ext4.tmp" "$out/rootfs.ext4"

# MicroBoot boots off its own initrd (RAM), not off /dev/vda — nothing
# under $work persists once this VM stops. Wrap the whole finished $out
# directory (rootfs.ext4 + the raw kernel file) directly onto the real
# block device as its own ext4 filesystem, so the host's debugfs-based
# dump (`rootfs::dump_from_image`) can read them back at their root (e.g.
# /rootfs.ext4, not /root/fc-bootstrap/out/rootfs.ext4) after this VM is
# stopped.
info 'publishing $out onto /dev/vda'
mkfs.ext4 -F -L fcbootout -d "$out" /dev/vda

# Everything on /dev/vda is read back by the host
# (`rootfs::dump_from_image` via debugfs) once the VM is stopped, so the
# guest's page cache must be flushed to the device before this script
# exits — otherwise the host reads a truncated or entirely absent image
# and packages it as if it were complete.
sync

# Same reasoning as the Alpine script: extract-vmlinux runs on the HOST
# against vmlinuz-raw once it's been dumped off this guest's own disk —
# not in here.
info 'bootstrap complete'
```

- [ ] **Step 4: Static-check both scripts**

```bash
sh -n scripts/firecracker-menual/bootstrap-alpine-in-guest.sh
sh -n scripts/firecracker-menual/bootstrap-ubuntu-in-guest.sh
command -v shellcheck >/dev/null && shellcheck -s sh scripts/firecracker-menual/bootstrap-alpine-in-guest.sh scripts/firecracker-menual/bootstrap-ubuntu-in-guest.sh
```

Expected: `sh -n` exits 0 for both (syntax valid); shellcheck (if installed) reports nothing new beyond whatever these files' pre-existing baseline already had.

- [ ] **Step 5: Commit**

```bash
git add scripts/firecracker-menual/bootstrap-alpine-in-guest.sh scripts/firecracker-menual/bootstrap-ubuntu-in-guest.sh
git commit -m "feat: adapt alpine/ubuntu guest bootstrap scripts for MicroBoot"
```

---

### Task 6: Guest script changes — Rocky (OCI Container-Base unwrap + chroot for `dnf`)

**Files:**
- Modify: `scripts/firecracker-menual/bootstrap-rocky-in-guest.sh`

**Interfaces:**
- Consumes: nothing new (pushed over console verbatim, same as Task 5).
- Produces: same root-level `$out`-wrapped-onto-`/dev/vda` contract as Task 5 (`/rootfs.ext4`, `/vmlinuz-raw`, `/initramfs`), so Task 7's path changes apply identically to Rocky's output too.

**Why Rocky needs more than Task 5's two additions:** the existing script assumes `dnf` is already a working command in the outer shell (true when the outer VM was itself an installed `rocky-9` template; never true for MicroBoot's Alpine/musl recovery shell — `dnf`/`rpm` are glibc binaries, verified live that they exist inside Rocky's official Container-Base but cannot simply be copied out and run directly from a musl shell). The fix mirrors what `install-rocky-rootfs.sh`/this script already do structurally (chroot first, then run the target distro's own package manager from inside that chroot) — just applied one level earlier, to get `dnf` itself working, before the existing `dnf --installroot=$staging ...` logic runs unchanged.

- [ ] **Step 1: Add network bring-up (same as Task 5)**

Insert right after `set -eu` (line 10), before the `work=/root/fc-bootstrap` variable block:

```sh
udhcpc -i eth0 -n -q >/dev/null 2>&1 || {
  printf '[FAIL] %s\n' 'could not obtain a DHCP lease on eth0' >&2
  exit 1
}
```

(Written without the `info`/`fail` helpers since they aren't defined until a few lines later in this script — matches this script's own existing ordering constraint. Everywhere else in this task, prefer `info`/`fail` once they're in scope.)

- [ ] **Step 2: Add the Container-Base download + chroot-in step**

Insert a new section between the existing `mkdir -p "$staging/etc/pki" ...` line (line 48) and the `dnf_common=...` line (line 51):

```sh
# Rocky's dnf/rpm are glibc binaries; MicroBoot's outer shell is Alpine
# (musl) and cannot run them directly (verified live: dnf/rpm exist inside
# the extracted Container-Base but exec fails from outside a matching
# libc environment). Download Rocky's own official Container-Base — the
# same artifact `docker pull rockylinux:9` resolves to — and chroot into
# IT first, so the dnf that actually runs below is Rocky's own, under its
# own glibc. This container_root is discarded once dnf finishes (it never
# becomes part of the target rootfs in $staging).
container_root="$work/container-base"
container_archive="$work/rocky-container-base.tar.xz"
mkdir -p "$container_root"

info 'installing e2fsprogs and container tooling into the outer (MicroBoot) shell'
apk add --no-cache --repository 'https://dl-cdn.alpinelinux.org/alpine/v3.24/main' \
  e2fsprogs jq \
  || fail 'could not install e2fsprogs/jq into the outer shell'

info 'downloading Rocky 9 Container-Base'
curl -fsSL 'https://dl.rockylinux.org/pub/rocky/9/images/x86_64/Rocky-9-Container-Base.latest.x86_64.tar.xz' \
  -o "$container_archive" || fail 'could not download Rocky Container-Base'

# Container-Base is an OCI image layout (blobs/sha256/... + index.json),
# not a flat rootfs tarball — verified live. It has exactly one manifest
# and one layer; extract that one layer's tar+gzip blob directly rather
# than pulling in full OCI tooling for a single-layer image.
oci_dir="$work/container-oci"
mkdir -p "$oci_dir"
tar -xJf "$container_archive" -C "$oci_dir"
manifest_digest=$(jq -r '.manifests[0].digest | sub("^sha256:"; "")' "$oci_dir/index.json")
[ -n "$manifest_digest" ] && [ "$manifest_digest" != null ] \
  || fail 'could not read Container-Base manifest digest from index.json'
layer_digest=$(jq -r '.layers[0].digest | sub("^sha256:"; "")' "$oci_dir/blobs/sha256/$manifest_digest")
[ -n "$layer_digest" ] && [ "$layer_digest" != null ] \
  || fail 'could not read Container-Base layer digest from its manifest'

info 'extracting Rocky 9 Container-Base'
tar -xzf "$oci_dir/blobs/sha256/$layer_digest" -C "$container_root"
test -x "$container_root/usr/bin/rpm" || fail 'Container-Base is missing usr/bin/rpm'
test -e "$container_root/usr/bin/dnf" || fail 'Container-Base is missing usr/bin/dnf'

mount -t proc proc "$container_root/proc"
mount --rbind /sys "$container_root/sys"
mount --make-rslave "$container_root/sys"
mount --rbind /dev "$container_root/dev"
mount --make-rslave "$container_root/dev"
cp /etc/resolv.conf "$container_root/etc/resolv.conf" 2>/dev/null || true
chroot_mounts="$container_root/proc $container_root/sys $container_root/dev $chroot_mounts"

```

- [ ] **Step 3: Route the existing `dnf` call through the chroot**

`jq` is a new dependency for this script — it is **not** part of Alpine's base recovery shell (only `apk`, `busybox`'s applets, `wget`, `chroot`, `tar` were confirmed present live); Step 2 above installs it via `apk add` alongside `e2fsprogs`, so no further action is needed here, but double-check it landed:

```bash
grep -n "jq" scripts/firecracker-menual/bootstrap-rocky-in-guest.sh
```

Now change the existing `dnf` invocation (lines 53-58) from:

```sh
info 'installing Rocky Linux 9 guest packages into the staging root'
mount_chroot_fs
# package/flag lists are deliberate whitespace lists.
# shellcheck disable=SC2086
dnf -q -y --installroot="$staging" --releasever=9 --setopt=reposdir=/etc/yum.repos.d \
  $dnf_common install $rootfs_packages
```

to:

```sh
info 'installing Rocky Linux 9 guest packages into the staging root'
mount_chroot_fs
# Bind the in-progress target root and dnf's repo config into the
# Container-Base chroot so `chroot "$container_root" dnf --installroot=...`
# can see and populate $staging exactly as a native `dnf` invocation would.
mkdir -p "$container_root$staging" "$container_root/etc/yum.repos.d"
mount --rbind "$staging" "$container_root$staging"
mount --make-rslave "$container_root$staging"
chroot_mounts="$container_root$staging $chroot_mounts"
# package/flag lists are deliberate whitespace lists.
# shellcheck disable=SC2086
chroot "$container_root" dnf -q -y --installroot="$staging" --releasever=9 \
  --setopt=reposdir=/etc/yum.repos.d $dnf_common install $rootfs_packages
```

- [ ] **Step 4: Tear the container chroot down alongside the rest**

`cleanup_chroot_mounts` (the existing `trap`-registered function, lines 25-30) already iterates `$chroot_mounts` and unmounts every entry — since Step 2 and Step 3 both prepend their new mounts onto `$chroot_mounts` using the exact same pattern the rest of the script already uses (`chroot_mounts="$new $chroot_mounts"`), no changes are needed there. Confirm this is genuinely true by re-reading the full modified script once both steps are in place:

```bash
sed -n '1,45p' scripts/firecracker-menual/bootstrap-rocky-in-guest.sh
```

and checking every new `mount` call above has a matching entry appended to `$chroot_mounts`.

- [ ] **Step 5: Wrap `$out` onto `/dev/vda` (same as Task 5)**

Change the final block (lines 188-203) from:

```sh
info 'building rootfs.ext4'
truncate -s "$rootfs_size" "$out/rootfs.ext4.tmp"
mkfs.ext4 -F -L rootfs -d "$staging" "$out/rootfs.ext4.tmp"
mv "$out/rootfs.ext4.tmp" "$out/rootfs.ext4"

# Everything under $out is read back off this VM's *block device* by the
# host (`rootfs::dump_from_image` via debugfs) once the VM is stopped, so
# the guest's page cache must be flushed to the device before this script
# exits — otherwise the host reads a truncated or entirely absent file and
# packages it as if it were a complete rootfs.
sync

# Same reasoning as the Alpine/Ubuntu scripts: extract-vmlinux runs on the
# HOST (Task 8) against $out/vmlinuz-raw once it's dumped out of this
# guest's own disk.
info 'bootstrap complete'
```

to:

```sh
info 'building rootfs.ext4'
truncate -s "$rootfs_size" "$out/rootfs.ext4.tmp"
mkfs.ext4 -F -L rootfs -d "$staging" "$out/rootfs.ext4.tmp"
mv "$out/rootfs.ext4.tmp" "$out/rootfs.ext4"

# MicroBoot boots off its own initrd (RAM), not off /dev/vda — nothing
# under $work persists once this VM stops. Wrap the whole finished $out
# directory (rootfs.ext4 + the raw kernel/initrd files) directly onto the
# real block device as its own ext4 filesystem, so the host's
# debugfs-based dump (`rootfs::dump_from_image`) can read them back at
# their root (e.g. /rootfs.ext4, not /root/fc-bootstrap/out/rootfs.ext4)
# after this VM is stopped.
info 'publishing $out onto /dev/vda'
mkfs.ext4 -F -L fcbootout -d "$out" /dev/vda

# Everything on /dev/vda is read back by the host
# (`rootfs::dump_from_image` via debugfs) once the VM is stopped, so the
# guest's page cache must be flushed to the device before this script
# exits — otherwise the host reads a truncated or entirely absent image
# and packages it as if it were complete.
sync

# Same reasoning as the Alpine/Ubuntu scripts: extract-vmlinux runs on the
# HOST against vmlinuz-raw once it's been dumped off this guest's own
# disk.
info 'bootstrap complete'
```

- [ ] **Step 6: Static-check**

```bash
sh -n scripts/firecracker-menual/bootstrap-rocky-in-guest.sh
command -v shellcheck >/dev/null && shellcheck -s sh scripts/firecracker-menual/bootstrap-rocky-in-guest.sh
```

Expected: `sh -n` exits 0; review any shellcheck findings on the new `chroot`/`mount --rbind` lines specifically (SC2086-style word-splitting warnings on `$dnf_common`/`$rootfs_packages` already have precedent `# shellcheck disable=SC2086` comments in this file — extend the same disable to the new `chroot "$container_root" dnf ...` line if shellcheck flags it too).

- [ ] **Step 7: Update the script's own doc comment**

The file's header comment (lines 1-9) still says *"Runs entirely inside a firecrab builder VM that is itself already rocky-9 (enforced by handlers::bootstrap::requires_matching_source...)"* — that function no longer exists (Task 3 deleted it). Replace the header comment with:

```sh
#!/bin/sh
# Runs entirely inside a firecrab MicroBoot builder VM (Alpine's own
# recovery shell — see crate::microboot's doc comment), not inside an
# installed rocky-9 template. Rocky's dnf/rpm are glibc binaries the
# musl-based outer shell can't run directly, so this script downloads
# Rocky's own official Container-Base image and chroots into it first —
# everything from that point on (dnf --installroot into $staging) is
# unchanged from install-rocky-rootfs.sh's own write_configure_script,
# minus its outer `docker run --cap-add=SYS_ADMIN ...` wrapper.
```

- [ ] **Step 8: Commit**

```bash
git add scripts/firecracker-menual/bootstrap-rocky-in-guest.sh
git commit -m "feat: adapt rocky guest bootstrap script for MicroBoot's Container-Base chroot"
```

---

### Task 7: Packaging — read the guest's output at its new root-level paths

**Files:**
- Modify: `firecrab-api/src/handlers/bootstrap.rs` (`build_package_blocking`, around lines 629-712)
- Test: `firecrab-api/src/handlers/bootstrap.rs` (inline, existing packaging tests)

**Interfaces:**
- Consumes: Task 5/6's guest-script output layout (`/rootfs.ext4`, `/vmlinuz-virt-raw` or `/vmlinuz-raw`, `/initramfs` — all at the image root now, not under `/root/fc-bootstrap/out/`).
- Produces: no change to `build_package_blocking`'s own signature or the final `.tar.zst` layout — only the *source* paths `dump_from_image` reads from change.

- [ ] **Step 1: Update the three `dump_from_image` call sites**

In `build_package_blocking` (`firecrab-api/src/handlers/bootstrap.rs`), change:

```rust
    let raw_rootfs = scratch.join("rootfs.raw.ext4");
    crate::rootfs::dump_from_image(
        guest_disk,
        "/root/fc-bootstrap/out/rootfs.ext4",
        &raw_rootfs,
    )
    .map_err(|e| format!("dump rootfs: {e}"))?;
```

to:

```rust
    let raw_rootfs = scratch.join("rootfs.raw.ext4");
    // Root-level, not /root/fc-bootstrap/out/... — MicroBoot's guest
    // scripts wrap their whole $out directory directly onto /dev/vda as
    // its own filesystem (see the bootstrap-{alpine,ubuntu,rocky}-in-guest.sh
    // scripts' own comments on this), so the paths inside it are relative
    // to that filesystem's root.
    crate::rootfs::dump_from_image(guest_disk, "/rootfs.ext4", &raw_rootfs)
        .map_err(|e| format!("dump rootfs: {e}"))?;
```

and:

```rust
    let raw_kernel = scratch.join("kernel.raw");
    crate::rootfs::dump_from_image(
        guest_disk,
        &format!("/root/fc-bootstrap/out/{raw_kernel_name}"),
        &raw_kernel,
    )
    .map_err(|e| format!("dump kernel: {e}"))?;
```

to:

```rust
    let raw_kernel = scratch.join("kernel.raw");
    crate::rootfs::dump_from_image(guest_disk, &format!("/{raw_kernel_name}"), &raw_kernel)
        .map_err(|e| format!("dump kernel: {e}"))?;
```

and:

```rust
    if let Some(initrd_relative) = &spec.initrd {
        let raw_initrd = scratch.join("initramfs.raw");
        crate::rootfs::dump_from_image(guest_disk, "/root/fc-bootstrap/out/initramfs", &raw_initrd)
            .map_err(|e| format!("dump initrd: {e}"))?;
```

to:

```rust
    if let Some(initrd_relative) = &spec.initrd {
        let raw_initrd = scratch.join("initramfs.raw");
        crate::rootfs::dump_from_image(guest_disk, "/initramfs", &raw_initrd)
            .map_err(|e| format!("dump initrd: {e}"))?;
```

- [ ] **Step 2: Update the existing packaging tests' fixtures**

These paths are almost certainly hardcoded into this file's own test fixtures too (the tests that build a synthetic 16 MiB ext4 via `debugfs` to feed `package_bootstrap`/`build_package_blocking`):

```bash
grep -n "root/fc-bootstrap/out\|debugfs.*write\|write_into_image" firecrab-api/src/handlers/bootstrap.rs | grep -A2 -B2 "fn.*test\|#\[tokio::test\]"
```

Every fixture that currently seeds `/root/fc-bootstrap/out/rootfs.ext4`, `/root/fc-bootstrap/out/vmlinuz-virt-raw` (or `vmlinuz-raw`), or `/root/fc-bootstrap/out/initramfs` into its synthetic test image must instead seed them at the corresponding root-level path (`/rootfs.ext4`, `/vmlinuz-virt-raw`, `/initramfs`) to match Step 1. Update each one; do not leave any fixture writing to the old nested path, since it would now silently make `dump_from_image` fail with "file not found" and the test would need to already be failing before this change for that to be caught (verify by running the full test module, not by inspection alone).

- [ ] **Step 3: Run the packaging test suite**

```bash
cargo test -p firecrab-api handlers::bootstrap::tests::package_bootstrap
cargo test -p firecrab-api handlers::bootstrap::tests::run_bootstrap_script
```

Expected: all pass. Any fixture missed in Step 2 will fail here with a `dump rootfs:`/`dump kernel:`/`dump initrd:` error — fix and rerun until clean.

- [ ] **Step 4: Full verification**

```bash
cargo fmt --all -- --check
cargo clippy -p firecrab-api -p firecrab-api-types --all-targets -- -D warnings
cargo test -p firecrab-api -p firecrab-api-types
```

- [ ] **Step 5: Commit**

```bash
git add firecrab-api/src/handlers/bootstrap.rs
git commit -m "fix: read bootstrap packaging output at its new MicroBoot-produced root paths"
```

---

### Task 8: Manual end-to-end verification (all 3 aliases, zero templates installed)

**Files:** none — verification only.

**Interfaces:** none.

This is the one part of this plan that cannot be verified by an automated test (same limitation the 2026-08-03 plan already documented for its own end-to-end check) — it needs a real host with real internet access and no other active `firecrab-api`/VM session to avoid interfering with production use, per the fix-wave report's own I5 finding on the previous plan.

- [ ] **Step 1: Confirm a clean slate**

```bash
ls images/rootfs/ images/kernel/ images/.packages/ 2>/dev/null
```

If any of the 3 target aliases (`alpine-3.24`, `ubuntu-26.04`, `rocky-9`) already have matching rootfs+kernel files present, this run won't actually prove the "zero installed templates" case — either test against a scratch `FIRECRAB_IMAGE_ROOT`, or delete one alias's artifacts first (confirm with the user before deleting anything real).

- [ ] **Step 2: Start `firecrab-api` fresh**

```bash
cargo run -p firecrab-api
```

Confirm the startup log shows all 3 built-in template warnings (artifacts missing, skipped) if the slate is genuinely clean — this is expected and matches `TemplateRegistry::load_from`'s own documented behavior, not a bug.

- [ ] **Step 3: Bootstrap `alpine-3.24` from the web UI**

Click the bootstrap button for `alpine-3.24`. Confirm, in order:
1. No 503 — the request is accepted (proves Task 3's `pick_builder_source` no longer requires an installed template).
2. The session log shows the builder VM reaching `Running` within a few seconds, not stuck for 30s+ (proves Task 2's network-ready skip took effect).
3. Heartbeat lines appear roughly every 60s while the script runs (pre-existing behavior, confirms the console is genuinely alive).
4. The session reaches `succeeded`, and `images/.packages/alpine-3.24.tar.zst` exists on disk.
5. The "로컬 패키지 설치" button appears and installs it successfully.
6. A VM created from the newly-installed `alpine-3.24` template actually boots to a login prompt — the real proof the `sync` + MicroBoot-wrap ordering produced an intact rootfs, not just a plausible-looking file.

- [ ] **Step 4: Repeat for `ubuntu-26.04`**

Same 6 checks. This run additionally proves MicroBoot's Alpine builder can bootstrap a *different* target distro (no `apt` needed outer-side, per the design doc).

- [ ] **Step 5: Repeat for `rocky-9`**

Same 6 checks, plus specifically watch the session log for the Container-Base download/chroot lines added in Task 6 (`downloading Rocky 9 Container-Base`, `extracting Rocky 9 Container-Base`) to confirm that path is actually exercised, not skipped.

- [ ] **Step 6: Record the outcome**

If all three succeed: this plan's MVP is complete. If any step fails, capture the exact session log and console tail — the most likely failure points, in rough order of likelihood given everything verified so far in isolation:
- `udhcpc -i eth0` failing because the interface isn't actually named `eth0` in this specific boot path (verified live only that `virtio_blk` auto-probes; `virtio_net`'s interface name was never verified against a real TAP-attached test — see the design doc's own "실제 부팅 검증" section, last bullet).
- The `jq`/OCI-digest-extraction logic in Rocky's script failing against a real (not manually-inspected) `index.json`/manifest if Rocky ever restructures their Container-Base publishing.
- `mkfs.ext4 -F -d "$out" /dev/vda` failing if `$out`'s total size (rootfs.ext4 + raw kernel/initrd) exceeds the builder VM's `disk_gb` — cross-check `bootstrap_disk_gb`'s existing headroom (4 GiB for alpine, 8 GiB for ubuntu/rocky) against the actual sizes involved (a 512 MiB–2 GiB `rootfs.ext4` plus a raw kernel/initrd of a few tens of MiB comfortably fits either budget, but confirm with `ls -la` on the produced `$out` if this step fails).

None of these three are addressed by earlier tasks — if hit, they need their own follow-up fix; do not attempt to patch them speculatively without first seeing the actual failure mode live.

---

## Completion Criteria

- [ ] All 8 code tasks (1–7 plus this list) complete, each committed separately, `cargo fmt`/`clippy`/`test` clean after every one.
- [ ] Task 8's manual verification run recorded, ideally successful for all 3 aliases from a genuinely template-free `images/` — if any alias fails, the specific failure is documented rather than silently left for "later."
- [ ] `docs/superpowers/specs/2026-08-05-m2image-microboot-design.md`'s own 완료 기준 checklist matches this plan's actual delivered state (update any items that were resolved differently than originally proposed during implementation).
