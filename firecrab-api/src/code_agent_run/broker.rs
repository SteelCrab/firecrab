//! Fixed-origin provider relay. The guest receives a run token, never a shared provider key.
use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use tokio::io::AsyncWrite;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub kind: String,
    pub id: u64,
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}
// Keep incomplete credential matches across HTTP chunks before forwarding any bytes.
struct Redactor<'a> {
    key: &'a [u8],
    pending: Vec<u8>,
}
impl<'a> Redactor<'a> {
    fn new(key: &'a str) -> Self {
        Self {
            key: key.as_bytes(),
            pending: Vec::new(),
        }
    }
    fn push(&mut self, chunk: &[u8], finish: bool) -> Vec<u8> {
        self.pending.extend_from_slice(chunk);
        let limit = if finish {
            self.pending.len()
        } else {
            self.pending
                .len()
                .saturating_sub(self.key.len().saturating_sub(1))
        };
        let mut output = Vec::new();
        let mut offset = 0;
        while offset < limit {
            if !self.key.is_empty() && self.pending[offset..].starts_with(self.key) {
                output.extend_from_slice(b"[redacted]");
                offset += self.key.len();
            } else {
                output.push(self.pending[offset]);
                offset += 1;
            }
        }
        self.pending.drain(..offset);
        output
    }
}
pub(super) async fn relay(
    state: &AppState,
    context: &Context,
    run: &mut CodeAgentRun,
    r: Request,
    output: &mut (impl AsyncWrite + Unpin),
) -> Result<(), &'static str> {
    if r.kind != "request"
        || r.headers.len() > 8
        || r.headers
            .iter()
            .any(|(k, v)| k.len() > 100 || v.len() > 2048)
    {
        return Err("protocol_error");
    }
    if !current(state, context) || !active(state, run.workspace_id, run.id) {
        return Err("bindings_changed");
    }
    let saved = context.snapshot.ai.as_ref().ok_or("auth_missing")?;
    if r.method == "GET" && r.path == "/v1/models" {
        native::write_frame(
            output,
            &json!({"kind":"start","id":r.id,"status":200,"contentType":"application/json"}),
        )
        .await?;
        let models = if run.model.is_empty() {
            json!([])
        } else {
            json!([{"id":run.model,"object":"model","owned_by":"assigned-auth"}])
        };
        native::write_frame(output,&json!({"kind":"data","id":r.id,"data":STANDARD.encode(serde_json::to_vec(&json!({"object":"list","data":models})).unwrap())})).await?;
        return native::write_frame(output, &json!({"kind":"end","id":r.id})).await;
    }
    let allowed = match context.profile.provider {
        AiProvider::Openai => [
            "/v1/responses",
            "/v1/responses/compact",
            "/v1/chat/completions",
        ]
        .contains(&r.path.as_str()),
        AiProvider::Anthropic => {
            matches!(r.path.as_str(), "/v1/messages" | "/v1/messages?beta=true")
        }
    };
    if r.method != "POST"
        || !allowed
        || !r.body["model"].as_str().is_some_and(|m| {
            !m.is_empty()
                && m.len() <= 200
                && m.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.:/".contains(&b))
                && (context.request.model.is_empty() || m == context.request.model)
        })
        || run.provider_requests >= 100
    {
        return Err("provider_request_blocked");
    }
    let guard = state.pipe_ai_mutations.clone().lock_owned().await;
    if !current(state, context) || !active(state, run.workspace_id, run.id) {
        return Err("bindings_changed");
    }
    let origin = match context.profile.provider {
        AiProvider::Openai => "https://api.openai.com",
        AiProvider::Anthropic => "https://api.anthropic.com",
    };
    #[cfg(test)]
    let origin = state.code_agent_test_origin.as_deref().unwrap_or(origin);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| "provider_unavailable")?;
    let mut request = client.post(format!("{origin}{}", r.path)).json(&r.body);
    request = match context.profile.provider {
        AiProvider::Openai => request.bearer_auth(&saved.api_key),
        AiProvider::Anthropic => request
            .header("x-api-key", &saved.api_key)
            .header("anthropic-version", "2023-06-01"),
    };
    for (key, value) in &r.headers {
        if matches!(
            key.as_str(),
            "anthropic-beta" | "openai-beta" | "x-codex-beta-features"
        ) {
            request = request.header(key, value);
        }
    }
    let response = tokio::time::timeout(Duration::from_secs(25), request.send())
        .await
        .map_err(|_| "provider_timed_out")?
        .map_err(|_| "provider_unavailable")?;
    drop(guard);
    if !response.status().is_success() {
        return Err(match response.status().as_u16() {
            401 | 403 => "authentication_rejected",
            429 => "rate_limited",
            _ => "provider_unavailable",
        });
    }
    if !current(state, context) || !active(state, run.workspace_id, run.id) {
        return Err("bindings_changed");
    }
    if run.model.is_empty() {
        run.model = r.body["model"].as_str().unwrap_or("").into();
    }
    let model = r.body["model"].as_str().unwrap_or("");
    if !run.models_used.iter().any(|used| used == model) {
        run.models_used.push(model.into());
    }
    run.provider_requests += 1;
    run.phase = Phase::Running;
    if !save(state, run) {
        return Err("bindings_changed");
    }
    let content = if response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"))
    {
        "text/event-stream"
    } else {
        "application/json"
    };
    native::write_frame(
        output,
        &json!({"kind":"start","id":r.id,"status":200,"contentType":content}),
    )
    .await?;
    let mut chunks = response.bytes_stream();
    let mut bytes = 0usize;
    let mut redactor = Redactor::new(&saved.api_key);
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|_| "provider_unavailable")?;
        bytes += chunk.len();
        if bytes > 16 * 1024 * 1024 {
            return Err("provider_output_limited");
        }
        if !current(state, context) || !active(state, run.workspace_id, run.id) {
            return Err("bindings_changed");
        }
        let safe = redactor.push(&chunk, false);
        for part in safe.chunks(32768) {
            native::write_frame(
                output,
                &json!({"kind":"data","id":r.id,"data":STANDARD.encode(part)}),
            )
            .await?;
        }
    }
    if !current(state, context) || !active(state, run.workspace_id, run.id) {
        return Err("bindings_changed");
    }
    let tail = redactor.push(&[], true);
    if !tail.is_empty() {
        native::write_frame(
            output,
            &json!({"kind":"data","id":r.id,"data":STANDARD.encode(tail)}),
        )
        .await?;
    }
    native::write_frame(output, &json!({"kind":"end","id":r.id})).await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn code_run_broker_redacts_credentials_at_every_stream_boundary() {
        let source = "data: before sk-host-only-key after\n\n";
        for at in 0..=source.len() {
            let mut r = Redactor::new("sk-host-only-key");
            let mut bytes = r.push(&source.as_bytes()[..at], false);
            bytes.extend(r.push(&source.as_bytes()[at..], false));
            bytes.extend(r.push(&[], true));
            assert_eq!(bytes, b"data: before [redacted] after\n\n");
        }
    }
}
