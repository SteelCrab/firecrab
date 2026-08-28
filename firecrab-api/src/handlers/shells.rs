//! Shell repository CRUD — versioned guest scripts injected on VM start.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use firecrab_api_types::{
    CreateShellRequest, CreateShellRevisionRequest, ShellDetailResponse, ShellResponse,
    ShellRevisionResponse,
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::handlers::vms::parse_id;
use crate::persistence::PersistenceError;
use crate::server::RequestId;
use crate::shells::{MAX_SHELL_CONTENT_BYTES, content_sha256};
use crate::state::AppState;

pub async fn list_shells(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
) -> Result<Json<Vec<ShellResponse>>, AppError> {
    let store = state.store.clone();
    let rows = tokio::task::spawn_blocking(move || store.list_shells())
        .await
        .map_err(|_| AppError::internal(request_id.0))?
        .map_err(|error| {
            tracing::error!(request_id = %request_id.0, %error, "failed to list shells");
            AppError::internal(request_id.0)
        })?;
    Ok(Json(rows))
}

pub async fn get_shell(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<ShellDetailResponse>, AppError> {
    let id = parse_id(&id, request_id.0)?;
    let store = state.store.clone();
    let detail = tokio::task::spawn_blocking(move || store.shell_detail(id))
        .await
        .map_err(|_| AppError::internal(request_id.0))?
        .map_err(|error| {
            tracing::error!(request_id = %request_id.0, %error, "failed to load shell");
            AppError::internal(request_id.0)
        })?
        .ok_or_else(|| AppError::not_found(request_id.0))?;
    Ok(Json(detail))
}

pub async fn create_shell(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    ValidatedJson(req): ValidatedJson<CreateShellRequest>,
) -> Result<(StatusCode, Json<ShellRevisionResponse>), AppError> {
    let fields = validate_shell_write(&req.name, req.description.as_deref(), &req.content);
    if !fields.is_empty() {
        return Err(AppError::validation(fields, request_id.0));
    }

    let id = Uuid::new_v4();
    let revision_id = Uuid::new_v4();
    let sha = content_sha256(&req.content);
    let now = now_ms();
    let store = state.store.clone();
    let name = req.name.clone();
    let description = req.description.clone();
    let content = req.content.clone();
    let sha_for_db = sha.clone();
    tokio::task::spawn_blocking(move || {
        store.create_shell(
            id,
            &name,
            description.as_deref(),
            revision_id,
            &content,
            &sha_for_db,
            now,
        )
    })
    .await
    .map_err(|_| AppError::internal(request_id.0))?
    .map_err(|error| {
        tracing::error!(request_id = %request_id.0, %error, "failed to create shell");
        AppError::internal(request_id.0)
    })?;

    tracing::info!(
        request_id = %request_id.0,
        shell_id = %id,
        revision_id = %revision_id,
        name = %req.name,
        "shell created"
    );
    Ok((
        StatusCode::CREATED,
        Json(ShellRevisionResponse {
            shell_id: id,
            revision_id,
            version: 1,
            content_sha256: sha,
            content: req.content,
            created_at_ms: now,
        }),
    ))
}

pub async fn create_shell_revision(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(req): ValidatedJson<CreateShellRevisionRequest>,
) -> Result<(StatusCode, Json<ShellRevisionResponse>), AppError> {
    let shell_id = parse_id(&id, request_id.0)?;
    let mut fields = BTreeMap::new();
    validate_content(&req.content, &mut fields);
    if !fields.is_empty() {
        return Err(AppError::validation(fields, request_id.0));
    }

    let revision_id = Uuid::new_v4();
    let sha = content_sha256(&req.content);
    let now = now_ms();
    let store = state.store.clone();
    let content = req.content.clone();
    let sha_for_db = sha.clone();
    let version = tokio::task::spawn_blocking(move || {
        store.add_shell_revision(shell_id, revision_id, &content, &sha_for_db, now)
    })
    .await
    .map_err(|_| AppError::internal(request_id.0))?
    .map_err(|error| match error {
        PersistenceError::MissingShell { .. } => AppError::not_found(request_id.0),
        other => {
            tracing::error!(request_id = %request_id.0, %other, "failed to add shell revision");
            AppError::internal(request_id.0)
        }
    })?;

    tracing::info!(
        request_id = %request_id.0,
        shell_id = %shell_id,
        revision_id = %revision_id,
        version,
        "shell revision created"
    );
    Ok((
        StatusCode::CREATED,
        Json(ShellRevisionResponse {
            shell_id,
            revision_id,
            version,
            content_sha256: sha,
            content: req.content,
            created_at_ms: now,
        }),
    ))
}

/// `GET /api/shells/{id}/revisions/{revision_id}` — full body of one past revision.
pub async fn get_shell_revision(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path((id, revision_id)): Path<(String, String)>,
) -> Result<Json<ShellRevisionResponse>, AppError> {
    let shell_id = parse_id(&id, request_id.0)?;
    let revision_id = parse_id(&revision_id, request_id.0)?;
    let store = state.store.clone();
    let detail = tokio::task::spawn_blocking(move || store.shell_revision(shell_id, revision_id))
        .await
        .map_err(|_| AppError::internal(request_id.0))?
        .map_err(|error| {
            tracing::error!(request_id = %request_id.0, %error, "failed to load shell revision");
            AppError::internal(request_id.0)
        })?
        .ok_or_else(|| AppError::not_found(request_id.0))?;
    Ok(Json(detail))
}

pub async fn delete_shell(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let id = parse_id(&id, request_id.0)?;
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || store.delete_shell(id))
        .await
        .map_err(|_| AppError::internal(request_id.0))?
        .map_err(|error| match error {
            PersistenceError::MissingShell { .. } => AppError::not_found(request_id.0),
            PersistenceError::ShellInUse { count, .. } => AppError::new(
                StatusCode::CONFLICT,
                "shell_in_use",
                if count == 1 {
                    "Shell is still pinned on a VM; unpin or delete that VM first"
                } else {
                    "Shell is still pinned on VMs; unpin or delete them first"
                },
                request_id.0,
            ),
            other => {
                tracing::error!(request_id = %request_id.0, %other, "failed to delete shell");
                AppError::internal(request_id.0)
            }
        })?;
    tracing::info!(request_id = %request_id.0, shell_id = %id, "shell deleted");
    Ok(StatusCode::NO_CONTENT)
}

