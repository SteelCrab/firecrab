//! CRUD for desired service configuration; saving a spec does not deploy a VM.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use firecrab_api_types::{MicroServiceResponse, MicroServiceSpec};
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::handlers::vms::{parse_id, validate_vm_env};
use crate::oci::{ImageReference, ImageVersion};
use crate::persistence::PersistenceError;
use crate::pipe_auth::SavedAuth;
use crate::server::RequestId;
use crate::state::AppState;

/// List saved service configurations in name order.
pub async fn list(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
) -> Result<Json<Vec<MicroServiceResponse>>, AppError> {
    database(request_id.0, move || state.store.list_micro_services())
        .await
        .map(Json)
}

/// Read a configuration by UUID, returning 404 if it does not exist.
pub async fn get(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<MicroServiceResponse>, AppError> {
    let id = parse_id(&id, request_id.0)?;
    let service = database(request_id.0, move || state.store.micro_service(id)).await?;
    service
        .map(Json)
        .ok_or_else(|| AppError::not_found(request_id.0))
}

/// Validate and save a new configuration without starting any VMs.
pub async fn create(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    ValidatedJson(spec): ValidatedJson<MicroServiceSpec>,
) -> Result<(StatusCode, Json<MicroServiceResponse>), AppError> {
    create_with_id(state, request_id, spec, Uuid::new_v4()).await
}

/// Save through normal validation and locks with a journal-reserved identity.
pub(crate) async fn create_with_id(
    state: AppState,
    request_id: RequestId,
    spec: MicroServiceSpec,
    id: Uuid,
) -> Result<(StatusCode, Json<MicroServiceResponse>), AppError> {
    validate(&spec, request_id.0)?;
    let _claim = state.service_mutations.lock().await;
    require_image_auth(&state, &spec.image, request_id.0).await?;
    super::pipe_storage::validate_binding(&state, &spec, request_id.0).await?;
    crate::queues::validate_bindings(&state, &spec, request_id.0).await?;
    let _network = state.network_mutations.lock().await;
    crate::network_policy::validate_selection(&state, spec.security_group_id, None, request_id.0)?;
    let service = MicroServiceResponse { id, spec };
    let record = service.clone();
    database(request_id.0, move || {
        state.store.insert_micro_service(&record)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(service)))
}

/// Replace all configuration fields after confirming that no deployment owns VMs.
pub async fn update(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(spec): ValidatedJson<MicroServiceSpec>,
) -> Result<Json<MicroServiceResponse>, AppError> {
    let id = parse_id(&id, request_id.0)?;
    update_from_stack(state, request_id.0, id, spec, None).await
}

pub(crate) async fn update_from_stack(
    state: AppState,
    request_id: Uuid,
    id: Uuid,
    spec: MicroServiceSpec,
    operation: Option<Uuid>,
) -> Result<Json<MicroServiceResponse>, AppError> {
    let request_id = RequestId(request_id);
    validate(&spec, request_id.0)?;
    let _claim = state.service_mutations.lock().await;
    ensure_stack_writer(&state, id, operation, request_id.0).await?;
    crate::service_runtime::ensure_editable(&state, id, request_id.0).await?;
    require_image_auth(&state, &spec.image, request_id.0).await?;
    super::pipe_storage::validate_binding(&state, &spec, request_id.0).await?;
    crate::queues::validate_bindings(&state, &spec, request_id.0).await?;
    let _network = state.network_mutations.lock().await;
    crate::network_policy::validate_selection(&state, spec.security_group_id, None, request_id.0)?;
    let service = MicroServiceResponse { id, spec };
    let record = service.clone();
    let changed = database(request_id.0, move || {
        state.store.update_micro_service(&record)
    })
    .await?;
    if !changed {
        return Err(AppError::not_found(request_id.0));
    }
    Ok(Json(service))
}

/// Remove a service and its deployment record after all owned VMs are cleaned up.
pub async fn delete(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let id = parse_id(&id, request_id.0)?;
    let _claim = state.service_mutations.lock().await;
    ensure_stack_writer(&state, id, None, request_id.0).await?;
    crate::service_runtime::ensure_editable(&state, id, request_id.0).await?;
    if state
        .store
        .database_bindings()
        .map_err(|_| AppError::internal(request_id.0))?
        .iter()
        .any(|b| b.input.service_id == id)
        || state
            .store
            .database_triggers()
            .map_err(|_| AppError::internal(request_id.0))?
            .iter()
            .any(|t| t.input.service_id == id)
    {
        return Err(AppError::conflict(
            "database_service_in_use",
            "Remove database bindings and finish retained triggers before deleting this service",
            request_id.0,
        ));
    }
    if !database(request_id.0, move || state.store.delete_micro_service(id)).await? {
        return Err(AppError::not_found(request_id.0));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// HTTP request IDs never authorize a controller mutation. Only internal callers
/// can supply an operation, checked against the durable running service step.
pub(crate) async fn ensure_stack_writer(
    state: &AppState,
    service: Uuid,
    operation: Option<Uuid>,
    request_id: Uuid,
) -> Result<(), AppError> {
    let store = state.store.clone();
    if !database(request_id, move || {
        store.stack_service_mutation_allowed(service, operation)
    })
    .await?
    {
        return Err(AppError::conflict(
            "stack_source_managed",
            "service is controlled by its stack; use Git sync or explicitly release ownership before direct edits, deploy or stop",
            request_id,
        ));
    }
    Ok(())
}

/// Scope service images to the verified account, never to independent registry logins.
/// Saving validates ownership locally; deployment resolves the image with this account's credentials.
pub(crate) async fn require_image_auth(
    state: &AppState,
    image: &str,
    request_id: Uuid,
) -> Result<SavedAuth, AppError> {
    let invalid = || {
        let message = "must belong to an account connected in Pipe Auth; use that GitHub account's GHCR namespace or that AWS account's ECR registry and region";
        AppError::validation(
            BTreeMap::from([("image".into(), message.into())]),
            request_id,
        )
    };
    let reference = ImageReference::parse(image).map_err(|_| invalid())?;
    let auths = super::pipe_auth::saved(state, RequestId(request_id)).await?;
    auths
        .into_iter()
        .find(|auth| match auth {
            SavedAuth::Firecrab { .. } => false,
            SavedAuth::Github { username, .. } => {
                reference.registry == "ghcr.io"
                    && reference
                        .repository
                        .split_once('/')
                        .is_some_and(|(owner, _)| owner.eq_ignore_ascii_case(username))
            }
            SavedAuth::Aws { .. } => {
                auth.connection().registry.as_deref() == Some(reference.registry.as_str())
            }
        })
        .ok_or_else(invalid)
}

/// Keep SQLite work off the async executor and never expose DB details or env values.
pub(crate) async fn database<T: Send + 'static>(
    request_id: Uuid,
    operation: impl FnOnce() -> Result<T, PersistenceError> + Send + 'static,
) -> Result<T, AppError> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| AppError::internal(request_id))?
        .map_err(|error| match error {
            PersistenceError::DuplicateMicroServiceName { .. } => AppError::validation(
                BTreeMap::from([("name".to_owned(), "is already registered".to_owned())]),
                request_id,
            ),
            other => {
                tracing::error!(%request_id, %other, "MicroService database operation failed");
                AppError::internal(request_id)
            }
        })
}

