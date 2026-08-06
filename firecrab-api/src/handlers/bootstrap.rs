//! Web-triggered from-scratch distro bootstraps: boot a builder VM off
//! *any* already-installed template (a disposable environment, not the
//! target), run a bootstrap script over its console that downloads the
//! target's official base, chroots in, installs packages + kernel via the
//! target's own package manager, and `mkfs.ext4 -d`s a finished rootfs —
//! then dump the result out of the builder VM's disk and package it as
//! `{alias}.tar.zst` for the existing `image_install.rs` pipeline to pick
//! up unchanged. See
//! `docs/superpowers/specs/2026-08-03-m2image-web-rebuild-design.md`.

use std::time::Duration;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use firecrab_api_types::{BootstrapResponse, BootstrapStatus, CreateVmRequest, EgressPolicy};
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::model::VmState;
use crate::server::RequestId;
use crate::state::AppState;

use super::builds::{builder_micro_network_id, builder_vm_name, mark_as_builder};
use super::vms::{create_vm, start_vm_request};

/// Matches `handlers::builds`'s own poll cadence.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Generous on purpose, same reasoning as `handlers::builds::BUILDER_BOOT_TIMEOUT`.
const BUILDER_BOOT_TIMEOUT: Duration = Duration::from_secs(600);

/// The 3 aliases this feature can bootstrap — deliberately not
/// `TemplateRegistry::known_specs()` directly, so a future built-in
/// addition doesn't silently become bootstrap-eligible without its own
/// guest script (Task 6 covers exactly these 3, no more).
const BOOTSTRAPPABLE_ALIASES: [&str; 3] = ["alpine-3.24", "ubuntu-26.04", "rocky-9"];

/// Sentinel the pushed script prints once it's done, followed by `:` and
/// its exit code — same shape as `packages::DONE_SENTINEL`, kept as its
/// own distinct string so a bootstrap's completion can never be confused
/// with an unrelated package action finishing on the same console.
const BOOTSTRAP_DONE_SENTINEL: &str = "FIRECRAB_BOOTSTRAP_DONE";

/// How long the guest-side bootstrap script may run before this module
/// gives up waiting — real network downloads (hundreds of MB) plus a real
/// package install, so far more generous than
/// `packages::PACKAGE_UPDATE_TIMEOUT`.
const BOOTSTRAP_SCRIPT_TIMEOUT: Duration = Duration::from_secs(1800);

const ALPINE_SCRIPT: &str =
    include_str!("../../../scripts/firecracker-menual/bootstrap-alpine-in-guest.sh");
const UBUNTU_SCRIPT: &str =
    include_str!("../../../scripts/firecracker-menual/bootstrap-ubuntu-in-guest.sh");
const ROCKY_SCRIPT: &str =
    include_str!("../../../scripts/firecracker-menual/bootstrap-rocky-in-guest.sh");

fn script_for(alias: &str) -> &'static str {
    match alias {
        "alpine-3.24" => ALPINE_SCRIPT,
        "ubuntu-26.04" => UBUNTU_SCRIPT,
        "rocky-9" => ROCKY_SCRIPT,
        other => unreachable!("start_bootstrap already rejected unknown alias {other}"),
    }
}

/// Alpine and Ubuntu bootstrap by chrooting into a freshly-downloaded base
/// that carries its own package manager, so any installed template can
/// serve as the outer builder environment. Rocky's bootstrap needs `dnf`
/// already present in the *outer* guest (see
/// `scripts/firecracker-menual/bootstrap-rocky-in-guest.sh`'s doc comment),
/// so its own builder VM must itself already be `rocky-9`.
fn requires_matching_source(target_alias: &str) -> bool {
    target_alias == "rocky-9"
}

