//! CRUD and enable/disable controls for host-local scheduled VM operations.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use chrono::Utc;
use firecrab_api_types::{ScheduleResponse, ScheduleRun, ScheduleSpec, SetScheduleEnabledRequest};
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::handlers::vms::parse_id;
use crate::persistence::{PersistenceError, ScheduleMutation};
use crate::server::RequestId;
use crate::state::AppState;

async fn database<T: Send + 'static>(
    request_id: Uuid,
    operation: impl FnOnce() -> Result<T, PersistenceError> + Send + 'static,
) -> Result<T, AppError> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| AppError::internal(request_id))?
        .map_err(|error| {
            tracing::error!(%request_id, %error, "schedule database operation failed");
            AppError::internal(request_id)
        })
}

pub async fn list(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
) -> Result<Json<Vec<ScheduleResponse>>, AppError> {
    database(request.0, move || state.store.list_schedules())
        .await
        .map(Json)
}

pub async fn get(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<ScheduleResponse>, AppError> {
    let id = parse_id(&id, request.0)?;
    database(request.0, move || state.store.schedule(id))
        .await?
        .map(Json)
        .ok_or_else(|| AppError::not_found(request.0))
}

fn validate(
    state: &AppState,
    spec: &ScheduleSpec,
    now: i64,
    request: Uuid,
) -> Result<Option<i64>, AppError> {
    let mut fields = BTreeMap::new();
    if spec.name.trim().is_empty()
        || spec.name.len() > 64
        || spec.name.chars().any(char::is_control)
    {
        fields.insert(
            "name".to_owned(),
            "use 1–64 bytes without control characters".to_owned(),
        );
    }
    if !state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains_key(&spec.vm_id)
    {
        fields.insert("vmId".to_owned(), "target VM does not exist".to_owned());
    }
    let next = match crate::scheduler::next_run(&spec.timing, now) {
        Ok(Some(at)) => Some(at),
        Ok(None) => {
            if spec.enabled {
                fields.insert(
                    "timing".to_owned(),
                    "enabled schedules need a future execution time".to_owned(),
                );
            }
            None
        }
        Err(error) => {
            fields.insert("timing".to_owned(), error);
            None
        }
    };
    if !fields.is_empty() {
        return Err(AppError::validation(fields, request));
    }
    Ok(if spec.enabled { next } else { None })
}

pub async fn create(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    ValidatedJson(spec): ValidatedJson<ScheduleSpec>,
) -> Result<(StatusCode, Json<ScheduleResponse>), AppError> {
    let now = Utc::now().timestamp_millis();
    let next = validate(&state, &spec, now, request.0)?;
    let schedule = ScheduleResponse {
        id: Uuid::new_v4(),
        spec,
        next_run_at_ms: next,
        created_at_ms: now,
        updated_at_ms: now,
        last_run: None,
    };
    let record = schedule.clone();
    database(request.0, move || state.store.insert_schedule(&record)).await?;
    Ok((StatusCode::CREATED, Json(schedule)))
}

fn changed(result: ScheduleMutation, request: Uuid) -> Result<(), AppError> {
    match result {
        ScheduleMutation::Done => Ok(()),
        ScheduleMutation::Missing => Err(AppError::not_found(request)),
        ScheduleMutation::Expired => Err(AppError::validation(
            BTreeMap::from([(
                "timing".to_owned(),
                "no unconsumed future execution time remains".to_owned(),
            )]),
            request,
        )),
        ScheduleMutation::Busy => Err(AppError::new(
            StatusCode::CONFLICT,
            "schedule_running",
            "disable the schedule and wait for its active operation to finish before editing, enabling or deleting it",
            request,
        )),
    }
}

pub async fn update(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(spec): ValidatedJson<ScheduleSpec>,
) -> Result<Json<ScheduleResponse>, AppError> {
    let parsed = parse_id(&id, request.0)?;
    let now = Utc::now().timestamp_millis();
    validate(&state, &spec, now, request.0)?;
    let store = state.store.clone();
    changed(
        database(request.0, move || {
            store.mutate_schedule(parsed, Some(&spec), None, now)
        })
        .await?,
        request.0,
    )?;
    get(State(state), Extension(request), Path(id)).await
}

pub async fn set_enabled(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(body): ValidatedJson<SetScheduleEnabledRequest>,
) -> Result<Json<ScheduleResponse>, AppError> {
    let parsed = parse_id(&id, request.0)?;
    let now = Utc::now().timestamp_millis();
    if body.enabled {
        let Json(mut existing) =
            get(State(state.clone()), Extension(request), Path(id.clone())).await?;
        if existing.spec.enabled {
            return Ok(Json(existing));
        }
        existing.spec.enabled = true;
        validate(&state, &existing.spec, now, request.0)?;
    }
    let store = state.store.clone();
    changed(
        database(request.0, move || {
            store.mutate_schedule(parsed, None, Some(body.enabled), now)
        })
        .await?,
        request.0,
    )?;
    get(State(state), Extension(request), Path(id)).await
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let id = parse_id(&id, request.0)?;
    changed(
        database(request.0, move || {
            state
                .store
                .mutate_schedule(id, None, None, Utc::now().timestamp_millis())
        })
        .await?,
        request.0,
    )?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn history(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<Vec<ScheduleRun>>, AppError> {
    let id = parse_id(&id, request.0)?;
    database(request.0, move || {
        if state.store.schedule(id)?.is_none() {
            return Ok(None);
        }
        state.store.schedule_runs(id).map(Some)
    })
    .await?
    .map(Json)
    .ok_or_else(|| AppError::not_found(request.0))
}

#[cfg(test)]
mod tests;
