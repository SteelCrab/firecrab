mod advanced;
mod api;
mod databases;
mod deploy;
mod details;
mod forms;
mod i18n;
mod manager;
mod model;
mod notes;
mod render;
mod sandbox;
mod terminal;
mod updates;
mod vm_list;
mod volumes;
mod workflows;

use forms::Form;
use model::{VIEWS, Vm, escape, list, text};
use serde_json::{Value, json};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::spawn_local;
use web_sys::{
    Document, Element, Event, FormData, HtmlFormElement, HtmlInputElement, HtmlSelectElement,
    KeyboardEvent,
};

type Shared = Rc<RefCell<App>>;

#[derive(Default)]
struct App {
    route: String,
    locale: String,
    filter: String,
    query: String,
    vms: Vec<Vm>,
    vm_rows: Vec<Option<web_sys::HtmlElement>>,
    vm_indices: Vec<usize>,
    vm_window: Option<(usize, usize)>,
    vm_scroll_top: i32,
    search_index: Vec<String>,
    resource: Value,
    cache: BTreeMap<String, Value>,
    detail: Option<Value>,
    detail_loading: bool,
    detail_result: String,
    form: Option<Form>,
    dirty: bool,
    loaded: bool,
    rendering: bool,
    busy: bool,
    refreshing: bool,
    error: String,
    connection_error: String,
    field_errors: BTreeMap<String, String>,
    notice: String,
    generation: u64,
    detail_generation: u64,
    detail_polling: Option<u64>,
    service_deployment: Option<Value>,
    deploy: deploy::State,
    job: Option<(String, Value)>,
    workflows: workflows::State,
    databases: databases::State,
    advanced: advanced::State,
    sandbox: sandbox::State,
    volumes: volumes::State,
    console: Option<terminal::State>,
    console_retry: Option<Closure<dyn FnMut()>>,
    panel_open: bool,
    panel_width: u32,
    panel_dragging: bool,
    panel_drag_x: i32,
    panel_drag_width: u32,
    detail_tab: String,
    detail_log: String,
    detail_log_loaded: bool,
    show_env: bool,
    rendered_selected: String,
    pending_vm: Option<String>,
    palette_query: String,
    palette_index: usize,
    history_index: i32,
    restoring_history: bool,
}

fn window() -> web_sys::Window {
    web_sys::window().expect("browser window")
}
fn document() -> Document {
    window().document().expect("browser document")
}
fn element(id: &str) -> Option<Element> {
    document().get_element_by_id(id)
}
fn set_html(id: &str, html: &str) {
    if let Some(node) = element(id) {
        node.set_inner_html(html);
    }
}
fn render_page(app: &Shared) {
    let html = render::page(&app.borrow());
    app.borrow_mut().rendering = true;
    set_html("root", &html);
    app.borrow_mut().rendering = false;
    update_host(app);
    vm_list::cache(app);
    render_detail(app);
    if app.borrow().route == "ai-sandbox" {
        sandbox::render(app);
    }
}
fn update_host(app: &Shared) {
    let state = app.borrow();
    let Some(host) = state.cache.get("/api/host") else {
        return;
    };
    set_html("host-status", &render::host_meters(&state));
    if state.route == "vms"
        && state.loaded
        && let Some(summary) = element("vm-summary")
    {
        summary.set_outer_html(&render::vm_summary(&state));
    }
    let name = text(&host["platform"], "name");
    if !name.is_empty()
        && let Some(platform) = element("sidebar-platform")
    {
        platform.set_text_content(Some(&format!(
            "{name} · {}",
            text(&host["platform"], "architecture")
        )));
    }
}

fn render_content(app: &Shared) {
    if app.borrow().route == "ai-sandbox" {
        sandbox::render(app);
        return;
    }
    let cluster_focus = advanced::cluster::focused_input();
    let (html, loaded) = {
        let state = app.borrow();
        (render::content(&state), state.loaded)
    };
    app.borrow_mut().rendering = true;
    set_html("content", &html);
    advanced::cluster::restore_focus(cluster_focus);
    app.borrow_mut().rendering = false;
    if let Some(content) = element("content") {
        let _ = content.set_attribute("data-loaded", if loaded { "true" } else { "false" });
    }
    render_messages(app);
    vm_list::cache(app);
    render_detail(app);
    workflows::sync_form();
    advanced::services::sync(app);
}
fn render_editor(app: &Shared) {
    if app.borrow().route == "pipe/database" {
        render_content(app);
        return;
    }

    if app.borrow().route == "vms/new" {
        render_content(app);
    } else {
        render_detail(app);
        workflows::sync_form();
    }
    advanced::services::sync(app);
    let state = app.borrow();
    if let Some((path, html)) = &state.workflows.form_result
        && state.form.as_ref().is_some_and(|form| &form.path == path)
    {
        set_html("form-result", html);
    }
}
fn render_detail(app: &Shared) {
    let more_open = element("vm-more-actions").is_some_and(|el| el.has_attribute("open"));
    let focused = document()
        .active_element()
        .filter(|el| el.closest("#detail-panel").ok().flatten().is_some())
        .map(|el| el.id())
        .unwrap_or_default();
    let (open, width, html, selection) = {
        let mut state = app.borrow_mut();
        let narrow = window()
            .inner_width()
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(1440.0)
            < 1280.0;
        if !narrow {
            let viewport = window()
                .inner_width()
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(1440.0) as u32;
            state.panel_width = state
                .panel_width
                .min(viewport.saturating_sub(716).clamp(320, 720));
        }
        let open =
            (state.route != "security-groups" || state.detail.is_some() || state.form.is_some())
                && state.panel_open
                && ![
                    "vms/new",
                    "queues",
                    "cluster",
                    "pipe/database",
                    "ai-sandbox",
                ]
                .contains(&state.route.as_str())
                && (!narrow || state.detail.is_some() || state.form.is_some());
        let html = if open {
            details::panel(&state)
        } else {
            String::new()
        };
        let selected = state
            .detail
            .as_ref()
            .map(|d| text(d, "id").to_string())
            .unwrap_or_default();
        let mut updates = Vec::new();
        if state.rendered_selected != selected {
            for id in [&state.rendered_selected, &selected] {
                if let Some(index) = state.vms.iter().position(|vm| vm.id == *id)
                    && let Some(row) = state.vm_rows.get(index).and_then(Option::as_ref)
                {
                    updates.push((row.clone(), *id == selected));
                }
            }
            state.rendered_selected = selected;
        }
        (open, state.panel_width, html, updates)
    };
    app.borrow_mut().rendering = true;
    if let Some(panel) =
        element("detail-panel").and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
    {
        panel.set_hidden(!open);
        let _ = panel.set_attribute("data-view", &app.borrow().route);
        let _ = panel.set_attribute("style", &format!("width:{width}px"));
        if open {
            panel.set_inner_html(&html);
            if more_open && let Some(menu) = element("vm-more-actions") {
                let _ = menu.set_attribute("open", "");
            }
        }
    }
    app.borrow_mut().rendering = false;
    if !focused.is_empty()
        && let Some(control) =
            element(&focused).and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
    {
        let options = web_sys::FocusOptions::new();
        options.set_prevent_scroll(true);
        let _ = control.focus_with_options(&options);
    }
    for (row, selected) in selection {
        let _ = row.set_attribute("aria-selected", if selected { "true" } else { "false" });
        row.set_class_name(if selected { "selected" } else { "" });
    }
    if let Ok(buttons) = document().query_selector_all("[data-action=service-run]") {
        let editing = app
            .borrow()
            .form
            .as_ref()
            .is_some_and(|form| form.title != "Deploy MicroService");
        for index in 0..buttons.length() {
            if let Some(button) = buttons
                .item(index)
                .and_then(|button| button.dyn_into::<Element>().ok())
            {
                if editing {
                    let _ = button.set_attribute("disabled", "");
                } else {
                    let _ = button.remove_attribute("disabled");
                }
            }
        }
    }
    if let Some(toggle) = element("panel-toggle") {
        toggle.set_text_content(Some(i18n::t(if open {
            "Hide details"
        } else {
            "Show details"
        })));
    }
    advanced::services::sync(app);
}

