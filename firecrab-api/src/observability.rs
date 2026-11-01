//! Operator-only SigNoz integration over the existing per-service exporters.
use crate::{
    ai_sandbox::access::Caller,
    error::AppError,
    extract::ValidatedJson,
    handlers::micro_services::database,
    otlp::{self, DeliveryBatch, ExportRecord, RecordKind, Severity},
    persistence::observability::SignozConfig,
    server::RequestId,
    service_observation,
    state::AppState,
};
use axum::{Extension, Json, extract::State};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    time::Duration,
};
use uuid::Uuid;

const SOURCE_LIMIT: usize = 100;
const DISPLAY_HISTORY_LIMIT: usize = 200;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Source {
    id: Uuid,
    name: String,
    selected: bool,
    available: bool,
    status: Option<otlp::ExportStatus>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Batch {
    service_id: Uuid,
    service_name: String,
    #[serde(flatten)]
    batch: DeliveryBatch,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot {
    observed_at_ms: i64,
    provider: &'static str,
    config: Option<SignozConfig>,
    sources: Vec<Source>,
    history: Vec<Batch>,
    history_limit_per_service: usize,
    display_history_limit: usize,
    queue_limit_per_service: usize,
    queue_persistent: bool,
}

async fn snapshot(state: AppState, request: Uuid) -> Result<Snapshot, AppError> {
    database(request, move || {
        let config = state.store.signoz_config()?;
        let mut sources = Vec::new();
        let mut history = Vec::new();
        for service in state.store.list_micro_services()? {
            let selected = config
                .as_ref()
                .is_some_and(|c| c.service_ids.contains(&service.id));
            let status = if selected {
                state
                    .observations
                    .exporter(service.id)
                    .map(|exporter| exporter.status())
            } else {
                None
            };
            if let Some(status) = &status {
                history.extend(status.history.iter().map(|batch| Batch {
                    service_id: service.id,
                    service_name: service.spec.name.clone(),
                    batch: batch.clone(),
                }));
            }
            let available = !state.store.service_is_controller_owned(service.id)?
                && (selected || state.store.otlp_endpoint(service.id)?.is_none());
            sources.push(Source {
                id: service.id,
                name: service.spec.name,
                selected,
                available,
                status,
            });
        }
        history.sort_by_key(|row| std::cmp::Reverse(row.batch.timestamp_ms));
        history.truncate(DISPLAY_HISTORY_LIMIT);
        Ok(Snapshot {
            observed_at_ms: chrono::Utc::now().timestamp_millis(),
            provider: "signoz",
            config,
            sources,
            history,
            history_limit_per_service: otlp::HISTORY_LIMIT,
            display_history_limit: DISPLAY_HISTORY_LIMIT,
            queue_limit_per_service: otlp::QUEUE_LIMIT,
            queue_persistent: false,
        })
    })
    .await
}

pub(crate) async fn get(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: Caller,
) -> Result<Json<Snapshot>, AppError> {
    service_observation::operator(caller, request.0)?;
    Ok(Json(snapshot(state, request.0).await?))
}

pub(crate) async fn configure(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: Caller,
    ValidatedJson(mut config): ValidatedJson<SignozConfig>,
) -> Result<Json<Snapshot>, AppError> {
    service_observation::operator(caller, request.0)?;
    config.endpoint = otlp::validate_endpoint(&config.endpoint)
        .map_err(|_| invalid_endpoint(request.0))?
        .to_string();
    if config.endpoint.trim_end_matches('/').ends_with("/v1/logs") {
        return Err(invalid_endpoint(request.0));
    }
    let unique: HashSet<_> = config.service_ids.iter().copied().collect();
    if unique.is_empty() || unique.len() > SOURCE_LIMIT || unique.len() != config.service_ids.len()
    {
        return Err(AppError::validation(
            BTreeMap::from([(
                "serviceIds".into(),
                "Select 1 to 100 distinct services".into(),
            )]),
            request.0,
        ));
    }
    config.service_ids.sort();
    let claim = state.service_mutations.clone().lock_owned().await;
    let mut failures = BTreeMap::new();
    for id in &config.service_ids {
        if let Err(error) = service_observation::ensure_local_service(&state, *id, request.0).await
        {
            failures.insert(id.to_string(), error.description());
            continue;
        }
        let store = state.store.clone();
        let id = *id;
        let (service, active, managed, endpoint) = database(request.0, move || {
            Ok((
                store.micro_service(id)?,
                store.service_deployment(id)?,
                store.signoz_source(id)?,
                store.otlp_endpoint(id)?,
            ))
        })
        .await?;
        let Some(service) = service else {
            failures.insert(id.to_string(), "Service no longer exists".into());
            continue;
        };
        if endpoint.is_some() && !managed {
            failures.insert(
                id.to_string(),
                "Service already has an independently configured exporter".into(),
            );
            continue;
        }
        if let Some(active) = &active
            && (active.phase.is_busy()
                || active.update.as_ref().is_some_and(|u| !u.completed)
                || active.recovery.as_ref().is_some_and(|r| r.repair.is_some()))
        {
            failures.insert(
                id.to_string(),
                "Wait for the service operation to finish".into(),
            );
            continue;
        }
        for spec in
            std::iter::once(&service.spec).chain(active.as_ref().map(|deployment| &deployment.spec))
        {
            if let Err(error) = otlp::preflight_spec(&config.endpoint, spec, request.0) {
                failures.insert(id.to_string(), error.description());
            }
        }
    }
    if !failures.is_empty() {
        return Err(AppError::validation(failures, request.0));
    }
    // Keep the mutation claim until both persistence and runtime reconciliation finish,
    // even when the client cancels the HTTP request.
    let working = state.clone();
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let _claim = claim;
        let _runtime = runtime.enter();
        working.store.save_signoz(Some(&config))?;
        service_observation::export_pass_blocking(&working)
    })
    .await
    .map_err(|_| AppError::internal(request.0))?
    .map_err(|_| AppError::internal(request.0))?;
    Ok(Json(snapshot(state, request.0).await?))
}

