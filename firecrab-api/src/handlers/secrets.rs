//! Secrets web-console API. Listing and transfer previews contain metadata only.

use std::collections::{BTreeMap, BTreeSet};

use axum::Json;
use axum::extract::{Extension, Path, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::handlers::vms::parse_id;
use crate::secrets::{
    self, Change, ChangeSummary, Connection, ConnectionSummary, Contents, Direction, Plan,
    PlannedValue, Provider, Secret, SecretError, SecretSource, SecretSummary, Settings, Vault,
    now_ms,
};
use crate::server::RequestId;
use crate::state::AppState;

/// Secret creation or update; omitting the value preserves it during an update.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SecretInput {
    /// Unique local name, also used when matching external values.
    name: String,
    /// User description; an omitted description defaults to empty.
    #[serde(default)]
    description: String,
    /// Required for creation; an explicit empty string stores an empty value.
    value: Option<String>,
    /// Expected current version for an update; omitted during creation.
    version: Option<u64>,
}

/// Connection creation or update with credentials supplied separately from settings.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ConnectionInput {
    /// Unique connection name displayed in the console and import history.
    name: String,
    /// External adapter; changing it requires creating a new connection.
    provider: Provider,
    /// Non-credential endpoint and scope configuration.
    settings: Settings,
    /// Supplied credential keys; omitted keys retain their stored values on update.
    #[serde(default)]
    credentials: BTreeMap<String, String>,
    /// Expected current version for an update; omitted during creation.
    version: Option<u64>,
}

/// Request to capture an external snapshot and compare it with local values.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PreviewInput {
    /// Whether the captured values will be imported or uploaded.
    direction: Direction,
    /// Selected local secret IDs for uploads; ignored for imports.
    #[serde(default)]
    secret_ids: Vec<Uuid>,
}

/// Changed names explicitly selected from a previously captured preview.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApplyInput {
    /// Names to apply; empty selections and unchanged rows are rejected.
    names: Vec<String>,
}

/// Public preview response with names and change types, never secret values.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Preview {
    /// Identifier used by the single-use apply endpoint.
    id: Uuid,
    /// External connection used to capture this snapshot.
    connection_id: Uuid,
    /// Transfer direction shown in the console.
    direction: Direction,
    /// Ten-minute expiry deadline in Unix milliseconds.
    expires_at: u64,
    /// Addition, replacement, or unchanged result for each captured name.
    changes: Vec<ChangeSummary>,
}

/// Prevents caching of successful and failed Secrets responses.
pub(crate) async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

/// Maps vault errors to safe HTTP responses without echoing values or provider bodies.
fn api_error(error: SecretError, id: Uuid) -> AppError {
    match error {
        SecretError::NotFound => AppError::not_found(id),
        SecretError::Duplicate => AppError::validation(
            BTreeMap::from([("name".into(), "is already registered".into())]),
            id,
        ),
        SecretError::Invalid(message) => {
            AppError::validation(BTreeMap::from([("secrets".into(), message.into())]), id)
        }
        SecretError::Conflict => AppError::conflict(
            "secrets_changed",
            "The values changed. Refresh or preview the transfer again.",
            id,
        ),
        SecretError::Expired => AppError::conflict(
            "preview_expired",
            "The preview expired. Preview the transfer again.",
            id,
        ),
        SecretError::Provider => AppError::unavailable(
            "External service request failed. Check the connection and its permissions.",
            id,
        ),
        SecretError::Storage | SecretError::Encryption => AppError::unavailable(
            "PipeSecrets storage or its encryption key is unavailable.",
            id,
        ),
    }
}

/// Runs blocking file-lock and encryption work outside the async executor.
async fn vault<T: Send + 'static>(
    state: &AppState,
    id: Uuid,
    operation: impl FnOnce(&Vault) -> Result<T, SecretError> + Send + 'static,
) -> Result<T, AppError> {
    let store = state.secrets.clone();
    tokio::task::spawn_blocking(move || operation(&store))
        .await
        .map_err(|_| AppError::internal(id))?
        .map_err(|error| api_error(error, id))
}