fn render_messages(app: &Shared) {
    let state = app.borrow();
    set_html(
        "messages",
        &if state.route == "pipe/database" {
            String::new()
        } else {
            render::messages(&state)
        },
    );
    if state.route == "secrets"
        && let Ok(Some(button)) = document().query_selector("[data-action=create][data-id=secrets]")
    {
        if !state.loaded || !state.connection_error.is_empty() {
            let _ = button.set_attribute("disabled", "");
        } else {
            let _ = button.remove_attribute("disabled");
        }
    }
    if let Some(status) = element("api-status") {
        status.set_text_content(Some(if state.connection_error.is_empty() {
            ""
        } else {
            "API unavailable — retrying every 3s"
        }));
    }
}
fn update_toolbar(app: &Shared) {
    let state = app.borrow();
    if let Ok(tabs) = document().query_selector_all(".vm-filter") {
        for i in 0..tabs.length() {
            if let Some(tab) = tabs
                .item(i)
                .and_then(|node| node.dyn_into::<Element>().ok())
            {
                let id = tab.get_attribute("data-id").unwrap_or_default();
                let count = state
                    .vms
                    .iter()
                    .filter(|vm| id == "all" || vm.state == id)
                    .count();
                let _ = tab.set_attribute(
                    "aria-selected",
                    if state.filter == id { "true" } else { "false" },
                );
                if let Ok(Some(counter)) = tab.query_selector(".vm-filter-count") {
                    counter.set_text_content(Some(&count.to_string()));
                }
            }
        }
    }
    if let Ok(Some(alert)) = document().query_selector(".sidebar-alert") {
        let errors = state.vms.iter().filter(|vm| vm.state == "error").count();
        alert.set_text_content(Some(
            if errors == 0 {
                String::new()
            } else {
                errors.to_string()
            }
            .as_str(),
        ));
    }
    if let Ok(Some(counter)) = document().query_selector(".sidebar-count") {
        counter.set_text_content(Some(&state.vms.len().to_string()));
    }
}

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    std::panic::set_hook(Box::new(|error| {
        web_sys::console::error_1(&JsValue::from_str(&error.to_string()))
    }));
    let locale = window()
        .local_storage()?
        .and_then(|storage| storage.get_item("firecrab.locale").ok().flatten())
        .filter(|locale| locale == "ko")
        .unwrap_or("en".into());
    let route = route_from_hash();
    let app = Rc::new(RefCell::new(App {
        route,
        locale,
        filter: "all".into(),
        detail_tab: "overview".into(),
        panel_open: window()
            .local_storage()
            .ok()
            .flatten()
            .and_then(|s| s.get_item("firecrab.detailPanel").ok().flatten())
            .as_deref()
            != Some("closed"),
        panel_width: window()
            .local_storage()
            .ok()
            .flatten()
            .and_then(|s| s.get_item("firecrab.detailPanelWidth").ok().flatten())
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(384)
            .clamp(320, 720),
        ..App::default()
    }));
    if let Some(root) = element("root") {
        root.set_attribute("data-engine", "wasm")?;
    }
    i18n::set(&app.borrow().locale);
    if let Some(root) = document().document_element() {
        root.set_attribute("lang", &app.borrow().locale)?;
    }
    app.borrow_mut().history_index = history_index().unwrap_or(0);
    save_history_index(app.borrow().history_index, None);
    render_page(&app);
    listen(&app)?;
    details::install(&app)?;
    vm_list::install(&app)?;
    terminal::install(&app)?;
    load_route(app.clone());
    updates::resume(app.clone());
    let poll = app.clone();
    let callback = Closure::<dyn FnMut()>::new(move || {
        terminal::tick(&poll);
        advanced::services::tick(&poll);
        updates::tick(&poll);
        poll_host(&poll);
        let state = poll.borrow();
        if state.busy || state.refreshing || state.route.starts_with("console/") {
            return;
        }
        drop(state);
        details::tick(&poll);
        volumes::tick(&poll);
        if poll.borrow().route == "ai-sandbox" {
            sandbox::tick(&poll);
            return;
        }
        refresh(poll.clone());
        poll_job(poll.clone());
    });
    window().set_interval_with_callback_and_timeout_and_arguments_0(
        callback.as_ref().unchecked_ref(),
        3000,
    )?;
    // One application-lifetime timer and delegated listener set, independent of render count.
    callback.forget();
    Ok(())
}
fn route_from_hash() -> String {
    let hash = window().location().hash().unwrap_or_default();
    let hash = hash.split('?').next().unwrap_or(&hash);
    let route = hash.trim_start_matches('#').trim_start_matches('/');
    if route == "vms/new"
        || route.starts_with("console/")
        || [
            "pipe/auth",
            "pipe/ai",
            "pipe/storage",
            "pipe/route",
            "pipe/registry",
            "pipe/database",
            "pipe/firecrab-tunnel",
            "pipe/quick-tunnel",
            "pipe/named-tunnel",
        ]
        .contains(&route)
        || VIEWS.iter().any(|(id, _, _, _)| *id == route)
    {
        return route.into();
    }
    let _ = window()
        .history()
        .and_then(|history| history.replace_state_with_url(&JsValue::NULL, "", Some("#/vms")));
    "vms".into()
}
fn endpoint(route: &str) -> &'static str {
    match route {
        "cluster" => "/api/cluster/hosts",
        "pools" => "/api/pools",
        "schedules" => "/api/schedules",
        "security-groups" => "/api/security-groups",
        "secrets" => "/api/secrets",
        "services" => "/api/micro-services",
        "networks" => "/api/micro-networks",
        "storages" => "/api/micro-storages",
        "volumes" => "/api/microblockstorage",
        "images" => "/api/images",
        "kernels" => "/api/kernels",
        "shells" => "/api/shells",
        "host" => "/api/host",
        "pipe/auth" => "/api/pipe/auth",
        "pipe/ai" => "/api/pipe/ai/connections",
        "pipe/storage" => "/api/pipe/storage",
        "pipe/registry" => "/api/pipe/registries",
        "pipe/database" => "/api/pipe/databases",
        route if route.starts_with("pipe") => "/api/pipe",
        _ => "/api/vms",
    }
}
fn load_route(app: Shared) {
    let route = app.borrow().route.clone();
    terminal::stop(&app);
    if let Some(id) = route.strip_prefix("console/") {
        let id = js_sys::decode_uri_component(id)
            .ok()
            .and_then(|id| id.as_string())
            .unwrap_or_else(|| id.into());
        terminal::start(app.clone(), id);
        return;
    }
    if route == "ai-sandbox" {
        sandbox::load(app);
        return;
    }
    if route == "vms/new" {
        load_create_vm(app);
        return;
    }
    refresh(app.clone());
    if route == "cluster" {
        advanced::cluster::restore(&app);
    }
    let mut aux = vec!["/api/host", "/api/update"];
    if route == "host" {
        aux.push("/api/info");
    }
    if route == "images" {
        aux.extend(["/api/microregistry", "/api/kernels"]);
    }
    if route == "networks" {
        aux.push("/api/network");
    }
    if route == "vms" {
        aux.extend(["/api/micro-networks", "/api/shells", "/api/storage"]);
    }
    if route == "volumes" {
        aux.push("/api/storage");
    }
    if ["services", "pools", "security-groups"].contains(&route.as_str()) {
        aux.extend(["/api/micro-networks", "/api/security-groups"]);
    }
    if route == "pools" {
        aux.extend(["/api/images", "/api/storage"]);
    }
    if route == "vms" {
        aux.push("/api/schedules");
    }
    if route == "secrets" {
        advanced::secrets::connections(&app);
    }
    if route == "security-groups" {
        advanced::security::load(&app);
    }
    if route == "services" {
        aux.extend(["/api/pipe", "/api/pipe/storage", "/api/queues"]);
    }
    if route != "vms" {
        aux.push("/api/vms");
    }
    let generation = app.borrow().generation;
    for path in aux {
        let app = app.clone();
        spawn_local(async move {
            if let Ok(value) = api::request("GET", path, None).await {
                let mut state = app.borrow_mut();
                if state.generation != generation {
                    return;
                }
                if path == "/api/vms"
                    && let Ok(vms) = serde_json::from_value::<Vec<Vm>>(value.clone())
                {
                    state.vms = vms;
                }
                state.cache.insert(path.into(), value);
                let visible = matches!(state.route.as_str(), "host" | "images" | "volumes")
                    && state.form.is_none();
                drop(state);
                if visible {
                    render_content(&app);
                }
                if path == "/api/update" {
                    let available = app
                        .borrow()
                        .cache
                        .get(path)
                        .is_some_and(|value| value["updateAvailable"] == true);
                    set_html(
                        "update-indicator",
                        &if available {
                            render::button("Update", "update-open", "")
                        } else {
                            String::new()
                        },
                    );
                }
                advanced::aux_loaded(&app, path);
                if path == "/api/host" {
                    update_host(&app);
                }
            }
        });
    }
    if route == "host" {
        load_manager(app);
    }
}
fn refresh(app: Shared) {
    if app.borrow().route == "ai-sandbox" {
        sandbox::load(app);
        return;
    }
    if app.borrow().route == "queues" {
        advanced::queues::refresh(app);
        return;
    }
    let mut state = app.borrow_mut();
    if state.refreshing || state.route == "vms/new" {
        return;
    }
    state.refreshing = true;
    let route = state.route.clone();
    let generation = state.generation;
    drop(state);
    spawn_local(async move {
        let result = api::request("GET", endpoint(&route), None).await;
        let placements = if route == "cluster" {
            Some(api::request("GET", "/api/cluster/placements", None).await)
        } else {
            None
        };
        let result = match &placements {
            Some(Err(error)) => Err(error.clone()),
            _ => result,
        };
        let mut state = app.borrow_mut();
        if state.generation != generation {
            return;
        }
        state.refreshing = false;
        let mut changed = !state.loaded || !state.connection_error.is_empty();
        if let Some(Ok(placements)) = placements {
            changed |= state.cache.get("/api/cluster/placements") != Some(&placements);
            state
                .cache
                .insert("/api/cluster/placements".into(), placements);
        }
        match result {
            Ok(value) => {
                if route == "vms" {
                    match serde_json::from_value::<Vec<Vm>>(value) {
                        Ok(mut vms) => {
                            vms.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
                            changed |= state.vms != vms;
                            if changed {
                                state.search_index = vms
                                    .iter()
                                    .map(|vm| {
                                        format!(
                                            "{}\0{}\0{}",
                                            model::lowercase(&vm.name),
                                            model::lowercase(vm.ipv4.as_deref().unwrap_or("")),
                                            model::lowercase(&vm.template)
                                        )
                                    })
                                    .collect();
                                if state.form.is_none()
                                    && state.detail.as_ref().is_some_and(|detail| {
                                        !vms.iter().any(|vm| vm.id == text(detail, "id"))
                                    })
                                {
                                    state.detail = None;
                                    state.detail_generation += 1;
                                }
                                state.vms = vms;
                            }
                        }
                        Err(error) => {
                            state.loaded = true;
                            state.connection_error = format!("Invalid VM response: {error}");
                            drop(state);
                            render_messages(&app);
                            return;
                        }
                    }
                } else {
                    changed |= state.resource != value;
                    state.cache.insert(endpoint(&route).into(), value.clone());
                    state.resource = value;
                }
                state.connection_error.clear();
                state.loaded = true;
            }
            Err(error) => {
                state.connection_error = error;
                state.loaded = true;
            }
        }
        let has_form = state.form.is_some();
        drop(state);
        advanced::after_refresh(&app);
        databases::after_refresh(&app);
        databases::tick(&app);
        if !has_form {
            if changed {
                render_content(&app);
            } else {
                render_messages(&app);
            }
            if route == "vms" {
                update_toolbar(&app);
            }
            workflows::after_refresh(&app);
            let pending = app.borrow_mut().pending_vm.take();
            if route == "vms"
                && let Some(id) = pending
            {
                show_detail(app.clone(), id);
            }
        }
    });
}
fn load_create_vm(app: Shared) {
    let generation = app.borrow().generation;
    spawn_local(async move {
        for path in [
            "/api/images",
            "/api/micro-networks",
            "/api/storage",
            "/api/shells",
            "/api/microblockstorage",
        ] {
            let result = api::request("GET", path, None).await;
            let mut state = app.borrow_mut();
            if state.generation != generation {
                return;
            }
            match result {
                Ok(value) => {
                    state.cache.insert(path.into(), value);
                }
                Err(error) => {
                    state.error = error;
                    state.loaded = true;
                    drop(state);
                    render_content(&app);
                    return;
                }
            }
        }
        let mut state = app.borrow_mut();
        state.form = Some(forms::vm_create(&state.cache));
        state.loaded = true;
        drop(state);
        render_content(&app);
    });
}
fn history_index() -> Option<i32> {
    window()
        .history()
        .ok()
        .and_then(|h| h.state().ok())
        .and_then(|state| {
            js_sys::Reflect::get(&state, &JsValue::from_str("firecrabRouteIndex")).ok()
        })
        .and_then(|v| v.as_f64())
        .map(|v| v as i32)
}
fn save_history_index(index: i32, url: Option<&str>) {
    if let Ok(history) = window().history() {
        let state = history
            .state()
            .ok()
            .filter(JsValue::is_object)
            .unwrap_or_else(|| js_sys::Object::new().into());
        let _ = js_sys::Reflect::set(
            &state,
            &JsValue::from_str("firecrabRouteIndex"),
            &JsValue::from_f64(f64::from(index)),
        );
        let _ = history.replace_state_with_url(&state, "", url);
    }
}
fn allow_discard(app: &Shared) -> bool {
    if app
        .borrow()
        .databases
        .row
        .as_ref()
        .is_some_and(|r| r.uncertain)
    {
        return false;
    }
    let dirty = app.borrow().dirty;
    !dirty
        || window()
            .confirm_with_message(i18n::t("Discard unsaved changes?"))
            .unwrap_or(false)
}
fn navigate(app: &Shared) {
    let next = route_from_hash();
    let next_index = history_index();
    let mut state = app.borrow_mut();
    if state.restoring_history {
        state.restoring_history = false;
        return;
    }
    if state.route == next && advanced::same_context(&state) {
        if let Some(index) = next_index {
            state.history_index = index;
        }
        return;
    }
    drop(state);
    let allowed = allow_discard(app);
    let mut state = app.borrow_mut();
    if !allowed {
        if let Some(index) = next_index.filter(|index| *index != state.history_index) {
            state.restoring_history = true;
            let delta = state.history_index - index;
            drop(state);
            let _ = window().history().and_then(|h| h.go_with_delta(delta));
        } else {
            save_history_index(state.history_index, Some(&format!("#/{}", state.route)));
        }
        return;
    }
    state.history_index = next_index
        .filter(|index| *index != state.history_index)
        .unwrap_or(state.history_index + 1);
    save_history_index(state.history_index, None);
    state.advanced.reveals.clear();
    state.advanced.queues = advanced::queues::State::default();
    state.advanced.ai = advanced::ai::State::default();
    state.sandbox = sandbox::State::default();
    state.advanced.storage = advanced::storage::State::default();
    state.databases = databases::State::default();
    state.volumes = volumes::State::default();
    state.route = next;
    state.generation += 1;
    state.detail_generation += 1;
    state.form = None;
    state.detail = None;
    state.detail_loading = false;
    state.detail_result.clear();
    state.dirty = false;
    state.busy = false;
    state.loaded = false;
    state.refreshing = false;
    state.error.clear();
    state.connection_error.clear();
    state.notice.clear();
    state.job = None;
    state.query.clear();
    state.filter = "all".into();
    drop(state);
    render_page(app);
    load_route(app.clone());
}
fn listen(app: &Shared) -> Result<(), JsValue> {
    let target = document();
    let clicked = app.clone();
    let click = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        let Some(target) = event
            .target()
            .and_then(|target| target.dyn_into::<Element>().ok())
        else {
            return;
        };
        if (target.closest("#vm-more-actions").ok().flatten().is_none()
            || target
                .closest("[data-action], a[href]")
                .ok()
                .flatten()
                .is_some())
            && let Some(menu) = element("vm-more-actions")
        {
            let _ = menu.remove_attribute("open");
        }
        if target.closest("[role=menu]").ok().flatten().is_none() {
            set_html("menu-root", "");
        }
        if target.closest("a[href]").ok().flatten().is_some() {
            set_html("palette-root", "");
        }
        let Some(button) = target.closest("[data-action]").ok().flatten() else {
            if let Some(row) = target.closest("tr[data-id]").ok().flatten() {
                show_detail(
                    clicked.clone(),
                    row.get_attribute("data-id").unwrap_or_default(),
                );
            }
            return;
        };
        event.prevent_default();
        let action = button.get_attribute("data-action").unwrap_or_default();
        let id = button.get_attribute("data-id").unwrap_or_default();
        if clicked.borrow().busy && !matches!(action.as_str(), "palette" | "palette-close" | "menu")
        {
            return;
        }
        dispatch(clicked.clone(), &action, &id, &button);
    });
    target.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
    click.forget();
    let changed = app.clone();
    let input = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        let Some(target) = event
            .target()
            .and_then(|target| target.dyn_into::<Element>().ok())
        else {
            return;
        };
        if !target.is_connected() || changed.borrow().rendering {
            return;
        }
        if databases::input(&changed, &target) {
            return;
        }
        match target.id().as_str() {
            "vm-search" => {
                if let Ok(input) = target.dyn_into::<HtmlInputElement>() {
                    changed.borrow_mut().query = input.value();
                    vm_list::filter(&changed);
                }
            }
            "palette-query" => {
                if let Ok(input) = target.dyn_into::<HtmlInputElement>() {
                    details::palette_input(&changed, input.value());
                }
            }
            _ => {
                if advanced::cluster::input(&changed, &target) {
                    return;
                }
                if target.id() == "field-hostPort" && advanced::services::change(&changed, &target)
                {
                    return;
                }
                if sandbox::input(&changed, &target) {
                    return;
                }
                if advanced::ai::input(&changed, &target) {
                    return;
                }
                if volumes::input(&changed, &target) {
                    return;
                }
                if target.closest("#editor").ok().flatten().is_some() {
                    changed.borrow_mut().dirty = true;
                    workflows::form_input(&changed, &target);
                    advanced::form_input(&changed, &target);
                }
            }
        }
    });
    target.add_event_listener_with_callback("input", input.as_ref().unchecked_ref())?;
    input.forget();
    let language = app.clone();
    let change = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        let Some(target) = event
            .target()
            .and_then(|target| target.dyn_into::<Element>().ok())
        else {
            return;
        };
        if !target.is_connected() || language.borrow().rendering {
            return;
        }
        if databases::change(&language, &target)
            || sandbox::change(&language, &target)
            || volumes::input(&language, &target)
        {
            return;
        }
        if deploy::change(&language, &target) || advanced::change(&language, &target) {
            return;
        }
        if terminal::change(&language, &target) {
            return;
        }
        if workflows::change(&language, &target) {
            return;
        }
        if target.id() == "language" {
            if let Ok(select) = target.dyn_into::<HtmlSelectElement>() {
                if !allow_discard(&language) {
                    select.set_value(&language.borrow().locale);
                    return;
                }
                let mut state = language.borrow_mut();
                state.locale = select.value();
                i18n::set(&state.locale);
                state.dirty = false;
                if let Ok(Some(storage)) = window().local_storage() {
                    let _ = storage.set_item("firecrab.locale", &state.locale);
                }
                if let Some(root) = document().document_element() {
                    let _ = root.set_attribute("lang", &state.locale);
                }
                drop(state);
                let menu_open = document()
                    .query_selector(".app-frame[data-nav-open]")
                    .ok()
                    .flatten()
                    .is_some();
                render_page(&language);
                if menu_open && let Ok(Some(frame)) = document().query_selector(".app-frame") {
                    let _ = frame.set_attribute("data-nav-open", "");
                }
            }
        } else if target.closest("#editor").ok().flatten().is_some() {
            language.borrow_mut().dirty = true;
        }
    });
    target.add_event_listener_with_callback("change", change.as_ref().unchecked_ref())?;
    change.forget();
    let submitted = app.clone();
    let submit = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        let Some(form) = event
            .target()
            .and_then(|target| target.dyn_into::<HtmlFormElement>().ok())
        else {
            return;
        };
        if databases::submit(&submitted, &form)
            || sandbox::submit(&submitted, &form.id())
            || advanced::queues::submit(&submitted, &form)
        {
            event.prevent_default();
            return;
        }
        if form.id() == "ai-editor" {
            event.prevent_default();
            advanced::ai::submit(&submitted);
            return;
        }
        if form.id() != "editor" {
            return;
        }
        event.prevent_default();
        save_form(submitted.clone(), form);
    });
    target.add_event_listener_with_callback("submit", submit.as_ref().unchecked_ref())?;
    submit.forget();
    let routed = app.clone();
    let hash = Closure::<dyn FnMut(web_sys::HashChangeEvent)>::new(
        move |event: web_sys::HashChangeEvent| {
            if window().location().href().ok().as_deref() == Some(event.new_url().as_str()) {
                navigate(&routed);
            }
        },
    );
    window().add_event_listener_with_callback("hashchange", hash.as_ref().unchecked_ref())?;
    hash.forget();
    let keyboard = app.clone();
    let key = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
        if event.is_composing() || event.key() == "Process" || event.key_code() == 229 {
            return;
        }
        if event
            .target()
            .and_then(|target| target.dyn_into::<Element>().ok())
            .is_some_and(|target| target.closest("#terminal").ok().flatten().is_some())
        {
            return;
        }
        if sandbox::key(&keyboard, &event) || details::tab_key(&keyboard, &event) {
            return;
        }
        if details::resize_key(&keyboard, &event) {
            return;
        }
        if details::palette_key(&keyboard, &event) {
            return;
        }
        if (event.meta_key() || event.ctrl_key()) && event.key().eq_ignore_ascii_case("k") {
            event.prevent_default();
            open_palette(&keyboard);
            return;
        }
        if event.key() == "Escape" {
            if let Some(menu) = element("vm-more-actions").filter(|el| el.has_attribute("open")) {
                let _ = menu.remove_attribute("open");
                if let Some(summary) = element("vm-more-summary")
                    .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
                {
                    let _ = summary.focus();
                }
                event.prevent_default();
                return;
            }
            if element("menu-root").is_some_and(|el| !el.inner_html().is_empty()) {
                set_html("menu-root", "");
                event.prevent_default();
                return;
            }
            if element("tooltip-root").is_some_and(|el| !el.inner_html().is_empty()) {
                set_html("tooltip-root", "");
                event.prevent_default();
                return;
            }
            set_html("palette-root", "");
            set_html("tooltip-root", "");
            if updates::close_on_escape(&keyboard)
                || workflows::close_on_escape(&keyboard)
                || terminal::close_on_escape(&keyboard)
            {
                event.prevent_default();
                return;
            }
            if !keyboard.borrow().dirty && keyboard.borrow().panel_open {
                details::dispatch(&keyboard, "panel-toggle", "");
            }
            return;
        }
        let tag = document()
            .active_element()
            .map(|element| element.tag_name())
            .unwrap_or_default();
        if ["INPUT", "TEXTAREA", "SELECT"].contains(&tag.as_str())
            || document()
                .active_element()
                .is_some_and(|el| el.closest("#detail-panel").ok().flatten().is_some())
            || keyboard.borrow().route != "vms"
        {
            return;
        }
        if !["ArrowDown", "ArrowUp"].contains(&event.key().as_str()) {
            return;
        }
        let state = keyboard.borrow();
        let visible: Vec<_> = state
            .vms
            .iter()
            .filter(|vm| vm.matches(&state.filter, &state.query))
            .collect();
        if visible.is_empty() {
            return;
        }
        let current = state
            .detail
            .as_ref()
            .and_then(|detail| visible.iter().position(|vm| vm.id == text(detail, "id")));
        let next = match (current, event.key().as_str()) {
            (Some(i), "ArrowDown") => (i + 1).min(visible.len() - 1),
            (Some(i), _) => i.saturating_sub(1),
            (None, _) => 0,
        };
        let id = visible[next].id.clone();
        drop(state);
        event.prevent_default();
        show_detail(keyboard.clone(), id);
    });
    target.add_event_listener_with_callback("keydown", key.as_ref().unchecked_ref())?;
    key.forget();
    let leaving = app.clone();
    let before = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        if leaving.borrow().dirty {
            event.prevent_default();
        }
    });
    window().add_event_listener_with_callback("beforeunload", before.as_ref().unchecked_ref())?;
    before.forget();
    advanced::install(app)?;
    Ok(())
}
fn open_palette(app: &Shared) {
    if element("palette-query").is_some() {
        set_html("palette-root", "");
        return;
    }
    app.borrow_mut().palette_query.clear();
    app.borrow_mut().palette_index = 0;
    set_html("palette-root", &render::palette(&app.borrow()));
    if let Some(input) =
        element("palette-query").and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    {
        let _ = input.focus();
    }
}
fn dispatch(app: Shared, action: &str, id: &str, button: &Element) {
    if deploy::dispatch(&app, action) {
        return;
    }
    if action != "vm-menu" {
        set_html("menu-root", "");
    }
    if databases::dispatch(&app, action, id, button) || sandbox::action(&app, action, id) {
        return;
    }
    if volumes::dispatch(&app, action, id) || advanced::dispatch(&app, action, id, button) {
        return;
    }
    if updates::dispatch(&app, action) || workflows::ssh_dispatch(&app, action, id) {
        return;
    }
    if terminal::dispatch(&app, action, id) {
        return;
    }
    if details::dispatch(&app, action, id) {
        return;
    }
    if workflows::dispatch(&app, action, id) {
        return;
    }
    match action {
        "refresh" => {
            app.borrow_mut().cache.remove("security-connections");
            refresh(app)
        }
        "palette" => open_palette(&app),
        "palette-close" => set_html("palette-root", ""),
        "menu" => {
            if let Ok(Some(frame)) = document().query_selector(".app-frame") {
                let _ = frame.toggle_attribute("data-nav-open");
            }
        }
        "filter" => {
            app.borrow_mut().filter = id.into();
            update_toolbar(&app);
            vm_list::filter(&app);
        }
        "resource-edit" | "service-run" => {
            if !allow_discard(&app) {
                return;
            }
            app.borrow_mut().dirty = false;
            show_detail(app.clone(), id.into());
            let kind = if action == "service-run" {
                "service-deploy".to_string()
            } else {
                app.borrow().route.clone()
            };
            open_form(app, &kind, id);
        }
        "vm-menu" => {
            let state = app.borrow();
            let Some(vm) = state.vms.iter().find(|vm| vm.id == id) else {
                return;
            };
            let rect = button.get_bounding_client_rect();
            let mut menu = format!(
                "<div role=\"menu\" class=\"wasm-row-menu\" style=\"top:{}px;right:{}px\">",
                rect.bottom(),
                (window()
                    .inner_width()
                    .ok()
                    .and_then(|v| v.as_f64())
                    .unwrap_or(1440.0)
                    - rect.right())
                .max(8.0)
            );
            menu.push_str(&render::button("SSH connect", "ssh", id));
            if !matches!(
                vm.extra.get("purpose").and_then(Value::as_str),
                Some("pool" | "queue")
            ) {
                menu.push_str(&render::button(
                    if vm.state == "running" {
                        "stop"
                    } else {
                        "start"
                    },
                    if vm.state == "running" {
                        "vm-stop"
                    } else {
                        "vm-start"
                    },
                    id,
                ));
            }
            menu.push_str("</div>");
            set_html(
                "menu-root",
                &menu.replace("type=\"button\"", "type=\"button\" role=\"menuitem\""),
            );
        }
        "detail" => show_detail(app, id.into()),
        "close-detail" => {
            if !allow_discard(&app) {
                return;
            }
            let mut state = app.borrow_mut();
            state.form = None;
            state.dirty = false;
            state.detail = None;
            state.detail_generation += 1;
            state.detail_result.clear();
            drop(state);
            render_content(&app);
        }
        "create" => open_form(app, id, ""),
        "edit" => {
            let detail_id = app
                .borrow()
                .detail
                .as_ref()
                .map(|detail| text(detail, "id").to_string())
                .unwrap_or_default();
            open_form(app, id, &detail_id);
        }
        "cancel" => {
            let mut state = app.borrow_mut();
            state.form = None;
            state.dirty = false;
            let new = state.route == "vms/new";
            drop(state);
            if new {
                let _ = window().location().set_hash("/vms");
            } else {
                render_content(&app);
            }
        }
        "vm-start" | "vm-stop" | "vm-pause" | "vm-resume" => {
            if app.borrow().vms.iter().any(|vm| {
                vm.id == id
                    && matches!(
                        vm.extra.get("purpose").and_then(Value::as_str),
                        Some("pool" | "queue")
                    )
            }) {
                return;
            }
            let suffix = action.trim_start_matches("vm-");
            mutate(
                app,
                "POST".into(),
                format!("/api/vms/{}/{suffix}", api::encode(id)),
                None,
                None,
            );
        }
        "request" => {
            let method = button.get_attribute("data-method").unwrap_or_default();
            let path = button.get_attribute("data-path").unwrap_or_default();
            if (method == "DELETE" || path == "/api/update" || path.ends_with("/stop"))
                && !window()
                    .confirm_with_message(&if path == "/api/pipe/auth/firecrab" {
                        i18n::t("Disconnect the Firecrab account from this host? To revoke its account integration token, open console.firecrab.dev.").into()
                    } else {
                        format!("{}?", button.text_content().unwrap_or_default())
                    })
                    .unwrap_or(false)
            {
                return;
            }
            let body = button
                .get_attribute("data-body")
                .and_then(|value| serde_json::from_str::<Value>(&value).ok())
                .filter(|value| !value.is_null());
            mutate(app, method, path, body, None);
        }
        "logs" => fetch_result(
            app,
            format!("/api/vms/{}/log", api::encode(id)),
            ResultKind::Logs,
        ),
        "export-log" => fetch_result(
            app,
            format!("/api/vms/{}/log", api::encode(id)),
            ResultKind::Download,
        ),
        "ssh" => fetch_result(
            app,
            format!("/api/vms/{}/ssh-host-key", api::encode(id)),
            ResultKind::Ssh,
        ),
        "deployment" => fetch_result(
            app,
            format!("/api/micro-services/{}/deployment", api::encode(id)),
            ResultKind::Properties,
        ),
        "shell-revision" => {
            let parts: Vec<_> = id.split('/').collect();
            if parts.len() == 2 {
                fetch_result(
                    app,
                    format!(
                        "/api/shells/{}/revisions/{}",
                        api::encode(parts[0]),
                        api::encode(parts[1])
                    ),
                    ResultKind::Properties,
                );
            }
        }
        "registry-repositories" => fetch_result(
            app,
            format!("/api/pipe/registries/{}/repositories", api::encode(id)),
            ResultKind::Repositories(id.into()),
        ),
        "registry-tags" => fetch_result(
            app,
            format!("/api/pipe/registry-tags?reference={}", api::encode(id)),
            ResultKind::Tags(id.into()),
        ),
        "registry-pull-ref" => mutate(
            app,
            "POST".into(),
            "/api/pipe/registry-transfers".into(),
            Some(json!({"action":"pull","reference":id})),
            None,
        ),
        "registry-images" => fetch_result(
            app,
            "/api/pipe/registry-images".into(),
            ResultKind::Properties,
        ),
        "database-tables" => fetch_result(
            app,
            format!("/api/pipe/databases/{}/tables", api::encode(id)),
            ResultKind::Tables(id.into()),
        ),
        "database-preview" => {
            if let Ok((id, schema, table, offset)) =
                serde_json::from_str::<(String, String, String, u32)>(id)
            {
                preview_database(app, id, schema, table, offset);
            }
        }
        "job-refresh" => poll_job(app),
        "update-progress" => {
            fetch_result(app, "/api/update/progress".into(), ResultKind::Properties)
        }
        "manager-edit" => open_manager_form(app),
        "manager-apply"
            if window()
                .confirm_with_message(i18n::t("Apply resources and restart the management VM?"))
                .unwrap_or(false) =>
        {
            manager::apply(app)
        }
        _ => {}
    }
}
fn open_form(app: Shared, kind: &str, id: &str) {
    if !allow_discard(&app) {
        return;
    }
    let mut state = app.borrow_mut();
    state.workflows.form_result = None;
    let mut data = state.detail.clone().unwrap_or(Value::Null);
    if matches!(kind, "route" | "named-tunnel") {
        data = state.resource["config"].clone();
        if data.is_null() {
            data = json!({"listen":"127.0.0.1:8080", "routes":[], "tunnel":null});
        }
    }
    if kind == "shells" {
        data["content"] = data["latestContent"].clone();
    }
    let edit_id = if kind == "image-kernel" {
        text(&data, "alias").to_string()
    } else {
        id.into()
    };
    if kind.starts_with("vm-") && ["pool", "queue"].contains(&text(&data, "purpose")) {
        return;
    }
    if kind == "secret-connection" {
        data = list(
            state
                .cache
                .get("/api/secrets/connections")
                .unwrap_or(&Value::Null),
        )
        .iter()
        .find(|row| row["id"] == id)
        .cloned()
        .unwrap_or(Value::Null);
    }
    state.form = forms::resource_form(kind, &data, &edit_id);
    state.panel_open = true;
    state.dirty = false;
    state.error.clear();
    state.field_errors.clear();
    drop(state);
    workflows::prepare_form(&app);
    advanced::prepare(&app);
    advanced::services::open(&app);
    render_editor(&app);
}
fn save_form(app: Shared, form_element: HtmlFormElement) {
    if advanced::services::before_save(&app) {
        return;
    }
    let state = app.borrow();
    if state.busy {
        return;
    }
    let Some(mut form) = state.form.clone() else {
        return;
    };
    drop(state);
    advanced::security::sync_rules();
    let values = match FormData::new_with_form(&form_element) {
        Ok(values) => values,
        Err(error) => {
            app.borrow_mut().error = api::error(error);
            render_messages(&app);
            return;
        }
    };
    let values: BTreeMap<_, _> = form
        .fields
        .iter()
        .map(|field| {
            (
                field.key.into(),
                if field.kind == "multi" {
                    values
                        .get_all(field.key)
                        .iter()
                        .filter_map(|value| value.as_string())
                        .collect::<Vec<_>>()
                        .join("\n")
                } else {
                    values.get(field.key).as_string().unwrap_or_default()
                },
            )
        })
        .collect();
    for field in &mut form.fields {
        field.value = values.get(field.key).cloned().unwrap_or_default();
    }
    app.borrow_mut().form = Some(form.clone());
    let errors = form.validate(&values);
    if !errors.is_empty() {
        app.borrow_mut().field_errors = errors;
        render_editor(&app);
        return;
    }
    let mut payload = match form.payload(&values) {
        Ok(payload) => payload,
        Err(error) => {
            app.borrow_mut().field_errors.clear();
            app.borrow_mut().error = error;
            render_messages(&app);
            return;
        }
    };
    if form.title == "Deploy update"
        && let Err(error) = deploy::prepare_update(&app, &mut payload)
    {
        app.borrow_mut().error = error;
        render_messages(&app);
        return;
    }
    let path = if form.title == "Connect registry" {
        if let Some(object) = payload.as_object_mut() {
            object.remove("registry");
            if object
                .get("provider")
                .is_some_and(|v| v.is_null() || v == "")
            {
                object.remove("provider");
            }
            if object.get("accountName").is_some_and(|v| v == "") {
                object.remove("accountName");
            }
        }
        format!(
            "/api/pipe/registries/{}",
            api::encode(
                values
                    .get("registry")
                    .map(String::as_str)
                    .unwrap_or_default()
            )
        )
    } else {
        form.path.clone()
    };
    if !databases::edit::before_submit(&form) {
        return;
    }
    // Preserve entered values if the API rejects a mutation.
    for field in &mut form.fields {
        field.value = values.get(field.key).cloned().unwrap_or_default();
    }
    app.borrow_mut().form = Some(form.clone());
    mutate(
        app,
        form.method.into(),
        path,
        payload.as_object().map(|_| payload.clone()),
        form.controller,
    );
}
fn mutate(
    app: Shared,
    method: String,
    path: String,
    body: Option<Value>,
    controller: Option<String>,
) {
    advanced::capture_form(&app);
    let submits_form = app.borrow().form.as_ref().is_some_and(|form| {
        form.path == path
            || (form.title == "Connect registry" && path.starts_with("/api/pipe/registries/"))
    });
    {
        let mut state = app.borrow_mut();
        if state.busy {
            return;
        }
        state.busy = true;
        state.generation += 1;
        state.detail_generation += 1;
        state.refreshing = false;
        state.error.clear();
        state.field_errors.clear();
        state.notice.clear();
    }
    render_messages(&app);
    if app.borrow().form.is_some() {
        render_editor(&app);
    }
    let generation = app.borrow().generation;
    spawn_local(async move {
        let result =
            api::request_detailed(&method, &path, body.as_ref(), controller.as_deref()).await;
        let succeeded = result.is_ok();
        let mut state = app.borrow_mut();
        if state.generation != generation {
            drop(state);
            render_messages(&app);
            return;
        }
        state.busy = false;
        match result {
            Ok(value) => {
                let return_route = state
                    .form
                    .as_ref()
                    .filter(|_| submits_form)
                    .map(|form| form.return_route.clone());
                if submits_form {
                    state.form = None;
                    state.dirty = false;
                    if path == volumes::ENDPOINT {
                        state.panel_open = false;
                    }
                }
                if path == "/api/micro-services"
                    || (submits_form
                        && path.starts_with("/api/micro-services/")
                        && !path.ends_with("/deployment")
                        && !path.ends_with("/updates"))
                {
                    state.detail = Some(value.clone());
                    let mut rows = model::list(&state.resource).to_vec();
                    rows.retain(|row| row["id"] != value["id"]);
                    rows.push(value.clone());
                    state.resource = json!(rows);
                }
                if path == "/api/schedules"
                    || path == "/api/security-groups"
                    || (path.starts_with("/api/pipe/auth/") && method == "PUT")
                {
                    state.detail = Some(value.clone());
                }
                state.notice = if path == "/api/pipe/auth/firecrab" && method == "PUT" {
                    "Connected to the Firecrab account."
                } else if path.ends_with("/rules") {
                    "Rules saved."
                } else {
                    "Saved."
                }
                .into();
                if method == "DELETE"
                    && path.starts_with("/api/pipe/databases/")
                    && !path
                        .trim_start_matches("/api/pipe/databases/")
                        .contains('/')
                {
                    state.notice = "Connection removed. The source database is unchanged.".into();
                }
                let selected = state
                    .detail
                    .as_ref()
                    .map(|detail| text(detail, "id").to_string());
                if method == "DELETE" {
                    if state.route == "services" && path.starts_with("/api/micro-services/") {
                        if let Some(id) = &selected {
                            state.resource = json!(
                                model::list(&state.resource)
                                    .iter()
                                    .filter(|row| row["id"] != *id)
                                    .collect::<Vec<_>>()
                            );
                        }
                        state.form = None;
                    }
                    state.detail = None;
                    state.detail_generation += 1;
                } else if path == "/api/vms" {
                    state.pending_vm = Some(text(&value, "id").into());
                } else if let Some(id) = &selected {
                    if path == format!("/api/vms/{}", api::encode(id))
                        || path.starts_with(&format!("/api/vms/{}/", api::encode(id)))
                    {
                        if let Some(base) = state.detail.as_mut().and_then(Value::as_object_mut)
                            && let Some(updated) = value.as_object()
                        {
                            base.extend(updated.clone());
                        }
                    } else if path.ends_with("/deployment")
                        || path.ends_with("/stop")
                        || path.ends_with("/updates")
                        || path.ends_with("/rollback")
                        || path.ends_with("/deployment/retry")
                    {
                        state.service_deployment = Some(value.clone());
                        state.detail_result.clear();
                    } else if text(&value, "id") == id
                        && let Some(base) = state.detail.as_mut().and_then(Value::as_object_mut)
                        && let Some(updated) = value.as_object()
                    {
                        base.extend(updated.clone());
                    }
                }
                if path.ends_with("/api/service/resources") {
                    state.cache.insert("manager".into(), value.clone());
                    state.workflows.manager_operation = None;
                    state.notice = "Saved. Apply to restart with these resources.".into();
                }
                if path.ends_with("/api/service/resources/apply") {
                    state.workflows.manager_pending = true;
                }
                if path == "/api/pipe/registry-transfers" {
                    workflows::remember_job(
                        text(&value, "alias"),
                        body.as_ref()
                            .map(|body| text(body, "reference"))
                            .unwrap_or(""),
                    );
                }
                if path == "/api/oci/import" && value["status"] == "running" {
                    workflows::remember_import(text(&value, "alias"));
                }
                let status = text(&value, "status");
                let job_path = if path == "/api/oci/import" {
                    Some(format!(
                        "/api/oci/import/{}",
                        api::encode(text(&value, "alias"))
                    ))
                } else if path == "/api/pipe/registry-transfers" {
                    Some(format!(
                        "/api/pipe/registry-transfers/{}",
                        api::encode(text(&value, "alias"))
                    ))
                } else if !status.is_empty()
                    && (path.ends_with("/install") || path.ends_with("/package"))
                {
                    Some(path.clone())
                } else if path == "/api/microregistry/register" {
                    Some(format!(
                        "/api/microregistry/jobs/{}",
                        api::encode(text(&value, "jobId"))
                    ))
                } else {
                    None
                };
                if let Some(path) = job_path {
                    state.job = Some((path, value));
                }
                if let Some(route) = return_route.filter(|route| *route != state.route) {
                    drop(state);
                    let _ = window().location().set_hash(&format!("/{route}"));
                    return;
                }
            }
            Err(error) => {
                state.field_errors = error.fields;
                state.error = if path.ends_with("/rules") && error.message.starts_with("HTTP 409:")
                {
                    "Stop affected VMs and service deployments before changing rules.".into()
                } else if path.starts_with("/api/micro-services/")
                    && (path.ends_with("/updates")
                        || path.ends_with("/rollback")
                        || path.ends_with("/deployment/retry"))
                {
                    deploy::failure(error.message)
                } else {
                    error.message
                };
            }
        }
        volumes::mutation_finished(&mut state, &path, succeeded);
        let registry_done = path == "/api/pipe/registry-transfers"
            && state
                .job
                .as_ref()
                .is_some_and(|(_, v)| matches!(text(v, "status"), "succeeded" | "failed"));
        drop(state);
        if registry_done {
            workflows::job_finished(&app);
        }
        render_content(&app);
        if succeeded {
            advanced::after_mutation(&app, &path);
            if path.starts_with("/api/pipe/database") {
                databases::after_mutation(&app);
            }
            volumes::after_mutation(&app, &path);
        }
        refresh(app);
    });
}
fn show_detail(app: Shared, id: String) {
    let state = app.borrow();
    if state.busy {
        return;
    }
    drop(state);
    if !allow_discard(&app) {
        return;
    }
    vm_list::reveal(&app, &id);
    let mut state = app.borrow_mut();
    state.form = None;
    state.dirty = false;
    state.panel_open = true;
    state.detail_tab = "overview".into();
    state.service_deployment = None;
    state.detail_log_loaded = false;
    state.show_env = false;
    state.volumes.reset_editor();
    state.detail_generation += 1;
    let detail_generation = state.detail_generation;
    let generation = state.generation;
    let route = state.route.clone();
    let snapshot = if route == "vms" {
        state
            .vms
            .iter()
            .find(|vm| vm.id == id)
            .and_then(|vm| serde_json::to_value(vm).ok())
    } else {
        list(&state.resource)
            .iter()
            .find(|row| {
                ["id", "alias", "version", "provider", "registry"]
                    .iter()
                    .any(|key| row[key] == id)
            })
            .cloned()
    };
    state.detail_loading = matches!(
        route.as_str(),
        "vms" | "services" | "networks" | "storages" | "shells" | "images"
    );
    state.detail = snapshot.clone();
    state.detail_result.clear();
    drop(state);
    render_detail(&app);
    let fetch = matches!(
        route.as_str(),
        "vms" | "services" | "networks" | "storages" | "shells" | "images"
    );
    advanced::detail_selected(&app);
    if !fetch {
        return;
    }
    spawn_local(async move {
        let result = api::request(
            "GET",
            &format!("{}/{}", endpoint(&route), api::encode(&id)),
            None,
        )
        .await;
        let mut state = app.borrow_mut();
        if state.generation != generation || state.detail_generation != detail_generation {
            return;
        }
        state.detail_loading = false;
        match result {
            Ok(value) => {
                let mut merged = snapshot.unwrap_or_else(|| json!({}));
                if let (Some(base), Some(detail)) = (merged.as_object_mut(), value.as_object()) {
                    base.extend(detail.clone());
                    state.detail = Some(merged);
                } else {
                    state.detail = Some(value);
                }
            }
            Err(error) => state.error = error,
        }
        let editing = state.form.is_some();
        drop(state);
        if !editing {
            render_detail(&app);
            render_messages(&app);
        }
        if app.borrow().route == "services" {
            details::tick(&app);
        }
    });
}

