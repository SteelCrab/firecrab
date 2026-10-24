pub mod ai;
pub mod images;
pub mod pools;
pub mod schedules;
pub mod secrets;
pub mod security;
pub mod services;
pub mod storage;
use crate::{
    App, Shared, api, element,
    model::{list, text},
    render_content, render_detail, render_messages, window,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::spawn_local;
use web_sys::{Element, Event, HtmlInputElement, HtmlSelectElement};
#[derive(Default)]
pub struct State {
    pub context: String,
    pub storage: storage::State,
    pub ai: ai::State,
    pub services: services::State,
    pub secrets_external: bool,
    pub reveal_generation: u64,
    pub reveals: BTreeMap<String, (String, f64)>,
    pub plan: Option<Value>,
    pub plan_names: Vec<String>,
    pub plan_expired: bool,
    pub security_loading: bool,
}
pub fn content(app: &App) -> String {
    match app.route.as_str() {
        "pools" => pools::content(app),
        "schedules" => schedules::content(app),
        "secrets" => secrets::content(app),
        "security-groups" => security::content(app),
        "pipe/storage" => storage::content(app),
        _ => String::new(),
    }
}
pub fn detail(app: &App, data: &Value) -> Option<String> {
    match app.route.as_str() {
        "pools" => Some(pools::detail(app, data)),
        "schedules" => Some(schedules::detail(app, data)),
        "security-groups" => Some(if list(&data["rules"]).is_empty() {
            "<p>No allow rules. All new peer connections are denied.</p>".into()
        } else {
            crate::render::properties(&data["rules"])
        }),
        "secrets" => Some(String::new()),
        _ => None,
    }
}
pub fn actions(app: &App, data: &Value) -> String {
    match app.route.as_str() {
        "pools" => pools::actions(data),
        "schedules" => schedules::actions(data),
        _ => String::new(),
    }
}
pub fn dispatch(app: &Shared, action: &str, id: &str, button: &Element) -> bool {
    if action == "schedule-day" {
        let Some(input) =
            element("field-days").and_then(|el| el.dyn_into::<HtmlInputElement>().ok())
        else {
            return true;
        };
        let old = input.value();
        let mut days: Vec<_> = old.split(',').filter(|s| !s.is_empty()).collect();
        if days.contains(&id) {
            days.retain(|day| *day != id);
        } else {
            days.push(id);
        }
        let value = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
            .into_iter()
            .filter(|d| days.contains(d))
            .collect::<Vec<_>>()
            .join(",");
        input.set_value(&value);
        let _ = button.set_attribute(
            "aria-pressed",
            if days.contains(&id) { "true" } else { "false" },
        );
        app.borrow_mut().dirty = true;
        return true;
    }
    services::dispatch(app, action, id)
        || ai::dispatch(app, action, id)
        || storage::dispatch(app, action, id)
        || images::dispatch(app, action, id)
        || pools::dispatch(app, action, id)
        || schedules::dispatch(app, action, id)
        || secrets::dispatch(app, action, id)
        || security::dispatch(app, action, id)
}
pub fn change(app: &Shared, target: &Element) -> bool {
    if services::change(app, target) || ai::change(app, target) || storage::change(app, target) {
        return true;
    }
    if target.id() == "schedule-filter" {
        let value = target.clone().unchecked_into::<HtmlSelectElement>().value();
        let _ = window()
            .location()
            .set_hash(&format!("/schedules?vm={}", api::encode(&value)));
        return true;
    }
    if target.has_attribute("data-plan-name") {
        let name = target.get_attribute("data-plan-name").unwrap_or_default();
        let checked = target
            .clone()
            .unchecked_into::<HtmlInputElement>()
            .checked();
        let mut state = app.borrow_mut();
        state.advanced.plan_names.retain(|s| s != &name);
        if checked {
            state.advanced.plan_names.push(name);
        }
        drop(state);
        secrets::render_plan(app);
        return true;
    }
    if target.closest("#editor").ok().flatten().is_some() {
        form_input(app, target);
    }
    false
}
pub fn form_input(_app: &Shared, _target: &Element) {
    sync_form();
}
pub fn sync_form() {
    secrets::sync_editor();
    if let Some(network) =
        element("field-microNetworkId").and_then(|el| el.dyn_into::<HtmlSelectElement>().ok())
        && element("editor").is_some_and(|el| el.get_attribute("data-policy-network").is_some())
    {
        network.set_disabled(true);
    }
    let value = |key: &str| {
        element(&format!("field-{key}"))
            .and_then(|el| el.dyn_into::<HtmlSelectElement>().ok())
            .map(|el| el.value())
            .unwrap_or_default()
    };
    let repeat = value("repeat");
    if !repeat.is_empty() {
        for (key, show) in [
            ("expression", repeat == "cron"),
            ("time", repeat != "cron"),
            ("date", repeat == "once"),
            ("days", repeat == "weekly"),
            ("timeZone", repeat != "once"),
        ] {
            visible(key, show);
        }
    }
    if let Some(credentials) =
        element("field-credentialRef").and_then(|el| el.dyn_into::<HtmlSelectElement>().ok())
    {
        let own = credentials.value() == "connection";
        let replace = value("replaceKeys") != "false";
        for key in ["accessKeyId", "secretAccessKey", "sessionToken"] {
            visible(key, own && replace);
        }
        visible("replaceKeys", own);
    }
    let provider = value("provider");
    if element("field-projectId").is_some()
        && let Some(label) = element("field-token")
            .and_then(|el| el.closest(".field").ok().flatten())
            .and_then(|el| el.query_selector("label").ok().flatten())
    {
        label.set_text_content(Some(if provider == "firecrab" {
            "Bearer token (optional)"
        } else {
            "Access token"
        }));
    }
    if !provider.is_empty() && element("field-projectId").is_some() {
        for (key, show) in [
            ("baseUrl", provider != "awsSecretsManager"),
            ("projectId", provider == "infisical"),
            ("environment", provider == "infisical"),
            ("secretPath", provider == "infisical"),
            ("region", provider == "awsSecretsManager"),
            ("prefix", provider == "awsSecretsManager"),
            ("token", provider != "awsSecretsManager"),
            ("accessKeyId", provider == "awsSecretsManager"),
            ("secretAccessKey", provider == "awsSecretsManager"),
            ("sessionToken", provider == "awsSecretsManager"),
        ] {
            visible(key, show);
        }
    }
}
fn visible(key: &str, show: bool) {
    if let Some(el) = element(&format!("field-{key}"))
        .and_then(|el| el.closest(".field").ok().flatten())
        .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
    {
        el.set_hidden(!show);
    }
}
pub fn prepare(app: &Shared) {
    let mut state = app.borrow_mut();
    let Some(mut form) = state.form.take() else {
        return;
    };
    if form.return_route == "services" {
        form.choose(
            "storageConnectionId",
            state.cache.get("/api/pipe/storage").unwrap_or(&Value::Null),
            "id",
            "name",
        );
    }
    if form.title == "New security group" {
        form.base["disabledNetworks"] = json!(
            list(
                state
                    .cache
                    .get("/api/micro-networks")
                    .unwrap_or(&Value::Null)
            )
            .iter()
            .filter(|n| !text(n, "ipv6Cidr").is_empty())
            .map(|n| n["id"].clone())
            .collect::<Vec<_>>()
        );
    }
    if form.return_route == "pools" {
        let images = json!(
            list(state.cache.get("/api/images").unwrap_or(&Value::Null))
                .iter()
                .filter(|v| v["installed"] == true && !text(v, "alias").starts_with("__"))
                .cloned()
                .collect::<Vec<_>>()
        );
        form.choose("template", &images, "alias", "alias");
        if let Some(image) = list(&images).first() {
            for field in &mut form.fields {
                if field.key == "template" && field.value.is_empty() {
                    field.value = text(image, "alias").into();
                }
                if field.key == "diskGb" {
                    field.value = image["minDiskGb"].as_u64().unwrap_or(2).max(2).to_string();
                }
            }
        }
        for field in &mut form.fields {
            if field.key == "microNetworkId" && field.value.is_empty() {
                field.value = field
                    .options
                    .first()
                    .map(|(id, _)| id.clone())
                    .unwrap_or_default();
            }
        }
    }
    if form.return_route == "schedules" {
        let vms = json!(
            state
                .vms
                .iter()
                .filter(|vm| vm.extra.get("purpose").and_then(Value::as_str) != Some("pool"))
                .map(|vm| json!({"id":vm.id,"name":vm.name}))
                .collect::<Vec<_>>()
        );
        form.choose("vmId", &vms, "id", "name");
        form.base["vmNames"] = json!(
            state
                .vms
                .iter()
                .map(|vm| (vm.id.clone(), json!(vm.name)))
                .collect::<BTreeMap<_, _>>()
        );
        if let Some(field) = form.fields.iter_mut().find(|f| f.key == "vmId")
            && field.value.is_empty()
        {
            field.value = query("vm")
                .filter(|s| !s.is_empty())
                .or_else(|| field.options.first().map(|(id, _)| id.clone()))
                .unwrap_or_default();
        }
    }
    let policy = form.return_route == "services";
    if policy {
        form.base["policyError"] = json!(true);
    }
    state.form = Some(form);
    drop(state);
    if policy {
        security::policy(app);
    }
    sync_form();
}
pub fn query(key: &str) -> Option<String> {
    let hash = window().location().hash().unwrap_or_default();
    hash.split_once('?').and_then(|(_, q)| {
        q.split('&').find_map(|part| {
            part.split_once('=')
                .filter(|(k, _)| *k == key)
                .map(|(_, v)| {
                    js_sys::decode_uri_component(v)
                        .ok()
                        .and_then(|v| v.as_string())
                        .unwrap_or_default()
                })
        })
    })
}
pub fn same_context(app: &App) -> bool {
    app.advanced.context == window().location().hash().unwrap_or_default()
}
pub fn aux_loaded(app: &Shared, path: &str) {
    if app.borrow().form.is_some() {
        capture_form(app);
        crate::workflows::prepare_form(app);
        prepare(app);
        crate::render_editor(app);
    }
    if path == "/api/schedules" && app.borrow().route == "vms" && app.borrow().form.is_none() {
        render_detail(app);
    }
}
pub fn after_refresh(app: &Shared) {
    services::after_refresh(app);
    if app.borrow().route == "images" {
        images::load(app);
        crate::workflows::restore_job(app);
    }
    if app.borrow().route == "security-groups" {
        security::load(app);
    }
    let hash = window().location().hash().unwrap_or_default();
    if app.borrow().advanced.context == hash {
        return;
    }
    app.borrow_mut().advanced.context = hash;
    let route = app.borrow().route.clone();
    if route == "vms"
        && let Some(vm) = query("vm").filter(|s| !s.is_empty())
    {
        crate::show_detail(app.clone(), vm);
    }
    if route == "schedules" {
        if query("new").is_some() {
            crate::open_form(app.clone(), "schedules", "");
        } else if let Some(id) = query("schedule") {
            crate::show_detail(app.clone(), id);
        }
    }
}
pub fn detail_selected(app: &Shared) {
    let (route, id) = {
        let s = app.borrow();
        (
            s.route.clone(),
            s.detail
                .as_ref()
                .map(|d| text(d, "id").to_string())
                .unwrap_or_default(),
        )
    };
    if route == "vms" && !id.is_empty() {
        let hash = format!("#/vms?vm={}", api::encode(&id));
        crate::save_history_index(app.borrow().history_index, Some(&hash));
        app.borrow_mut().advanced.context = hash;
    }
    if route == "pools" && !id.is_empty() {
        load_extra(
            app,
            "leases",
            &format!("/api/pools/{}/leases", api::encode(&id)),
        );
    }
    if route == "schedules" {
        load_extra(
            app,
            "runs",
            &format!("/api/schedules/{}/runs", api::encode(&id)),
        );
    }
}
pub fn load_extra(app: &Shared, key: &str, path: &str) {
    let a = app.clone();
    let key = key.to_string();
    let path = path.to_string();
    let generation = app.borrow().detail_generation;
    spawn_local(async move {
        let result = api::request("GET", &path, None).await;
        let mut s = a.borrow_mut();
        if s.detail_generation != generation {
            return;
        }
        match result {
            Ok(v) => {
                if let Some(d) = s.detail.as_mut() {
                    d[&key] = v;
                }
            }
            Err(e) => s.error = e,
        };
        let editing = s.form.is_some();
        drop(s);
        if !editing {
            render_detail(&a);
        }
        render_messages(&a);
    });
}
pub fn after_mutation(app: &Shared, path: &str) {
    services::after_mutation(app, path);
    if path == "/api/security-groups" && app.borrow().detail.is_some() {
        let id = app
            .borrow()
            .detail
            .as_ref()
            .map(|d| text(d, "id").to_string())
            .unwrap_or_default();
        crate::open_form(app.clone(), "security-rules", &id);
    }
    if path.starts_with("/api/secrets") {
        app.borrow_mut().advanced.reveals.clear();
        app.borrow_mut().advanced.reveal_generation += 1;
    }
    if path.starts_with("/api/secrets/connections") {
        secrets::connections(app);
    }
    if app.borrow().route == "pools" {
        detail_selected(app);
    }
}
pub fn install(app: &Shared) -> Result<(), JsValue> {
    for (target, event) in [
        (window().unchecked_into::<web_sys::EventTarget>(), "blur"),
        (
            crate::document().unchecked_into::<web_sys::EventTarget>(),
            "visibilitychange",
        ),
    ] {
        let a = app.clone();
        let callback = Closure::<dyn FnMut(Event)>::new(move |_| {
            a.borrow_mut().advanced.reveal_generation += 1;
            if !a.borrow().advanced.reveals.is_empty() {
                a.borrow_mut().advanced.reveals.clear();
                if a.borrow().route == "secrets" && a.borrow().form.is_none() {
                    render_content(&a);
                }
            }
        });
        target.add_event_listener_with_callback(event, callback.as_ref().unchecked_ref())?;
        callback.forget();
    }
    let a = app.clone();
    let tick = Closure::<dyn FnMut()>::new(move || secrets::tick(&a));
    window().set_interval_with_callback_and_timeout_and_arguments_0(
        tick.as_ref().unchecked_ref(),
        1000,
    )?;
    tick.forget();
    Ok(())
}

pub fn capture_form(app: &Shared) {
    let mut s = app.borrow_mut();
    if let Some(form) = s.form.as_mut() {
        for f in &mut form.fields {
            if let Some(el) = element(&format!("field-{}", f.key)) {
                if let Some(v) = el.dyn_ref::<HtmlInputElement>() {
                    f.value = v.value();
                } else if let Some(v) = el.dyn_ref::<HtmlSelectElement>() {
                    f.value = v.value();
                } else if let Some(v) = el.dyn_ref::<web_sys::HtmlTextAreaElement>() {
                    f.value = v.value();
                }
            }
        }
    }
}