/// `POST /api/images/{alias}/bootstrap` — boots a builder VM off any
/// already-installed template and registers a new bootstrap session for
/// `alias`. Returns immediately, same convention as `start_build`; the
/// caller polls `GET /api/images/bootstrap/{bootstrapId}`.
pub async fn start_bootstrap(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(alias): Path<String>,
) -> Result<(StatusCode, Json<BootstrapResponse>), AppError> {
    if !BOOTSTRAPPABLE_ALIASES.contains(&alias.as_str()) {
        return Err(AppError::not_found(request_id.0));
    }

    if state.bootstraps.any_active() {
        return Err(AppError::conflict(
            "bootstrap_in_progress",
            "a bootstrap is already running; wait for it to finish before starting another",
            request_id.0,
        ));
    }

    let source_alias = pick_builder_source(&state, &alias, request_id.0)?;
    let source = state
        .templates
        .resolve_alias(&source_alias)
        .ok_or_else(|| AppError::internal(request_id.0))?;

    let micro_network_id = builder_micro_network_id(&state, request_id.0).await?;

    let create_request = CreateVmRequest {
        name: builder_vm_name(&format!("bootstrap-{alias}")),
        template: source_alias.clone(),
        ram: 1024,
        cpu: 1,
        disk_gb: bootstrap_disk_gb(&alias),
        egress_policy: EgressPolicy::Internet,
        micro_network_id,
        storage_root: None,
    };
    let _ = source; // only needed to confirm the source alias actually resolves

    let (_status, Json(created)) = create_vm(
        State(state.clone()),
        Extension(request_id),
        ValidatedJson(create_request),
    )
    .await?;

    mark_as_builder(&state, created.id, request_id.0).await?;

    let _vm_response = start_vm_request(
        State(state.clone()),
        Extension(request_id),
        Path(created.id.to_string()),
    )
    .await?;

    let bootstrap_id = state.bootstraps.begin(&alias, &source_alias, created.id);

    let state_for_watch = state.clone();
    tokio::spawn(async move {
        watch_bootstrap_boot(&state_for_watch, bootstrap_id, created.id).await;
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(state.bootstraps.get(bootstrap_id).expect("just inserted")),
    ))
}

/// Every installed rootfs's disk floor plus generous headroom for: the
/// downloaded official base archive, the fully-installed staging tree, and
/// the final `mkfs.ext4`'d image sitting alongside it before it's dumped
/// out — all three exist on the builder VM's own disk at once mid-build.
/// Sized per target rather than derived from the source template, since the
/// source is just a disposable outer environment unrelated to how big the
/// target ends up.
fn bootstrap_disk_gb(target_alias: &str) -> u16 {
    match target_alias {
        "alpine-3.24" => 4,
        _ => 8, // ubuntu-26.04, rocky-9 — 2G rootfs_size each, per default_specs()
    }
}

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

