//! Preview bindings reuse Pipe's saved credentials and independent address updater.
use crate::{
    ai_sandbox::{self, access::Caller},
    error::AppError,
    extract::ValidatedJson,
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use firecrab_api_types::*;
use firecrab_helper_protocol::{
    firecrab_tunnel::{self, Target},
    pipe::{Operation, QuickTunnelRequest, ServiceAction, Snapshot},
};
use sha2::{Digest, Sha256};
use std::{net::SocketAddr, time::Duration};
use tokio::sync::Mutex;
use uuid::Uuid;

// Keep durable claims and helper mutations ordered, including unknown HTTP outcomes.
static MUTATIONS: Mutex<()> = Mutex::const_new(());

fn failure(id: RequestId, code: &'static str) -> AppError {
    AppError::new(
        StatusCode::CONFLICT,
        code,
        "Preview binding needs inspection; saved credentials and unrelated connections are preserved",
        id.0,
    )
}
fn denied(id: RequestId) -> AppError {
    AppError::new(
        StatusCode::FORBIDDEN,
        "sandbox_permission_denied",
        "Sandbox project or resource permission denied",
        id.0,
    )
}
fn store_error<E>(id: RequestId) -> impl FnOnce(E) -> AppError {
    move |_| AppError::internal(id.0)
}
fn persist(
    state: &AppState,
    id: RequestId,
    binding: &mut SandboxTunnelBinding,
) -> Result<(), AppError> {
    binding.updated_at = ai_sandbox::now().max(binding.updated_at.saturating_add(1));
    state
        .store
        .save_sandbox_tunnel(binding)
        .map_err(store_error(id))
}
fn authorize(
    state: &AppState,
    id: RequestId,
    caller: Caller,
    binding: &SandboxTunnelBinding,
    write: bool,
) -> Result<(), AppError> {
    if write {
        caller.operator().map_err(|_| denied(id))?;
    }
    let permission = if write {
        SandboxPermission::Execute
    } else {
        SandboxPermission::Read
    };
    caller
        .project(state, binding.project_id, permission)
        .map_err(|_| denied(id))?;
    let saved = state
        .store
        .sandbox_task(binding.task_id)
        .map_err(store_error(id))?
        .ok_or_else(|| AppError::not_found(id.0))?;
    caller
        .task(state, &saved, permission)
        .map_err(|_| denied(id))?;
    if saved.task.project_id != binding.project_id
        || saved.task.attempt_id != binding.attempt_id
        || saved.task.runtime != Some(binding.runtime)
        || saved.caller.principal != binding.owner
    {
        return Err(failure(id, "sandbox_preview_reference_changed"));
    }
    Ok(())
}
fn load(
    state: &AppState,
    id: RequestId,
    caller: Caller,
    key: Uuid,
    write: bool,
) -> Result<SandboxTunnelBinding, AppError> {
    let binding = state
        .store
        .sandbox_tunnel(key)
        .map_err(store_error(id))?
        .ok_or_else(|| AppError::not_found(id.0))?;
    authorize(state, id, caller, &binding, write)?;
    Ok(binding)
}
async fn inspect(state: &AppState, id: RequestId) -> Result<Snapshot, AppError> {
    Ok(super::pipe::call(&state.network, Operation::Inspect, id)
        .await?
        .0)
}
fn resolved(target: &SandboxTunnelResolvedTarget) -> Target {
    match target {
        SandboxTunnelResolvedTarget::Apps { upstreams } => Target::Apps {
            upstreams: upstreams.clone(),
        },
        SandboxTunnelResolvedTarget::Nginx { host } => Target::Nginx { host: host.clone() },
    }
}
fn address<'a>(
    snapshot: &'a Snapshot,
    binding: &SandboxTunnelBinding,
) -> Option<&'a firecrab_tunnel::Address> {
    snapshot
        .firecrab_tunnels
        .iter()
        .find(|a| a.subdomain == binding.spec.subdomain)
}
/// A concurrent Pipe edit must be reviewed before a binding can stop or delete it.
fn require_applied(
    snapshot: &Snapshot,
    binding: &SandboxTunnelBinding,
    id: RequestId,
) -> Result<(), AppError> {
    let current =
        address(snapshot, binding).ok_or_else(|| failure(id, "sandbox_preview_address_missing"))?;
    if binding.connector_id != Some(current.connector_id)
        || binding
            .applied_target
            .as_ref()
            .is_none_or(|target| resolved(target) != current.target)
        || snapshot.firecrab_tunnels.iter().any(|peer| {
            peer.subdomain != current.subdomain && peer.connector_id == current.connector_id
        })
    {
        return Err(failure(id, "sandbox_preview_connection_changed"));
    }
    Ok(())
}
fn target_addresses(
    state: &AppState,
    id: RequestId,
    binding: &SandboxTunnelBinding,
    snapshot: &Snapshot,
) -> Result<(SandboxTunnelResolvedTarget, Vec<SocketAddr>), AppError> {
    if ai_sandbox::guest::binding(state, binding.runtime.vm_id).ok() != Some(binding.runtime) {
        return Err(failure(id, "sandbox_preview_runtime_changed"));
    }
    let lease = state
        .store
        .active_lease(binding.runtime.vm_id)
        .map_err(store_error(id))?
        .ok_or_else(|| failure(id, "sandbox_preview_no_address"))?;
    match &binding.spec.target {
        SandboxTunnelTarget::VmApp { runtime, port }
            if *runtime == binding.runtime && *port != 0 =>
        {
            let upstreams = vec![SocketAddr::new(lease.ipv4.into(), *port)];
            Ok((
                SandboxTunnelResolvedTarget::Apps {
                    upstreams: upstreams.clone(),
                },
                upstreams,
            ))
        }
        SandboxTunnelTarget::Nginx { host } => {
            let routes: Vec<_> = snapshot
                .config
                .iter()
                .flat_map(|c| &c.routes)
                .filter(|r| r.host == *host && r.network.is_none())
                .collect();
            let mut upstreams: Vec<_> = routes.iter().flat_map(|r| r.upstreams.clone()).collect();
            upstreams.sort();
            upstreams.dedup();
            if upstreams.is_empty()
                || upstreams
                    .iter()
                    .any(|a| a.ip() != std::net::IpAddr::V4(lease.ipv4))
            {
                return Err(failure(id, "sandbox_preview_route_not_owned"));
            }
            Ok((
                SandboxTunnelResolvedTarget::Nginx { host: host.clone() },
                upstreams,
            ))
        }
        _ => Err(failure(id, "sandbox_preview_invalid_target")),
    }
}
async fn ready(addresses: &[SocketAddr]) -> bool {
    for address in addresses {
        if !matches!(
            tokio::time::timeout(
                Duration::from_secs(2),
                tokio::net::TcpStream::connect(address)
            )
            .await,
            Ok(Ok(_))
        ) {
            return false;
        }
    }
    true
}
fn observe(binding: &mut SandboxTunnelBinding, snapshot: &Snapshot) {
    let Some(current) = address(snapshot, binding) else {
        binding.helper_connected = false;
        if binding.state == SandboxTunnelState::Deleting {
            binding.local_removed = true;
            binding.state = SandboxTunnelState::Deleted;
            binding.remote_retirement = SandboxTunnelRetirement::OperatorRequired;
        } else if binding.state != SandboxTunnelState::Deleted {
            binding.state = SandboxTunnelState::NeedsReconciliation;
            binding.error_code = Some("sandbox_preview_address_missing".into());
        }
        return;
    };
    let matches = binding
        .applied_target
        .as_ref()
        .is_some_and(|t| resolved(t) == current.target)
        && binding
            .connector_id
            .is_none_or(|id| id == current.connector_id);
    if !matches {
        binding.helper_connected = false;
        binding.state = SandboxTunnelState::NeedsReconciliation;
        binding.error_code = Some("sandbox_preview_connection_changed".into());
        return;
    }
    binding.connector_id = Some(current.connector_id);
    binding.public_url = Some(current.public_url.clone());
    binding.helper_connected = current.connected;
    let stopped = snapshot
        .quick_tunnels
        .iter()
        .find(|c| c.id == current.connector_id)
        .is_some_and(|c| c.unit.active_state == "inactive" && c.unit.main_pid == 0);
    if matches!(
        binding.state,
        SandboxTunnelState::Stopping | SandboxTunnelState::Stopped
    ) {
        binding.helper_connected = current.connected && !stopped;
        binding.state = if stopped {
            SandboxTunnelState::Stopped
        } else {
            SandboxTunnelState::Stopping
        };
        return;
    }
    if binding.state == SandboxTunnelState::Deleting {
        return;
    }
    binding.state = if current.connected && binding.app_ready {
        SandboxTunnelState::Connected
    } else {
        SandboxTunnelState::Connecting
    };
    binding.error_code = current
        .error
        .as_ref()
        .map(|_| "sandbox_preview_updater_pending".into());
}

