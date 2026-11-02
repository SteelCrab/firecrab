//! Browser clients close independently of the retained native editor execution.
use super::*;
use axum::{extract::DefaultBodyLimit, routing::put};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Client {
    pub id: Uuid,
    pub editor: Uuid,
    pub instance: String,
    pub activations: Vec<Uuid>,
    pub window_proof: Option<Uuid>,
    pub revision: u64,
    pub command: String,
    pub status: String,
    pub operation: Option<Uuid>,
    pub operation_sha: Option<String>,
    pub error: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Register {
    instance: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Control {
    pub operation_id: Uuid,
    pub expected_revision: u64,
    pub action: String,
    pub window_proof: Uuid,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct FileProof {
    pub path: String,
    pub revision: u64,
    pub sha256: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", deny_unknown_fields)]
pub(super) enum Report {
    Active {
        revision: u64,
        #[serde(rename = "windowProof")]
        window_proof: Uuid,
    },
    Error {
        revision: u64,
        #[serde(rename = "windowProof")]
        window_proof: Uuid,
        message: String,
    },
    Flushed {
        revision: u64,
        #[serde(rename = "windowProof")]
        window_proof: Uuid,
        files: Vec<FileProof>,
        #[serde(rename = "activeFile")]
        active_file: Option<String>,
        #[serde(rename = "openFiles")]
        open_files: Vec<String>,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClosedFile {
    pub path: String,
    pub revision: u64,
    pub base_version: u64,
    pub content_base64: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Snapshot {
    pub client: Uuid,
    pub revision: u64,
    pub window_proof: Uuid,
    pub files: Vec<ClosedFile>,
    pub active_file: Option<String>,
    pub open_files: Vec<String>,
    pub created_at: u64,
    pub sha256: String,
}
pub(super) fn digest<T: Serialize>(value: &T) -> Result<String, Error> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}
pub(super) fn snapshot_sha(s: &Snapshot) -> Result<String, Error> {
    digest(&(
        s.client,
        s.revision,
        s.window_proof,
        &s.files,
        &s.active_file,
        &s.open_files,
        s.created_at,
    ))
}
pub(super) fn canonical_sha(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/ai-sandbox/projects/{project}/workspace/editors/{view}/clients/{client}",get(read).put(register))
        .route("/api/ai-sandbox/projects/{project}/workspace/editors/{view}/clients/{client}/control",put(control))
        .route("/api/ai-sandbox/projects/{project}/workspace/editors/{view}/clients/{client}/report",post(report))
        .route("/api/ai-sandbox/projects/{project}/workspace/editors/{view}/closed-snapshots",get(list))
        .route("/api/ai-sandbox/projects/{project}/workspace/editors/{view}/closed-snapshots/{client}/{revision}",get(snapshot))
        .layer(DefaultBodyLimit::max(32*1024))
}
/// Run private, bounded DB operations without requiring a live workload VM.
async fn access<T: Send + 'static>(
    state: AppState,
    caller: Caller,
    project: Uuid,
    view: Uuid,
    id: Uuid,
    write: bool,
    work: impl FnOnce(&super::super::db::ProjectDb, &Editor) -> Result<T, Error> + Send + 'static,
) -> Result<Json<T>, AppError> {
    files::run(state.workspaces.file_slots.clone(), id, async move {
        let permission = if write {
            SandboxPermission::Write
        } else {
            SandboxPermission::Read
        };
        authorize(&state, caller, project, permission, id)?;
        let b = binding(&state, project).map_err(|e| e.response(id))?;
        let db = open_as(&state, &b, caller).map_err(|e| e.response(id))?;
        let e = db.editor(caller, view, true).map_err(|e| e.response(id))?;
        let result = work(&db, &e).map_err(|e| e.response(id))?;
        authorize(&state, caller, project, permission, id)?;
        Ok(Json(result))
    })
    .await
}
async fn read(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path((project, view, client)): Path<(Uuid, Uuid, Uuid)>,
) -> Result<Json<Client>, AppError> {
    access(state, caller, project, view, id.0, false, move |db, e| {
        db.editor_client(e, client)
    })
    .await
}
async fn register(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path((project, view, client)): Path<(Uuid, Uuid, Uuid)>,
    ValidatedJson(request): ValidatedJson<Register>,
) -> Result<Json<Client>, AppError> {
    let local = state.clone();
    access(state, caller, project, view, id.0, true, move |db, e| {
        match db.editor_client(e, client) {
            Ok(c) if c.instance == request.instance => return Ok(c),
            Ok(_) => {
                return Err(Error::Conflict(
                    "Browser client belongs to another editor execution",
                ));
            }
            Err(Error::Missing) => {}
            Err(error) => return Err(error),
        }
        let b = binding(&local, project)?;
        current(&local, caller, &b, e, id.0).map_err(|_| {
            Error::Conflict("Open a running editor before registering its browser client")
        })?;
        db.register_editor_client(e, client, &request.instance)
    })
    .await
}
async fn control(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path((project, view, client)): Path<(Uuid, Uuid, Uuid)>,
    ValidatedJson(request): ValidatedJson<Control>,
) -> Result<Json<Client>, AppError> {
    access(state, caller, project, view, id.0, true, move |db, e| {
        db.control_editor_client(e, client, &request)
    })
    .await
}
async fn report(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path((project, view, client)): Path<(Uuid, Uuid, Uuid)>,
    ValidatedJson(request): ValidatedJson<Report>,
) -> Result<Json<Client>, AppError> {
    access(state, caller, project, view, id.0, true, move |db, e| {
        db.report_editor_client(e, client, &request)
    })
    .await
}
async fn list(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path((project, view)): Path<(Uuid, Uuid)>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    access(state, caller, project, view, id.0, false, move |db, e| {
        db.closed_editor_snapshots(e)
    })
    .await
}
async fn snapshot(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path((project, view, client, revision)): Path<(Uuid, Uuid, Uuid, u64)>,
) -> Result<Json<Snapshot>, AppError> {
    access(state, caller, project, view, id.0, false, move |db, e| {
        db.closed_editor_snapshot(e, client, revision).map(|r| r.0)
    })
    .await
}
