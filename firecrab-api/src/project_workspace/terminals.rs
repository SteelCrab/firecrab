//! Multiple real PTYs share the retained folder, with durable replay and fenced input ownership.
use super::{terminals_db::Terminal, terminals_guest as wire, *};
use axum::{
    extract::{
        DefaultBodyLimit, Query,
        ws::{Message, WebSocketUpgrade},
    },
    response::Response,
};
use futures_util::{SinkExt, StreamExt};

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    #[serde(default)]
    cursor: u64,
}

/// Register project PTY lifecycle, replay and input-control endpoints.
pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/terminals",
            get(list),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/terminals/{session}",
            get(metadata).put(create),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/terminals/{session}/output",
            get(output),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/terminals/{session}/control",
            post(claim).put(renew).delete(release),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/terminals/{session}/input",
            post(input),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/terminals/{session}/stop",
            post(stop),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/terminals/{session}/socket",
            get(socket),
        )
        .layer(DefaultBodyLimit::max(32 * 1024))
}

/// Schedule prompt replay for the affected project without launching a process.
fn wake(state: &AppState, project: Uuid) {
    state
        .workspaces
        .terminal_projects
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(project);
    state.workspaces.terminal_wake.notify_one();
}
/// Require the selected project runtime and all project/VM execution rights.
pub(super) fn execute(
    state: &AppState,
    caller: Caller,
    b: &Binding,
    id: Uuid,
) -> Result<(), AppError> {
    let project = authorize(state, caller, b.project_id, SandboxPermission::Read, id)?;
    if project.spec.vm_id != Some(b.vm_id) {
        return Err(Error::Conflict(
            "Reconcile the project's selected runtime before terminal execution",
        )
        .response(id));
    }
    caller
        .require_all(
            state,
            &[
                (
                    SandboxResource::Project { id: b.project_id },
                    SandboxPermission::Read,
                ),
                (
                    SandboxResource::Project { id: b.project_id },
                    SandboxPermission::Write,
                ),
                (
                    SandboxResource::Project { id: b.project_id },
                    SandboxPermission::Execute,
                ),
                (SandboxResource::Vm { id: b.vm_id }, SandboxPermission::Read),
                (
                    SandboxResource::Vm { id: b.vm_id },
                    SandboxPermission::Execute,
                ),
            ],
        )
        .map_err(|_| denied(id))
}
/// Reject missing project execution rights before opening a guest context.
pub(super) fn preauthorize(
    state: &AppState,
    caller: Caller,
    project: Uuid,
    id: Uuid,
) -> Result<(), AppError> {
    authorize(state, caller, project, SandboxPermission::Read, id)?;
    let binding = binding(state, project).map_err(|e| e.response(id))?;
    execute(state, caller, &binding, id)
}
/// Reject an old process record after a runtime or binding generation change.
fn live(record: &Terminal, context: &files::Files) -> Result<(), Error> {
    if record.project != context.binding.project_id
        || record.runtime != context.runtime
        || record.generation != context.binding.generation
        || record.status != ProjectTerminalStatus::Running
    {
        return Err(Error::Conflict(
            "This terminal has no confirmed live process in the current project runtime",
        ));
    }
    Ok(())
}
/// Project runtime changes into interrupted history without replaying commands.
fn observed(state: &AppState, binding: &Binding, mut record: Terminal) -> Terminal {
    let vm = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&record.runtime.vm_id)
        .cloned();
    if matches!(
        record.status,
        ProjectTerminalStatus::Pending | ProjectTerminalStatus::Running
    ) && (binding.generation != record.generation
        || binding.vm_id != record.runtime.vm_id
        || vm.as_ref().is_none_or(|v| {
            v.state != VmState::Running || v.last_runtime_id != Some(record.runtime.runtime_id)
        }))
    {
        record.status = ProjectTerminalStatus::Interrupted;
        record.cleanup_required = vm.is_some_and(|v| {
            v.state == VmState::Error && v.last_runtime_id == Some(record.runtime.runtime_id)
        });
    }
    record
}
/// Authorize project reads before opening retained terminal records.
fn read_db(
    state: &AppState,
    caller: Caller,
    project: Uuid,
    id: Uuid,
) -> Result<(Binding, db::ProjectDb), AppError> {
    authorize(state, caller, project, SandboxPermission::Read, id)?;
    let b = binding(state, project).map_err(|e| e.response(id))?;
    let db = open(state, &b).map_err(|e| e.response(id))?;
    authorize(state, caller, project, SandboxPermission::Read, id)?;
    Ok((b, db))
}
/// List retained terminal identities without exposing input control tokens.
async fn list(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path(project): Path<Uuid>,
    caller: Caller,
) -> Result<Json<Vec<ProjectTerminal>>, AppError> {
    tokio::task::spawn_blocking(move || {
        let (b, db) = read_db(&state, caller, project, rid.0)?;
        db.terminals()
            .and_then(|records| {
                records
                    .into_iter()
                    .map(|r| db.terminal_response(&observed(&state, &b, r), state.workspaces.owner))
                    .collect()
            })
            .map(Json)
            .map_err(|e| e.response(rid.0))
    })
    .await
    .map_err(|_| AppError::internal(rid.0))?
}
/// Return one authorized terminal’s process and control metadata.
async fn metadata(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path((project, session)): Path<(Uuid, Uuid)>,
    caller: Caller,
) -> Result<Json<ProjectTerminal>, AppError> {
    tokio::task::spawn_blocking(move || {
        let (b, db) = read_db(&state, caller, project, rid.0)?;
        let record = db.terminal(session).map_err(|e| e.response(rid.0))?;
        db.terminal_response(&observed(&state, &b, record), state.workspaces.owner)
            .map(Json)
            .map_err(|e| e.response(rid.0))
    })
    .await
    .map_err(|_| AppError::internal(rid.0))?
}
/// Read a bounded retained output page at an absolute byte cursor.
async fn output(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path((project, session)): Path<(Uuid, Uuid)>,
    caller: Caller,
    Query(cursor): Query<Cursor>,
) -> Result<Json<ProjectTerminalOutput>, AppError> {
    replay(state, caller, project, session, cursor.cursor, rid.0)
        .await
        .map(Json)
}
/// Read one consistent metadata/output snapshot under project ownership.
async fn snapshot(
    state: AppState,
    caller: Caller,
    project: Uuid,
    session: Uuid,
    cursor: u64,
    id: Uuid,
) -> Result<(ProjectTerminal, ProjectTerminalOutput), AppError> {
    tokio::task::spawn_blocking(move || {
        let (binding, db) = read_db(&state, caller, project, id)?;
        let record = db.terminal(session).map_err(|e| e.response(id))?;
        let page = db
            .terminal_output(&record, cursor)
            .map_err(|e| e.response(id))?;
        let metadata = db
            .terminal_response(&observed(&state, &binding, record), state.workspaces.owner)
            .map_err(|e| e.response(id))?;
        Ok((metadata, page))
    })
    .await
    .map_err(|_| AppError::internal(id))?
}
/// Poll a running guest only when required, otherwise serve retained history.
async fn replay(
    state: AppState,
    caller: Caller,
    project: Uuid,
    session: Uuid,
    cursor: u64,
    id: Uuid,
) -> Result<ProjectTerminalOutput, AppError> {
    snapshot(state, caller, project, session, cursor, id)
        .await
        .map(|(_, page)| page)
}

