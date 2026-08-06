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
                state.bootstraps.set_status_from(
                    bootstrap_id,
                    BootstrapStatus::Booting,
                    BootstrapStatus::Running,
                );
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

#[cfg(test)]
mod tests {
    use axum::response::IntoResponse;
    use tempfile::tempdir;

    use super::*;
    use crate::handlers::vms::test_support::test_state;

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
}
