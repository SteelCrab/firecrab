//! Dashboard-triggered benchmark job endpoints.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use uuid::Uuid;

use crate::benchmark_jobs::{
    BenchmarkCommand, BenchmarkJob, CancelJobError, StartBenchmarkJobRequest, StartJobError,
};
use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::server::RequestId;
use crate::state::AppState;

/// Dashboard safety ceiling for sequential boot samples.
const BOOT_COUNT_MAX: u32 = 100;
/// Dashboard safety ceiling for concurrent VM workers.
const CREATE_CONCURRENCY_MAX: u32 = 100;
/// Dashboard safety ceiling for running density VMs.
const DENSITY_MAX_VMS: u32 = 100;
/// Dashboard safety ceiling for lifecycle repetitions.
const LIFECYCLE_ITERATIONS_MAX: u32 = 1_000;

/// Returns recent in-memory benchmark job snapshots, newest first.
pub async fn list_jobs(State(state): State<AppState>) -> Json<Vec<BenchmarkJob>> {
    Json(state.benchmark_jobs.list())
}

/// Returns one benchmark job snapshot.
pub async fn get_job(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<Uuid>,
) -> Result<Json<BenchmarkJob>, AppError> {
    state
        .benchmark_jobs
        .get(id)
        .map(Json)
        .ok_or_else(|| AppError::not_found(request_id.0))
}

/// Validates and starts one host-local benchmark child process.
pub async fn start_job(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    ValidatedJson(request): ValidatedJson<StartBenchmarkJobRequest>,
) -> Result<(StatusCode, Json<BenchmarkJob>), AppError> {
    let fields =
        validate_request(&state, &request).map_err(|_| AppError::internal(request_id.0))?;
    if !fields.is_empty() {
        return Err(AppError::validation(fields, request_id.0));
    }
    match state.benchmark_jobs.start(request) {
        Ok(job) => Ok((StatusCode::ACCEPTED, Json(job))),
        Err(StartJobError::AlreadyRunning) => Err(AppError::conflict(
            "benchmark_running",
            "another benchmark job is already running",
            request_id.0,
        )),
        Err(StartJobError::BinaryUnavailable) => Err(AppError::unavailable(
            "firecrab-bench is not installed or executable",
            request_id.0,
        )),
    }
}

/// Cancels the currently active benchmark job.
pub async fn cancel_job(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<BenchmarkJob>), AppError> {
    match state.benchmark_jobs.cancel(id) {
        Ok(job) => Ok((StatusCode::ACCEPTED, Json(job))),
        Err(CancelJobError::NotFound) => Err(AppError::not_found(request_id.0)),
        Err(CancelJobError::NotRunning) => Err(AppError::conflict(
            "benchmark_not_running",
            "benchmark job is not running",
            request_id.0,
        )),
    }
}

/// Validates image, network, resource, and command-specific safety limits.
fn validate_request(
    state: &AppState,
    request: &StartBenchmarkJobRequest,
) -> Result<BTreeMap<String, String>, crate::persistence::PersistenceError> {
    let mut fields = BTreeMap::new();
    if request.template.trim().is_empty()
        || state.templates.resolve_alias(&request.template).is_none()
    {
        fields.insert(
            "template".to_owned(),
            "must be an installed image alias".to_owned(),
        );
    }
    if state
        .store
        .micro_network(request.micro_network_id)?
        .is_none()
    {
        fields.insert(
            "microNetworkId".to_owned(),
            "must identify an existing MicroNetwork".to_owned(),
        );
    }
    if !(128..=8_192).contains(&request.ram) {
        fields.insert(
            "ram".to_owned(),
            "must be between 128 and 8192 MiB".to_owned(),
        );
    }
    if !(1..=32).contains(&request.cpu) {
        fields.insert("cpu".to_owned(), "must be between 1 and 32".to_owned());
    }
    if !(1..=256).contains(&request.disk_gb) {
        fields.insert(
            "diskGb".to_owned(),
            "must be between 1 and 256 GiB".to_owned(),
        );
    }
    match request.command {
        BenchmarkCommand::Boot => {
            validate_limit(&mut fields, "count", request.count, 1, BOOT_COUNT_MAX)
        }
        BenchmarkCommand::Create => validate_limit(
            &mut fields,
            "concurrency",
            request.concurrency,
            1,
            CREATE_CONCURRENCY_MAX,
        ),
        BenchmarkCommand::Density => {
            validate_limit(&mut fields, "maxVms", request.max_vms, 1, DENSITY_MAX_VMS);
            validate_limit(&mut fields, "step", request.step, 1, DENSITY_MAX_VMS);
            if request
                .max_vms
                .zip(request.step)
                .is_some_and(|(max, step)| step > max)
            {
                fields.insert("step".to_owned(), "must not exceed maxVms".to_owned());
            }
            if !request.confirm_density {
                fields.insert(
                    "confirmDensity".to_owned(),
                    "must acknowledge the host resource impact".to_owned(),
                );
            }
        }
        BenchmarkCommand::Lifecycle => validate_limit(
            &mut fields,
            "iterations",
            request.iterations,
            1,
            LIFECYCLE_ITERATIONS_MAX,
        ),
    }
    Ok(fields)
}