enum ResultKind {
    Properties,
    Logs,
    Download,
    Ssh,
    Tables(String),
    Repositories(String),
    Tags(String),
}
fn fetch_result(app: Shared, path: String, kind: ResultKind) {
    app.borrow_mut().detail_result.clear();
    set_html("detail-result", "<p role=\"status\">Loading…</p>");
    let generation = app.borrow().generation;
    app.borrow_mut().detail_generation += 1;
    let detail_generation = app.borrow().detail_generation;
    spawn_local(async move {
        let result = api::request("GET", &path, None).await;
        let mut state = app.borrow_mut();
        if state.generation != generation || state.detail_generation != detail_generation {
            return;
        }
        match result {
            Err(error) => state.error = error,
            Ok(value) => {
                let html = match kind {
                    ResultKind::Properties => render::properties(&value),
                    ResultKind::Logs | ResultKind::Download => {
                        let logs = log_text(&value);
                        if matches!(kind, ResultKind::Download) {
                            download("firecrab-console.log", &logs);
                            String::new()
                        } else {
                            format!("<pre class=\"wasm-log\">{}</pre>", escape(&logs))
                        }
                    }
                    ResultKind::Ssh => format!(
                        "{}<p>Download the SSH key, then run:</p><pre>chmod 600 firecrab.pem\nssh -i firecrab.pem root@{}</pre>",
                        render::properties(&value),
                        escape(
                            state
                                .detail
                                .as_ref()
                                .map(|detail| text(detail, "ipv4"))
                                .unwrap_or("VM_IP")
                        )
                    ),
                    ResultKind::Tables(id) => {
                        let mut html = if list(&value["tables"]).is_empty() {
                            "<p>No tables.</p>".into()
                        } else {
                            String::new()
                        };
                        for table in list(&value["tables"]) {
                            let schema = text(table, "schema");
                            let name = text(table, "name");
                            html.push_str(&render::button(
                                &format!("{schema}.{name}"),
                                "database-preview",
                                &json!([id, schema, name, 0]).to_string(),
                            ));
                        }
                        html
                    }
                    ResultKind::Repositories(registry) => list(&value["repositories"])
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|name| {
                            render::button(name, "registry-tags", &format!("{registry}/{name}"))
                        })
                        .collect::<Vec<_>>()
                        .join(""),
                    ResultKind::Tags(reference) => list(&value["tags"])
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|tag| {
                            render::button(
                                &format!("Pull {tag}"),
                                "registry-pull-ref",
                                &format!("{reference}:{tag}"),
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(""),
                };
                state.detail_result = html;
            }
        }
        drop(state);
        render_detail(&app);
        render_messages(&app);
    });
}
fn log_text(value: &Value) -> String {
    ["pipelineLines", "pipelineLog", "consoleLog", "log"]
        .iter()
        .filter_map(|key| {
            let v = &value[key];
            if let Some(s) = v.as_str() {
                Some(s.into())
            } else if v.is_array() {
                Some(
                    list(v)
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn preview_database(app: Shared, id: String, schema: String, table: String, offset: u32) {
    app.borrow_mut().detail_result.clear();
    set_html("detail-result", "<p role=\"status\">Loading…</p>");
    let generation = app.borrow().generation;
    app.borrow_mut().detail_generation += 1;
    let detail_generation = app.borrow().detail_generation;
    spawn_local(async move {
        let result = api::request(
            "POST",
            &format!("/api/pipe/databases/{}/preview", api::encode(&id)),
            Some(&json!({"schema":schema,"table":table,"offset":offset,"limit":50})),
        )
        .await;
        let mut state = app.borrow_mut();
        if state.generation != generation || state.detail_generation != detail_generation {
            return;
        }
        match result {
            Err(error) => state.error = error,
            Ok(value) => {
                let mut html = "<div class=\"table-scroll\"><table class=\"wasm-table pipe-database-preview\"><thead><tr>".to_string();
                for column in list(&value["columns"]) {
                    html.push_str(&format!("<th>{}</th>", escape(text(column, "name"))));
                }
                html.push_str("</tr></thead><tbody>");
                for row in list(&value["rows"]) {
                    html.push_str("<tr>");
                    for cell in list(row) {
                        html.push_str(&format!(
                            "<td><div class=\"pipe-database-cell\" tabindex=\"0\">{}</div></td>",
                            escape(cell.as_str().unwrap_or("NULL"))
                        ));
                    }
                    html.push_str("</tr>");
                }
                html.push_str("</tbody></table></div>");
                if offset > 0 {
                    html.push_str(&render::button(
                        "Previous 50 rows",
                        "database-preview",
                        &json!([id, schema, table, offset.saturating_sub(50)]).to_string(),
                    ));
                }
                if value["hasMore"] == true {
                    html.push_str(&render::button(
                        "Next 50 rows",
                        "database-preview",
                        &json!([id, schema, table, offset + 50]).to_string(),
                    ));
                }
                state.detail_result = html;
            }
        }
        drop(state);
        render_detail(&app);
        render_messages(&app);
    });
}
fn poll_job(app: Shared) {
    let Some((path, value)) = app.borrow().job.clone() else {
        return;
    };
    if matches!(
        text(&value, "status"),
        "complete" | "completed" | "succeeded" | "failed" | "error"
    ) {
        return;
    }
    let generation = app.borrow().generation;
    spawn_local(async move {
        let result = api::request("GET", &path, None).await;
        let mut state = app.borrow_mut();
        if state.generation != generation {
            return;
        }
        match result {
            Ok(value) => {
                let done = matches!(
                    text(&value, "status"),
                    "complete" | "completed" | "succeeded" | "failed" | "error"
                );
                state.job = Some((path, value));
                if done {
                    state.notice =
                        "Background operation finished. See progress for the result.".into();
                }
            }
            Err(error) => state.error = error,
        }
        let registry_done = state.job.as_ref().is_some_and(|(path, value)| {
            path.starts_with("/api/pipe/registry-transfers/")
                && matches!(text(value, "status"), "succeeded" | "failed")
        });
        let redraw = state.form.is_none();
        drop(state);
        if registry_done {
            workflows::job_finished(&app);
        }
        if redraw {
            render_content(&app);
        }
    });
}
fn poll_host(app: &Shared) {
    let app = app.clone();
    spawn_local(async move {
        if let Ok(value) = api::request("GET", "/api/host", None).await {
            app.borrow_mut().cache.insert("/api/host".into(), value);
            update_host(&app);
        }
        if app.borrow().route != "host" {
            return;
        }
        let result = api::request("GET", "/api/info", None).await;
        let mut state = app.borrow_mut();
        match result {
            Ok(info) => {
                state.cache.insert("/api/info".into(), info);
            }
            Err(_) => {
                state
                    .cache
                    .insert("/api/info".into(), json!({"status":"unavailable"}));
            }
        }
        let redraw = state.route == "host" && state.form.is_none();
        drop(state);
        if redraw {
            render_content(&app);
        }
    });
}
fn manager_endpoint() -> String {
    if let Ok(Some(meta)) = document().query_selector("meta[name='firecrab-resource-endpoint']")
        && let Some(endpoint) = meta.get_attribute("content")
        && endpoint.starts_with('/')
        && !endpoint.starts_with("//")
    {
        return format!(
            "{}{}",
            window().location().origin().unwrap_or_default(),
            endpoint
        );
    }
    format!(
        "http://{}:5524/api/service/resources",
        window().location().hostname().unwrap_or("localhost".into())
    )
}
fn load_manager(app: Shared) {
    if window().location().protocol().unwrap_or_default() == "http:"
        && ["localhost", "127.0.0.1"]
            .contains(&window().location().hostname().unwrap_or_default().as_str())
    {
        manager::load(app);
    }
}
fn open_manager_form(app: Shared) {
    let manager = app.borrow().cache.get("manager").cloned();
    if let Some(manager) = manager {
        let mut form = forms::manager_form(&manager, manager_endpoint());
        form.controller = Some(text(&manager, "controllerId").into());
        app.borrow_mut().form = Some(form);
        render_content(&app);
    }
}
fn download(name: &str, text: &str) {
    terminal::download_text(name, text);
}
