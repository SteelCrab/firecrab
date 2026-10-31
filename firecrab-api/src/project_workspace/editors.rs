//! Private, authenticated OpenVSCode sessions use the retained project folder and native UID scope.
mod db;
mod package;
mod proxy;
#[cfg(test)]
mod tests;
use super::*;
use axum::http::HeaderMap;
pub(super) use db::Editor;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Open {
    view_id: Uuid,
    working_directory: String,
    file: Option<String>,
    request_id: Uuid,
}
/// Register private editor metadata and explicit open operations.
pub(super) fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/ai-sandbox/projects/{project}/workspace/editors/{view}",
        get(read).put(start),
    )
}
pub(super) use db::SCHEMA;
pub(crate) use proxy::dispatch;
/// Prevent public terminal creation from adopting a private editor UID scope.
pub(super) fn private_attempt(db: &super::db::ProjectDb, id: Uuid) -> Result<bool, Error> {
    db.private_attempt(id)
}
/// Expose routing metadata without the guest token or native attempt identity.
fn response(project: Uuid, e: &Editor) -> serde_json::Value {
    serde_json::json!({"id":e.id,"viewId":e.view,"workingDirectory":e.directory,"file":e.file,"openRequest":e.open_request,"status":e.status,"host":proxy::host(project,e.id)})
}
/// Commit the authoritative record before updating the derived routing cache.
fn persist(ctx: &files::Files, editor: &Editor) -> Result<(), Error> {
    ctx.db.save_editor(editor)?;
    ctx.state
        .workspaces
        .editor_cache
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert((ctx.binding.project_id, editor.id), editor.clone());
    Ok(())
}
/// Recheck project rights, runtime binding and execution permissions.
fn current(
    state: &AppState,
    caller: Caller,
    b: &Binding,
    e: &Editor,
    id: Uuid,
) -> Result<(), AppError> {
    if e.caller.principal != caller.principal {
        return Err(denied(id));
    }
    {
        use std::os::unix::fs::MetadataExt;
        let path = state
            .workspaces
            .root
            .join(b.project_id.to_string())
            .join("project.db");
        let metadata = fs::symlink_metadata(path).map_err(|_| {
            Error::Conflict("The retained project database is missing").response(id)
        })?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(
                Error::Conflict("The retained project database identity changed").response(id),
            );
        }
    }
    terminals::execute(state, caller, b, id)?;
    let runtime = guest::binding(state, b.vm_id)
        .map_err(|_| Error::Conflict("The editor runtime is not running").response(id))?;
    if b.generation != e.generation || runtime != e.runtime {
        return Err(Error::Conflict(
            "The editor runtime changed; explicitly reopen this private editor",
        )
        .response(id));
    }
    Ok(())
}
/// Return the caller’s retained editor metadata, including stopped runtimes.
async fn read(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path((project, view)): Path<(Uuid, Uuid)>,
    caller: Caller,
) -> Result<Json<serde_json::Value>, AppError> {
    blocking(id.0, move || {
        caller
            .project(&state, project, SandboxPermission::Read)
            .map_err(|_| Error::Denied)?;
        let b = binding(&state, project)?;
        let db = open(&state, &b)?;
        let mut e = db.editor(caller, view, true)?;
        if current(&state, caller, &b, &e, id.0).is_err() {
            e.status = "interrupted".into();
        }
        Ok(Json(response(project, &e)))
    })
    .await
}
/// Accept a bounded HTTP origin for this editor’s framing policy.
fn parent_origin(headers: &HeaderMap) -> Result<String, Error> {
    let origin = headers
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .or_else(|| {
            headers
                .get("host")
                .and_then(|v| v.to_str().ok())
                .map(|s| format!("http://{s}"))
        })
        .ok_or(Error::Invalid(
            "Editor opening requires a valid parent origin",
        ))?;
    let parsed =
        reqwest::Url::parse(&origin).map_err(|_| Error::Invalid("Invalid parent origin"))?;
    if origin.len() > 255
        || !matches!(parsed.scheme(), "http" | "https")
        || parsed.origin().ascii_serialization() != origin
        || parsed
            .host_str()
            .is_none_or(|s| s == "editor.localhost" || s.ends_with(".editor.localhost"))
    {
        return Err(Error::Invalid("Invalid editor parent origin"));
    }
    Ok(origin)
}
/// Explicitly open a fixed unprivileged editor server without replaying uncertain input.
async fn start(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path((project, view)): Path<(Uuid, Uuid)>,
    caller: Caller,
    headers: HeaderMap,
    ValidatedJson(request): ValidatedJson<Open>,
) -> Result<Json<serde_json::Value>, AppError> {
    if request.view_id != view || view.is_nil() || request.request_id.is_nil() {
        return Err(
            Error::Invalid("Use stable, non-empty view and open request identities").response(id.0),
        );
    }
    files_guest::path(&request.working_directory, true).map_err(|e| e.response(id.0))?;
    if let Some(file) = &request.file {
        files_guest::path(file, false).map_err(|e| e.response(id.0))?;
    }
    let origin = parent_origin(&headers).map_err(|e| e.response(id.0))?;
    terminals::preauthorize(&state, caller, project, id.0)?;
    let permit = state
        .workspaces
        .editor_launches
        .clone()
        .try_acquire_owned()
        .map_err(|_| Error::Busy.response(id.0))?;
    files::run(state.workspaces.file_slots.clone(),id.0,async move{
        let _permit=permit;
        let mut ctx=files::Files::open(state.clone(),project,caller,false,id.0).await?;
        terminals::execute(&state,caller,&ctx.binding,id.0)?;
        ctx.db.view(caller,view).map_err(|e|e.response(id.0))?;
        let existing=match ctx.db.editor(caller,view,true){Ok(e)=>Some(e),Err(Error::Missing)=>None,Err(e)=>return Err(e.response(id.0))};
        if let Some(mut e)=existing.clone()
            &&e.runtime==ctx.runtime&&e.generation==ctx.binding.generation&&e.directory==request.working_directory&&e.status=="ready"{
                let reply=terminals_guest::request(&mut ctx,terminals_guest::frame(e.attempt,2,0,0,0,&[]).map_err(|e|e.response(id.0))?).await;
                if reply.is_ok_and(|r|r.status==ProjectTerminalStatus::Running&&e.proof.as_ref()==Some(&r.proof)){
                    e.file=request.file;e.open_request=request.request_id;e.parent_origin=origin;persist(&ctx,&e).map_err(|e|e.response(id.0))?;
                    return Ok(Json(response(project,&e)));
                }
        }
        if let Some(old)=&existing
            &&old.runtime==ctx.runtime{
                // An uncertain launch is terminated before a fresh fixed server launch is permitted.
                let stop=match terminals_guest::request(&mut ctx,terminals_guest::frame(old.attempt,5,0,0,0,&[]).map_err(|e|e.response(id.0))?).await {
                    Ok(reply)=>Some(reply),Err(Error::Missing)if old.proof.is_none()=>None,Err(e)=>return Err(e.response(id.0)),
                };
                if stop.is_some_and(|r|r.status==ProjectTerminalStatus::Running||r.cleanup_required){return Err(Error::Conflict("The previous private editor has not terminated").response(id.0));}
        }
        let archive=package::archive(&state).await.map_err(|e|e.response(id.0))?;
        terminals::execute(&state,caller,&ctx.binding,id.0)?;
        let mut editor=Editor{id:existing.as_ref().map(|e|e.id).unwrap_or_else(Uuid::new_v4),view,caller,directory:request.working_directory.clone(),file:request.file,
            open_request:request.request_id,runtime:ctx.runtime,generation:ctx.binding.generation,attempt:Uuid::new_v4(),proof:None,
            token:format!("{}{}",Uuid::new_v4().simple(),Uuid::new_v4().simple()),parent_origin:origin,status:"starting".into()};
        persist(&ctx,&editor).map_err(|e|e.response(id.0))?;
        let frame=terminals_guest::frame(editor.attempt,1,0,24,80,editor.directory.as_bytes()).map_err(|e|e.response(id.0))?;
        let native=terminals_guest::request(&mut ctx,frame).await.map_err(|e|e.response(id.0))?;
        editor.proof=Some(native.proof);persist(&ctx,&editor).map_err(|e|e.response(id.0))?;
        current(&state,caller,&ctx.binding,&editor,id.0)?;
        package::install(&mut ctx,&archive,&editor).await.map_err(|e|e.response(id.0))?;
        let package=package::guest_package().map_err(|e|e.response(id.0))?;
        let config=serde_json::json!({"id":editor.id,"attempt":editor.attempt,"uid":editor.proof.as_ref().map(|p|p.uid),"token":editor.token});
        let setup=format!("set -eu\nbase64 -d <<'FIRECRAB_EDITOR_PREPARE' | {package}/node - {}\n{}\nFIRECRAB_EDITOR_PREPARE\n",
            STANDARD.encode(serde_json::to_vec(&config).map_err(|_|AppError::internal(id.0))?),STANDARD.encode(include_str!("editors/editor/prepare.cjs")));
        current(&state,caller,&ctx.binding,&editor,id.0)?;
        let result=guest::project_editor_protocol(&state,&ctx.runtime,&ctx.db.directory,setup.into_bytes(),1024).await.map_err(|_|Error::Conflict("Private editor preparation failed").response(id.0))?;
        if result.code!=0{return Err(Error::Conflict("Private editor data preparation was rejected").response(id.0));}
        let directory=format!("/workspace/shared/.firecrab-workspace/editors/{}",editor.id);
        let command=format!("HOME={directory}/data/home exec {package}/bin/openvscode-server --socket-path {directory}/data/server.sock --connection-token-file {directory}/token --server-data-dir {directory}/data/server --user-data-dir {directory}/data/user-data --extensions-dir {directory}/data/extensions --telemetry-level off --accept-server-license-terms\n");
        // This input is a fixed unprivileged server command. Never replay it after an uncertain response.
        let launch=terminals_guest::frame(editor.attempt,3,0,0,0,command.as_bytes()).map_err(|e|e.response(id.0))?;
        current(&state,caller,&ctx.binding,&editor,id.0)?;
        terminals_guest::request(&mut ctx,launch).await.map_err(|e|e.response(id.0))?;
        for _ in 0..50{
            current(&state,caller,&ctx.binding,&editor,id.0)?;
            let probe=format!("test -S {directory}/data/server.sock\n");
            let result=guest::project_editor_protocol(&state,&ctx.runtime,&ctx.db.directory,probe.into_bytes(),128).await;
            if result.is_ok_and(|r|r.code==0){editor.status="ready".into();persist(&ctx,&editor).map_err(|e|e.response(id.0))?;return Ok(Json(response(project,&editor)));}
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        Err(Error::Conflict("The private editor did not become ready; inspect its retained launch status before reopening").response(id.0))
    }).await
}

/// Reconcile retained editor processes without ever launching a server or replaying commands.
pub(super) fn start_maintenance(state: AppState) {
    tokio::spawn(async move {
        loop {
            let local = state.clone();
            let projects =
                tokio::task::spawn_blocking(move || local.store.sandbox_projects()).await;
            if let Ok(Ok(projects)) = projects {
                for project in projects {
                    let local = state.clone();
                    let project = project.id;
                    let records = blocking(Uuid::new_v4(), move || {
                        let b = binding(&local, project)?;
                        let db = open(&local, &b)?;
                        let records = db.editors()?;
                        let mut cache = local
                            .workspaces
                            .editor_cache
                            .lock()
                            .unwrap_or_else(|p| p.into_inner());
                        for record in &records {
                            cache.insert((project, record.id), record.clone());
                        }
                        Ok(records)
                    })
                    .await;
                    let Ok(records) = records else {
                        continue;
                    };
                    for mut record in records {
                        if !matches!(record.status.as_str(), "ready" | "starting") {
                            continue;
                        }
                        let id = Uuid::new_v4();
                        let local = state.clone();
                        let _ = files::run(state.workspaces.file_slots.clone(), id, async move {
                            let b = binding(&local, project).map_err(|e| e.response(id))?;
                            let allowed = current(&local, record.caller, &b, &record, id).is_ok();
                            let running = guest::binding(&local, b.vm_id).ok();
                            if running != Some(record.runtime) || b.generation != record.generation
                            {
                                record.status = "interrupted".into();
                                let db = open(&local, &b).map_err(|e| e.response(id))?;
                                db.save_editor(&record).map_err(|e| e.response(id))?;
                                local
                                    .workspaces
                                    .editor_cache
                                    .lock()
                                    .unwrap_or_else(|p| p.into_inner())
                                    .insert((project, record.id), record);
                                return Ok(());
                            }
                            let mut ctx = files::Files::open(
                                local.clone(),
                                project,
                                Caller::default(),
                                false,
                                id,
                            )
                            .await?;
                            let state = terminals_guest::request(
                                &mut ctx,
                                terminals_guest::frame(record.attempt, 2, 0, 0, 0, &[])
                                    .map_err(|e| e.response(id))?,
                            )
                            .await;
                            match state {
                                Ok(native) => {
                                    if let Some(proof) = &record.proof
                                        && proof != &native.proof
                                    {
                                        return Err(Error::Conflict(
                                            "Private editor process identity changed",
                                        )
                                        .response(id));
                                    }
                                    if !allowed
                                        || record.status == "starting"
                                        || native.cleanup_required
                                    {
                                        let stopped = terminals_guest::request(
                                            &mut ctx,
                                            terminals_guest::frame(record.attempt, 5, 0, 0, 0, &[])
                                                .map_err(|e| e.response(id))?,
                                        )
                                        .await
                                        .map_err(|e| e.response(id))?;
                                        if stopped.status == ProjectTerminalStatus::Running
                                            || stopped.cleanup_required
                                        {
                                            return Err(Error::Busy.response(id));
                                        }
                                        record.status = "interrupted".into();
                                        persist(&ctx, &record).map_err(|e| e.response(id))?;
                                    } else if native.status != ProjectTerminalStatus::Running {
                                        record.status = "interrupted".into();
                                        persist(&ctx, &record).map_err(|e| e.response(id))?;
                                    }
                                }
                                Err(Error::Missing) => {
                                    record.status = "interrupted".into();
                                    persist(&ctx, &record).map_err(|e| e.response(id))?;
                                }
                                Err(e) => return Err(e.response(id)),
                            }
                            Ok(())
                        })
                        .await;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
}