/// Collect configuration errors, including resource bounds and environment conflicts.
pub(crate) fn validate(spec: &MicroServiceSpec, request_id: Uuid) -> Result<(), AppError> {
    let mut fields = BTreeMap::new();
    if !dns_label(&spec.name) {
        fields.insert("name".to_owned(), "must be 1-63 lowercase letters, digits or hyphens; start and end with a letter or digit".to_owned());
    }
    if !valid_image(&spec.image) {
        fields.insert(
            "image".to_owned(),
            "must be an OCI image reference with a valid tag or sha256 digest".to_owned(),
        );
    }
    if spec.port == 0 {
        fields.insert("port".to_owned(), "must be between 1 and 65535".to_owned());
    }
    if spec.health_path.len() > 2048
        || !spec.health_path.starts_with('/')
        || spec.health_path.starts_with("//")
        || spec.health_path.bytes().any(|b| {
            !b.is_ascii()
                || b.is_ascii_control()
                || b.is_ascii_whitespace()
                || matches!(b, b'?' | b'#' | b'\\')
        })
    {
        fields.insert(
            "healthPath".to_owned(),
            "must be an absolute HTTP path of at most 2048 bytes, without query or fragment"
                .to_owned(),
        );
    }
    if let Some(domain) = &spec.domain
        && !firecrab_helper_protocol::pipe::valid_route_hostname(domain)
    {
        fields.insert(
            "domain".to_owned(),
            "must be a lowercase DNS hostname without scheme, port or path; use null to clear"
                .to_owned(),
        );
    }
    if !(128..=32768).contains(&spec.ram) || !spec.ram.is_power_of_two() {
        fields.insert(
            "ram".to_owned(),
            "must be a power of two between 128 and 32768 MiB".to_owned(),
        );
    }
    if !(1..=32).contains(&spec.cpu) {
        fields.insert("cpu".to_owned(), "must be between 1 and 32".to_owned());
    }
    if !(1..=100).contains(&spec.replicas) {
        fields.insert(
            "replicas".to_owned(),
            "must be between 1 and 100".to_owned(),
        );
    }
    if !(1..=3600).contains(&spec.startup_timeout) {
        fields.insert(
            "startupTimeout".to_owned(),
            "must be between 1 and 3600 seconds".to_owned(),
        );
    }
    if spec.drain_timeout > 3600 {
        fields.insert(
            "drainTimeout".to_owned(),
            "must be between 0 and 3600 seconds".to_owned(),
        );
    }
    if let Some(error) = validate_vm_env(&spec.env) {
        fields.insert("env".to_owned(), error);
    }
    let keys = spec
        .secrets
        .keys()
        .map(|key| (key.clone(), String::new()))
        .collect();
    if let Some(error) = validate_vm_env(&keys) {
        fields.insert("secrets".to_owned(), error);
    }
    if spec.env.len() + spec.secrets.len() > 64 {
        fields.insert(
            "secrets".to_owned(),
            "env and secrets together allow at most 64 entries".to_owned(),
        );
    }
    if spec.secrets.values().any(Uuid::is_nil) {
        fields.insert(
            "secrets".to_owned(),
            "must reference non-nil PipeSecrets UUIDs".to_owned(),
        );
    }
    if spec.secrets.keys().any(|key| spec.env.contains_key(key)) {
        fields.insert(
            "secrets".to_owned(),
            "an environment name must not appear in both env and secrets".to_owned(),
        );
    }
    if !fields.is_empty() {
        return Err(AppError::validation(fields, request_id));
    }
    Ok(())
}

/// Accept a lowercase DNS label with alphanumeric ends and at most 63 bytes.
fn dns_label(label: &str) -> bool {
    let bytes = label.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 63
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// Validate a bounded OCI reference and the permitted tag or digest syntax.
fn valid_image(image: &str) -> bool {
    if image.len() > 2048
        || image
            .bytes()
            .any(|b| !b.is_ascii() || b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return false;
    }
    let Ok(reference) = ImageReference::parse(image) else {
        return false;
    };
    match reference.version {
        ImageVersion::Digest(_) => true,
        ImageVersion::Tag(tag) => {
            let bytes = tag.as_bytes();
            !bytes.is_empty()
                && bytes.len() <= 128
                && (bytes[0].is_ascii_alphanumeric() || bytes[0] == b'_')
                && bytes
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
        }
    }
}

#[cfg(test)]
mod tests;