/// Available saved addresses contain no registration tokens.
pub async fn addresses(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
) -> Result<Json<Vec<SandboxTunnelAddress>>, AppError> {
    caller.operator().map_err(|_| denied(id))?;
    let snapshot = inspect(&state, id).await?;
    let mut addresses = Vec::new();
    for address in snapshot.firecrab_tunnels {
        addresses.push(SandboxTunnelAddress {
            claimed_by: state
                .store
                .sandbox_tunnel_claim(&address.subdomain)
                .map_err(store_error(id))?,
            subdomain: address.subdomain,
            public_url: address.public_url,
            connected: address.connected,
        });
    }
    Ok(Json(addresses))
}
pub async fn list(
    State(state): State<AppState>,
    Path(project): Path<Uuid>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
) -> Result<Json<Vec<SandboxTunnelBinding>>, AppError> {
    caller
        .project(&state, project, SandboxPermission::Read)
        .map_err(|_| denied(id))?;
    let rows = state
        .store
        .sandbox_tunnels(project)
        .map_err(store_error(id))?;
    Ok(Json(
        rows.into_iter()
            .filter(|row| authorize(&state, id, caller, row, false).is_ok())
            .collect(),
    ))
}
pub async fn get(
    State(state): State<AppState>,
    Path(key): Path<Uuid>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
) -> Result<Json<SandboxTunnelBinding>, AppError> {
    Ok(Json(load(&state, id, caller, key, false)?))
}
/// Save an explicit task/address selection, then publish only after local app readiness.
pub async fn save(
    State(state): State<AppState>,
    Path((project, key)): Path<(Uuid, Uuid)>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    ValidatedJson(spec): ValidatedJson<SandboxTunnelSpec>,
) -> Result<Json<SandboxTunnelBinding>, AppError> {
    caller.operator().map_err(|_| denied(id))?;
    caller
        .project(&state, project, SandboxPermission::Execute)
        .map_err(|_| denied(id))?;
    if key.is_nil() || !firecrab_tunnel::subdomain(&spec.subdomain) {
        return Err(failure(id, "sandbox_preview_invalid_target"));
    }
    let _lock = MUTATIONS.lock().await;
    let mut binding =
        if let Some(mut old) = state.store.sandbox_tunnel(key).map_err(store_error(id))? {
            authorize(&state, id, caller, &old, true)?;
            if old.project_id != project
                || old.task_id != spec.task_id
                || old.spec.subdomain != spec.subdomain
                || matches!(
                    old.state,
                    SandboxTunnelState::Deleted | SandboxTunnelState::Deleting
                )
            {
                return Err(failure(id, "sandbox_preview_identity_conflict"));
            }
            if old.spec == spec {
                return Ok(Json(old));
            }
            let snapshot = inspect(&state, id).await?;
            if old.applied_target.is_some() {
                require_applied(&snapshot, &old, id)?;
            }
            old.spec = spec;
            old.public_https = SandboxTunnelVerification::Unverified;
            old.web_socket_handshake = SandboxTunnelVerification::Unverified;
            old
        } else {
            if state
                .store
                .sandbox_tunnel_claim(&spec.subdomain)
                .map_err(store_error(id))?
                .is_some()
            {
                return Err(failure(id, "sandbox_preview_address_claimed"));
            }
            let task = state
                .store
                .sandbox_task(spec.task_id)
                .map_err(store_error(id))?
                .ok_or_else(|| AppError::not_found(id.0))?;
            caller
                .task(&state, &task, SandboxPermission::Execute)
                .map_err(|_| denied(id))?;
            if task.task.project_id != project {
                return Err(failure(id, "sandbox_preview_identity_conflict"));
            }
            let runtime = task
                .task
                .runtime
                .ok_or_else(|| failure(id, "sandbox_preview_runtime_missing"))?;
            SandboxTunnelBinding {
                id: key,
                project_id: project,
                task_id: task.task.id,
                attempt_id: task.task.attempt_id,
                runtime,
                owner: task.caller.principal,
                spec,
                state: SandboxTunnelState::Waiting,
                app_ready: false,
                connector_id: None,
                applied_target: None,
                public_url: None,
                helper_connected: false,
                public_https: SandboxTunnelVerification::Unverified,
                web_socket_handshake: SandboxTunnelVerification::Unverified,
                public_shutdown: SandboxTunnelVerification::Unverified,
                remote_retirement: SandboxTunnelRetirement::NotRequested,
                local_removed: false,
                created_at: ai_sandbox::now(),
                updated_at: ai_sandbox::now(),
                error_code: None,
            }
        };
    if address(&inspect(&state, id).await?, &binding).is_none() {
        return Err(failure(id, "sandbox_preview_register_in_pipe"));
    }
    persist(&state, id, &mut binding)?;
    publish_binding(&state, id, &mut binding).await?;
    Ok(Json(binding))
}
async fn publish_binding(
    state: &AppState,
    id: RequestId,
    binding: &mut SandboxTunnelBinding,
) -> Result<(), AppError> {
    let _runtime = state.lock_vm(binding.runtime.vm_id).await;
    let snapshot = inspect(state, id).await?;
    if address(&snapshot, binding).is_none() {
        return Err(failure(id, "sandbox_preview_register_in_pipe"));
    }
    if binding.applied_target.is_some() {
        require_applied(&snapshot, binding, id)?;
    }
    let (target, addresses) = target_addresses(state, id, binding, &snapshot)?;
    binding.app_ready = ready(&addresses).await;
    if !binding.app_ready {
        binding.state = SandboxTunnelState::Waiting;
        binding.error_code = Some("sandbox_preview_app_not_ready".into());
        persist(state, id, binding)?;
        return Ok(());
    }
    binding.state = SandboxTunnelState::Connecting;
    binding.applied_target = Some(target.clone());
    binding.connector_id = None;
    binding.error_code = None;
    persist(state, id, binding)?;
    let response = super::pipe::call(
        &state.network,
        Operation::ManageFirecrabTunnel {
            request: firecrab_tunnel::Request::Save {
                subdomain: binding.spec.subdomain.clone(),
                target: resolved(&target),
            },
        },
        id,
    )
    .await;
    match response {
        Ok(Json(snapshot)) => observe(binding, &snapshot),
        Err(_) => {
            binding.state = SandboxTunnelState::NeedsReconciliation;
            binding.error_code = Some("sandbox_preview_publish_uncertain".into());
        }
    }
    persist(state, id, binding)
}
/// Explicit publish/resume uses saved credentials; it never registers a new token.
pub async fn publish(
    State(state): State<AppState>,
    Path(key): Path<Uuid>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
) -> Result<Json<SandboxTunnelBinding>, AppError> {
    let _lock = MUTATIONS.lock().await;
    let mut binding = load(&state, id, caller, key, true)?;
    if matches!(
        binding.state,
        SandboxTunnelState::Deleted | SandboxTunnelState::Deleting
    ) {
        return Err(failure(id, "sandbox_preview_deleted"));
    }
    publish_binding(&state, id, &mut binding).await?;
    Ok(Json(binding))
}
/// Reconnect checks persisted references and actual helper state without repeating publication.
pub async fn reconcile(
    State(state): State<AppState>,
    Path(key): Path<Uuid>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
) -> Result<Json<SandboxTunnelBinding>, AppError> {
    let _lock = MUTATIONS.lock().await;
    let mut binding = load(&state, id, caller, key, true)?;
    let snapshot = inspect(&state, id).await?;
    let mut runtime_changed = false;
    if !matches!(
        binding.state,
        SandboxTunnelState::Deleted | SandboxTunnelState::Deleting
    ) {
        match target_addresses(&state, id, &binding, &snapshot) {
            Ok((_, addresses)) => binding.app_ready = ready(&addresses).await,
            Err(_) => {
                binding.app_ready = false;
                runtime_changed = true;
            }
        }
    }
    observe(&mut binding, &snapshot);
    if runtime_changed {
        binding.state = SandboxTunnelState::NeedsReconciliation;
        binding.error_code = Some("sandbox_preview_runtime_changed".into());
    }
    persist(&state, id, &mut binding)?;
    Ok(Json(binding))
}
/// Stop only the binding's exclusive connector, retaining the fixed address and token.
pub async fn stop(
    State(state): State<AppState>,
    Path(key): Path<Uuid>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
) -> Result<Json<SandboxTunnelBinding>, AppError> {
    let _lock = MUTATIONS.lock().await;
    let mut binding = load(&state, id, caller, key, true)?;
    if binding.state == SandboxTunnelState::Stopped {
        return Ok(Json(binding));
    }
    if binding.applied_target.is_none() {
        binding.state = SandboxTunnelState::Stopped;
        persist(&state, id, &mut binding)?;
        return Ok(Json(binding));
    }
    let snapshot = inspect(&state, id).await?;
    require_applied(&snapshot, &binding, id)?;
    let connector = binding
        .connector_id
        .ok_or_else(|| failure(id, "sandbox_preview_connector_missing"))?;
    binding.state = SandboxTunnelState::Stopping;
    binding.helper_connected = false;
    binding.public_https = SandboxTunnelVerification::Unverified;
    binding.web_socket_handshake = SandboxTunnelVerification::Unverified;
    persist(&state, id, &mut binding)?;
    match super::pipe::call(
        &state.network,
        Operation::ManageQuickTunnel {
            request: QuickTunnelRequest::Control {
                id: connector,
                service_action: ServiceAction::Stop,
            },
        },
        id,
    )
    .await
    {
        Ok(Json(snapshot)) => observe(&mut binding, &snapshot),
        Err(_) => binding.error_code = Some("sandbox_preview_stop_uncertain".into()),
    }
    persist(&state, id, &mut binding)?;
    Ok(Json(binding))
}
/// Delete local configuration only; never claim dashboard token retirement or public shutdown.
pub async fn delete(
    State(state): State<AppState>,
    Path(key): Path<Uuid>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
) -> Result<Json<SandboxTunnelBinding>, AppError> {
    let _lock = MUTATIONS.lock().await;
    let mut binding = load(&state, id, caller, key, true)?;
    if binding.state == SandboxTunnelState::Deleted {
        return Ok(Json(binding));
    }
    if binding.applied_target.is_none() {
        binding.state = SandboxTunnelState::Deleted;
        binding.local_removed = true;
        persist(&state, id, &mut binding)?;
        return Ok(Json(binding));
    }
    let snapshot = inspect(&state, id).await?;
    require_applied(&snapshot, &binding, id)?;
    binding.state = SandboxTunnelState::Deleting;
    binding.remote_retirement = SandboxTunnelRetirement::OperatorRequired;
    binding.public_https = SandboxTunnelVerification::Unverified;
    binding.web_socket_handshake = SandboxTunnelVerification::Unverified;
    persist(&state, id, &mut binding)?;
    match super::pipe::call(
        &state.network,
        Operation::ManageFirecrabTunnel {
            request: firecrab_tunnel::Request::Delete {
                subdomain: binding.spec.subdomain.clone(),
            },
        },
        id,
    )
    .await
    {
        Ok(Json(snapshot)) => observe(&mut binding, &snapshot),
        Err(_) => binding.error_code = Some("sandbox_preview_delete_uncertain".into()),
    }
    persist(&state, id, &mut binding)?;
    Ok(Json(binding))
}