pub(crate) async fn disconnect(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: Caller,
) -> Result<Json<Snapshot>, AppError> {
    service_observation::operator(caller, request.0)?;
    let claim = state.service_mutations.clone().lock_owned().await;
    let working = state.clone();
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let _claim = claim;
        let _runtime = runtime.enter();
        working.store.save_signoz(None)?;
        service_observation::export_pass_blocking(&working)
    })
    .await
    .map_err(|_| AppError::internal(request.0))?
    .map_err(|_| AppError::internal(request.0))?;
    Ok(Json(snapshot(state, request.0).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Probe {
    endpoint: String,
}

fn invalid_endpoint(request: Uuid) -> AppError {
    AppError::validation(BTreeMap::from([("endpoint".into(), "Use an HTTP(S) collector base URL without credentials, query, fragment, or /v1/logs suffix".into())]), request)
}

pub(crate) async fn test_connection(
    State(_state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: Caller,
    ValidatedJson(probe): ValidatedJson<Probe>,
) -> Result<Json<DeliveryBatch>, AppError> {
    service_observation::operator(caller, request.0)?;
    let endpoint =
        otlp::validate_endpoint(&probe.endpoint).map_err(|_| invalid_endpoint(request.0))?;
    if endpoint.path().trim_end_matches('/').ends_with("/v1/logs") {
        return Err(invalid_endpoint(request.0));
    }
    Ok(Json(
        otlp::probe(endpoint.as_str())
            .await
            .map_err(|_| invalid_endpoint(request.0))?,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TestLog {
    service_id: Uuid,
}

pub(crate) async fn send_test(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: Caller,
    ValidatedJson(test): ValidatedJson<TestLog>,
) -> Result<Json<DeliveryBatch>, AppError> {
    service_observation::operator(caller, request.0)?;
    let claim = state.service_mutations.clone().lock_owned().await;
    let store = state.store.clone();
    database(request.0, move || store.signoz_config())
        .await?
        .filter(|config| !config.paused && config.service_ids.contains(&test.service_id))
        .ok_or_else(|| {
            AppError::conflict(
                "observability_source_inactive",
                "Select an active SigNoz source before sending a test",
                request.0,
            )
        })?;
    let exporter = state
        .observations
        .exporter(test.service_id)
        .ok_or_else(|| {
            AppError::conflict(
                "exporter_pending",
                "The service exporter is not ready",
                request.0,
            )
        })?;
    // Enqueue while the claim excludes a concurrent endpoint change, then wait
    // without blocking other service operations.
    let record = ExportRecord {
        timestamp_unix_nano: chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0).max(0) as u64,
        kind: RecordKind::Event,
        severity: Severity::Info,
        instance_id: None,
        stream: None,
        body: format!("Firecrab SigNoz integration test {}", request.0),
    };
    let pending = exporter.submit_test(record).map_err(|_| {
        AppError::conflict(
            "exporter_unavailable",
            "The exporter queue is full or the connection changed",
            request.0,
        )
    })?;
    drop(claim);
    let result = tokio::time::timeout(Duration::from_secs(40), pending)
        .await
        .map_err(|_| AppError::gateway_timeout(request.0))?
        .map_err(|_| {
            AppError::conflict(
                "exporter_unavailable",
                "The exporter queue is full or the connection changed",
                request.0,
            )
        })?;
    Ok(Json(result))
}

#[cfg(test)]
mod tests;
