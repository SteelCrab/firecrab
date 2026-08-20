//! Benchmark result ingestion and commit-history listing.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use serde_json::Value;
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::server::RequestId;
use crate::state::AppState;

/// Newest 200 results are enough for the first history dashboard.
const HISTORY_LIMIT: u32 = 200;

/// Returns the newest benchmark results for the history dashboard.
pub async fn list_benchmarks(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
) -> Result<Json<Vec<Value>>, AppError> {
    let store = state.store.clone();
    let results = tokio::task::spawn_blocking(move || store.list_benchmark_results(HISTORY_LIMIT))
        .await
        .map_err(|_| AppError::internal(request_id.0))?
        .map_err(|error| {
            tracing::error!(request_id = %request_id.0, %error, "failed to list benchmark results");
            AppError::internal(request_id.0)
        })?;
    Ok(Json(results))
}

/// Validates and stores one versioned benchmark result document.
pub async fn create_benchmark(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    ValidatedJson(result): ValidatedJson<Value>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    let fields = validate_result(&result);
    if !fields.is_empty() {
        return Err(AppError::validation(fields, request_id.0));
    }
    let run = result["run"].as_object().expect("validated run object");
    let run_id =
        Uuid::parse_str(run["run_id"].as_str().expect("validated run id")).expect("validated UUID");
    let commit_sha = run["commit_sha"].as_str().expect("validated commit");
    let branch = run["branch"].as_str().expect("validated branch");
    let timestamp = run["timestamp"].as_str().expect("validated timestamp");
    let test_name = result["test"].as_str().expect("validated test");
    let result_json =
        serde_json::to_string(&result).map_err(|_| AppError::internal(request_id.0))?;
    let store = state.store.clone();
    let commit = commit_sha.to_owned();
    let branch = branch.to_owned();
    let timestamp = timestamp.to_owned();
    let test = test_name.to_owned();
    tokio::task::spawn_blocking(move || {
        store.insert_benchmark_result(run_id, &commit, &branch, &timestamp, &test, &result_json)
    })
    .await
    .map_err(|_| AppError::internal(request_id.0))?
    .map_err(|error| {
        tracing::error!(request_id = %request_id.0, %error, "failed to store benchmark result");
        AppError::internal(request_id.0)
    })?;
    Ok((StatusCode::CREATED, Json(result)))
}

fn validate_result(result: &Value) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    if result.get("schema_version").and_then(Value::as_u64) != Some(2) {
        fields.insert("schema_version".to_owned(), "must be 2".to_owned());
    }
    let Some(run) = result.get("run").and_then(Value::as_object) else {
        fields.insert("run".to_owned(), "must be an object".to_owned());
        return fields;
    };
    match run.get("run_id").and_then(Value::as_str) {
        Some(id) if Uuid::parse_str(id).is_ok() => {}
        _ => {
            fields.insert("run.run_id".to_owned(), "must be a UUID".to_owned());
        }
    }
    for name in ["commit_sha", "branch", "timestamp"] {
        if run
            .get(name)
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            fields.insert(
                format!("run.{name}"),
                "must be a non-empty string".to_owned(),
            );
        }
    }
    if result
        .get("test")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        fields.insert("test".to_owned(), "must be a non-empty string".to_owned());
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_state(root: &std::path::Path) -> AppState {
        let templates = crate::templates::TemplateRegistry::from_specs(root, std::iter::empty())
            .expect("empty template list");
        AppState::with_db_file(templates, root.join("benchmarks.db"))
            .await
            .expect("benchmark test database")
    }

    #[test]
    fn validation_accepts_the_common_schema_identity() {
        let result = serde_json::json!({
            "schema_version": 2,
            "run": {
                "run_id": Uuid::new_v4(),
                "commit_sha": "abc123",
                "branch": "main",
                "timestamp": "2026-08-20T00:00:00Z"
            },
            "test": "vm_boot"
        });
        assert!(validate_result(&result).is_empty());
    }

    #[test]
    fn validation_rejects_missing_identity_fields() {
        let fields = validate_result(&serde_json::json!({"schema_version": 1, "run": {}}));
        assert!(fields.contains_key("schema_version"));
        assert!(fields.contains_key("run.run_id"));
        assert!(fields.contains_key("test"));
    }

    #[tokio::test]
    async fn create_then_list_round_trips_the_common_schema() {
        let directory = tempfile::tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let result = serde_json::json!({
            "schema_version": 2,
            "run": {
                "run_id": Uuid::new_v4(),
                "commit_sha": "abc123",
                "branch": "main",
                "timestamp": "2026-08-20T00:00:00Z"
            },
            "test": "vm_boot"
        });
        let request_id = RequestId(Uuid::new_v4());

        let (status, Json(stored)) = create_benchmark(
            State(state.clone()),
            Extension(request_id),
            ValidatedJson(result.clone()),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(stored, result);

        let Json(history) = list_benchmarks(State(state), Extension(RequestId(Uuid::new_v4())))
            .await
            .unwrap();
        assert_eq!(history, vec![result]);
    }
}
