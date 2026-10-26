use super::*;

pub fn input(app: &Shared, node: &Element) -> bool {
    let id = node.id();
    if !id.starts_with("sb-") {
        return false;
    }
    if resources::input(app, node) {
        return true;
    }
    let value = if let Some(n) = node.dyn_ref::<HtmlTextAreaElement>() {
        n.value()
    } else if let Some(n) = node.dyn_ref::<HtmlInputElement>() {
        n.value()
    } else {
        return false;
    };
    let mut a = app.borrow_mut();
    if id == "sb-input" {
        a.sandbox.input = value;
    } else if id == "sb-assigned-agent" || id == "sb-retry-limit" {
        if !a.sandbox.coordination.is_object() {
            a.sandbox.coordination = json!({"dependencies":[],"assignedAgent":null,"retryLimit":3});
        }
        if id == "sb-assigned-agent" {
            a.sandbox.coordination["assignedAgent"] = if value.trim().is_empty() {
                Value::Null
            } else {
                json!(value.trim())
            };
        } else {
            a.sandbox.coordination["retryLimit"] = json!(value.parse::<u8>().unwrap_or(3));
        }
    } else if let Some(e) = &mut a.sandbox.editor {
        match id.as_str() {
            "sb-name" => e.spec["name"] = json!(value),
            "sb-source" => {
                e.spec["sourcePath"] = if value.is_empty() {
                    Value::Null
                } else {
                    json!(value)
                }
            }
            "sb-instructions" => e.spec["instructions"] = json!(value),
            "sb-git-ref" => {
                if !e.spec["context"].is_object() {
                    e.spec["context"] = json!({"gitRef":null,"selectedSkills":[],"references":[]});
                }
                e.spec["context"]["gitRef"] = if value.trim().is_empty() {
                    Value::Null
                } else {
                    json!(value.trim())
                };
            }
            "sb-skills" | "sb-references" => {
                if !e.spec["context"].is_object() {
                    e.spec["context"] = json!({"gitRef":null,"selectedSkills":[],"references":[]});
                }
                let key = if id == "sb-skills" {
                    "selectedSkills"
                } else {
                    "references"
                };
                e.spec["context"][key] = json!(
                    value
                        .lines()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                );
            }
            "sb-model" => {
                e.model = value;
                e.spec["selection"] = list(&e.catalog["models"])
                    .iter()
                    .find(|m| text(m, "id") == e.model && m["verified"] == true)
                    .map(|_| json!({"connectionId":e.connection,"model":e.model}))
                    .unwrap_or(Value::Null);
            }
            _ => return false,
        };
    } else {
        return false;
    }
    dirty(&mut a);
    drop(a);
    controls(app);
    true
}
fn controls(app: &Shared) {
    if let Some(button) =
        element("sb-send").and_then(|n| n.dyn_into::<web_sys::HtmlButtonElement>().ok())
    {
        button.set_disabled(!can_send(&app.borrow()));
    }
    let a = app.borrow();
    if let Some(e) = &a.sandbox.editor {
        for (id, locked) in [
            ("sb-save", e.busy || text(&e.spec, "name").trim().is_empty()),
            (
                "sb-verify-",
                e.busy || e.loading || e.model.trim().is_empty() || e.connection.is_empty(),
            ),
        ] {
            if let Some(button) =
                element(id).and_then(|n| n.dyn_into::<web_sys::HtmlButtonElement>().ok())
            {
                button.set_disabled(locked);
            }
        }
    }
}
pub fn change(app: &Shared, node: &Element) -> bool {
    let id = node.id();
    if !id.starts_with("sb-") {
        return false;
    }
    if resources::input(app, node) {
        return true;
    }
    let value = node
        .dyn_ref::<HtmlSelectElement>()
        .map(|n| n.value())
        .or_else(|| node.dyn_ref::<HtmlInputElement>().map(|n| n.value()))
        .unwrap_or_default();
    let mut a = app.borrow_mut();
    match id.as_str() {
        "sb-kind" => a.sandbox.kind = value,
        "sb-selection" => a.sandbox.selection = serde_json::from_str(&value).unwrap_or(Value::Null),
        "sb-connection" => {
            if let Some(e) = &mut a.sandbox.editor {
                e.connection = value;
                e.model.clear();
                e.spec["selection"] = Value::Null;
                e.catalog = Value::Null;
                e.revision += 1;
            }
            drop(a);
            render_after_event(app);
            load_catalog(app, false);
            return true;
        }
        "sb-vm" => {
            if let Some(e) = &mut a.sandbox.editor {
                e.spec["environmentId"] = Value::Null;
                e.spec["vmId"] = if value.is_empty() {
                    Value::Null
                } else {
                    json!(value)
                };
            }
        }
        "sb-environment" => {
            if let Some(e) = &mut a.sandbox.editor {
                e.spec["environmentId"] = if value.is_empty() {
                    Value::Null
                } else {
                    json!(value)
                };
                if !e.spec["environmentId"].is_null() {
                    e.spec["vmId"] = Value::Null;
                }
            }
        }
        "sb-task-role" => a.sandbox.environment_role = value,
        "sb-retry-limit" | "sb-assigned-agent" => {
            drop(a);
            return input(app, node);
        }
        _ if node.has_attribute("data-sb-dependency") => {
            if !a.sandbox.coordination.is_object() {
                a.sandbox.coordination =
                    json!({"dependencies":[],"assignedAgent":null,"retryLimit":3});
            }
            let id = node.get_attribute("data-sb-dependency").unwrap_or_default();
            let mut ids = list(&a.sandbox.coordination["dependencies"])
                .iter()
                .filter(|v| **v != id)
                .cloned()
                .collect::<Vec<_>>();
            if node.clone().unchecked_into::<HtmlInputElement>().checked() {
                ids.push(json!(id));
            }
            a.sandbox.coordination["dependencies"] = json!(ids);
        }
        "sb-tools" => {
            if let Some(e) = &mut a.sandbox.editor {
                e.tools = node.clone().unchecked_into::<HtmlInputElement>().checked();
            }
        }
        "sb-name" | "sb-model" | "sb-source" | "sb-instructions" | "sb-input" | "sb-git-ref"
        | "sb-skills" | "sb-references" => {
            drop(a);
            return input(app, node);
        }
        _ if node.has_attribute("data-sb-path") => {
            if let Some(r) = &mut a.sandbox.review {
                let p = node.get_attribute("data-sb-path").unwrap_or_default();
                if node.clone().unchecked_into::<HtmlInputElement>().checked() {
                    r.paths.insert(p);
                } else {
                    r.paths.remove(&p);
                }
            }
        }
        _ => return false,
    }
    dirty(&mut a);
    drop(a);
    render_after_event(app);
    true
}
pub fn submit(app: &Shared, id: &str) -> bool {
    match id {
        "sb-composer" => send(app, None),
        "sb-project-form" => save_project(app),
        "sb-resource-form" => resources::save(app),
        _ => return false,
    };
    true
}
pub fn key(app: &Shared, event: &KeyboardEvent) -> bool {
    if app.borrow().route != "ai-sandbox" {
        return false;
    }
    if (event.meta_key() || event.ctrl_key())
        && event.key() == "Enter"
        && document()
            .active_element()
            .is_some_and(|n| n.id() == "sb-input")
    {
        event.prevent_default();
        if can_send(&app.borrow()) {
            send(app, None);
        }
        return true;
    }
    if event.key() == "Escape"
        && (app.borrow().sandbox.editor.is_some()
            || app.borrow().sandbox.review.is_some()
            || app.borrow().sandbox.resource_editor.is_some())
    {
        event.prevent_default();
        let a = app.borrow();
        let locked = a.sandbox.editor.as_ref().is_some_and(|e| e.busy)
            || a.sandbox.review.as_ref().is_some_and(|r| r.busy)
            || a.sandbox.resource_editor.as_ref().is_some_and(|r| r.busy);
        drop(a);
        if !locked {
            close_dialog(app);
        }
        return true;
    }
    let focused = document().active_element();
    if focused
        .as_ref()
        .is_some_and(|n| n.get_attribute("role").as_deref() == Some("tab"))
        && ["ArrowLeft", "ArrowRight", "Home", "End"].contains(&event.key().as_str())
    {
        let current = focused.unwrap().id();
        let review = current.starts_with("sb-review-");
        let first = if review {
            "sb-review-changes-tab"
        } else {
            "sb-chat-tab"
        };
        let last = if review {
            "sb-review-activity-tab"
        } else {
            "sb-tasks-tab"
        };
        let next = match event.key().as_str() {
            "Home" => first,
            "End" => last,
            _ => {
                if current == first {
                    last
                } else {
                    first
                }
            }
        };
        event.prevent_default();
        let action = if next == "sb-chat-tab" {
            "sb-tab-chat"
        } else if next == "sb-tasks-tab" {
            "sb-tab-tasks"
        } else if next == "sb-review-changes-tab" {
            "sb-review-changes"
        } else {
            "sb-review-activity"
        };
        action_impl(app, action, "");
        if let Some(n) = element(next).and_then(|n| n.dyn_into::<HtmlElement>().ok()) {
            let _ = n.focus();
        }
        return true;
    }
    false
}
pub fn action(app: &Shared, name: &str, id: &str) -> bool {
    if !name.starts_with("sb-") {
        return false;
    }
    action_impl(app, name, id);
    true
}
fn action_impl(app: &Shared, name: &str, id: &str) {
    if resources::action(app, name, id) {
        return;
    }
    match name {
        "sb-new" | "sb-settings" => open_editor(app, name == "sb-settings"),
        "sb-close" => {
            if !app.borrow().sandbox.editor.as_ref().is_some_and(|e| e.busy)
                && !app.borrow().sandbox.review.as_ref().is_some_and(|r| r.busy)
                && !app
                    .borrow()
                    .sandbox
                    .resource_editor
                    .as_ref()
                    .is_some_and(|r| r.busy)
            {
                close_dialog(app);
            }
        }
        "sb-refresh" => load(app.clone()),
        "sb-project" => choose_project(app, id),
        "sb-discover" => load_catalog(app, true),
        "sb-verify" => verify(app),
        "sb-send" => send(app, None),
        "sb-stop" => mutate_task(app, id, "cancel"),
        "sb-reconcile" => mutate_task(app, id, "reconcile"),
        "sb-retry" => {
            let task = app
                .borrow()
                .sandbox
                .tasks
                .iter()
                .find(|t| text(t, "id") == id)
                .cloned();
            if let Some(task) = task {
                send(app, Some(task));
            }
        }
        "sb-review" => {
            let mut a = app.borrow_mut();
            a.sandbox.opener = document()
                .active_element()
                .map(|n| n.id())
                .unwrap_or_default();
            a.sandbox.review = Some(Review {
                id: id.into(),
                task: None,
                tab: String::new(),
                paths: BTreeSet::new(),
                error: String::new(),
                load_error: String::new(),
                busy: false,
                polling: false,
            });
            drop(a);
            render(app);
            poll_review(app);
        }
        "sb-apply" => apply(app),
        "sb-export" => {
            if let Some(t) = app
                .borrow()
                .sandbox
                .review
                .as_ref()
                .and_then(|r| r.task.as_ref())
            {
                crate::download(
                    &format!("sandbox-{}.json", text(t, "id")),
                    &serde_json::to_string_pretty(t).unwrap_or_default(),
                );
            }
        }
        "sb-delete" => delete_project(app),
        "sb-older" => older(app),
        "sb-tab-chat" | "sb-tab-tasks" => {
            app.borrow_mut().sandbox.tab = if name == "sb-tab-chat" {
                "chat"
            } else {
                "tasks"
            }
            .into();
            render(app);
        }
        "sb-review-changes" | "sb-review-activity" => {
            if let Some(r) = &mut app.borrow_mut().sandbox.review {
                r.tab = if name == "sb-review-changes" {
                    "changes"
                } else {
                    "activity"
                }
                .into();
            }
            render(app);
        }
        "sb-suggest-plan" | "sb-suggest-code" => {
            let mut a = app.borrow_mut();
            let code = name == "sb-suggest-code";
            a.sandbox.input = tr(
                &a,
                if code {
                    "Find a bug and add a regression test"
                } else {
                    "Help me plan my next feature"
                },
                if code {
                    "버그를 찾고 회귀 테스트를 추가해줘"
                } else {
                    "다음 기능의 구현 계획을 세워줘"
                },
            )
            .into();
            a.sandbox.kind = if code { "code" } else { "chat" }.into();
            dirty(&mut a);
            drop(a);
            render(app);
        }
        _ => {}
    }
}
fn choose_project(app: &Shared, id: &str) {
    let mut a = app.borrow_mut();
    if a.sandbox.busy {
        return;
    }
    if !a.sandbox.input.trim().is_empty()
        && !window()
            .confirm_with_message(tr(
                &a,
                "Discard the unsent message and switch projects?",
                "보내지 않은 내용을 지우고 프로젝트를 바꿀까요?",
            ))
            .unwrap_or(false)
    {
        return;
    }
    let s = &mut a.sandbox;
    s.project = id.into();
    s.tasks.clear();
    s.tasks_loaded = false;
    s.task_error.clear();
    s.input.clear();
    s.selection = Value::Null;
    s.pending = None;
    s.review = None;
    s.resource_editor = None;
    s.environments.clear();
    s.previews.clear();
    s.resources_at = 0.0;
    s.resources_polling = false;
    s.coordination = Value::Null;
    s.environment_role.clear();
    s.context += 1;
    s.polling = false;
    s.since = None;
    s.older_end = false;
    s.stopping.clear();
    dirty(&mut a);
    drop(a);
    let _ = window().history().and_then(|h| {
        h.replace_state_with_url(
            &JsValue::NULL,
            "",
            Some(&format!("#/ai-sandbox?project={}", api::encode(id))),
        )
    });
    render(app);
    tick(app);
}
fn open_editor(app: &Shared, existing: bool) {
    let mut a = app.borrow_mut();
    let s = &mut a.sandbox;
    if s.busy || s.tasks.iter().any(active) {
        return;
    }
    let p = if existing { project(s).cloned() } else { None };
    let id = if let Some(p) = &p {
        Ok(text(p, "id").into())
    } else {
        uuid()
    };
    let id = match id {
        Ok(v) => v,
        Err(e) => {
            s.error = e;
            drop(a);
            render(app);
            return;
        }
    };
    let spec = p.as_ref().map(|p| p["spec"].clone()).unwrap_or_else(
        || json!({"name":"","sourcePath":null,"vmId":null,"selection":null,"instructions":""}),
    );
    let connection = spec["selection"]["connectionId"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| {
            s.accounts
                .first()
                .map(|v| text(v, "id").into())
                .unwrap_or_default()
        });
    let model = text(&spec["selection"], "model").to_owned();
    let catalog = s.catalogs.get(&connection).cloned().unwrap_or(Value::Null);
    s.opener = document()
        .active_element()
        .map(|n| n.id())
        .unwrap_or_default();
    s.editor = Some(Editor {
        id,
        existing,
        spec,
        connection,
        model,
        tools: true,
        catalog,
        loading: false,
        busy: false,
        revision: 0,
        error: String::new(),
    });
    dirty(&mut a);
    drop(a);
    render(app);
    load_catalog(app, false);
}
fn load_catalog(app: &Shared, discover: bool) {
    let mut a = app.borrow_mut();
    let generation = a.generation;
    let Some(e) = &mut a.sandbox.editor else {
        return;
    };
    if e.connection.is_empty() || e.loading || e.busy {
        return;
    }
    e.loading = true;
    e.revision += 1;
    let (connection, id, revision) = (e.connection.clone(), e.id.clone(), e.revision);
    drop(a);
    render_after_event(app);
    let app = app.clone();
    spawn_local(async move {
        let result = api::request(
            "GET",
            &format!(
                "/api/ai-sandbox/connections/{}/models?discover={discover}",
                api::encode(&connection)
            ),
            None,
        )
        .await;
        if !live(&app, generation) {
            return;
        }
        let mut a = app.borrow_mut();
        let Some(e) = &mut a.sandbox.editor else {
            return;
        };
        if e.id != id || e.connection != connection || e.revision != revision {
            return;
        }
        e.loading = false;
        match result {
            Ok(v) => {
                e.catalog = v.clone();
                a.sandbox.catalogs.insert(connection, v);
            }
            Err(error) => e.error = error,
        }
        drop(a);
        render(&app);
    });
}
fn verify(app: &Shared) {
    let mut a = app.borrow_mut();
    let generation = a.generation;
    let Some(e) = &mut a.sandbox.editor else {
        return;
    };
    if e.busy || e.loading || e.model.trim().is_empty() {
        return;
    }
    e.busy = true;
    e.error.clear();
    let (id, connection, model, tools) = (
        e.id.clone(),
        e.connection.clone(),
        e.model.trim().to_owned(),
        e.tools,
    );
    drop(a);
    render(app);
    let app = app.clone();
    spawn_local(async move {
        let result = api::request(
            "POST",
            &format!(
                "/api/ai-sandbox/connections/{}/verify",
                api::encode(&connection)
            ),
            Some(&json!({"model":model,"tools":tools})),
        )
        .await;
        if !live(&app, generation) {
            return;
        }
        let mut a = app.borrow_mut();
        let Some(e) = &mut a.sandbox.editor else {
            return;
        };
        if e.id != id || e.connection != connection {
            return;
        }
        e.busy = false;
        match result {
            Ok(v) => {
                let mut models = list(&e.catalog["models"])
                    .iter()
                    .filter(|m| m["id"] != v["id"])
                    .cloned()
                    .collect::<Vec<_>>();
                models.push(v.clone());
                e.catalog =
                    json!({"discoveryAvailable":e.catalog["discoveryAvailable"],"models":models});
                e.spec["selection"] = json!({"connectionId":connection,"model":v["id"]});
                let catalog = e.catalog.clone();
                a.sandbox.catalogs.insert(connection, catalog);
            }
            Err(error) => e.error = error,
        }
        drop(a);
        render(&app);
    });
}
fn save_project(app: &Shared) {
    let mut a = app.borrow_mut();
    let generation = a.generation;
    let Some(e) = &mut a.sandbox.editor else {
        return;
    };
    if e.busy {
        return;
    }
    e.busy = true;
    e.error.clear();
    let (id, spec) = (e.id.clone(), e.spec.clone());
    drop(a);
    render(app);
    let app = app.clone();
    spawn_local(async move {
        let result = api::request(
            "PUT",
            &format!("/api/ai-sandbox/projects/{}", api::encode(&id)),
            Some(&spec),
        )
        .await;
        if !live(&app, generation) {
            return;
        }
        let mut a = app.borrow_mut();
        let Some(e) = &mut a.sandbox.editor else {
            return;
        };
        if e.id != id {
            return;
        }
        e.busy = false;
        match result {
            Ok(p) => {
                let s = &mut a.sandbox;
                let rows = s.projects.get_or_insert_default();
                if let Some(old) = rows.iter_mut().find(|v| v["id"] == p["id"]) {
                    *old = p;
                } else {
                    rows.push(p);
                }
                s.editor = None;
                let changed = s.project != id;
                s.project = id.clone();
                if changed {
                    s.tasks.clear();
                    s.tasks_loaded = false;
                    s.since = None;
                    s.polling = false;
                    s.context += 1;
                    s.pending = None;
                    s.selection = Value::Null;
                }
                dirty(&mut a);
            }
            Err(error) => {
                e.error = error;
            }
        }
        drop(a);
        render(&app);
        tick(&app);
    });
}
fn send(app: &Shared, retry: Option<Value>) {
    let mut a = app.borrow_mut();
    if a.sandbox.busy || a.sandbox.tasks.iter().any(active) {
        return;
    }
    if retry.is_none() && !can_send(&a) {
        return;
    }
    let Some(p) = project(&a.sandbox).cloned() else {
        return;
    };
    let request = if let Some(task) = &retry {
        let mut r = task["request"].clone();
        r["selection"] = task["selection"].clone();
        r["retryOf"] = task["id"].clone();
        r
    } else {
        json!({"kind":if a.sandbox.kind=="code"{"code"}else{"chat"},"input":a.sandbox.input.trim(),"selection":a.sandbox.selection,"retryOf":null,"coordination":if a.sandbox.coordination.is_object(){a.sandbox.coordination.clone()}else{json!({"dependencies":[],"assignedAgent":null,"retryLimit":3})},"environmentRole":if a.sandbox.environment_role.is_empty(){Value::Null}else{json!(a.sandbox.environment_role)}})
    };
    if text(&request, "input").is_empty() {
        return;
    }
    let id = if let Some((previous, id)) = &a.sandbox.pending
        && *previous == request
    {
        id.clone()
    } else {
        match uuid() {
            Ok(id) => id,
            Err(e) => {
                a.sandbox.error = e;
                drop(a);
                render(app);
                return;
            }
        }
    };
    a.sandbox.pending = Some((request.clone(), id.clone()));
    a.sandbox.busy = true;
    a.sandbox.error.clear();
    let (generation, context, project) =
        (a.generation, a.sandbox.context, text(&p, "id").to_owned());
    drop(a);
    render(app);
    let app = app.clone();
    spawn_local(async move {
        let result = api::request(
            "PUT",
            &format!(
                "/api/ai-sandbox/projects/{}/tasks/{}",
                api::encode(&project),
                api::encode(&id)
            ),
            Some(&request),
        )
        .await;
        if !live(&app, generation) {
            return;
        }
        let mut a = app.borrow_mut();
        if a.sandbox.context != context {
            return;
        }
        a.sandbox.busy = false;
        match result {
            Ok(t) => {
                update_task(&mut a, t);
                a.sandbox.pending = None;
                a.sandbox.input.clear();
                a.sandbox.review = None;
                if request["kind"] == "code" {
                    a.sandbox.tab = "tasks".into();
                }
                dirty(&mut a);
            }
            Err(e) => a.sandbox.error = e,
        }
        drop(a);
        render(&app);
    });
}
fn mutate_task(app: &Shared, id: &str, operation: &str) {
    let mut a = app.borrow_mut();
    if a.sandbox.busy {
        return;
    }
    a.sandbox.busy = true;
    a.sandbox.error.clear();
    if operation == "cancel" {
        a.sandbox.stopping = id.into();
    }
    let (generation, context) = (a.generation, a.sandbox.context);
    drop(a);
    render(app);
    let (app, id, operation) = (app.clone(), id.to_owned(), operation.to_owned());
    spawn_local(async move {
        let result = api::request(
            "POST",
            &format!("/api/ai-sandbox/tasks/{}/{operation}", api::encode(&id)),
            None,
        )
        .await;
        if !live(&app, generation) {
            return;
        }
        let mut a = app.borrow_mut();
        if a.sandbox.context != context {
            return;
        }
        a.sandbox.busy = false;
        match result {
            Ok(t) => update_task(&mut a, t),
            Err(e) => {
                a.sandbox.error = e;
                a.sandbox.stopping.clear();
            }
        }
        drop(a);
        render(&app);
    });
}
fn apply(app: &Shared) {
    let mut a = app.borrow_mut();
    let generation = a.generation;
    let Some(r) = &mut a.sandbox.review else {
        return;
    };
    if r.busy || !r.load_error.is_empty() {
        return;
    }
    let Some(task) = &r.task else {
        return;
    };
    if active(task) || task["state"] == "interrupted" {
        return;
    }
    let paths = list(&task["changes"])
        .iter()
        .filter(|c| c["applied"] != true && r.paths.contains(text(c, "path")))
        .map(|c| c["path"].clone())
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return;
    }
    r.busy = true;
    r.error.clear();
    let id = r.id.clone();
    drop(a);
    render(app);
    let app = app.clone();
    spawn_local(async move {
        let result = api::request(
            "POST",
            &format!("/api/ai-sandbox/tasks/{}/apply", api::encode(&id)),
            Some(&json!({"paths":paths})),
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
        r.busy = false;
        match result {
            Ok(t) => update_task(&mut a, t),
            Err(e) => r.error = e,
        }
        drop(a);
        render(&app);
    });
}
fn older(app: &Shared) {
    let mut a = app.borrow_mut();
    if a.sandbox.busy {
        return;
    }
    let Some(first) = a.sandbox.tasks.first() else {
        return;
    };
    let path = format!(
        "/api/ai-sandbox/projects/{}/tasks?before={}",
        api::encode(&a.sandbox.project),
        api::encode(text(first, "id"))
    );
    let (generation, context) = (a.generation, a.sandbox.context);
    a.sandbox.busy = true;
    drop(a);
    render(app);
    let app = app.clone();
    spawn_local(async move {
        let result = api::request("GET", &path, None).await;
        if !live(&app, generation) {
            return;
        }
        let mut a = app.borrow_mut();
        if a.sandbox.context != context {
            return;
        }
        a.sandbox.busy = false;
        match result {
            Ok(v) => {
                a.sandbox.older_end = list(&v).len() < 50;
                merge_tasks(&mut a.sandbox.tasks, list(&v).iter().cloned());
            }
            Err(e) => a.sandbox.error = e,
        }
        drop(a);
        render(&app);
    });
}
fn delete_project(app: &Shared) {
    let mut a = app.borrow_mut();
    if a.sandbox.busy || a.sandbox.tasks.iter().any(active) {
        return;
    }
    if !window().confirm_with_message(tr(&a,"Delete this project's conversation and retained task copies? Source files and the MicroVM are preserved.","대화와 보관된 작업 복사본을 삭제할까요? 원본 파일과 MicroVM은 유지한다.")).unwrap_or(false){return;}
    let (id, generation) = (a.sandbox.project.clone(), a.generation);
    a.sandbox.busy = true;
    drop(a);
    render(app);
    let app = app.clone();
    spawn_local(async move {
        let result = api::request(
            "DELETE",
            &format!("/api/ai-sandbox/projects/{}", api::encode(&id)),
            None,
        )
        .await;
        if !live(&app, generation) {
            return;
        }
        let mut a = app.borrow_mut();
        a.sandbox.busy = false;
        match result {
            Ok(_) => {
                a.sandbox
                    .projects
                    .as_mut()
                    .unwrap()
                    .retain(|p| text(p, "id") != id);
                a.sandbox.project.clear();
                a.sandbox.tasks.clear();
                a.sandbox.tasks_loaded = false;
                a.sandbox.pending = None;
                a.sandbox.context += 1;
                a.sandbox.input.clear();
                dirty(&mut a);
            }
            Err(e) => a.sandbox.error = e,
        }
        drop(a);
        render(&app);
        load(app.clone());
    });
}
