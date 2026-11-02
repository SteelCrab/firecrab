//! Bounded commands share the Sandbox pending-effect fence and event journal.
use super::*;
use native::command as wire;

pub(super) fn arguments(value: &Value) -> Result<(Uuid, SandboxGuestCommand), String> {
    let mut object = value
        .as_object()
        .ok_or("Invalid command arguments")?
        .clone();
    let operation = serde_json::from_value(
        object
            .remove("operationId")
            .ok_or("Command has no admitted identity")?,
    )
    .map_err(|_| "Invalid command identity")?;
    let input: super::super::ArgvArgs = serde_json::from_value(Value::Object(object))
        .map_err(|_| "Invalid bounded command arguments")?;
    let command = input.request()?;
    super::super::guest_protocol::validate(&command)
        .map_err(|_| "Invalid command limits or arguments")?;
    if operation == Uuid::nil() {
        return Err("Command identity is empty".into());
    }
    Ok((operation, command))
}
pub(super) fn prepare(saved: &mut SavedTask, call: &mut model::Call) {
    if !saved.task.request.project_shell || saved.guest_pending {
        return;
    }
    let Some(fields) = call.arguments.as_object_mut() else {
        return;
    };
    fields.insert("operationId".into(), json!(Uuid::new_v4()));
    if let Ok((operation, command)) = arguments(&call.arguments)
        && native::path(
            saved.task.request.project_directory.as_deref().unwrap(),
            &command.cwd,
            true,
        )
        .is_ok()
    {
        saved.guest_command = Some(operation);
        saved.guest_pending = true;
    } else {
        call.arguments
            .as_object_mut()
            .unwrap()
            .remove("operationId");
    }
}
fn output(
    reply: &wire::Reply,
    accounts: &[SavedConnection],
) -> (String, String, Option<i32>, bool) {
    let stdout = redact(accounts, &String::from_utf8_lossy(&reply.stdout));
    let mut stderr = redact(accounts, &String::from_utf8_lossy(&reply.stderr));
    if reply.truncated {
        stderr.push_str("\n[Output reached the admitted retention limit.]");
    }
    if reply.state == 3 {
        stderr.push_str("\n[Command interrupted; no successful completion is assumed.]");
    }
    (
        stdout,
        stderr,
        reply.exit,
        reply.exit != Some(0) || reply.state != 2,
    )
}
pub(super) async fn tool(
    state: &AppState,
    saved: &mut SavedTask,
    call: &model::Call,
) -> Result<(String, String, Option<i32>, bool), String> {
    if !saved.task.request.project_shell {
        return Err("Shell commands were not enabled for this request".into());
    }
    let (operation, command) = arguments(&call.arguments)?;
    if !saved.guest_pending || saved.guest_command != Some(operation) {
        return Err("Command has no durable pending-effect checkpoint".into());
    }
    let target = target(saved)?;
    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(command.limits.wall_seconds + 10);
    let mut action = 1;
    loop {
        let reply = wire::request(
            state.clone(),
            target.clone(),
            saved.caller,
            operation,
            command.clone(),
            action,
            Uuid::new_v4(),
        )
        .await
        .map_err(failure)?;
        if reply.state >= 2 {
            let accounts = state
                .store
                .ai_connections()
                .map_err(|_| "Cannot redact command output")?;
            // Unknown outcomes still pass through explicit receipt/UID reconciliation.
            if reply.state == 2 && !reply.cleanup_required {
                saved.guest_pending = false;
            }
            return Ok(output(&reply, &accounts));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("Project command deadline requires receipt reconciliation".into());
        }
        action = 2;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
pub(super) async fn settle(
    state: &AppState,
    saved: &mut SavedTask,
    actor: access::Caller,
) -> Result<(), String> {
    let Some(operation) = saved.guest_command else {
        return Ok(());
    };
    if !saved.guest_pending {
        return Ok(());
    }
    let (index, command) = saved
        .task
        .events
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, e)| e.name == "run_argv")
        .find_map(|(i, e)| {
            serde_json::from_str::<Value>(&e.input)
                .ok()
                .and_then(|v| arguments(&v).ok())
                .filter(|(id, _)| *id == operation)
                .map(|(_, c)| (i, c))
        })
        .ok_or("The pending command has no canonical event")?;
    let target = target(saved)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let result = wire::request(
            state.clone(),
            target.clone(),
            actor,
            operation,
            command.clone(),
            3,
            Uuid::new_v4(),
        )
        .await;
        match result {
            Ok(reply) if reply.state >= 2 && !reply.cleanup_required => {
                let accounts = state
                    .store
                    .ai_connections()
                    .map_err(|_| "Cannot redact recovered command output")?;
                let (out, err, exit, _) = output(&reply, &accounts);
                let event = &mut saved.task.events[index];
                event.output = out;
                event.stderr = err;
                event.exit_code = exit;
                saved.guest_pending = false;
                if reply.state == 3 {
                    saved.task.state = SandboxTaskState::Interrupted;
                    saved.task.error_code = Some("sandbox_project_command_interrupted".into());
                    saved.task.error = Some("Command processes stopped; completion is unknown and partial output is retained".into());
                }
                return Ok(());
            }
            Ok(_) => {}
            Err(error) if error.code() == "workspace_busy" => {}
            Err(error) => return Err(failure(error)),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("Project command cleanup remains unconfirmed".into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