/// Lists local secret metadata sorted by name, without revealing values.
pub(crate) async fn list(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
) -> Result<Json<Vec<SecretSummary>>, AppError> {
    vault(&state, id.0, |store| {
        store.read(|contents| {
            let mut secrets: Vec<_> = contents
                .secrets
                .iter()
                .map(|secret| secret.summary.clone())
                .collect();
            secrets.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(secrets)
        })
    })
    .await
    .map(Json)
}

/// Checks names and byte limits before an input reaches the encrypted store.
fn validate_secret(input: &SecretInput) -> Result<(), SecretError> {
    secrets::validate_name(&input.name)?;
    if input.description.len() > 1000 {
        return Err(SecretError::Invalid(
            "description must be at most 1000 bytes",
        ));
    }
    if let Some(value) = &input.value {
        secrets::validate_value(value)?;
    }
    Ok(())
}

/// Creates a unique local secret and returns only its metadata.
pub(crate) async fn create(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    ValidatedJson(input): ValidatedJson<SecretInput>,
) -> Result<(StatusCode, Json<SecretSummary>), AppError> {
    validate_secret(&input).map_err(|error| api_error(error, id.0))?;
    let value = input
        .value
        .ok_or_else(|| api_error(SecretError::Invalid("value is required"), id.0))?;
    let summary = vault(&state, id.0, move |store| {
        store.write(|contents| {
            if contents.secrets.len() >= secrets::MAX_SECRETS {
                return Err(SecretError::Invalid(
                    "PipeSecrets supports at most 1000 secrets",
                ));
            }
            if contents
                .secrets
                .iter()
                .any(|secret| secret.summary.name == input.name)
            {
                return Err(SecretError::Duplicate);
            }
            let summary = SecretSummary {
                id: Uuid::new_v4(),
                name: input.name,
                description: input.description,
                version: 1,
                created_at: now_ms(),
                updated_at: now_ms(),
                source: None,
            };
            contents.secrets.push(Secret {
                summary: summary.clone(),
                value,
            });
            Ok(summary)
        })
    })
    .await?;
    Ok((StatusCode::CREATED, Json(summary)))
}

/// Updates metadata and an optional value only when the supplied version matches.
pub(crate) async fn update(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(input): ValidatedJson<SecretInput>,
) -> Result<Json<SecretSummary>, AppError> {
    let id = parse_id(&id, request.0)?;
    validate_secret(&input).map_err(|error| api_error(error, request.0))?;
    vault(&state, request.0, move |store| {
        store.write(|contents| {
            if contents
                .secrets
                .iter()
                .any(|secret| secret.summary.name == input.name && secret.summary.id != id)
            {
                return Err(SecretError::Duplicate);
            }
            let secret = contents
                .secrets
                .iter_mut()
                .find(|secret| secret.summary.id == id)
                .ok_or(SecretError::NotFound)?;
            if input.version != Some(secret.summary.version) {
                return Err(SecretError::Conflict);
            }
            secret.summary.name = input.name;
            secret.summary.description = input.description;
            secret.summary.version += 1;
            secret.summary.updated_at = now_ms();
            if let Some(value) = input.value {
                secret.value = value;
            }
            Ok(secret.summary.clone())
        })
    })
    .await
    .map(Json)
}

/// Reveals one value and its current version after an explicit POST request.
pub(crate) async fn reveal(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let id = parse_id(&id, request.0)?;
    vault(&state, request.0, move |store| {
        store.read(|contents| {
            let secret = contents
                .secrets
                .iter()
                .find(|secret| secret.summary.id == id)
                .ok_or(SecretError::NotFound)?;
            Ok(json!({"value":secret.value,"version":secret.summary.version}))
        })
    })
    .await
    .map(Json)
}