fn validate_shell_write(
    name: &str,
    description: Option<&str>,
    content: &str,
) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    if !valid_shell_name(name) {
        fields.insert(
            "name".to_owned(),
            "must be 1-64 ASCII letters, numbers, '.', '_' or '-'".to_owned(),
        );
    }
    if let Some(description) = description {
        if description.len() > 512 {
            fields.insert(
                "description".to_owned(),
                "must be at most 512 characters".to_owned(),
            );
        }
    }
    validate_content(content, &mut fields);
    fields
}

fn validate_content(content: &str, fields: &mut BTreeMap<String, String>) {
    if content.is_empty() {
        fields.insert("content".to_owned(), "must not be empty".to_owned());
    } else if content.len() > MAX_SHELL_CONTENT_BYTES {
        fields.insert(
            "content".to_owned(),
            format!("must be at most {MAX_SHELL_CONTENT_BYTES} bytes"),
        );
    } else if content.contains('\0') {
        fields.insert(
            "content".to_owned(),
            "must not contain NUL bytes".to_owned(),
        );
    }
}

fn valid_shell_name(name: &str) -> bool {
    let len = name.len();
    (1..=64).contains(&len)
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use crate::templates::TemplateRegistry;
    use axum::extract::{Extension, Path, State};
    use axum::response::IntoResponse;

    async fn app_state() -> AppState {
        let dir = tempfile::tempdir().unwrap();
        let templates = TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
        AppState::with_db_file(templates, dir.path().join("test.db"))
            .await
            .expect("state")
    }

    #[tokio::test]
    async fn create_list_revise_delete_shell() {
        let state = app_state().await;
        let rid = RequestId(Uuid::new_v4());

        let (status, Json(created)) = create_shell(
            State(state.clone()),
            Extension(rid),
            ValidatedJson(CreateShellRequest {
                name: "web-init".to_owned(),
                description: Some("demo".to_owned()),
                content: "echo hi\n".to_owned(),
            }),
        )
        .await
        .expect("create");
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(created.version, 1);
        assert!(!created.content_sha256.is_empty());

        let Json(shells) = list_shells(State(state.clone()), Extension(rid))
            .await
            .expect("list");
        assert_eq!(shells.len(), 1);
        assert_eq!(shells[0].name, "web-init");
        assert_eq!(shells[0].latest_version, 1);

        let (status, Json(revised)) = create_shell_revision(
            State(state.clone()),
            Extension(rid),
            Path(created.shell_id.to_string()),
            ValidatedJson(CreateShellRevisionRequest {
                content: "echo hi2\n".to_owned(),
            }),
        )
        .await
        .expect("revise");
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(revised.version, 2);

        let Json(detail) = get_shell(
            State(state.clone()),
            Extension(rid),
            Path(created.shell_id.to_string()),
        )
        .await
        .expect("get");
        assert_eq!(detail.revisions.len(), 2);
        assert_eq!(detail.latest_content.as_deref(), Some("echo hi2\n"));

        let v1_id = detail
            .revisions
            .iter()
            .find(|r| r.version == 1)
            .expect("v1 summary")
            .id;
        let Json(past) = get_shell_revision(
            State(state.clone()),
            Extension(rid),
            Path((created.shell_id.to_string(), v1_id.to_string())),
        )
        .await
        .expect("get revision");
        assert_eq!(past.version, 1);
        assert_eq!(past.content, "echo hi\n");
        assert_eq!(past.revision_id, v1_id);

        let missing = get_shell_revision(
            State(state.clone()),
            Extension(rid),
            Path((created.shell_id.to_string(), Uuid::new_v4().to_string())),
        )
        .await
        .unwrap_err();
        assert_eq!(missing.into_response().status(), StatusCode::NOT_FOUND);

        let status = delete_shell(
            State(state.clone()),
            Extension(rid),
            Path(created.shell_id.to_string()),
        )
        .await
        .expect("delete");
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[test]
    fn rejects_a_description_longer_than_512_bytes() {
        let too_long = "x".repeat(513);
        let fields = validate_shell_write("ok", Some(&too_long), "echo hi\n");
        assert_eq!(
            fields.get("description").map(String::as_str),
            Some("must be at most 512 characters")
        );

        let at_limit = "x".repeat(512);
        let fields = validate_shell_write("ok", Some(&at_limit), "echo hi\n");
        assert!(
            !fields.contains_key("description"),
            "512 bytes must still be accepted: {fields:?}"
        );
    }

    #[tokio::test]
    async fn rejects_empty_content() {
        let state = app_state().await;
        let error = create_shell(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(CreateShellRequest {
                name: "x".to_owned(),
                description: None,
                content: String::new(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
    }
}