/// Verify actual TLS HTTP content or the WebSocket handshake. No fixture sets these flags.
pub async fn verify(
    State(state): State<AppState>,
    Path(key): Path<Uuid>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    ValidatedJson(request): ValidatedJson<SandboxTunnelVerify>,
) -> Result<Json<SandboxTunnelBinding>, AppError> {
    let _lock = MUTATIONS.lock().await;
    let mut binding = load(&state, id, caller, key, true)?;
    let snapshot = inspect(&state, id).await?;
    require_applied(&snapshot, &binding, id)?;
    if binding.state != SandboxTunnelState::Connected
        || !request.path.starts_with('/')
        || request.path.starts_with("//")
        || request.path.len() > 1024
        || request
            .path
            .bytes()
            .any(|b| !b.is_ascii_graphic() || matches!(b, b'\\' | b'#'))
    {
        return Err(failure(id, "sandbox_preview_invalid_verification"));
    }
    let url = format!(
        "https://{}.{}/{}",
        binding.spec.subdomain,
        firecrab_tunnel::DOMAIN,
        request.path.trim_start_matches('/')
    );
    let client = reqwest::Client::builder()
        .http1_only()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| AppError::internal(id.0))?;
    let mut check = client.get(url);
    if request.web_socket {
        check = check
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==");
    } else if request
        .body_sha256
        .as_ref()
        .is_none_or(|s| s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(failure(id, "sandbox_preview_body_digest_required"));
    }
    let result = async {
        let mut response = check.send().await.ok()?;
        if request.web_socket {
            return (response.status() == StatusCode::SWITCHING_PROTOCOLS
                && response
                    .headers()
                    .get("sec-websocket-accept")
                    .is_some_and(|h| h == "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="))
            .then_some(());
        }
        if !response.status().is_success() {
            return None;
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            if bytes.len() + chunk.len() > 65_536 {
                return None;
            }
            bytes.extend_from_slice(&chunk);
        }
        (format!("{:x}", Sha256::digest(&bytes))
            == request.body_sha256.as_ref()?.to_ascii_lowercase())
        .then_some(())
    }
    .await
    .is_some();
    let verification = if result {
        SandboxTunnelVerification::Verified
    } else {
        SandboxTunnelVerification::Unverified
    };
    if request.web_socket {
        binding.web_socket_handshake = verification;
    } else {
        binding.public_https = verification;
    }
    binding.error_code = (!result).then(|| "sandbox_preview_public_check_failed".into());
    persist(&state, id, &mut binding)?;
    Ok(Json(binding))
}

#[cfg(test)]
#[path = "sandbox_tunnels_tests.rs"]
mod tests;
