//! Explicit runs keep a single request ID across lost responses and page reloads.
mod actions;
mod view;
use super::*;
use serde_json::json;
pub(super) use view::content;
use web_sys::HtmlTextAreaElement;

const JOURNAL: &str = "firecrab.code.agent-run.v1.";
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Run {
    id: String,
    workspace_id: String,
    revision: u64,
    connection_id: String,
    agent: String,
    cli_version: String,
    model: String,
    #[serde(default)]
    models_used: Vec<String>,
    vm_id: String,
    runtime_id: String,
    operation_id: String,
    phase: String,
    output: String,
    truncated: bool,
    provider_requests: u32,
    error: Option<String>,
}
impl Run {
    fn active(&self) -> bool {
        matches!(
            self.phase.as_str(),
            "queued" | "starting" | "running" | "cancelling"
        )
    }
    fn valid(&self, workspace: &str) -> bool {
        self.workspace_id == workspace
            && self.revision > 0
            && [
                &self.id,
                &self.connection_id,
                &self.vm_id,
                &self.runtime_id,
                &self.operation_id,
            ]
            .iter()
            .all(|s| uuid(s))
            && self.models_used.len() <= 100
            && self.models_used.iter().all(|m| m.len() <= 200)
            && self.output.len() <= 65536
            && self.provider_requests <= 100
            && [
                "queued",
                "starting",
                "running",
                "cancelling",
                "succeeded",
                "failed",
                "cancelled",
                "interrupted",
            ]
            .contains(&self.phase.as_str())
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Pending {
    id: String,
    workspace: String,
    request: Value,
}
#[derive(Default)]
pub(super) struct State {
    workspace: String,
    generation: u64,
    rows: Vec<Run>,
    loaded: bool,
    loading: bool,
    busy: bool,
    epoch: u64,
    error: bool,
    prompt: String,
    model: String,
    pending: Option<Pending>,
}
fn remember(workspace: &str, pending: Option<&Pending>) -> bool {
    let Ok(Some(storage)) = crate::window().session_storage() else {
        return false;
    };
    let key = format!("{JOURNAL}{workspace}");
    match pending {
        Some(p) => serde_json::to_string(p).ok().is_some_and(|raw| {
            storage.set_item(&key, &raw).is_ok()
                && storage.get_item(&key).ok().flatten().as_deref() == Some(&raw)
        }),
        None => storage.remove_item(&key).is_ok(),
    }
}
fn restore(workspace: &str) -> Option<Pending> {
    let raw = crate::window()
        .session_storage()
        .ok()
        .flatten()?
        .get_item(&format!("{JOURNAL}{workspace}"))
        .ok()
        .flatten()?;
    let p: Pending = serde_json::from_str(&raw).ok()?;
    (p.workspace == workspace
        && uuid(&p.id)
        && p.request["prompt"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty() && s.len() <= 16384)
        && p.request["expectedRevision"]
            .as_u64()
            .is_some_and(|r| r > 0)
        && ["vmId", "runtimeId", "operationId"]
            .iter()
            .all(|k| p.request[k].as_str().is_some_and(uuid)))
    .then_some(p)
}
fn runtime(app: &App) -> Option<&Runtime> {
    match &app.code.runtime {
        RuntimeState::Current(r)
            if r.phase == "ready"
                && r.error.is_none()
                && r.vm_state.as_deref() == Some("running")
                && r.runtime_id.as_deref().is_some_and(uuid)
                && !lifecycle::unresolved(app) =>
        {
            Some(r)
        }
        _ => None,
    }
}
fn ready(app: &App, w: &Workspace) -> bool {
    let s = &app.code.agent_run;
    s.workspace == w.id
        && s.generation == app.generation
        && s.loaded
        && !s.error
        && !s.busy
        && s.pending.is_none()
        && !s.rows.iter().any(Run::active)
        && runtime(app).is_some()
        && w.connections.ai == "present_unverified"
        && agent_auth::execution_supported(app, w)
        && app.code.binding.is_none()
        && !auth::is_open(app)
        && !app.code.editor.busy
}
fn can_start(app: &App, w: &Workspace) -> bool {
    let s = &app.code.agent_run;
    ready(app, w)
        && !s.prompt.trim().is_empty()
        && s.prompt.len() <= 16384
        && s.model.len() <= 200
        && s.model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:/".contains(&b))
}
pub(super) fn input(app: &Shared, target: &Element) -> bool {
    let mut a = app.borrow_mut();
    match target.id().as_str() {
        "code-agent-prompt" => {
            if let Some(el) = target.dyn_ref::<HtmlTextAreaElement>() {
                a.code.agent_run.prompt = el.value();
            }
        }
        "code-agent-model" => {
            if let Some(el) = target.dyn_ref::<HtmlInputElement>() {
                a.code.agent_run.model = el.value();
            }
        }
        _ => return false,
    }
    if let Some(button) = element("code-agent-run-start") {
        if binding::selected(&a).is_some_and(|w| can_start(&a, w)) {
            let _ = button.remove_attribute("disabled");
        } else {
            let _ = button.set_attribute("disabled", "");
        }
    }
    true
}
pub(super) fn load(app: Shared) {
    let mut a = app.borrow_mut();
    if !is_editor(&a.route) {
        return;
    }
    let Some(w) = binding::selected(&a).cloned() else {
        return;
    };
    if a.code.agent_run.workspace != w.id || a.code.agent_run.generation != a.generation {
        let old = std::mem::take(&mut a.code.agent_run);
        let same = old.workspace == w.id;
        let pending = restore(&w.id);
        let prompt = pending
            .as_ref()
            .and_then(|p| p.request["prompt"].as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| if same { old.prompt } else { String::new() });
        let model = pending
            .as_ref()
            .and_then(|p| p.request["model"].as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| if same { old.model } else { String::new() });
        a.code.agent_run = State {
            workspace: w.id.clone(),
            generation: a.generation,
            epoch: old.epoch + 1,
            pending,
            prompt,
            model,
            ..State::default()
        };
    }
    if a.code.agent_run.loading || a.code.agent_run.busy {
        return;
    }
    a.code.agent_run.loading = true;
    let (generation, epoch) = (a.generation, a.code.agent_run.epoch);
    drop(a);
    spawn_local(async move {
        let rows = api::request("GET", &format!("{WORKSPACES}/{}/agent-runs", w.id), None)
            .await
            .ok()
            .and_then(|v| serde_json::from_value::<Vec<Run>>(v).ok())
            .filter(|rows| rows.len() <= 20 && rows.iter().all(|r| r.valid(&w.id)));
        let mut a = app.borrow_mut();
        if a.generation != generation
            || a.code.agent_run.epoch != epoch
            || a.code.agent_run.workspace != w.id
        {
            return;
        }
        let s = &mut a.code.agent_run;
        s.loading = false;
        let changed = s.error != rows.is_none() || !s.loaded || rows.as_ref() != Some(&s.rows);
        s.error = rows.is_none();
        s.loaded = rows.is_some();
        if let Some(rows) = rows {
            if s.pending
                .as_ref()
                .is_some_and(|p| rows.iter().any(|r| r.id == p.id))
            {
                remember(&w.id, None);
                s.pending = None;
            }
            s.rows = rows;
        }
        drop(a);
        if changed {
            render_content(&app);
        }
    });
}
pub(super) fn action(app: &Shared, action: &str) -> bool {
    actions::action(app, action)
}
