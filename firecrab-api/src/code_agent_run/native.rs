use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
pub(crate) struct Native;
pub(super) async fn write_frame(
    output: &mut (impl AsyncWrite + Unpin),
    value: &Value,
) -> Result<(), &'static str> {
    let bytes = serde_json::to_vec(value).map_err(|_| "protocol_error")?;
    if bytes.len() > 12 * 1024 * 1024 {
        return Err("protocol_error");
    }
    output
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .await
        .map_err(|_| "guest_unavailable")?;
    output
        .write_all(&bytes)
        .await
        .map_err(|_| "guest_unavailable")?;
    output.flush().await.map_err(|_| "guest_unavailable")
}
async fn read_frame(input: &mut (impl AsyncRead + Unpin)) -> Result<Value, &'static str> {
    let length = input.read_u32().await.map_err(|_| "guest_unavailable")? as usize;
    if length > 12 * 1024 * 1024 {
        return Err("protocol_error");
    }
    let mut bytes = vec![0; length];
    input
        .read_exact(&mut bytes)
        .await
        .map_err(|_| "guest_unavailable")?;
    serde_json::from_slice(&bytes).map_err(|_| "protocol_error")
}
fn text(agent: CodeAgent, line: &str) -> (String, bool) {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return (String::new(), false);
    };
    let kind = value["type"].as_str().unwrap_or("");
    match agent {
        CodeAgent::Codex => {
            let item = &value["item"];
            let message = if kind == "item.completed" && item["type"] == "agent_message" {
                item["text"].as_str()
            } else if kind == "item.completed" && item["type"] == "command_execution" {
                item["aggregated_output"].as_str()
            } else if matches!(kind, "error" | "turn.failed") {
                Some("Agent reported a failed turn.")
            } else {
                None
            };
            (
                message.unwrap_or("").into(),
                matches!(kind, "error" | "turn.failed"),
            )
        }
        CodeAgent::ClaudeCode => {
            let message = if kind == "result" {
                value["result"].as_str()
            } else {
                None
            };
            (
                message.unwrap_or("").into(),
                kind == "result" && value["is_error"] == true,
            )
        }
        CodeAgent::OpenCode => {
            let message = if kind == "text" {
                value["part"]["text"].as_str()
            } else {
                None
            };
            (message.unwrap_or("").into(), kind == "error")
        }
    }
}
impl Driver for Native {
    async fn execute(
        &self,
        state: &AppState,
        context: &Context,
        run: &mut CodeAgentRun,
    ) -> Result<Outcome, &'static str> {
        let binding = crate::code_runtime::auth_binding(state, &context.record)
            .map_err(|_| "runtime_unavailable")?;
        let code = STANDARD.encode(format!(
            "{}\n{}\n{}\nagent_main()\n",
            include_str!("../code_agent_auth/guest.py"),
            include_str!("../code_editor/guest.py"),
            include_str!("guest.py")
        ));
        let remote = format!("python3 -u -c 'import base64;exec(base64.b64decode(\"{code}\"))'");
        let directory = crate::code_runtime::directory(state, run.workspace_id);
        let mut command = guest::code_transport(state, &binding, &directory, Some(&remote))
            .map_err(|_| "runtime_unavailable")?;
        command.stderr(std::process::Stdio::null());
        let mut child = command.spawn().map_err(|_| "guest_unavailable")?;
        let mut input = child.stdin.take().ok_or("guest_unavailable")?;
        let mut output = child.stdout.take().ok_or("guest_unavailable")?;
        let saved = context.snapshot.ai.as_ref().ok_or("auth_missing")?;
        let payload = json!({"workspace":run.workspace_id,"commit":context.record.runtime.initial_commit,"run":run.id,"adapter":context.profile.id,"model":run.model,"prompt":saved.redact(&context.request.prompt),"profiles":serde_json::from_str::<Value>(code_agent_auth::MANIFEST).map_err(|_|"protocol_error")?,"packages":serde_json::from_str::<Value>(include_str!("packages.json")).map_err(|_|"protocol_error")?});
        write_frame(&mut input, &payload).await?;
        let mut raw = String::new();
        let mut requests = 0u32;
        let mut failed = false;
        let result = async {
            loop {
                let frame = read_frame(&mut output).await?;
                match frame["kind"].as_str() {
                    Some("request") => {
                        requests += 1;
                        if requests > 128 {
                            return Err("provider_request_limited");
                        }
                        let request =
                            serde_json::from_value(frame).map_err(|_| "protocol_error")?;
                        broker::relay(state, context, run, request, &mut input).await?;
                    }
                    Some("output") => {
                        let line = frame["line"].as_str().ok_or("protocol_error")?;
                        let (message, error) = text(run.agent, line);
                        failed |= error;
                        if !message.is_empty() {
                            raw.push_str(&saved.redact(&message));
                            raw.push('\n');
                        }
                        if raw.len() > 65536 {
                            let mut end = 65536;
                            while !raw.is_char_boundary(end) {
                                end -= 1;
                            }
                            raw.truncate(end);
                            run.truncated = true;
                        }
                        // Hold a credential-length tail before publishing partial output, including split echoes.
                        let sanitized = saved.redact(&raw);
                        let mut end = sanitized.len().saturating_sub(saved.api_key.len());
                        while !sanitized.is_char_boundary(end) {
                            end -= 1;
                        }
                        run.output = sanitized[..end].into();
                        if !save(state, run) {
                            return Err("bindings_changed");
                        }
                    }
                    Some("exit") => {
                        run.output = saved.redact(&raw);
                        let code = frame["code"]
                            .as_i64()
                            .and_then(|v| i32::try_from(v).ok())
                            .ok_or("protocol_error")?;
                        return Ok(Outcome { code, failed });
                    }
                    Some("error") => {
                        return Err(match frame["reason"].as_str() {
                            Some("cli_unavailable") => "cli_unavailable",
                            Some("cli_untrusted") => "cli_untrusted",
                            Some("version_mismatch") => "version_mismatch",
                            Some("timed_out") => "timed_out",
                            Some("cli_install_failed") => "cli_install_failed",
                            Some("cli_platform_unsupported") => "cli_platform_unsupported",
                            Some("output_limit") => "output_limit",
                            Some("agent_busy") => "agent_busy",
                            _ => "guest_agent_rejected",
                        });
                    }
                    Some("ready") => {}
                    _ => return Err("protocol_error"),
                }
            }
        }
        .await;
        let _ = child.kill().await;
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn code_run_output_uses_agent_messages_and_never_raw_cli_metadata() {
        assert_eq!(
            text(
                CodeAgent::Codex,
                r#"{"type":"item.completed","item":{"type":"agent_message","text":"done"}}"#
            ),
            ("done".into(), false)
        );
        assert_eq!(
            text(
                CodeAgent::ClaudeCode,
                r#"{"type":"result","result":"changed","is_error":false}"#
            ),
            ("changed".into(), false)
        );
        assert_eq!(
            text(
                CodeAgent::OpenCode,
                r#"{"type":"text","part":{"text":"ok"}}"#
            ),
            ("ok".into(), false)
        );
        assert_eq!(
            text(
                CodeAgent::Codex,
                r#"{"type":"thread.started","secret":"hidden"}"#
            ),
            (String::new(), false)
        );
    }
}
