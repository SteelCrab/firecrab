//! Durable kernel HTTP admission and original-attempt recovery.
use super::{
    iac,
    stacks::{database, persistence_error},
};
use crate::{
    error::AppError,
    file_lock::FileLock,
    image_install::Architecture,
    kernel_lifecycle as runtime,
    oci::kernel,
    persistence::iac_kernels::{KernelIntent, KernelOutcome, KernelRequest},
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use firecrab_api_types::{ImageInstallStatus, KernelInstallResponse};
use firecrab_stack::{NativeOperation, NativeOperationState};
use uuid::Uuid;

fn runtime_error(rid: Uuid) -> AppError {
    AppError::unavailable("Inspect and recover the original kernel operation", rid)
}
fn snapshot(state: &AppState, intent: &KernelIntent) -> KernelInstallResponse {
    let version = &intent.definition().version;
    let mut value = crate::kernel_manager::snapshot(&state.kernel_installs, version);
    value.version = version.clone();
    value.status = match intent.operation.state {
        NativeOperationState::Running => ImageInstallStatus::Running,
        NativeOperationState::Succeeded => ImageInstallStatus::Succeeded,
        _ => ImageInstallStatus::Failed,
    };
    if value.log.is_empty() {
        value.log = match intent.operation.state {
            NativeOperationState::Running => "Original kernel operation is running",
            NativeOperationState::Succeeded => "Original kernel operation completed",
            NativeOperationState::NeedsRecovery => "Recover the original kernel operation",
            NativeOperationState::Failed => "Original kernel operation failed before changes",
        }
        .into();
    }
    value
}
/// Queue installation before slow source work and return the original operation UUID.
pub async fn install(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path(version): Path<String>,
    headers: HeaderMap,
) -> Response {
    route(state, rid, headers, KernelRequest::Install(version)).await
}
/// Remove only an unused kernel with its original private content and filesystem scope.
pub async fn delete(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path(version): Path<String>,
    headers: HeaderMap,
) -> Response {
    route(state, rid, headers, KernelRequest::Delete(version)).await
}
async fn route(
    state: AppState,
    rid: RequestId,
    headers: HeaderMap,
    request: KernelRequest,
) -> Response {
    let id = match iac::operation_id(&headers, rid.0) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let observed = state.clone();
    let result = admit(state, rid, headers, id, request).await;
    let mut response = match result {
        Ok(intent) => match intent.operation.state {
            NativeOperationState::Succeeded => {
                // Immutable acknowledgements remain readable after later removal or transfer.
                match observed.store.kernel_receipt(id) {
                    Ok(Some(_)) if intent.deleting() => StatusCode::NO_CONTENT.into_response(),
                    Ok(Some(_)) => Json(snapshot(&observed, &intent)).into_response(),
                    _ => AppError::internal(rid.0).into_response(),
                }
            }
            NativeOperationState::Running =>
                (StatusCode::ACCEPTED, Json(snapshot(&observed, &intent))).into_response(),
            NativeOperationState::NeedsRecovery => AppError::conflict(
                "kernel_recovery_required",
                "Recover the original kernel operation before changing its target",
                rid.0,
            ).into_response(),
            NativeOperationState::Failed => AppError::new(
                StatusCode::BAD_GATEWAY,
                "kernel_unstarted",
                "Kernel preparation failed before changes; correct its prerequisites and use a new operation ID",
                rid.0,
            ).into_response(),
        },
        Err(error) => error.into_response(),
    };
    response.headers_mut().insert(
        "x-firecrab-operation-id",
        HeaderValue::from_str(&id.to_string()).expect("UUID header"),
    );
    response
}
/// The admission task survives its HTTP waiter and resolves retries before mutable target reads.
async fn admit(
    state: AppState,
    rid: RequestId,
    headers: HeaderMap,
    id: Uuid,
    request: KernelRequest,
) -> Result<KernelIntent, AppError> {
    tokio::spawn(async move {
        if let Some(expected) = headers.get("x-firecrab-endpoint-id") {
            let expected = expected.to_str().ok().and_then(|v| Uuid::parse_str(v).ok());
            if expected
                != Some(
                    state
                        .store
                        .iac_endpoint_id()
                        .map_err(|_| AppError::internal(rid.0))?,
                )
            {
                return Err(persistence_error(
                    crate::persistence::PersistenceError::IacEndpointConflict,
                    rid.0,
                ));
            }
        }
        let store = state.store.clone();
        let retry = request.clone();
        if let Some(intent) = database(rid.0, move || store.kernel_retry(id, &retry)).await? {
            return Ok(intent);
        }
        let requested = match &request {
            KernelRequest::Install(version) | KernelRequest::Delete(version) => version,
        };
        let releases = state.kernel_installs.base_url();
        let version = if requested == "stable" && matches!(request, KernelRequest::Install(_)) {
            kernel::latest_stable(state.templates.image_root_path(), releases)
                .await
                .map_err(|_| runtime_error(rid.0))?
                .version
        } else {
            requested.clone()
        };
        if crate::kernel_manager::cache_path(&version).is_none() {
            return Err(AppError::not_found(rid.0));
        }
        let target =
            crate::persistence::iac_kernels::resource(Architecture::HOST.as_str(), &version);
        if !state
            .store
            .iac_public_write_allowed(&target)
            .map_err(|e| persistence_error(e, rid.0))?
        {
            return Err(iac::managed(rid.0));
        }
        let guard = runtime::runtime_lock(state.templates.image_root_path(), &version)
            .map_err(|_| runtime_error(rid.0))?;
        let definition = runtime::definition(&state.templates, version, releases.is_some())
            .await
            .map_err(|_| runtime_error(rid.0))?;
        if matches!(request, KernelRequest::Delete(_)) {
            if definition.expected_image_sha256.is_none() {
                return Err(AppError::conflict(
                    "not_installed",
                    "Kernel is not installed on this host",
                    rid.0,
                ));
            }
            unused(
                &state,
                &definition.version,
                definition.expected_image_sha256.as_deref(),
                rid.0,
            )?;
        }
        let store = state.store.clone();
        let (intent, dispatch) =
            database(rid.0, move || store.begin_kernel(id, request, definition)).await?;
        if dispatch {
            dispatch_work(state, intent.clone(), guard);
        }
        Ok(intent)
    })
    .await
    .map_err(|_| AppError::internal(rid.0))?
}
/// Original durable progress remains available after API restart, without re-running source work.
pub async fn get(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path(version): Path<String>,
) -> Result<Json<KernelInstallResponse>, AppError> {
    if version != "stable" && crate::kernel_manager::cache_path(&version).is_none() {
        return Err(AppError::not_found(rid.0));
    }
    let store = state.store.clone();
    let requested = version.clone();
    let latest = database(rid.0, move || {
        store.latest_kernel(&KernelRequest::Install(requested))
    })
    .await?;
    Ok(Json(match latest {
        Some(intent) => snapshot(&state, &intent),
        None => crate::kernel_manager::snapshot(&state.kernel_installs, &version),
    }))
}
fn unused(state: &AppState, version: &str, sha: Option<&str>, rid: Uuid) -> Result<(), AppError> {
    let relative =
        crate::kernel_manager::cache_path(version).ok_or_else(|| AppError::not_found(rid))?;
    let referenced = state.templates.references_kernel(&relative).map_err(|_| {
        AppError::unavailable(
            "Image dependencies are unavailable; original kernel was retained",
            rid,
        )
    })?;
    if referenced
        || state
            .store
            .kernel_in_use(sha.unwrap_or(""))
            .map_err(|_| AppError::internal(rid))?
    {
        return Err(AppError::conflict(
            "kernel_in_use",
            "An image or retained VM still references this kernel",
            rid,
        ));
    }
    Ok(())
}
fn dispatch_work(
    state: AppState,
    intent: KernelIntent,
    guard: FileLock,
) -> tokio::task::JoinHandle<Result<(), crate::oci::ResolveError>> {
    if !intent.deleting() {
        let tracker = state.kernel_installs.clone();
        let worker = runtime::spawn_install(
            state.store.clone(),
            (*state.templates).clone(),
            intent,
            guard,
            Some(tracker),
        );
        return tokio::spawn(async move {
            worker
                .await
                .map_err(|_| {
                    crate::oci::ResolveError::Malformed("Kernel supervisor interrupted".into())
                })?
                .map(|_| ())
        });
    }
    tokio::spawn(async move {
        let store = state.store.clone();
        let original = intent.clone();
        let worker = tokio::spawn(async move {
            let _guard = guard;
            let _authority = runtime::authorize_namespace(&_guard, &state.templates, &intent)?;
            remove(&state, &intent).await
        });
        let result = worker.await.unwrap_or_else(|_| {
            Err(crate::oci::ResolveError::Malformed(
                "Kernel remover interrupted".into(),
            ))
        });
        if result.is_err() {
            let _ = store.fail_kernel(&original);
        }
        result
    })
}
async fn remove(state: &AppState, intent: &KernelIntent) -> Result<(), crate::oci::ResolveError> {
    let fail =
        || crate::oci::ResolveError::Malformed("Original kernel removal is incomplete".into());
    intent
        .definition()
        .root
        .check(state.templates.image_root_path())
        .map_err(|_| fail())?;
    unused(
        state,
        &intent.definition().version,
        intent.definition().expected_image_sha256.as_deref(),
        intent.operation.operation_id,
    )
    .map_err(|_| fail())?;
    state.store.check_kernel(intent).map_err(|_| fail())?;
    let relative =
        crate::kernel_manager::cache_path(&intent.definition().version).ok_or_else(fail)?;
    let path = intent.definition().root.path().join(relative);
    if tokio::fs::try_exists(&path).await.map_err(|_| fail())? {
        let actual = kernel::inspect_kernel(&path, Architecture::HOST).await?;
        if intent.definition().expected_image_sha256.as_deref() != Some(actual.encoded()) {
            return Err(fail());
        }
    }
    state.store.kernel_effects(intent).map_err(|_| fail())?;
    for path in [
        &path,
        &path.with_added_extension("json"),
        &path.with_added_extension("config"),
    ] {
        intent
            .definition()
            .root
            .check(state.templates.image_root_path())
            .map_err(|_| fail())?;
        state.store.check_kernel(intent).map_err(|_| fail())?;
        if let Err(error) = tokio::fs::remove_file(path).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(fail());
        }
    }
    state
        .store
        .finish_kernel(
            intent,
            &KernelOutcome {
                version: intent.definition().version.clone(),
                architecture: Architecture::HOST.as_str().into(),
                installed: false,
                image_sha256: intent.definition().expected_image_sha256.clone(),
                source_sha256: None,
            },
        )
        .map_err(|_| fail())?;
    state.kernel_installs.clear(&intent.definition().version);
    Ok(())
}
/// Reuse the same operation, physical root, source pin and exact observed execution.
pub(crate) async fn recover(
    state: AppState,
    rid: RequestId,
    id: Uuid,
    expected_generation: u64,
) -> Result<NativeOperation, AppError> {
    tokio::spawn(async move {
        let store = state.store.clone();
        let original = database(rid.0, move || store.kernel_intent(id))
            .await?
            .ok_or_else(|| AppError::not_found(rid.0))?;
        original
            .definition()
            .root
            .check(state.templates.image_root_path())
            .map_err(|_| runtime_error(rid.0))?;
        let guard = runtime::runtime_lock(
            state.templates.image_root_path(),
            &original.definition().version,
        )
        .map_err(|_| runtime_error(rid.0))?;
        let store = state.store.clone();
        let (intent, dispatch) = database(rid.0, move || {
            store.recover_kernel_at(id, expected_generation)
        })
        .await?;
        if dispatch {
            let _ = dispatch_work(state.clone(), intent, guard).await;
        }
        database(rid.0, move || state.store.iac_native_operation(id))
            .await?
            .ok_or_else(|| AppError::internal(rid.0))
    })
    .await
    .map_err(|_| AppError::internal(rid.0))?
}