/// Adds a field error when an optional numeric limit is absent or out of range.
fn validate_limit(
    fields: &mut BTreeMap<String, String>,
    name: &str,
    value: Option<u32>,
    minimum: u32,
    maximum: u32,
) {
    if value.is_none_or(|value| !(minimum..=maximum).contains(&value)) {
        fields.insert(
            name.to_owned(),
            format!("must be between {minimum} and {maximum}"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::TemplateRegistry;
    use axum::response::IntoResponse;

    async fn state_for(root: &std::path::Path) -> AppState {
        let templates = TemplateRegistry::from_specs(root, std::iter::empty()).unwrap();
        AppState::with_db_file(templates, root.join("state.db"))
            .await
            .unwrap()
    }

    fn request(command: BenchmarkCommand) -> StartBenchmarkJobRequest {
        StartBenchmarkJobRequest {
            command,
            template: "missing".to_owned(),
            micro_network_id: Uuid::nil(),
            ram: 512,
            cpu: 1,
            disk_gb: 8,
            count: Some(5),
            concurrency: Some(10),
            max_vms: Some(20),
            step: Some(10),
            iterations: Some(100),
            confirm_density: false,
        }
    }

    #[tokio::test]
    async fn density_requires_confirmation_and_valid_limits() {
        let root = tempfile::tempdir().unwrap();
        let state = state_for(root.path()).await;
        let mut request = request(BenchmarkCommand::Density);
        request.max_vms = Some(101);
        request.step = Some(0);
        let fields = validate_request(&state, &request).unwrap();
        assert!(fields.contains_key("confirmDensity"));
        assert!(fields.contains_key("maxVms"));
        assert!(fields.contains_key("step"));
    }

    #[test]
    fn command_limits_reject_missing_values() {
        let mut fields = BTreeMap::new();
        validate_limit(&mut fields, "count", None, 1, 100);
        assert_eq!(fields["count"], "must be between 1 and 100");
    }

    #[tokio::test]
    async fn empty_tracker_lists_no_jobs_and_unknown_jobs_are_not_found() {
        let root = tempfile::tempdir().unwrap();
        let state = state_for(root.path()).await;
        let Json(jobs) = list_jobs(State(state.clone())).await;
        assert!(jobs.is_empty());

        let request_id = Uuid::new_v4();
        let id = Uuid::new_v4();
        let get_error = get_job(
            State(state.clone()),
            Extension(RequestId(request_id)),
            Path(id),
        )
        .await
        .unwrap_err();
        assert_eq!(get_error.into_response().status(), StatusCode::NOT_FOUND);

        let cancel_error = cancel_job(State(state), Extension(RequestId(request_id)), Path(id))
            .await
            .unwrap_err();
        assert_eq!(cancel_error.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn invalid_start_returns_field_errors_before_spawning() {
        let root = tempfile::tempdir().unwrap();
        let state = state_for(root.path()).await;
        let error = start_job(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(request(BenchmarkCommand::Boot)),
        )
        .await
        .unwrap_err();
        assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn every_command_requires_its_own_limit() {
        let root = tempfile::tempdir().unwrap();
        let state = state_for(root.path()).await;
        for (command, field) in [
            (BenchmarkCommand::Boot, "count"),
            (BenchmarkCommand::Create, "concurrency"),
            (BenchmarkCommand::Lifecycle, "iterations"),
        ] {
            let mut request = request(command);
            request.count = None;
            request.concurrency = None;
            request.iterations = None;
            let fields = validate_request(&state, &request).unwrap();
            assert!(fields.contains_key(field));
        }
    }
}