/// Deletes a local secret unless a MicroService still references its identifier.
pub(crate) async fn delete(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let id = parse_id(&id, request.0)?;
    let _claim = state.service_mutations.lock().await;
    let store = state.store.clone();
    let services =
        super::micro_services::database(request.0, move || store.list_micro_services()).await?;
    if services
        .iter()
        .any(|service| service.spec.secrets.values().any(|value| *value == id))
    {
        return Err(AppError::conflict(
            "secret_in_use",
            "Remove this secret from its services before deleting it.",
            request.0,
        ));
    }
    vault(&state, request.0, move |store| {
        store.write(|contents| {
            let index = contents
                .secrets
                .iter()
                .position(|secret| secret.summary.id == id)
                .ok_or(SecretError::NotFound)?;
            contents.secrets.remove(index);
            Ok(json!({"deleted":id}))
        })
    })
    .await
    .map(Json)
}

/// Lists external connection metadata without credentials, sorted by name.
pub(crate) async fn connections(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
) -> Result<Json<Vec<ConnectionSummary>>, AppError> {
    vault(&state, id.0, |store| {
        store.read(|contents| {
            let mut records: Vec<_> = contents
                .connections
                .iter()
                .map(|connection| connection.summary.clone())
                .collect();
            records.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(records)
        })
    })
    .await
    .map(Json)
}

/// Validates and encrypts a new external connection before returning its metadata.
pub(crate) async fn create_connection(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    ValidatedJson(input): ValidatedJson<ConnectionInput>,
) -> Result<(StatusCode, Json<ConnectionSummary>), AppError> {
    let connection = Connection {
        summary: ConnectionSummary {
            id: Uuid::new_v4(),
            name: input.name,
            provider: input.provider,
            settings: input.settings,
            version: 1,
            updated_at: now_ms(),
        },
        credentials: input.credentials,
    };
    secrets::providers::validate(&connection).map_err(|error| api_error(error, id.0))?;
    let result = vault(&state, id.0, move |store| {
        store.write(|contents| {
            if contents.connections.len() >= 20 {
                return Err(SecretError::Invalid(
                    "at most 20 external connections are supported",
                ));
            }
            if contents
                .connections
                .iter()
                .any(|item| item.summary.name == connection.summary.name)
            {
                return Err(SecretError::Duplicate);
            }
            let summary = connection.summary.clone();
            contents.connections.push(connection);
            Ok(summary)
        })
    })
    .await?;
    Ok((StatusCode::CREATED, Json(result)))
}

/// Updates a matching connection version while retaining omitted credentials.
pub(crate) async fn update_connection(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(input): ValidatedJson<ConnectionInput>,
) -> Result<Json<ConnectionSummary>, AppError> {
    let id = parse_id(&id, request.0)?;
    vault(&state, request.0, move |store| {
        store.write(|contents| {
            if contents
                .connections
                .iter()
                .any(|item| item.summary.id != id && item.summary.name == input.name)
            {
                return Err(SecretError::Duplicate);
            }
            let connection = contents
                .connections
                .iter_mut()
                .find(|item| item.summary.id == id)
                .ok_or(SecretError::NotFound)?;
            if input.version != Some(connection.summary.version) {
                return Err(SecretError::Conflict);
            }
            if connection.summary.provider != input.provider {
                return Err(SecretError::Invalid(
                    "create a new connection to change the provider",
                ));
            }
            connection.summary.name = input.name;
            connection.summary.settings = input.settings;
            connection.summary.version += 1;
            connection.summary.updated_at = now_ms();
            connection.credentials.extend(input.credentials);
            secrets::providers::validate(connection)?;
            Ok(connection.summary.clone())
        })
    })
    .await
    .map(Json)
}

/// Removes a connection and its previews while preserving imported local copies.
pub(crate) async fn delete_connection(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let id = parse_id(&id, request.0)?;
    vault(&state, request.0, move |store| {
        store.write(|contents| {
            let index = contents
                .connections
                .iter()
                .position(|item| item.summary.id == id)
                .ok_or(SecretError::NotFound)?;
            contents.connections.remove(index);
            contents.plans.retain(|plan| plan.connection_id != id);
            Ok(json!({"deleted":id}))
        })
    })
    .await
    .map(Json)
}