/// Polls the builder VM's lifecycle state until it reaches `Running`
/// (session becomes `Running`... — see note below), a terminal failure, or
/// [`BUILDER_BOOT_TIMEOUT`] elapses. Mirrors
/// `handlers::builds::watch_builder_boot` exactly (same CAS-against-`Booting`
/// safety reasoning), adapted to `BootstrapStatus`'s own states — note this
/// module's `Running` means "VM up, bootstrap script executing", not
/// `BuildStatus::Ready`'s "VM up, waiting for a command" — because a
/// bootstrap session has no separate `Ready`-then-command step: the whole
/// script is dispatched as soon as the VM is usable (Task 7).
pub(crate) async fn watch_bootstrap_boot(state: &AppState, bootstrap_id: Uuid, vm_id: Uuid) {
    let deadline = tokio::time::Instant::now() + BUILDER_BOOT_TIMEOUT;
    loop {
        match state.bootstraps.get(bootstrap_id) {
            Some(session) if session.status == BootstrapStatus::Booting => {}
            _ => return,
        }

        let vm_state = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&vm_id)
            .map(|vm| vm.state);

        match vm_state {
            Some(VmState::Running) => {
                state.bootstraps.append_log(
                    bootstrap_id,
                    "builder VM is running — starting bootstrap script",
                );
                if state.bootstraps.set_status_from(
                    bootstrap_id,
                    BootstrapStatus::Booting,
                    BootstrapStatus::Running,
                ) {
                    let state_for_script = state.clone();
                    tokio::spawn(async move {
                        run_bootstrap_script(&state_for_script, bootstrap_id, vm_id).await;
                    });
                }
                return;
            }
            Some(state_now @ (VmState::Error | VmState::Stopped)) => {
                state.bootstraps.finish_err_from(
                    bootstrap_id,
                    BootstrapStatus::Booting,
                    format!("builder VM {vm_id} failed to boot (state: {state_now:?})"),
                );
                return;
            }
            None => return,
            Some(_) => {}
        }

        if tokio::time::Instant::now() >= deadline {
            state.bootstraps.finish_err_from(
                bootstrap_id,
                BootstrapStatus::Booting,
                format!(
                    "builder VM {vm_id} did not reach running within {}s",
                    BUILDER_BOOT_TIMEOUT.as_secs()
                ),
            );
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Writes the guest-native bootstrap script for `state.bootstraps.get(id).alias`
/// to the builder VM's console as a single heredoc (so the whole script
/// lands as one shell invocation — no chunking or base64 needed, since
/// `write_input` writes raw bytes to the guest's stdin pipe and the
/// guest's own shell parses embedded newlines exactly the way it would
/// typed input, including multi-line constructs), waits for
/// [`BOOTSTRAP_DONE_SENTINEL`], and advances the session on success.
pub(crate) async fn run_bootstrap_script(state: &AppState, bootstrap_id: Uuid, vm_id: Uuid) {
    let Some(session) = state.bootstraps.get(bootstrap_id) else {
        return;
    };
    let script = script_for(&session.alias);

    let Some(process) = state
        .processes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&vm_id)
        .cloned()
    else {
        state.bootstraps.finish_err_from(
            bootstrap_id,
            BootstrapStatus::Running,
            "builder VM's console process is no longer available",
        );
        return;
    };

    let (_backlog, mut receiver) = process.console.subscribe();
    let heredoc = format!(
        "cat > /root/fc-bootstrap.sh <<'FIRECRAB_BOOTSTRAP_SCRIPT_EOF'\n{script}\nFIRECRAB_BOOTSTRAP_SCRIPT_EOF\nsh /root/fc-bootstrap.sh; echo \"{BOOTSTRAP_DONE_SENTINEL}:$?\"\n"
    );
    process.console.write_input(heredoc.as_bytes()).await;

    match super::packages::wait_for_completion_with_sentinel(
        &mut receiver,
        BOOTSTRAP_SCRIPT_TIMEOUT,
        BOOTSTRAP_DONE_SENTINEL,
    )
    .await
    {
        Ok((0, tail)) => {
            state.bootstraps.append_log(bootstrap_id, tail);
            if state.bootstraps.set_status_from(
                bootstrap_id,
                BootstrapStatus::Running,
                BootstrapStatus::Packaging,
            ) {
                let state_for_package = state.clone();
                tokio::spawn(async move {
                    package_bootstrap(&state_for_package, bootstrap_id, vm_id).await;
                });
            }
        }
        Ok((code, tail)) => {
            state.bootstraps.finish_err_from(
                bootstrap_id,
                BootstrapStatus::Running,
                format!("bootstrap script exited with code {code}\n{tail}"),
            );
        }
        Err(reason) => {
            state
                .bootstraps
                .finish_err_from(bootstrap_id, BootstrapStatus::Running, reason);
        }
    }
}

/// Dumps the finished rootfs/kernel/initrd out of the builder VM's disk,
/// converts the raw kernel to an uncompressed ELF vmlinux the same way
/// `install-{alpine,ubuntu,rocky}-rootfs.sh` always did on the host, packs
/// everything into `{alias}.tar.zst` at the exact layout
/// `templates.rs::default_specs()` expects, writes it to
/// `image_install::staged_package_path` — the unmodified "가져오기"
/// pipeline picks it up from there — then deletes the builder VM.
async fn package_bootstrap(state: &AppState, bootstrap_id: Uuid, vm_id: Uuid) {
    let Some(session) = state.bootstraps.get(bootstrap_id) else {
        return;
    };
    let Some(spec) = crate::templates::TemplateRegistry::known_spec(&session.alias) else {
        state
            .bootstraps
            .finish_err(bootstrap_id, format!("no known spec for {}", session.alias));
        return;
    };

    let result = package_bootstrap_inner(state, &session, &spec).await;

    let _ = super::vms::delete_vm(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(vm_id.to_string()),
    )
    .await;

    match result {
        Ok(()) => state.bootstraps.finish_ok(bootstrap_id),
        Err(reason) => state.bootstraps.finish_err(bootstrap_id, reason),
    }
}

async fn package_bootstrap_inner(
    state: &AppState,
    session: &BootstrapResponse,
    spec: &crate::templates::TemplateSpec,
) -> Result<(), String> {
    let vm_record = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&session.vm_id)
        .cloned()
        .ok_or_else(|| "builder VM record vanished before packaging".to_owned())?;
    let disk_generation = vm_record
        .disk_generation
        .ok_or_else(|| "builder VM has no disk generation to package from".to_owned())?;
    let artifact_paths = crate::artifacts::VmArtifactPaths::for_vm(
        &state.vms_dir_for(&vm_record.storage_root),
        session.vm_id,
    );
    let guest_disk = artifact_paths.rootfs(disk_generation);
    let alias = session.alias.clone();
    let spec = spec.clone();
    let image_root = state.templates.image_root_path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        build_package_blocking(&guest_disk, &alias, &spec, &image_root)
    })
    .await
    .map_err(|error| format!("packaging task panicked: {error}"))?
}

