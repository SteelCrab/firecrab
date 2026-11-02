//! Bounded folder agents on the retained volume, using the existing task engine.
mod shell;
use super::*;
use crate::project_workspace::folder as native;
use serde_json::Value;

pub fn enabled(saved: &SavedTask) -> bool {
    saved.task.request.project_directory.is_some()
}
/// No command, source-copy context or dependency replay is implied by this explicit profile.
pub fn validate_request(
    project: &SandboxProjectSpec,
    input: &SandboxTaskInput,
) -> Result<(), String> {
    let Some(directory) = &input.project_directory else {
        if input.project_shell {
            return Err("Project shell requires an explicit retained folder".into());
        }
        return Ok(());
    };
    native::path(directory, "", true)?;
    if input.kind != SandboxTaskKind::Code
        || input.instruction_snapshot_id.is_none()
        || project.environment_id.is_some()
        || input.environment_role.is_some()
        || !input.coordination.dependencies.is_empty()
        || project.context != SandboxProjectContext::default()
    {
        return Err("Project file agents require saved rules, a selected user VM, and no host context, managed Environment or inherited changes".into());
    }
    Ok(())
}
pub fn target(saved: &SavedTask) -> Result<native::Target, String> {
    let directory = saved
        .task
        .request
        .project_directory
        .as_ref()
        .ok_or("No retained folder target")?;
    let context = saved
        .instructions
        .as_ref()
        .ok_or("No admitted folder instructions")?;
    let runtime = saved.task.runtime.ok_or("No admitted folder runtime")?;
    if context.snapshot.directory != *directory {
        return Err("Saved rules select a different folder".into());
    }
    native::path(directory, "", true)?;
    Ok(native::Target {
        project: saved.task.project_id,
        database: context.database_id,
        volume: context.volume_id,
        generation: context.generation,
        runtime,
        caller: saved.caller,
        directory: directory.clone(),
    })
}
/// Reject raw string fields before JSON escaping and before the public event checkpoint.
pub fn reject_credentials(call: &mut model::Call, accounts: &[SavedConnection]) {
    fn contains(value: &Value, accounts: &[SavedConnection]) -> bool {
        match value {
            Value::String(text) => redact(accounts, text) != *text,
            Value::Array(items) => items.iter().any(|value| contains(value, accounts)),
            Value::Object(fields) => fields
                .iter()
                .any(|(key, value)| redact(accounts, key) != *key || contains(value, accounts)),
            _ => false,
        }
    }
    let name = redact(accounts, &call.name);
    if name != call.name || contains(&call.arguments, accounts) {
        call.name = name;
        call.arguments = json!({"rejected":"Connected provider credentials cannot be included in file tool arguments"});
    }
}
/// Retain the server-generated operation id in the existing pre-effect event checkpoint.
pub fn prepare(saved: &mut SavedTask, call: &mut model::Call) {
    if enabled(saved) && call.name == "run_argv" {
        shell::prepare(saved, call);
        return;
    }
    if !enabled(saved) || call.name != "write_file" {
        return;
    }
    if saved.guest_pending {
        call.arguments =
            json!({"rejected":"Reconcile the prior pending effect before admitting another write"});
        return;
    }
    let Some(args) = call.arguments.as_object_mut() else {
        return;
    };
    args.insert("operationId".into(), json!(Uuid::new_v4()));
    let valid = serde_json::from_value::<WriteArgs>(call.arguments.clone()).is_ok_and(|args| {
        args.expected_version <= i64::MAX as u64
            && args.content.len() <= 1024 * 1024
            && native::path(
                saved.task.request.project_directory.as_deref().unwrap(),
                &args.path,
                false,
            )
            .is_ok()
    });
    if !valid {
        call.arguments
            .as_object_mut()
            .unwrap()
            .remove("operationId");
        return;
    }
    // Reuse the canonical pending-effect fence. Its event already contains the receipt id.
    saved.guest_command = None;
    saved.guest_pending = true;
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WriteArgs {
    path: String,
    content: String,
    expected_version: u64,
    operation_id: Uuid,
}

pub fn tools() -> Vec<Value> {
    [
        ("list_files","List allowed files in a folder relative to the admitted retained directory; empty path selects that directory.",json!({"path":{"type":"string"}})),
        ("read_file","Read a saved UTF-8 file and its exact version. A missing file has version zero for creation. Credential files are excluded.",json!({"path":{"type":"string"}})),
        ("write_file","Immediately save a UTF-8 file in the retained folder. Pass the exact version returned by read_file, or zero for a new file. A conflict requires another read; writes have durable receipts.",json!({"path":{"type":"string"},"content":{"type":"string"},"expectedVersion":{"type":"integer","minimum":0}})),
    ].into_iter().map(|(name,description,properties)|{
        let required=properties.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        json!({"name":name,"description":description,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})
    }).collect()
}
pub fn task_tools(shell: bool) -> Vec<Value> {
    let mut definitions = tools();
    if shell {
        let mut command = model::tools()
            .into_iter()
            .find(|tool| tool["name"] == "run_argv")
            .expect("fixed command tool");
        command["description"] = json!(
            "Run bounded literal argv in the selected project VM, with cwd relative to the admitted folder. A shell requires explicit argv such as /bin/sh -c. Commands edit shared files immediately. No host execution or isolated-copy staging occurs. Retained command IDs cannot execute twice."
        );
        command["parameters"]["properties"]["cwd"]["description"] =
            json!("Relative to the admitted folder; use an empty string for that folder.");
        definitions.push(command);
    }
    definitions
}
fn failure(error: crate::error::AppError) -> String {
    format!(
        "Project operation failed ({}); inspect its retained receipt before retrying",
        error.code()
    )
}
fn format_file(
    file: ProjectFile,
    relative: &str,
    accounts: &[SavedConnection],
) -> Result<Value, String> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let bytes = STANDARD
        .decode(file.content_base64)
        .map_err(|_| "Invalid retained file bytes")?;
    if bytes.len() > 32 * 1024 {
        return Err(
            "File exceeds the 32 KiB model read limit; use the editor for larger files".into(),
        );
    }
    let content = String::from_utf8(bytes).map_err(|_| "File is not UTF-8 text")?;
    Ok(
        json!({"path":relative,"projectPath":file.version.path,"version":file.version.id,
        "sha256":file.version.sha256,"content":redact(accounts,&content)}),
    )
}
fn reply(output: Value) -> Result<(String, String, Option<i32>, bool), String> {
    let encoded = serde_json::to_string(&output).map_err(|_| "Cannot encode folder result")?;
    if encoded.len() > 15 * 1024 {
        return Err("Folder result exceeds the model reply budget; select a smaller folder or use the editor".into());
    }
    Ok((encoded, String::new(), None, false))
}
pub async fn tool(
    state: &AppState,
    saved: &mut SavedTask,
    call: &model::Call,
) -> Result<(String, String, Option<i32>, bool), String> {
    if call.name == "run_argv" {
        return shell::tool(state, saved, call).await;
    }
    let target = target(saved)?;
    let id = Uuid::new_v4();
    let output=match call.name.as_str() {
        "list_files"=>{
            let args:PathArgs=serde_json::from_value(call.arguments.clone()).map_err(|_|"Invalid folder arguments")?;
            json!(native::list(state.clone(),target,args.path,id).await.map_err(failure)?)
        },
        "read_file"=>{
            let args:PathArgs=serde_json::from_value(call.arguments.clone()).map_err(|_|"Invalid file arguments")?;
            let relative=args.path.clone();
            match native::read(state.clone(),target,args.path,id).await {
                Ok(file)=>format_file(file,&relative,&state.store.ai_connections().map_err(|_|"Cannot redact retained file context")?)?,
                Err(error) if error.code()=="not_found"=>json!({"missing":true,"version":0}),
                Err(error)=>return Err(failure(error)),
            }
        },
        "write_file" => {
            let args: WriteArgs = serde_json::from_value(call.arguments.clone())
                .map_err(|_| "Invalid versioned write arguments")?;
            if !saved.guest_pending { return Err("The file write has no durable pending-effect checkpoint".into()); }
            let accounts = state.store.ai_connections().map_err(|_| "Cannot check file credentials")?;
            if redact(&accounts, &args.content) != args.content {
                saved.guest_pending = false;
                return Err("File writes cannot contain connected provider credentials".into());
            }
            let relative = args.path.clone();
            let request = native::WriteRequest {
                path: args.path, operation: args.operation_id, expected: args.expected_version,
                content: args.content.into_bytes(),
            };
            let mutation = match native::write(state.clone(), target.clone(), request.clone(), id).await {
                Ok(file) => SandboxFolderMutation {
                    operation_id: args.operation_id, path: file.version.path,
                    before_version: args.expected_version, after_version: file.version.id, sha256: file.version.sha256,
                },
                Err(error) => match native::settle(state.clone(), target, saved.caller, request, Uuid::new_v4()).await {
                    Ok(Some(mutation)) => mutation,
                    Ok(None) => { saved.guest_pending = false; return Err(failure(error)); }
                    // Leave the canonical fence set. The runner stops before the next effect.
                    Err(_) => return Err(failure(error)),
                },
            };
            saved.guest_pending = false;
            if !saved.task.folder_mutations.iter().any(|item| item.operation_id == mutation.operation_id) {
                saved.task.folder_mutations.push(mutation.clone());
            }
            json!({"path":relative,"projectPath":mutation.path,"version":mutation.after_version,
                "operationId":mutation.operation_id,"sha256":mutation.sha256})
        },
        _=>return Err("Only list_files, read_file and versioned write_file are available in the retained-folder file profile".into()),
    };
    reply(output)
}
/// Finish receipt observation even after cancellation; never rerun a model or repeat a write.
pub async fn settle(
    state: &AppState,
    saved: &mut SavedTask,
    actor: access::Caller,
) -> Result<(), String> {
    if !enabled(saved) {
        return Ok(());
    }
    shell::settle(state, saved, actor).await?;
    let target = target(saved)?;
    let writes = saved
        .task
        .events
        .iter()
        .filter(|event| event.name == "write_file")
        .filter_map(|event| serde_json::from_str::<WriteArgs>(&event.input).ok())
        .collect::<Vec<_>>();
    let mut mutations = Vec::new();
    for args in writes {
        // A completed native worker may still hold the same short project database lease.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            let result = native::settle(
                state.clone(),
                target.clone(),
                actor,
                native::WriteRequest {
                    path: args.path.clone(),
                    operation: args.operation_id,
                    expected: args.expected_version,
                    content: args.content.as_bytes().to_vec(),
                },
                Uuid::new_v4(),
            )
            .await;
            match result {
                Ok(Some(mutation)) => {
                    mutations.push(mutation);
                    break;
                }
                Ok(None) => break,
                Err(error)
                    if error.code() == "workspace_busy"
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(100)).await
                }
                Err(error) => return Err(failure(error)),
            }
        }
    }
    saved.task.folder_mutations = mutations;
    saved.guest_pending = false;
    Ok(())
}

#[cfg(test)]
mod tests;
