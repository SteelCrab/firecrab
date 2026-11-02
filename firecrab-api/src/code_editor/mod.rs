//! Authenticated, origin-isolated browser access to one live CODE runtime.
mod gateway;
mod native;
mod terminal;
use crate::{
    ai_sandbox::{access::Caller, guest},
    error::AppError,
    extract::ValidatedJson,
    persistence::code_runtime::Record,
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Redirect,
    routing::get,
};
pub(crate) use gateway::isolate;
use serde::Deserialize;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::watch;
use uuid::Uuid;

#[derive(Default)]
pub(crate) struct Editors {
    entries: Mutex<HashMap<Uuid, Arc<Session>>>,
    tickets: Mutex<HashMap<String, Ticket>>,
}
impl std::fmt::Debug for Editors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Editors").finish_non_exhaustive()
    }
}

struct Ticket {
    session: Arc<Session>,
    terminal: bool,
    expires: Instant,
}
pub(crate) struct Session {
    workspace: firecrab_api_types::CodeWorkspace,
    record: Record,
    binding: guest::Binding,
    phase: Mutex<&'static str>,
    cancel: watch::Sender<bool>,
    slots: Arc<tokio::sync::Semaphore>,
    site: String,
    client: reqwest::Client,
    socket: PathBuf,
    cookie: String,
    expires: Instant,
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.cancel.send(true);
        let _ = std::fs::remove_file(&self.socket);
        if let Some(dir) = self.socket.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }
}
use firecrab_api_types::{CodeEditorPhase, CodeEditorPrepare as Prepare, CodeEditorStatus as View};
fn error(id: RequestId, code: &'static str) -> AppError {
    AppError::new(
        StatusCode::CONFLICT,
        code,
        "Read the current ready workspace before opening its editor",
        id.0,
    )
}
fn operator(caller: Caller, id: RequestId) -> Result<(), AppError> {
    caller.operator().map_err(|_| {
        AppError::new(
            StatusCode::FORBIDDEN,
            "code_operator_required",
            "Only the controller operator can open CODE tools",
            id.0,
        )
    })
}
fn token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
fn current(state: &AppState, session: &Session) -> bool {
    state
        .store
        .code_workspace(session.record.runtime.workspace_id)
        .ok()
        .flatten()
        .is_some_and(|w| {
            w.revision == session.workspace.revision && w.spec == session.workspace.spec
        })
        && state
            .store
            .code_runtime(session.record.runtime.workspace_id)
            .ok()
            .flatten()
            .is_some_and(|r| r.runtime == session.record.runtime && r.spec == session.record.spec)
        && crate::code_runtime::auth_binding(state, &session.record).ok() == Some(session.binding)
}
fn view(s: &Session) -> View {
    View {
        workspace_id: s.record.runtime.workspace_id,
        revision: s.workspace.revision,
        vm_id: s.binding.vm_id,
        runtime_id: s.binding.runtime_id,
        operation_id: s.record.runtime.operation_id,
        phase: match *s.phase.lock().unwrap_or_else(|p| p.into_inner()) {
            "preparing" => CodeEditorPhase::Preparing,
            "ready" => CodeEditorPhase::Ready,
            _ => CodeEditorPhase::Failed,
        },
        version: native::VERSION.into(),
    }
}
fn find(state: &AppState, key: Uuid) -> Option<Arc<Session>> {
    let mut entries = state
        .code_editor
        .entries
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let s = entries.get(&key)?.clone();
    if s.expires <= Instant::now() || !current(state, &s) {
        let _ = s.cancel.send(true);
        entries.remove(&key);
        return None;
    }
    Some(s)
}
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/code/workspaces/{id}/editor",
            get(status).post(prepare),
        )
        .route("/api/code/workspaces/{id}/editor/open", get(open))
        .layer(axum::middleware::from_fn(
            crate::handlers::secrets::no_store,
        ))
}
async fn status(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<Uuid>,
) -> Result<Json<Option<View>>, AppError> {
    operator(caller, id)?;
    Ok(Json(find(&state, key).map(|s| view(&s))))
}
async fn prepare(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<Uuid>,
    headers: HeaderMap,
    ValidatedJson(input): ValidatedJson<Prepare>,
) -> Result<(StatusCode, Json<View>), AppError> {
    operator(caller, id)?;
    if gateway::target(&headers, key, "0123456789abcdef").is_none() {
        return Err(error(id, "code_editor_origin_required"));
    }

    let w = state
        .store
        .code_workspace(key)
        .map_err(|_| AppError::internal(id.0))?
        .ok_or_else(|| AppError::not_found(id.0))?;
    let record = state
        .store
        .code_runtime(key)
        .map_err(|_| AppError::internal(id.0))?
        .ok_or_else(|| error(id, "code_editor_runtime_required"))?;
    if w.revision != input.expected_revision
        || record.runtime.vm_id != input.vm_id
        || record.runtime.runtime_id != Some(input.runtime_id)
        || record.runtime.operation_id != input.operation_id
    {
        return Err(error(id, "code_editor_binding_changed"));
    }
    let binding = crate::code_runtime::auth_binding(&state, &record)
        .map_err(|_| error(id, "code_editor_runtime_required"))?;
    // Compatible AUTH/rename edits may advance configuration without changing the retained checkout.

    if let Some(s) = find(&state, key)
        && *s.phase.lock().unwrap_or_else(|p| p.into_inner()) != "failed"
    {
        return Ok((StatusCode::ACCEPTED, Json(view(&s))));
    }
    let dir = PathBuf::from(format!("/tmp/fc-editor-{}", Uuid::new_v4().simple()));
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .map_err(|_| AppError::internal(id.0))?;
    let (cancel, _) = watch::channel(false);
    let client = reqwest::Client::builder()
        .no_proxy()
        .pool_max_idle_per_host(8)
        .unix_socket(dir.join("proxy.sock"))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|_| AppError::internal(id.0))?;
    let s = Arc::new(Session {
        slots: Arc::new(tokio::sync::Semaphore::new(32)),
        site: Uuid::new_v4().simple().to_string()[..16].to_owned(),
        client,
        workspace: w,
        record,
        binding,
        phase: Mutex::new("preparing"),
        cancel,
        socket: dir.join("proxy.sock"),
        cookie: token(),
        expires: Instant::now() + Duration::from_secs(3600),
    });
    // Serialize admission after validation: duplicate prepares share the same bootstrap and session.
    {
        let mut entries = state
            .code_editor
            .entries
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = entries.get(&key).filter(|s| {
            s.expires > Instant::now()
                && *s.phase.lock().unwrap_or_else(|p| p.into_inner()) != "failed"
        }) {
            return Ok((StatusCode::ACCEPTED, Json(view(existing))));
        }
        entries.insert(key, s.clone());
    }
    let result = view(&s);
    tokio::spawn(native::run(state, s));
    Ok((StatusCode::ACCEPTED, Json(result)))
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Open {
    #[serde(default)]
    terminal: bool,
}
async fn open(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<Uuid>,
    Query(mode): Query<Open>,
    headers: HeaderMap,
) -> Result<Redirect, AppError> {
    operator(caller, id)?;
    let s = find(&state, key)
        .filter(|s| *s.phase.lock().unwrap_or_else(|p| p.into_inner()) == "ready")
        .ok_or_else(|| error(id, "code_editor_not_ready"))?;
    let target = gateway::target(&headers, key, &s.site)
        .ok_or_else(|| error(id, "code_editor_origin_required"))?;
    let ticket = token();
    let mut tickets = state
        .code_editor
        .tickets
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    tickets.retain(|_, t| t.expires > Instant::now());
    if tickets.len() >= 256 {
        return Err(error(id, "code_editor_busy"));
    }
    tickets.insert(
        ticket.clone(),
        Ticket {
            session: s,
            terminal: mode.terminal,
            expires: Instant::now() + Duration::from_secs(30),
        },
    );
    Ok(Redirect::to(&format!("{target}/_fc/connect/{ticket}")))
}
#[cfg(test)]
mod tests;