/// Admit a stable shell UUID before a native launch; exact retries never launch twice.
async fn create(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path((project, session)): Path<(Uuid, Uuid)>,
    caller: Caller,
    ValidatedJson(request): ValidatedJson<CreateProjectTerminal>,
) -> Result<Json<ProjectTerminal>, AppError> {
    if session.is_nil()
        || request.label.trim().is_empty()
        || request.label.chars().count() > 64
        || request.label.chars().any(char::is_control)
    {
        return Err(Error::Invalid(
            "Use a session identity and a terminal name of 1–64 characters",
        )
        .response(rid.0));
    }
    files_guest::path(&request.working_directory, true).map_err(|e| e.response(rid.0))?;
    wire::frame(
        session,
        1,
        0,
        request.rows,
        request.columns,
        request.working_directory.as_bytes(),
    )
    .map_err(|e| e.response(rid.0))?;
    files::run(state.workspaces.file_slots.clone(), rid.0, async move {
        preauthorize(&state, caller, project, rid.0)?;
        let mut context = files::Files::open(state.clone(), project, caller, false, rid.0).await?;
        execute(&state, caller, &context.binding, rid.0)?;
        match context.db.terminal(session) {
            Ok(record) => {
                if record.request != request || record.creator.principal != caller.principal {
                    return Err(Error::Conflict(
                        "Session identity belongs to a different creation request",
                    )
                    .response(rid.0));
                }
                wake(&state, project);
                return context
                    .db
                    .terminal_response(
                        &observed(&state, &context.binding, record),
                        state.workspaces.owner,
                    )
                    .map(Json)
                    .map_err(|e| e.response(rid.0));
            }
            Err(Error::Missing) => {}
            Err(e) => return Err(e.response(rid.0)),
        }
        if context
            .db
            .terminals()
            .map_err(|e| e.response(rid.0))?
            .iter()
            .filter(|r| {
                matches!(
                    r.status,
                    ProjectTerminalStatus::Pending | ProjectTerminalStatus::Running
                ) && r.runtime == context.runtime
            })
            .count()
            >= 8
        {
            return Err(
                Error::Conflict("A project supports at most eight active terminals")
                    .response(rid.0),
            );
        }
        let mut record = Terminal {
            id: session,
            project,
            request: request.clone(),
            creator: caller,
            runtime: context.runtime,
            generation: context.binding.generation,
            proof: None,
            status: ProjectTerminalStatus::Pending,
            start: 0,
            end: 0,
            guest_end: 0,
            truncated: false,
            cleanup_required: false,
            exit: None,
            dimensions: None,
        };
        context
            .db
            .admit_terminal(&record)
            .map_err(|e| e.response(rid.0))?;
        wake(&state, project);
        let packet = wire::frame(
            session,
            1,
            0,
            request.rows,
            request.columns,
            request.working_directory.as_bytes(),
        )
        .map_err(|e| e.response(rid.0))?;
        let reply = wire::request(&mut context, packet)
            .await
            .map_err(|e| e.response(rid.0))?;
        execute(&state, caller, &context.binding, rid.0)?;
        context
            .db
            .cache_terminal(&mut record, reply)
            .map_err(|e| e.response(rid.0))?;
        context
            .db
            .terminal_response(&record, state.workspaces.owner)
            .map(Json)
            .map_err(|e| e.response(rid.0))
    })
    .await
}
/// Explicitly acquire or take over a generation-fenced input lease.
async fn claim(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path((project, session)): Path<(Uuid, Uuid)>,
    caller: Caller,
    ValidatedJson(request): ValidatedJson<ClaimProjectTerminal>,
) -> Result<Json<ProjectTerminalLease>, AppError> {
    files::run(state.workspaces.file_slots.clone(), rid.0, async move {
        preauthorize(&state, caller, project, rid.0)?;
        let context = files::Files::open(state.clone(), project, caller, false, rid.0).await?;
        execute(&state, caller, &context.binding, rid.0)?;
        let record = context
            .db
            .terminal(session)
            .map_err(|e| e.response(rid.0))?;
        live(&record, &context).map_err(|e| e.response(rid.0))?;
        context
            .db
            .claim_terminal(session, state.workspaces.owner, caller, request)
            .map(Json)
            .map_err(|e| e.response(rid.0))
    })
    .await
}
/// Renew or release only the current caller, client and API-owner lease.
async fn renewal(
    state: AppState,
    caller: Caller,
    project: Uuid,
    session: Uuid,
    control: ProjectTerminalControl,
    release: bool,
    id: Uuid,
) -> Result<(), AppError> {
    files::run(state.workspaces.file_slots.clone(), id, async move {
        preauthorize(&state, caller, project, id)?;
        let context = files::Files::open(state.clone(), project, caller, false, id).await?;
        execute(&state, caller, &context.binding, id)?;
        live(
            &context.db.terminal(session).map_err(|e| e.response(id))?,
            &context,
        )
        .map_err(|e| e.response(id))?;
        context
            .db
            .renew_terminal(session, state.workspaces.owner, caller, &control, release)
            .map_err(|e| e.response(id))
    })
    .await
}
/// Extend the current input lease without accepting stale ownership.
async fn renew(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path((project, session)): Path<(Uuid, Uuid)>,
    caller: Caller,
    ValidatedJson(control): ValidatedJson<ProjectTerminalControl>,
) -> Result<Json<serde_json::Value>, AppError> {
    renewal(state, caller, project, session, control, false, rid.0).await?;
    Ok(Json(serde_json::json!({"renewed":true})))
}
/// Release control without terminating the retained shell.
async fn release(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path((project, session)): Path<(Uuid, Uuid)>,
    caller: Caller,
    ValidatedJson(control): ValidatedJson<ProjectTerminalControl>,
) -> Result<Json<serde_json::Value>, AppError> {
    renewal(state, caller, project, session, control, true, rid.0).await?;
    Ok(Json(serde_json::json!({"released":true})))
}
enum Action {
    Input(Vec<u8>),
    Resize(u16, u16),
    Stop,
}
/// Dispatch one authorized input, resize or stop without replaying uncertain input.
async fn action(
    state: AppState,
    caller: Caller,
    project: Uuid,
    session: Uuid,
    control: ProjectTerminalControl,
    action: Action,
    id: Uuid,
) -> Result<ProjectTerminal, AppError> {
    files::run(state.workspaces.file_slots.clone(), id, async move {
        preauthorize(&state, caller, project, id)?;
        let mut context = files::Files::open(state.clone(), project, caller, false, id).await?;
        execute(&state, caller, &context.binding, id)?;
        let mut record = context.db.terminal(session).map_err(|e| e.response(id))?;
        live(&record, &context).map_err(|e| e.response(id))?;
        context
            .db
            .check_terminal_control(session, state.workspaces.owner, caller, &control)
            .map_err(|e| e.response(id))?;
        let packet = match action {
            Action::Input(bytes) => wire::frame(session, 3, record.end, 0, 0, &bytes),
            Action::Resize(rows, columns) => {
                wire::frame(session, 4, record.end, rows, columns, &[])
            }
            Action::Stop => wire::frame(session, 5, record.end, 0, 0, &[]),
        }
        .map_err(|e| e.response(id))?;
        execute(&state, caller, &context.binding, id)?;
        let reply = wire::request(&mut context, packet)
            .await
            .map_err(|e| e.response(id))?;
        context
            .db
            .cache_terminal(&mut record, reply)
            .map_err(|e| e.response(id))?;
        execute(&state, caller, &context.binding, id)?;
        wake(&state, project);
        context
            .db
            .terminal_response(&record, state.workspaces.owner)
            .map_err(|e| e.response(id))
    })
    .await
}
/// Interpret bounded typed WebSocket input under the same execution policy.
async fn message(
    state: AppState,
    caller: Caller,
    project: Uuid,
    session: Uuid,
    message: ProjectTerminalMessage,
    id: Uuid,
) -> Result<(), AppError> {
    match message {
        ProjectTerminalMessage::Input {
            control,
            content_base64,
        } => {
            if content_base64.len() > 21848 {
                return Err(Error::Invalid("Terminal input exceeds 16 KiB").response(id));
            }
            let bytes = STANDARD
                .decode(content_base64)
                .map_err(|_| Error::Invalid("Invalid terminal input encoding").response(id))?;
            action(
                state,
                caller,
                project,
                session,
                control,
                Action::Input(bytes),
                id,
            )
            .await?;
        }
        ProjectTerminalMessage::Resize {
            control,
            rows,
            columns,
        } => {
            action(
                state,
                caller,
                project,
                session,
                control,
                Action::Resize(rows, columns),
                id,
            )
            .await?;
        }
        ProjectTerminalMessage::Renew { control } => {
            renewal(state, caller, project, session, control, false, id).await?;
        }
    }
    Ok(())
}
/// Forward a single authenticated input message to its controlled PTY.
async fn input(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path((project, session)): Path<(Uuid, Uuid)>,
    caller: Caller,
    ValidatedJson(value): ValidatedJson<ProjectTerminalMessage>,
) -> Result<Json<serde_json::Value>, AppError> {
    message(state, caller, project, session, value, rid.0).await?;
    Ok(Json(serde_json::json!({"accepted":true})))
}
/// Terminate only the controlled native UID scope.
async fn stop(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path((project, session)): Path<(Uuid, Uuid)>,
    caller: Caller,
    ValidatedJson(control): ValidatedJson<ProjectTerminalControl>,
) -> Result<Json<ProjectTerminal>, AppError> {
    action(
        state,
        caller,
        project,
        session,
        control,
        Action::Stop,
        rid.0,
    )
    .await
    .map(Json)
}
/// Stream retained output and recheck grants while accepting fenced input.
async fn socket(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path((project, session)): Path<(Uuid, Uuid)>,
    caller: Caller,
    Query(cursor): Query<Cursor>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, AppError> {
    replay(
        state.clone(),
        caller,
        project,
        session,
        cursor.cursor,
        rid.0,
    )
    .await?;
    Ok(upgrade.max_message_size(32*1024).protocols(["firecrab.terminal.v1"]).on_upgrade(move|socket|async move {
        let (mut tx,mut rx)=socket.split();let mut cursor=cursor.cursor;
        let mut last_metadata = String::new();
        let mut interval=tokio::time::interval(std::time::Duration::from_millis(250));
        loop {
            tokio::select! {
                _=interval.tick()=> {
                    match snapshot(state.clone(),caller,project,session,cursor,rid.0).await {
                        Ok((metadata,page))=> { cursor=page.next;
                            let status = serde_json::json!({"type":"state","terminal":metadata}).to_string();
                            if status != last_metadata {
                                last_metadata = status.clone();
                                if tx.send(Message::Text(status.into())).await.is_err() { break; }
                            }
                            if (page.next>page.start||page.truncated)&&tx.send(Message::Text(serde_json::json!({"type":"output","output":page}).to_string().into())).await.is_err() { break; }
                        }
                        Err(error) if error.code() == "workspace_busy" => {},
                        Err(_)=> { let _=tx.send(Message::Close(None)).await;break; }
                    }
                }
                received=rx.next()=>match received {
                    Some(Ok(Message::Text(text)))=> {
                        let parsed=serde_json::from_str::<ProjectTerminalMessage>(&text);
                        let result=match parsed {Ok(value)=>message(state.clone(),caller,project,session,value,rid.0).await,Err(_)=>Err(Error::Invalid("Invalid terminal message").response(rid.0))};
                        if result.is_err() { let _=tx.send(Message::Text(serde_json::json!({"type":"control_error","message":"Input was not confirmed. Recheck control ownership; do not replay previous input."}).to_string().into())).await; }
                    }
                    Some(Ok(Message::Ping(bytes)))=>{if tx.send(Message::Pong(bytes)).await.is_err(){break;}}
                    Some(Ok(Message::Pong(_)))=>{},_=>break,
                }
            }
        }
    }))
}

/// Start after VM reconciliation. Guest supervisors retain output while the API is absent.
pub(super) fn start(state: AppState) {
    tokio::spawn(async move {
        let initial = state.clone();
        let discovered = tokio::task::spawn_blocking(move || -> Result<Vec<Uuid>, Error> {
            let mut found = Vec::new();
            for project in initial.store.sandbox_projects()? {
                if let Ok(b) = binding(&initial, project.id)
                    && !b.provisioning
                    && let Ok(db) = open(&initial, &b)
                    && let Ok(records) = db.terminals()
                    && records.iter().any(needs_refresh)
                {
                    found.push(project.id);
                }
            }
            Ok(found)
        })
        .await;
        if let Ok(Ok(projects)) = discovered {
            for project in projects {
                wake(&state, project);
            }
        }
        loop {
            let projects = state
                .workspaces
                .terminal_projects
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .iter()
                .copied()
                .collect::<Vec<_>>();
            let mut active = false;
            for project in projects {
                let local = state.clone();
                let rid = Uuid::new_v4();
                let result = files::run(state.workspaces.file_slots.clone(), rid, async move {
                    let mut context =
                        files::Files::open(local.clone(), project, Caller::default(), false, rid)
                            .await?;
                    let records = context.db.terminals().map_err(|e| e.response(rid))?;
                    let mut more = false;
                    for mut record in records.into_iter().filter(needs_refresh) {
                        let request = wire::frame(record.id, 2, record.end, 0, 0, &[])
                            .map_err(|e| e.response(rid))?;
                        match wire::request(&mut context, request).await {
                            Ok(reply) => {
                                let changed = reply.end > record.end;
                                context
                                    .db
                                    .cache_terminal(&mut record, reply)
                                    .map_err(|e| e.response(rid))?;
                                let revoked =
                                    execute(&local, record.creator, &context.binding, rid).is_err();
                                if (record.cleanup_required || revoked)
                                    && record.runtime == context.runtime
                                    && record.generation == context.binding.generation
                                {
                                    let stop = wire::frame(record.id, 5, record.end, 0, 0, &[])
                                        .map_err(|e| e.response(rid))?;
                                    if let Ok(reply) = wire::request(&mut context, stop).await {
                                        context
                                            .db
                                            .cache_terminal(&mut record, reply)
                                            .map_err(|e| e.response(rid))?;
                                    }
                                }
                                more |= changed || record.end < record.guest_end;
                            }
                            Err(Error::Missing)
                                if record.status == ProjectTerminalStatus::Pending =>
                            {
                                // An uncertain launch is observed, never dispatched again.
                                if record.runtime != context.runtime
                                    || record.generation != context.binding.generation
                                {
                                    record.status = ProjectTerminalStatus::Interrupted;
                                    context
                                        .db
                                        .save_terminal(&record)
                                        .map_err(|e| e.response(rid))?;
                                }
                            }
                            Err(_) => {
                                more = true;
                            }
                        }
                    }
                    if !context
                        .db
                        .terminals()
                        .map_err(|e| e.response(rid))?
                        .iter()
                        .any(needs_refresh)
                    {
                        local
                            .workspaces
                            .terminal_projects
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .remove(&project);
                    }
                    Ok(more)
                })
                .await;
                active |= matches!(result, Ok(true));
            }
            tokio::select! {
                _=state.workspaces.terminal_wake.notified()=>{},
                _=tokio::time::sleep(std::time::Duration::from_millis(if active {100}else{1000}))=>{},
            }
        }
    });
}
/// Keep recovery active while a process or uncommitted output still needs reconciliation.
fn needs_refresh(record: &Terminal) -> bool {
    matches!(
        record.status,
        ProjectTerminalStatus::Pending | ProjectTerminalStatus::Running
    ) || record.cleanup_required
        || record.end < record.guest_end
}