/// Captures transfer values in the encrypted vault and returns a value-free preview.
pub(crate) async fn preview(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(input): ValidatedJson<PreviewInput>,
) -> Result<Json<Preview>, AppError> {
    let id = parse_id(&id, request.0)?;
    let (connection, local) = vault(&state, request.0, move |store| {
        store.read(|contents| {
            let connection = contents
                .connections
                .iter()
                .find(|item| item.summary.id == id)
                .ok_or(SecretError::NotFound)?
                .clone();
            Ok((connection, contents.secrets.clone()))
        })
    })
    .await?;
    if input.direction == Direction::Export
        && (input.secret_ids.is_empty()
            || input
                .secret_ids
                .iter()
                .any(|id| !local.iter().any(|secret| secret.summary.id == *id)))
    {
        return Err(api_error(
            SecretError::Invalid("select existing PipeSecrets values to upload"),
            request.0,
        ));
    }
    let remote = secrets::providers::list(&connection)
        .await
        .map_err(|error| api_error(error, request.0))?;
    let mut values = Vec::new();
    match input.direction {
        Direction::Import => {
            for (name, value) in &remote {
                let existing = local.iter().find(|secret| secret.summary.name == *name);
                values.push(PlannedValue {
                    summary: change(name, &existing.map(|secret| secret.value.clone()), value),
                    value: value.clone(),
                    local_id: existing.map(|secret| secret.summary.id),
                    local_version: existing.map(|secret| secret.summary.version),
                    remote_value: Some(value.clone()),
                });
            }
        }
        Direction::Export => {
            for secret in &local {
                if !input.secret_ids.contains(&secret.summary.id) {
                    continue;
                }
                if connection.summary.provider == Provider::AwsSecretsManager
                    && !secret
                        .summary
                        .name
                        .starts_with(&connection.summary.settings.prefix)
                {
                    return Err(api_error(
                        SecretError::Invalid(
                            "uploaded secret names must match the configured AWS prefix",
                        ),
                        request.0,
                    ));
                }
                let existing = remote.get(&secret.summary.name).cloned();
                values.push(PlannedValue {
                    summary: change(&secret.summary.name, &existing, &secret.value),
                    value: secret.value.clone(),
                    local_id: Some(secret.summary.id),
                    local_version: Some(secret.summary.version),
                    remote_value: existing,
                });
            }
        }
    }
    values.sort_by(|a, b| a.summary.name.cmp(&b.summary.name));
    let plan = Plan {
        id: Uuid::new_v4(),
        connection_id: id,
        connection_version: connection.summary.version,
        direction: input.direction,
        created_at: now_ms(),
        values,
    };
    let summary = Preview {
        id: plan.id,
        connection_id: id,
        direction: plan.direction,
        expires_at: plan.created_at + 600_000,
        changes: plan
            .values
            .iter()
            .map(|value| value.summary.clone())
            .collect(),
    };
    vault(&state, request.0, move |store| {
        store.write(|contents| {
            contents
                .plans
                .retain(|existing| existing.connection_id != id);
            contents.plans.push(plan);
            Ok(())
        })
    })
    .await?;
    Ok(Json(summary))
}

/// Classifies a captured source value against the destination's current value.
fn change(name: &str, existing: &Option<String>, value: &str) -> ChangeSummary {
    let change = match existing {
        None => Change::Add,
        Some(previous) if previous == value => Change::Unchanged,
        Some(_) => Change::Update,
    };
    ChangeSummary {
        name: name.to_owned(),
        change,
    }
}

