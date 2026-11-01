//! CODE reads saved configuration and current runtime observations. No credentials are fetched.
mod accounts;
pub(crate) mod auth;
mod binding;
mod editor;
mod setup;
mod view;
use crate::{App, Shared, api, document, element, render_content};
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use setup::submit;
pub use view::content;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{Element, HtmlElement, HtmlInputElement};

const WORKSPACES: &str = "/api/code/workspaces";

// Browser projections of the public API. Unknown additive fields are ignored.
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Workspace {
    id: String,
    revision: u64,
    spec: Spec,
    connections: Connections,
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
struct Spec {
    name: String,
    github: Repository,
    runtime: String,
    resources: Resources,
    agent: Option<Agent>,
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
struct Repository {
    account: String,
    owner: String,
    repository: String,
    branch: String,
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Resources {
    cpu: u32,
    ram: u32,
    disk_gb: u32,
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Agent {
    agent: String,
    connection_id: String,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Connections {
    github: String,
    ai: String,
}
#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Runtime {
    workspace_id: String,
    vm_id: String,
    configuration_revision: u64,
    operation_id: String,
    phase: String,
    project_path: String,
    runtime_id: Option<String>,
    initial_commit: Option<String>,
    vm_state: Option<String>,
    placement: Placement,
    error: Option<String>,
}
#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Placement {
    template: Template,
    micro_network_id: String,
    storage_root: String,
}
#[derive(Deserialize, PartialEq)]
struct Template {
    name: String,
    version: String,
}
#[derive(Default, PartialEq)]
enum RuntimeState {
    #[default]
    Loading,
    Absent,
    Current(Runtime),
    Failed(Problem),
}
#[derive(Clone, Copy, PartialEq)]
enum Problem {
    Access,
    Unavailable,
    Invalid,
}
impl From<api::Failure> for Problem {
    fn from(error: api::Failure) -> Self {
        if matches!(error.status, Some(401 | 403)) {
            Self::Access
        } else {
            Self::Unavailable
        }
    }
}
#[derive(Default)]
pub struct State {
    workspaces: Vec<Workspace>,
    runtime: RuntimeState,
    error: Option<Problem>,
    query: String,
    selection: Option<String>,
    setup: Option<setup::Setup>,
    auth: auth::State,
    binding: Option<binding::Draft>,
    binding_epoch: u64,
    observation_epoch: u64,
}
fn invalidate_observations(app: &mut App) {
    app.code.observation_epoch += 1;
    app.refreshing = false;
}

fn uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
        && id.bytes().any(|b| b != b'0' && b != b'-')
}
pub fn is_route(route: &str) -> bool {
    route == "code" || route == "code/new" || workspace_id(route).is_some()
}
fn workspace_id(route: &str) -> Option<&str> {
    let path = route.strip_prefix("code/")?;
    let id = path.strip_suffix("/editor").unwrap_or(path);
    uuid(id).then_some(id)
}
pub fn is_editor(route: &str) -> bool {
    workspace_id(route).is_some() && route.ends_with("/editor")
}
pub fn saving(app: &App) -> bool {
    binding::busy(app) || setup::saving(app)
}
pub fn change(app: &Shared, node: &Element) -> bool {
    binding::change(app, node) || setup::change(app, node)
}
fn tr<'a>(app: &App, en: &'a str, ko: &'a str) -> &'a str {
    if app.locale == "ko" { ko } else { en }
}
pub fn api_status(app: &App) -> (&'static str, &'static str) {
    if auth::is_open(app) {
        return auth::api_status(app);
    }
    if app.route == "code/new" {
        return setup::api_status(app);
    }
    if !app.loaded {
        ("checking", tr(app, "Checking…", "확인 중…"))
    } else if app.code.error.is_some() || matches!(app.code.runtime, RuntimeState::Failed(_)) {
        ("down", tr(app, "Status unavailable", "상태 확인 불가"))
    } else {
        ("live", tr(app, "Live", "실시간"))
    }
}
fn selected<'a>(route: &str, workspaces: &'a [Workspace]) -> Option<&'a Workspace> {
    if let Some(id) = workspace_id(route) {
        workspaces.iter().find(|w| w.id == id)
    } else {
        workspaces.first()
    }
}
fn decode_workspaces(value: Value) -> Result<Vec<Workspace>, Problem> {
    let workspaces: Vec<Workspace> = serde_json::from_value(value).map_err(|_| Problem::Invalid)?;
    let mut ids = std::collections::BTreeSet::new();
    if workspaces
        .iter()
        .any(|w| !uuid(&w.id) || w.revision == 0 || !ids.insert(w.id.clone()))
    {
        return Err(Problem::Invalid);
    }
    Ok(workspaces)
}
async fn runtime(id: &str) -> RuntimeState {
    let path = format!("{WORKSPACES}/{id}");
    match api::request_detailed("GET", &format!("{path}/runtime"), None, None).await {
        Ok(value) => match decode_runtime(id, value) {
            Ok(runtime) => RuntimeState::Current(runtime),
            Err(error) => RuntimeState::Failed(error),
        },
        Err(error) if error.status == Some(404) && error.code.as_deref() == Some("not_found") => {
            // A runtime 404 alone can also mean an old API or a concurrently deleted workspace.
            match api::request_detailed("GET", &path, None, None).await {
                Ok(value) if value["id"].as_str() == Some(id) => RuntimeState::Absent,
                Err(error) => RuntimeState::Failed(error.into()),
                _ => RuntimeState::Failed(Problem::Unavailable),
            }
        }
        Err(error) => RuntimeState::Failed(error.into()),
    }
}
fn decode_runtime(id: &str, value: Value) -> Result<Runtime, Problem> {
    let mut runtime: Runtime = serde_json::from_value(value).map_err(|_| Problem::Invalid)?;
    if runtime.workspace_id != id
        || !uuid(&runtime.vm_id)
        || !uuid(&runtime.operation_id)
        || runtime.configuration_revision == 0
    {
        return Err(Problem::Invalid);
    }
    if runtime.phase == "ready"
        && (runtime.vm_state.as_deref() != Some("running")
            || runtime.error.is_some()
            || !runtime.runtime_id.as_deref().is_some_and(uuid))
    {
        runtime.phase = "reconcile_required".into();
    }
    Ok(runtime)
}