/// The synchronous half of packaging: dump files, convert the kernel,
/// stage them under a scratch directory in the right `kernel/`/`rootfs/`
/// layout, tar+zstd, then publish atomically into the staged package
/// cache (temp-file-then-rename, same discipline as
/// `image_install::download_to`).
fn build_package_blocking(
    guest_disk: &std::path::Path,
    alias: &str,
    spec: &crate::templates::TemplateSpec,
    image_root: &std::path::Path,
) -> Result<(), String> {
    let scratch = image_root
        .join(".packages")
        .join(format!(".{alias}-bootstrap-scratch"));
    let _ = std::fs::remove_dir_all(&scratch);
    let kernel_dir = scratch.join("kernel");
    let rootfs_dir = scratch.join("rootfs");
    std::fs::create_dir_all(&kernel_dir)
        .map_err(|e| format!("mkdir {}: {e}", kernel_dir.display()))?;
    std::fs::create_dir_all(&rootfs_dir)
        .map_err(|e| format!("mkdir {}: {e}", rootfs_dir.display()))?;

    let raw_rootfs = scratch.join("rootfs.raw.ext4");
    crate::rootfs::dump_from_image(
        guest_disk,
        "/root/fc-bootstrap/out/rootfs.ext4",
        &raw_rootfs,
    )
    .map_err(|e| format!("dump rootfs: {e}"))?;
    let rootfs_dest = scratch.join(&spec.rootfs); // spec.rootfs = "rootfs/<exact-filename>.ext4"
    std::fs::create_dir_all(rootfs_dest.parent().unwrap()).ok();
    std::fs::rename(&raw_rootfs, &rootfs_dest).map_err(|e| format!("place rootfs: {e}"))?;

    let raw_kernel_name = if alias == "alpine-3.24" {
        "vmlinuz-virt-raw"
    } else {
        "vmlinuz-raw"
    };
    let raw_kernel = scratch.join("kernel.raw");
    crate::rootfs::dump_from_image(
        guest_disk,
        &format!("/root/fc-bootstrap/out/{raw_kernel_name}"),
        &raw_kernel,
    )
    .map_err(|e| format!("dump kernel: {e}"))?;

    let kernel_dest = scratch.join(&spec.kernel); // e.g. "kernel/vmlinux-ubuntu-26.04-x86_64"
    std::fs::create_dir_all(kernel_dest.parent().unwrap()).ok();
    // Compile-time repo-relative path, not `std::env::current_dir()`-based:
    // the deployed `firecrab-api.service` unit pins `WorkingDirectory` to
    // `@DATADIR@` (see `packaging/systemd/firecrab-api.service`), which has
    // no `scripts/` subdirectory at all, so a cwd-relative join would fail
    // in production every time regardless of where the process happens to
    // be started from. `env!("CARGO_MANIFEST_DIR")` is the same convention
    // `templates.rs::load_default` already uses to locate `images/`
    // relative to this crate at compile time, and it resolves correctly
    // here too: `install.sh` always runs `cargo build --release` in place
    // inside the checked-out repo on the target host (see `install.sh`),
    // so the path baked in at compile time is exactly this host's own
    // checkout, independent of the service's runtime working directory.
    let extract_vmlinux = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../scripts/firecracker-menual/extract-vmlinux");
    let output = std::process::Command::new(&extract_vmlinux)
        .arg(&raw_kernel)
        .output()
        .map_err(|e| format!("run extract-vmlinux ({}): {e}", extract_vmlinux.display()))?;
    if !output.status.success() {
        return Err(format!(
            "extract-vmlinux failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    std::fs::write(&kernel_dest, &output.stdout).map_err(|e| format!("write kernel: {e}"))?;

    if let Some(initrd_relative) = &spec.initrd {
        let raw_initrd = scratch.join("initramfs.raw");
        crate::rootfs::dump_from_image(guest_disk, "/root/fc-bootstrap/out/initramfs", &raw_initrd)
            .map_err(|e| format!("dump initrd: {e}"))?;
        let initrd_dest = scratch.join(initrd_relative);
        std::fs::create_dir_all(initrd_dest.parent().unwrap()).ok();
        std::fs::rename(&raw_initrd, &initrd_dest).map_err(|e| format!("place initrd: {e}"))?;
    }

    let package_name = crate::image_install::package_name(alias);
    let staged = crate::image_install::staged_package_path(image_root, alias);
    let staging_temp = staged.with_file_name(format!(".{package_name}.building"));
    std::fs::create_dir_all(staged.parent().unwrap()).ok();

    let members: Vec<std::path::PathBuf> = if spec.initrd.is_some() {
        vec![
            spec.kernel.clone(),
            spec.initrd.clone().unwrap(),
            spec.rootfs.clone(),
        ]
    } else {
        vec![spec.kernel.clone(), spec.rootfs.clone()]
    };
    let mut tar = std::process::Command::new("tar")
        .arg("--sparse")
        .arg("-C")
        .arg(&scratch)
        .arg("-cf")
        .arg("-")
        .args(&members)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn tar: {e}"))?;
    let tar_stdout = tar.stdout.take().ok_or("tar stdout missing")?;
    let zstd = std::process::Command::new("zstd")
        .args(["-T0", "-19", "-f", "-o"])
        .arg(&staging_temp)
        .stdin(tar_stdout)
        .status()
        .map_err(|e| format!("run zstd: {e}"))?;
    let tar_status = tar.wait().map_err(|e| format!("tar wait: {e}"))?;
    // Both subprocesses must succeed: `!a.success() || !b.success()` is the
    // De Morgan form of "error unless both tar and zstd succeeded" — it
    // trips (and this function returns Err) if *either* one failed, not
    // only if both did. Do not simplify this to `&&`: that would silently
    // ignore a single failed subprocess (e.g. tar exiting non-zero while
    // zstd still emits a zero-length or truncated archive from a closed
    // pipe) and let a broken package get renamed into place as if it had
    // succeeded.
    if !tar_status.success() || !zstd.success() {
        let _ = std::fs::remove_file(&staging_temp);
        return Err(format!("packaging failed (tar {tar_status}, zstd {zstd})"));
    }

    std::fs::rename(&staging_temp, &staged).map_err(|e| format!("publish package: {e}"))?;
    let _ = std::fs::remove_dir_all(&scratch);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::response::IntoResponse;
    use tempfile::tempdir;
    use tokio::sync::watch;

    use super::*;
    use crate::console::ConsoleBroker;
    use crate::firecracker::VmProcess;
    use crate::handlers::vms::test_support::test_state;

    /// Registers a fake console+process for `id`, the same way
    /// `handlers::packages`'s and `handlers::builds`'s own tests do —
    /// `run_bootstrap_script` requires a live `VmProcess` to write the
    /// heredoc + sentinel-wait command to, and the test fixture never
    /// actually boots Firecracker.
    fn register_fake_process(state: &AppState, id: Uuid) -> Arc<ConsoleBroker> {
        let console = Arc::new(ConsoleBroker::new());
        let (_exited_tx, exited_rx) = watch::channel(false);
        state.processes.lock().unwrap().insert(
            id,
            VmProcess {
                pid: 0,
                exited: exited_rx,
                console: console.clone(),
            },
        );
        console
    }

    /// See `handlers::packages`'s identical helper: `run_bootstrap_script`
    /// returns as soon as it has *spawned*, which only then subscribes to
    /// the console — output pushed before that subscription would be lost.
    async fn wait_for_console_subscriber(console: &ConsoleBroker) {
        for _ in 0..200 {
            if console.subscriber_count() > 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("run_bootstrap_script never subscribed to the console");
    }

    /// `debugfs`'s own `write` command doesn't create parent directories
    /// (verified directly: writing into a path whose parent doesn't yet
    /// exist fails with "File not found by ext2_lookup"), so a test fixture
    /// that wants to seed `/root/fc-bootstrap/out/<file>` the way a real
    /// bootstrap script run leaves it behind must `mkdir` each path
    /// component first — the same thing `rootfs.rs`'s own
    /// `real_rootfs_with_guest_dirs` test helper does for its fixtures.
    /// `rootfs::run_debugfs` itself stays module-private, so this shells
    /// out directly rather than widening that too.
    fn mkdir_in_image(disk_path: &std::path::Path, guest_dir: &str) {
        let output = std::process::Command::new("debugfs")
            .arg("-w")
            .arg("-R")
            .arg(format!("mkdir {guest_dir}"))
            .arg(disk_path)
            .output()
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("File not found"),
            "mkdir {guest_dir} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Builds a small ext4 image at `disk_path` with `/root/fc-bootstrap/out`
    /// already present, then seeds it with `files` (guest-relative paths
    /// under that directory, e.g. `"rootfs.ext4"` -> content).
    fn seed_builder_output_disk(disk_path: &std::path::Path, files: &[(&str, &[u8])]) {
        std::process::Command::new("mkfs.ext4")
            .args(["-q", "-F"])
            .arg(disk_path)
            .arg("16M")
            .status()
            .unwrap();
        for dir in ["/root", "/root/fc-bootstrap", "/root/fc-bootstrap/out"] {
            mkdir_in_image(disk_path, dir);
        }
        for (name, content) in files {
            crate::rootfs::write_into_image(
                disk_path,
                &format!("/root/fc-bootstrap/out/{name}"),
                content,
            )
            .unwrap();
        }
    }

    #[tokio::test]
    async fn package_bootstrap_writes_a_tar_zst_the_install_pipeline_can_read() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = crate::handlers::vms::test_support::record("builder", Uuid::new_v4());
        crate::handlers::vms::test_support::seed_vm(&state, &vm);
        let generation = Uuid::new_v4();
        let artifact_paths =
            crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id);
        artifact_paths.ensure_directories().unwrap();
        let disk_path = artifact_paths.rootfs(generation);

        // Build a small ext4 image on this disk path containing the exact
        // guest-side layout package_bootstrap expects to find and dump out.
        // `ubuntu-26.04` has `initrd: None` in `default_specs()`, so only
        // `rootfs.ext4` and `vmlinuz-raw` (Ubuntu's raw-kernel dump
        // filename per `bootstrap-ubuntu-in-guest.sh`) need seeding here.
        seed_builder_output_disk(
            &disk_path,
            &[
                ("rootfs.ext4", b"fake ext4 rootfs bytes"),
                ("vmlinuz-raw", b"fake vmlinux elf bytes"),
            ],
        );
        {
            let mut vms = state.vms.lock().unwrap();
            let stored = vms.get_mut(&vm.id).unwrap();
            stored.disk_generation = Some(generation);
        }

        let bootstrap_id = state.bootstraps.begin("ubuntu-26.04", "alpine-3.24", vm.id);
        state
            .bootstraps
            .set_status(bootstrap_id, BootstrapStatus::Packaging);

        package_bootstrap(&state, bootstrap_id, vm.id).await;

        let snapshot = state.bootstraps.get(bootstrap_id).unwrap();
        assert_eq!(
            snapshot.status,
            BootstrapStatus::Succeeded,
            "log: {}",
            snapshot.log
        );
        let staged = crate::image_install::staged_package_path(
            state.templates.image_root_path(),
            "ubuntu-26.04",
        );
        assert!(staged.is_file());

        let listing = std::process::Command::new("tar")
            .arg("--use-compress-program=zstd")
            .arg("-tf")
            .arg(&staged)
            .output()
            .unwrap();
        assert!(listing.status.success());
        let members = String::from_utf8_lossy(&listing.stdout);
        assert!(members.contains("kernel/vmlinux-ubuntu-26.04-x86_64"));
        assert!(members.contains("rootfs/ubuntu-rootfs-26.04-amd64.ext4"));
    }

    /// Covers the `if let Some(initrd_relative) = &spec.initrd` branch in
    /// `build_package_blocking` that the `ubuntu-26.04` test above never
    /// exercises (Ubuntu's spec has `initrd: None`) — `alpine-3.24` does
    /// carry an initrd, and its raw-kernel dump filename
    /// (`vmlinuz-virt-raw`, per `bootstrap-alpine-in-guest.sh`) differs
    /// from every other alias's `vmlinuz-raw`, which `build_package_blocking`
    /// branches on explicitly.
    #[tokio::test]
    async fn package_bootstrap_includes_the_initrd_when_the_spec_has_one() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = crate::handlers::vms::test_support::record("builder", Uuid::new_v4());
        crate::handlers::vms::test_support::seed_vm(&state, &vm);
        let generation = Uuid::new_v4();
        let artifact_paths =
            crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id);
        artifact_paths.ensure_directories().unwrap();
        let disk_path = artifact_paths.rootfs(generation);

        seed_builder_output_disk(
            &disk_path,
            &[
                ("rootfs.ext4", b"fake alpine rootfs bytes"),
                ("vmlinuz-virt-raw", b"fake vmlinux elf bytes"),
                ("initramfs", b"fake initramfs bytes"),
            ],
        );
        {
            let mut vms = state.vms.lock().unwrap();
            let stored = vms.get_mut(&vm.id).unwrap();
            stored.disk_generation = Some(generation);
        }

        let bootstrap_id = state.bootstraps.begin("alpine-3.24", "ubuntu-26.04", vm.id);
        state
            .bootstraps
            .set_status(bootstrap_id, BootstrapStatus::Packaging);

        package_bootstrap(&state, bootstrap_id, vm.id).await;

        let snapshot = state.bootstraps.get(bootstrap_id).unwrap();
        assert_eq!(
            snapshot.status,
            BootstrapStatus::Succeeded,
            "log: {}",
            snapshot.log
        );
        let staged = crate::image_install::staged_package_path(
            state.templates.image_root_path(),
            "alpine-3.24",
        );
        let listing = std::process::Command::new("tar")
            .arg("--use-compress-program=zstd")
            .arg("-tf")
            .arg(&staged)
            .output()
            .unwrap();
        assert!(listing.status.success());
        let members = String::from_utf8_lossy(&listing.stdout);
        assert!(members.contains("kernel/vmlinux-alpine-virt-x86_64"));
        assert!(members.contains("kernel/initramfs-alpine-virt-x86_64"));
        assert!(members.contains("rootfs/alpine-rootfs-3.24.1-x86_64.ext4"));
    }

    #[tokio::test]
    async fn start_bootstrap_rejects_an_unknown_target_alias() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let error = start_bootstrap(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path("no-such-alias".to_owned()),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn start_bootstrap_rejects_when_no_matching_source_is_installed() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);

        let error = start_bootstrap(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path("rocky-9".to_owned()),
        )
        .await
        .unwrap_err();

        assert_eq!(
            error.into_response().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn run_bootstrap_script_records_the_console_output_and_reaches_running_terminal_wait() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = crate::handlers::vms::test_support::record("builder", Uuid::new_v4());
        crate::handlers::vms::test_support::seed_vm(&state, &vm);
        let console = register_fake_process(&state, vm.id);
        let bootstrap_id = state.bootstraps.begin("ubuntu-26.04", "alpine-3.24", vm.id);
        state
            .bootstraps
            .set_status(bootstrap_id, BootstrapStatus::Running);

        let vm_id = vm.id;
        let push_sentinel = async {
            wait_for_console_subscriber(&console).await;
            console.push_output(format!("{}:0\n", BOOTSTRAP_DONE_SENTINEL).as_bytes());
        };
        // Driven with `join!` on this same task rather than a separate
        // `tokio::spawn` + `.await`: `run_bootstrap_script`'s own success
        // path spawns `package_bootstrap` (Task 8's stub, which fails the
        // session immediately — see its doc comment) as its very last step,
        // right after moving the session to `Packaging`. Spawning
        // `run_bootstrap_script` itself as a separate task would let the
        // runtime schedule that spawned successor ahead of this test's
        // resumption, so the assertion below could observe `Failed`
        // instead — a race that's real today only because the stub
        // finishes instantly; Task 8's real implementation will take
        // actual wall-clock time. Running everything on one task means the
        // assertion executes synchronously in the same poll cycle
        // `run_bootstrap_script` completes in, before the runtime gets a
        // chance to run that spawned successor.
        tokio::join!(
            run_bootstrap_script(&state, bootstrap_id, vm_id),
            push_sentinel
        );

        let snapshot = state.bootstraps.get(bootstrap_id).unwrap();
        assert_eq!(snapshot.status, BootstrapStatus::Packaging);
        assert!(snapshot.log.contains(BOOTSTRAP_DONE_SENTINEL));
    }
}
