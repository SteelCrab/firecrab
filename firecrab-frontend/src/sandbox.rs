//! AI Sandbox's browser state and REST lifecycle. Provider secrets never enter this module.
mod actions;
mod resources;
mod view;
use crate::{
    App, Shared, api, document, element,
    model::{escape, list, text},
    set_html, window,
};
pub use actions::{action, change, input, key, submit};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
pub use view::{content, icon};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::spawn_local;
use web_sys::{
    Element, HtmlDialogElement, HtmlElement, HtmlInputElement, HtmlSelectElement,
    HtmlTextAreaElement, KeyboardEvent,
};

#[derive(Default)]
pub struct State {
    projects: Option<Vec<Value>>,
    accounts: Vec<Value>,
    catalogs: BTreeMap<String, Value>,
    project: String,
    tasks: Vec<Value>,
    tasks_loaded: bool,
    task_error: String,
    error: String,
    tab: String,
    kind: String,
    input: String,
    selection: Value,
    busy: bool,
    stopping: String,
    pending: Option<(Value, String)>,
    editor: Option<Editor>,
    review: Option<Review>,
    context: u64,
    revision: u64,
    polling: bool,
    since: Option<u64>,
    older_end: bool,
    opener: String,
    environments: Vec<Value>,
    previews: Vec<Value>,
    resource_error: String,
    resources_polling: bool,
    resources_at: f64,
    resource_editor: Option<resources::Editor>,
    coordination: Value,
    environment_role: String,
}
struct Editor {
    id: String,
    existing: bool,
    spec: Value,
    connection: String,
    model: String,
    tools: bool,
    catalog: Value,
    loading: bool,
    busy: bool,
    revision: u64,
    error: String,
}
struct Review {
    id: String,
    task: Option<Value>,
    tab: String,
    paths: BTreeSet<String>,
    error: String,
    load_error: String,
    busy: bool,
    polling: bool,
}
fn tr<'a>(app: &App, en: &'a str, ko: &'a str) -> &'a str {
    if app.locale == "ko" { ko } else { en }
}
fn uuid() -> Result<String, String> {
    window()
        .crypto()
        .map(|c| c.random_uuid())
        .map_err(api::error)
}
fn active(task: &Value) -> bool {
    matches!(text(task, "state"), "waiting" | "accepted" | "in_progress")
}
fn project(state: &State) -> Option<&Value> {
    state
        .projects
        .as_ref()?
        .iter()
        .find(|p| text(p, "id") == state.project)
}
fn live(app: &Shared, generation: u64) -> bool {
    let a = app.borrow();
    a.generation == generation && a.route == "ai-sandbox"
}
fn dirty(app: &mut App) {
    app.dirty = !app.sandbox.input.trim().is_empty()
        || app.sandbox.editor.is_some()
        || app.sandbox.resource_editor.is_some();
}
fn effective(s: &State) -> Value {
    if s.selection.is_null() {
        project(s)
            .map(|p| p["spec"]["selection"].clone())
            .unwrap_or(Value::Null)
    } else {
        s.selection.clone()
    }
}
fn verified<'a>(s: &'a State, selection: &Value, tools: bool) -> Option<&'a Value> {
    list(&s.catalogs.get(text(selection, "connectionId"))?["models"])
        .iter()
        .find(|m| {
            m["id"] == selection["model"]
                && m["verified"] == true
                && (!tools || m["toolsVerified"] == true)
        })
}
fn can_send(a: &App) -> bool {
    let s = &a.sandbox;
    if s.busy
        || !s.tasks_loaded
        || !s.task_error.is_empty()
        || s.input.trim().is_empty()
        || s.tasks.iter().any(active)
    {
        return false;
    }
    let code = s.kind == "code";
    if verified(s, &effective(s), code).is_none() {
        return false;
    }
    if code && project(s).is_some_and(|p| p["spec"]["environmentId"].is_string()) {
        if s.environment_role.is_empty() {
            return false;
        }
        return project(s).is_some_and(|p| {
            !text(&p["spec"], "sourcePath").is_empty()
                && s.environments.iter().any(|e| {
                    e["id"] == p["spec"]["environmentId"]
                        && e["state"] == "ready"
                        && list(&e["roles"]).iter().any(|r| {
                            (s.environment_role.is_empty() || r["role"] == s.environment_role)
                                && r["agent"] == "ready"
                                && r["network"] == "ready"
                                && r["application"] == "ready"
                        })
                })
        });
    }
    !code
        || project(s).is_some_and(|p| {
            p["spec"]["sourcePath"].is_string()
                && a.vms
                    .iter()
                    .any(|v| v.id == text(&p["spec"], "vmId") && v.state == "running")
        })
}
fn summary(mut task: Value) -> Value {
    // Older saved responses omit the additive defaults in the current wire contract.
    if let Some(request) = task["request"].as_object_mut() {
        request
            .entry("coordination")
            .or_insert_with(|| json!({"dependencies":[],"assignedAgent":null,"retryLimit":3}));
        request.entry("environmentRole").or_insert(Value::Null);
    }
    if let Some(object) = task.as_object_mut() {
        object.entry("retryRoot").or_insert(Value::Null);
        for key in ["retryNumber", "testCount", "artifactCount"] {
            object.entry(key).or_insert(json!(0));
        }
    }
    if task.get("events").is_some() {
        task["eventCount"] = json!(list(&task["events"]).len());
        task["lastTool"] = task["events"]
            .as_array()
            .and_then(|v| v.last())
            .map(|v| v["name"].clone())
            .unwrap_or(Value::Null);
    }
    if task.get("changes").is_some() {
        task["changeCount"] = json!(list(&task["changes"]).len());
    }
    for (field, count) in [("tests", "testCount"), ("artifacts", "artifactCount")] {
        if task.get(field).is_some() {
            task[count] = json!(list(&task[field]).len());
        }
    }
    if task.get("tests").is_some() {
        task["failedTestCount"] = json!(
            list(&task["tests"])
                .iter()
                .filter(|test| test["passed"] == false)
                .count()
        );
    } else if task.get("events").is_some() {
        task["failedTestCount"] = json!(0);
    }
    if let Some(o) = task.as_object_mut() {
        o.remove("events");
        o.remove("changes");
        for key in [
            "tests",
            "artifacts",
            "environment",
            "gitCommit",
            "dependencyResults",
        ] {
            o.remove(key);
        }
    }
    task
}
fn merge_tasks(tasks: &mut Vec<Value>, incoming: impl IntoIterator<Item = Value>) {
    for task in incoming {
        if let Some(old) = tasks.iter_mut().find(|v| v["id"] == task["id"]) {
            if task["updatedAt"].as_u64() >= old["updatedAt"].as_u64() {
                *old = task;
            }
        } else {
            tasks.push(task);
        }
    }
    tasks.sort_by_key(|v| v["createdAt"].as_u64().unwrap_or(0));
}
fn update_task(a: &mut App, task: Value) {
    a.sandbox.revision += 1;
    merge_tasks(&mut a.sandbox.tasks, [summary(task.clone())]);
    if let Some(r) = &mut a.sandbox.review
        && r.id == text(&task, "id")
    {
        r.task = Some(task);
    }
}
fn render_after_event(app: &Shared) {
    // Removing a focused input can emit change again before the current closure returns.
    let app = app.clone();
    spawn_local(async move { render(&app) });
}
/// Render preserves editing focus, selection and scroll positions across asynchronous updates.
pub(super) fn render(app: &Shared) {
    if app.borrow().route != "ai-sandbox" {
        return;
    }
    let focus = document()
        .active_element()
        .map(|n| n.id())
        .unwrap_or_default();
    let selection = element(&focus).and_then(|n| {
        if let Some(n) = n.dyn_ref::<HtmlTextAreaElement>() {
            Some((n.selection_start().ok()??, n.selection_end().ok()??))
        } else if let Some(n) = n.dyn_ref::<HtmlInputElement>() {
            Some((n.selection_start().ok()??, n.selection_end().ok()??))
        } else {
            None
        }
    });
    let scroll: Vec<_> = ["sb-panel", "sb-review-panel", "sb-resource-dialog"]
        .iter()
        .filter_map(|id| element(id).map(|n| (*id, n.scroll_top())))
        .collect();
    let details = document()
        .query_selector_all("#content details[id]")
        .ok()
        .map(|nodes| {
            (0..nodes.length())
                .filter_map(|i| nodes.item(i)?.dyn_into::<Element>().ok())
                .map(|n| (n.id(), n.has_attribute("open")))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    app.borrow_mut().rendering = true;
    set_html("content", &content(&app.borrow()));
    if let Some(node) = element("content") {
        let _ = node.set_attribute(
            "data-loaded",
            if app.borrow().loaded { "true" } else { "false" },
        );
    }
    app.borrow_mut().rendering = false;
    for id in [
        "sb-project-dialog",
        "sb-review-dialog",
        "sb-resource-dialog",
    ] {
        if let Some(dialog) = element(id).and_then(|n| n.dyn_into::<HtmlDialogElement>().ok())
            && !dialog.open()
        {
            let _ = dialog.show_modal();
        }
    }
    for (id, top) in scroll {
        if let Some(n) = element(id) {
            n.set_scroll_top(top);
        }
    }
    for (id, open) in details {
        if let Some(n) = element(&id) {
            if open {
                let _ = n.set_attribute("open", "");
            } else {
                let _ = n.remove_attribute("open");
            }
        }
    }
    if let Some(node) = element(&focus).and_then(|n| n.dyn_into::<HtmlElement>().ok()) {
        let _ = node.focus();
    }
    if let (Some((start, end)), Some(node)) = (selection, element(&focus)) {
        if let Some(n) = node.dyn_ref::<HtmlTextAreaElement>() {
            let _ = n.set_selection_range(start, end);
        } else if let Some(n) = node.dyn_ref::<HtmlInputElement>() {
            let _ = n.set_selection_range(start, end);
        }
    }
}
fn close_dialog(app: &Shared) {
    let opener = {
        let mut a = app.borrow_mut();
        a.sandbox.editor = None;
        a.sandbox.review = None;
        a.sandbox.resource_editor = None;
        dirty(&mut a);
        std::mem::take(&mut a.sandbox.opener)
    };
    render(app);
    if let Some(node) = element(&opener).and_then(|n| n.dyn_into::<HtmlElement>().ok()) {
        let _ = node.focus();
    }
}
/// Load account metadata and verified models independently of task history.
pub fn load(app: Shared) {
    let generation = app.borrow().generation;
    spawn_local(async move {
        let result = async {
            let projects = api::request("GET", "/api/ai-sandbox/projects", None).await?;
            let accounts = api::request("GET", "/api/pipe/ai/connections", None).await?;
            let mut catalogs = BTreeMap::new();
            for account in list(&accounts) {
                let id = text(account, "id");
                if let Ok(value) = api::request(
                    "GET",
                    &format!("/api/ai-sandbox/connections/{}/models", api::encode(id)),
                    None,
                )
                .await
                {
                    catalogs.insert(id.to_owned(), value);
                }
            }
            let vms = api::request("GET", "/api/vms", None).await.ok();
            Ok::<_, String>((projects, accounts, catalogs, vms))
        }
        .await;
        if !live(&app, generation) {
            return;
        }
        let mut a = app.borrow_mut();
        a.loaded = true;
        match result {
            Ok((projects, accounts, catalogs, vms)) => {
                a.sandbox.projects = Some(list(&projects).to_vec());
                a.sandbox.accounts = list(&accounts).to_vec();
                a.sandbox.catalogs = catalogs;
                a.sandbox.error.clear();
                if let Some(v) = vms
                    && let Ok(vms) = serde_json::from_value(v)
                {
                    a.vms = vms;
                }
                if project(&a.sandbox).is_none() {
                    let requested = window()
                        .location()
                        .hash()
                        .unwrap_or_default()
                        .split("?project=")
                        .nth(1)
                        .unwrap_or("")
                        .to_owned();
                    a.sandbox.project = if list(&projects).iter().any(|v| v["id"] == requested) {
                        requested
                    } else {
                        list(&projects)
                            .first()
                            .map(|v| text(v, "id").to_owned())
                            .unwrap_or_default()
                    };
                }
            }
            Err(e) => {
                a.sandbox.projects = None;
                a.sandbox.error = e;
            }
        }
        drop(a);
        crate::update_toolbar(&app);
        render(&app);
        tick(&app);
    });
}
/// One application-owned polling timer, guarded by route, project and mutation revision.
pub fn tick(app: &Shared) {
    if app.borrow().route != "ai-sandbox" {
        return;
    }
    poll_review(app);
    resources::tick(app, false);
    let mut a = app.borrow_mut();
    let s = &mut a.sandbox;
    if s.project.is_empty() || s.polling {
        return;
    }
    s.polling = true;
    let (id, context, revision, since, generation) = (
        s.project.clone(),
        s.context,
        s.revision,
        s.since,
        a.generation,
    );
    drop(a);
    let app = app.clone();
    spawn_local(async move {
        let path = format!(
            "/api/ai-sandbox/projects/{}/tasks{}",
            api::encode(&id),
            since.map(|v| format!("?since={v}")).unwrap_or_default()
        );
        let result = api::request("GET", &path, None).await;
        if !live(&app, generation) {
            return;
        }
        let mut a = app.borrow_mut();
        let s = &mut a.sandbox;
        if s.context != context {
            return;
        }
        s.polling = false;
        if s.revision != revision {
            return;
        }
        let before = (s.tasks.clone(), s.task_error.clone(), s.tasks_loaded);
        match result {
            Ok(value) => {
                for t in list(&value) {
                    let at = t["updatedAt"].as_u64().unwrap_or(0);
                    s.since = Some(s.since.unwrap_or(0).max(at));
                }
                merge_tasks(&mut s.tasks, list(&value).iter().cloned());
                s.tasks_loaded = true;
                s.task_error.clear();
                if !s.tasks.iter().any(active) {
                    s.stopping.clear();
                }
            }
            Err(e) => s.task_error = e,
        }
        let changed = before != (s.tasks.clone(), s.task_error.clone(), s.tasks_loaded);
        drop(a);
        if changed {
            render(&app);
        }
    });
}
fn poll_review(app: &Shared) {
    let mut a = app.borrow_mut();
    let generation = a.generation;
    let Some(r) = &mut a.sandbox.review else {
        return;
    };
    if r.polling || (r.task.as_ref().is_some_and(|t| !active(t)) && r.load_error.is_empty()) {
        return;
    }
    r.polling = true;
    let id = r.id.clone();
    drop(a);
    let app = app.clone();
    spawn_local(async move {
        let result = api::request(
            "GET",
            &format!("/api/ai-sandbox/tasks/{}", api::encode(&id)),
            None,
        )
        .await;
        if !live(&app, generation) {
            return;
        }
        let mut a = app.borrow_mut();
        let Some(r) = &mut a.sandbox.review else {
            return;
        };
        if r.id != id {
            return;
        }
        r.polling = false;
        let changed = match result {
            Ok(task) => {
                let changed = r.task.as_ref() != Some(&task) || !r.load_error.is_empty();
                if r.task.is_none() {
                    r.paths = list(&task["changes"])
                        .iter()
                        .filter(|c| c["applied"] != true)
                        .map(|c| text(c, "path").into())
                        .collect();
                    r.tab = if list(&task["changes"]).is_empty() {
                        "activity"
                    } else {
                        "changes"
                    }
                    .into();
                }
                r.task = Some(task);
                r.load_error.clear();
                changed
            }
            Err(e) => {
                let changed = r.load_error != e;
                r.load_error = e;
                changed
            }
        };
        drop(a);
        if changed {
            render(&app);
        }
    });
}

#[cfg(test)]
#[path = "../tests/unit/sandbox/mod.rs"]
mod tests;