pub fn load(app: Shared) {
    if auth::is_open(&app.borrow()) {
        auth::load(app);
        return;
    }
    if app.borrow().route == "code/new" {
        setup::load(app);
        return;
    }
    let mut state = app.borrow_mut();
    if state.refreshing || !is_route(&state.route) {
        return;
    }
    state.refreshing = true;
    state.code.observation_epoch += 1;
    let epoch = state.code.observation_epoch;
    let generation = state.generation;
    let route = binding::observation_route(&state);
    drop(state);
    spawn_local(async move {
        let records = match api::request_detailed("GET", WORKSPACES, None, None).await {
            Ok(value) => decode_workspaces(value),
            Err(error) => Err(error.into()),
        };
        // Navigation fences both response publication and the next request.
        if app.borrow().generation != generation || app.borrow().code.observation_epoch != epoch {
            return;
        }
        let observation = match &records {
            Ok(workspaces) => match selected(&route, workspaces) {
                Some(workspace) => runtime(&workspace.id).await,
                None => RuntimeState::Loading,
            },
            Err(error) => RuntimeState::Failed(*error),
        };
        let mut state = app.borrow_mut();
        if state.generation != generation || state.code.observation_epoch != epoch {
            return;
        }
        state.refreshing = false;
        let records = if matches!(observation, RuntimeState::Failed(Problem::Access)) {
            Err(Problem::Access)
        } else {
            records
        };
        let changed = !state.loaded
            || state.code.runtime != observation
            || match &records {
                Ok(records) => state.code.error.is_some() || state.code.workspaces != *records,
                Err(error) => state.code.error != Some(*error),
            };
        state.loaded = true;
        state.code.runtime = observation;
        match records {
            Ok(records) => {
                if state.code.workspaces != records {
                    auth::invalidate(&mut state);
                }
                state.code.workspaces = records;
                state.code.error = None;
                state.connection_error.clear();
            }
            Err(error) => {
                auth::invalidate(&mut state);
                state.code.workspaces.clear();
                state.code.error = Some(error);
            }
        }
        drop(state);
        if changed {
            render_content(&app);
        }
    });
}

pub fn action(app: &Shared, action: &str) -> bool {
    if auth::action(app, action) {
        return true;
    }
    if binding::action(app, action) {
        return true;
    }
    if setup::action(app, action) {
        return true;
    }
    if action != "code-refresh" || !is_route(&app.borrow().route) {
        return false;
    }
    load(app.clone());
    true
}
pub fn input(app: &Shared, target: &Element) -> bool {
    if setup::input(app, target) {
        return true;
    }
    if target.id() != "code-search" {
        return false;
    }
    if let Some(input) = target.dyn_ref::<HtmlInputElement>() {
        app.borrow_mut().code.query = input.value();
        render_content(app);
    }
    true
}

/// Keep search cursor, keyboard focus and detail scroll position across observations.
pub fn render(app: &Shared) {
    let focus = document().active_element().filter(|el| {
        el.closest("#code-workspaces").ok().flatten().is_some() && !el.id().is_empty()
    });
    let selection = focus
        .as_ref()
        .and_then(|el| el.dyn_ref::<HtmlInputElement>())
        .map(|input| {
            (
                input.selection_start().ok().flatten(),
                input.selection_end().ok().flatten(),
            )
        });
    let scroll = element("code-details").map(|el| el.scroll_top());
    let html = content(&app.borrow());
    app.borrow_mut().rendering = true;
    crate::set_html("content", &html);
    app.borrow_mut().rendering = false;
    if let Some(status) = element("api-status") {
        let state = app.borrow();
        let (kind, label) = api_status(&state);
        let _ = status.set_attribute("data-state", kind);
        status.set_text_content(Some(label));
    }
    if let Some(content) = element("content") {
        let _ = content.set_attribute(
            "data-loaded",
            if app.borrow().loaded { "true" } else { "false" },
        );
    }
    if let Some(scroll) = scroll
        && let Some(detail) = element("code-details")
    {
        detail.set_scroll_top(scroll);
    }
    if let Some(focus) = focus.and_then(|old| element(&old.id())) {
        if let Some(el) = focus.dyn_ref::<HtmlElement>() {
            let options = web_sys::FocusOptions::new();
            options.set_prevent_scroll(true);
            let _ = el.focus_with_options(&options);
        }
        if let Some((Some(start), Some(end))) = selection
            && let Some(input) = focus.dyn_ref::<HtmlInputElement>()
        {
            let _ = input.set_selection_range(start, end);
        }
    }
}

#[cfg(test)]
mod tests;