/// Checks preview age, selected changes, connection version, and local identities.
fn selected_plan(
    contents: &Contents,
    id: Uuid,
    names: &[String],
) -> Result<(Plan, Connection), SecretError> {
    let plan = contents
        .plans
        .iter()
        .find(|plan| plan.id == id)
        .ok_or(SecretError::NotFound)?;
    if now_ms().saturating_sub(plan.created_at) >= 600_000 {
        return Err(SecretError::Expired);
    }
    let connection = contents
        .connections
        .iter()
        .find(|item| item.summary.id == plan.connection_id)
        .ok_or(SecretError::NotFound)?;
    if connection.summary.version != plan.connection_version {
        return Err(SecretError::Conflict);
    }
    let names: BTreeSet<_> = names.iter().collect();
    if names.is_empty()
        || names.iter().any(|name| {
            !plan.values.iter().any(|value| {
                &value.summary.name == *name && value.summary.change != Change::Unchanged
            })
        })
    {
        return Err(SecretError::Invalid(
            "select changed entries from the preview",
        ));
    }
    let mut selected = plan.clone();
    selected
        .values
        .retain(|value| names.contains(&value.summary.name));
    for value in &selected.values {
        let local = contents
            .secrets
            .iter()
            .find(|secret| secret.summary.name == value.summary.name);
        if local.map(|secret| secret.summary.version) != value.local_version
            || local.map(|secret| secret.summary.id) != value.local_id
        {
            return Err(SecretError::Conflict);
        }
    }
    Ok((selected, connection.clone()))
}

/// Applies selected snapshot names once, reporting completed and failed uploads.
pub(crate) async fn apply(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(input): ValidatedJson<ApplyInput>,
) -> Result<Json<Value>, AppError> {
    let id = parse_id(&id, request.0)?;
    let names = input.names;
    let selected = names.clone();
    let (plan, connection) = vault(&state, request.0, move |store| {
        store.read(|contents| selected_plan(contents, id, &selected))
    })
    .await?;
    if plan.direction == Direction::Import {
        let result = vault(&state, request.0, move |store| {
            store.write(|contents| {
                let (plan, connection) = selected_plan(contents, id, &names)?;
                let additions = plan
                    .values
                    .iter()
                    .filter(|value| value.local_version.is_none())
                    .count();
                if contents.secrets.len() + additions > secrets::MAX_SECRETS {
                    return Err(SecretError::Invalid(
                        "PipeSecrets supports at most 1000 secrets",
                    ));
                }
                let mut applied = Vec::new();
                for value in plan.values {
                    let source = SecretSource {
                        connection_id: connection.summary.id,
                        connection_name: connection.summary.name.clone(),
                        provider: connection.summary.provider,
                        remote_key: value.summary.name.clone(),
                        imported_at: now_ms(),
                    };
                    if let Some(secret) = contents
                        .secrets
                        .iter_mut()
                        .find(|secret| secret.summary.name == value.summary.name)
                    {
                        secret.value = value.value;
                        secret.summary.version += 1;
                        secret.summary.updated_at = now_ms();
                        secret.summary.source = Some(source);
                    } else {
                        contents.secrets.push(Secret {
                            summary: SecretSummary {
                                id: Uuid::new_v4(),
                                name: value.summary.name.clone(),
                                description: String::new(),
                                version: 1,
                                created_at: now_ms(),
                                updated_at: now_ms(),
                                source: Some(source),
                            },
                            value: value.value,
                        });
                    }
                    applied.push(value.summary.name);
                }
                contents.plans.retain(|plan| plan.id != id);
                Ok(json!({"applied":applied,"failed":[]}))
            })
        })
        .await?;
        return Ok(Json(result));
    }
    let remote = secrets::providers::list(&connection)
        .await
        .map_err(|error| api_error(error, request.0))?;
    if plan
        .values
        .iter()
        .any(|value| remote.get(&value.summary.name) != value.remote_value.as_ref())
    {
        return Err(api_error(SecretError::Conflict, request.0));
    }
    // Consume before sending writes: retries cannot accidentally upload the same preview twice.
    vault(&state, request.0, move |store| {
        store.write(|contents| {
            selected_plan(contents, id, &names)?;
            contents.plans.retain(|plan| plan.id != id);
            Ok(())
        })
    })
    .await?;
    let mut applied = Vec::new();
    let mut failed = Vec::new();
    for value in plan.values {
        match secrets::providers::upload(&connection, &value.summary.name, &value.value, value.remote_value.as_deref()).await {
            Ok(()) => applied.push(value.summary.name),
            Err(_) => failed.push(json!({"name":value.summary.name,"message":"Upload failed. Check the target and preview again."})),
        }
    }
    Ok(Json(json!({"applied":applied,"failed":failed})))
}

#[cfg(test)]
mod tests;
