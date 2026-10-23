mod api;
mod details;
mod forms;
mod i18n;
mod model;
mod render;
mod terminal;
mod updates;
mod vm_list;
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
    field_errors: BTreeMap<String, String>,
    notice: String,
    generation: u64,
    detail_generation: u64,
    job: Option<(String, Value)>,
    workflows: workflows::State,
    console: Option<terminal::State>,
    console_retry: Option<Closure<dyn FnMut()>>,
    panel_open: bool,
    panel_width: u32,
    panel_dragging: bool,
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
}
fn update_host(app: &Shared) {
    let state = app.borrow();
    let Some(host) = state.cache.get("/api/host") else {
        return;
    };
    if let Some(load) = element("host-load") {
        load.set_text_content(Some(
            &host["loadAverage1m"]
                .as_f64()
                .map(|value| format!("{value:.2}"))
                .unwrap_or("—".into()),
        ));
    }
    if let Some(memory) = element("host-memory") {
        memory.set_text_content(Some(
            &host["memoryTotalMib"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or("—".into()),
        ));
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
    let (html, messages, loaded) = {
        let state = app.borrow();
        (
            render::content(&state),
            render::messages(&state),
            state.loaded,
        )
    };
    app.borrow_mut().rendering = true;
    set_html("content", &html);
    app.borrow_mut().rendering = false;
    if let Some(content) = element("content") {
        let _ = content.set_attribute("data-loaded", if loaded { "true" } else { "false" });
    }
    set_html("messages", &messages);
    vm_list::cache(app);
    render_detail(app);
    workflows::sync_form();
}
fn render_editor(app: &Shared) {
    if app.borrow().route == "vms/new" {
        render_content(app);
    } else {
        render_detail(app);
        workflows::sync_form();
    }
}
fn render_detail(app: &Shared) {
    let (open, width, html, selection) = {
        let mut state = app.borrow_mut();
        let narrow = window()
            .inner_width()
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(1440.0)
            < 768.0;
        let open = state.panel_open
            && state.route != "vms/new"
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
        let _ = panel.set_attribute("style", &format!("width:{width}px"));
        if open {
            panel.set_inner_html(&html);
        }
    }
    app.borrow_mut().rendering = false;
    for (row, selected) in selection {
        let _ = row.set_attribute("aria-selected", if selected { "true" } else { "false" });
        row.set_class_name(if selected { "selected" } else { "" });
    }
    if let Some(toggle) = element("panel-toggle") {
        toggle.set_text_content(Some(i18n::t(if open {
            "Hide details"
        } else {
            "Show details"
        })));
    }
}

fn render_messages(app: &Shared) {
    set_html("messages", &render::messages(&app.borrow()));
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
    let poll = app.clone();
    let callback = Closure::<dyn FnMut()>::new(move || {
        terminal::tick(&poll);
        updates::tick(&poll);
        poll_host(&poll);
        let state = poll.borrow();
        if state.busy
            || state.form.is_some()
            || state.refreshing
            || state.route.starts_with("console/")
        {
            return;
        }
        drop(state);
        details::tick(&poll);
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
    let route = hash.trim_start_matches('#').trim_start_matches('/');
    if route == "vms/new"
        || route.starts_with("console/")
        || [
            "pipe/auth",
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
        "services" => "/api/micro-services",
        "networks" => "/api/micro-networks",
        "storages" => "/api/micro-storages",
        "images" => "/api/images",
        "kernels" => "/api/kernels",
        "shells" => "/api/shells",
        "host" => "/api/host",
        "pipe/auth" => "/api/pipe/auth",
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
    if route == "vms/new" {
        load_create_vm(app);
        return;
    }
    refresh(app.clone());
    let mut aux = vec!["/api/host"];
    if route == "host" {
        aux.extend(["/api/info", "/api/update"]);
    }
    if route == "images" {
        aux.extend(["/api/microregistry", "/api/kernels"]);
    }
    if route == "networks" {
        aux.push("/api/network");
    }
    if route == "services" || route == "vms" {
        aux.extend(["/api/micro-networks", "/api/shells", "/api/storage"]);
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
                let visible =
                    matches!(state.route.as_str(), "host" | "images") && state.form.is_none();
                drop(state);
                if visible {
                    render_content(&app);
                }
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
    let mut state = app.borrow_mut();
    if state.refreshing || state.form.is_some() {
        return;
    }
    state.refreshing = true;
    let route = state.route.clone();
    let generation = state.generation;
    drop(state);
    spawn_local(async move {
        let result = api::request("GET", endpoint(&route), None).await;
        let mut state = app.borrow_mut();
        if state.generation != generation {
            return;
        }
        state.refreshing = false;
        let mut changed = !state.loaded || !state.error.is_empty();
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
                                state.vms = vms;
                            }
                        }
                        Err(error) => {
                            state.loaded = true;
                            state.error = format!("Invalid VM response: {error}");
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
                state.error.clear();
                state.loaded = true;
            }
            Err(error) => {
                state.error = error;
                state.loaded = true;
            }
        }
        let has_form = state.form.is_some();
        drop(state);
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
    if state.route == next {
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
    state.route = next;
    state.generation += 1;
    state.detail_generation += 1;
    state.form = None;
    state.detail = None;
    state.detail_loading = false;
    state.detail_result.clear();
    state.dirty = false;
    state.loaded = false;
    state.refreshing = false;
    state.error.clear();
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
                if target.closest("#editor").ok().flatten().is_some() {
                    changed.borrow_mut().dirty = true;
                    workflows::form_input(&changed, &target);
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
                render_page(&language);
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
            set_html("palette-root", "");
            set_html("tooltip-root", "");
            if updates::close_on_escape(&keyboard)
                || workflows::close_on_escape(&keyboard)
                || terminal::close_on_escape(&keyboard)
            {
                event.prevent_default();
            }
            return;
        }
        let tag = document()
            .active_element()
            .map(|element| element.tag_name())
            .unwrap_or_default();
        if ["INPUT", "TEXTAREA", "SELECT"].contains(&tag.as_str())
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
        "refresh" => refresh(app),
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
            if !allow_discard(&app) {
                return;
            }
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
        "vm-start" | "vm-stop" => {
            let suffix = if action == "vm-start" {
                "start"
            } else {
                "stop"
            };
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
                    .confirm_with_message(&format!(
                        "{}?",
                        button.text_content().unwrap_or_default()
                    ))
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
        "manager-apply" => {
            let state = app.borrow();
            let controller = state
                .cache
                .get("manager")
                .map(|manager| text(manager, "controllerId").to_string());
            drop(state);
            if window()
                .confirm_with_message(i18n::t("Apply resources and restart the management VM?"))
                .unwrap_or(false)
            {
                mutate(
                    app,
                    "POST".into(),
                    format!("{}/apply", manager_endpoint()),
                    Some(json!({})),
                    controller,
                );
            }
        }
        _ => {}
    }
}
fn open_form(app: Shared, kind: &str, id: &str) {
    if !allow_discard(&app) {
        return;
    }
    let mut state = app.borrow_mut();
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
    state.form = forms::resource_form(kind, &data, &edit_id);
    state.panel_open = true;
    state.dirty = false;
    state.error.clear();
    state.field_errors.clear();
    drop(state);
    workflows::prepare_form(&app);
    render_editor(&app);
}
fn save_form(app: Shared, form_element: HtmlFormElement) {
    let state = app.borrow();
    if state.busy {
        return;
    }
    let Some(mut form) = state.form.clone() else {
        return;
    };
    drop(state);
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
            app.borrow_mut().error = error;
            render_messages(&app);
            return;
        }
    };
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
        let mut state = app.borrow_mut();
        state.busy = false;
        if state.generation != generation {
            drop(state);
            render_messages(&app);
            return;
        }
        match result {
            Ok(value) => {
                let return_route = state.form.as_ref().map(|form| form.return_route.clone());
                state.form = None;
                state.dirty = false;
                state.notice = "Saved.".into();
                let selected = state
                    .detail
                    .as_ref()
                    .map(|detail| text(detail, "id").to_string());
                if method == "DELETE" {
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
                    } else if path.ends_with("/deployment") || path.ends_with("/stop") {
                        state.detail_result = render::properties(&value);
                    } else if let Some(base) = state.detail.as_mut().and_then(Value::as_object_mut)
                        && let Some(updated) = value.as_object()
                    {
                        base.extend(updated.clone());
                    }
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
                state.error = error.message;
            }
        }
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
    state.detail_log_loaded = false;
    state.show_env = false;
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
    let generation = app.borrow().generation;
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
                        let mut html = String::new();
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
    let generation = app.borrow().generation;
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
                let mut html = "<table class=\"wasm-table\"><thead><tr>".to_string();
                for column in list(&value["columns"]) {
                    html.push_str(&format!("<th>{}</th>", escape(text(column, "name"))));
                }
                html.push_str("</tr></thead><tbody>");
                for row in list(&value["rows"]) {
                    html.push_str("<tr>");
                    for cell in list(row) {
                        html.push_str(&format!(
                            "<td>{}</td>",
                            escape(cell.as_str().unwrap_or("NULL"))
                        ));
                    }
                    html.push_str("</tr>");
                }
                html.push_str("</tbody></table>");
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
    format!(
        "http://{}:5524/api/service/resources",
        window().location().hostname().unwrap_or("localhost".into())
    )
}
fn load_manager(app: Shared) {
    if window().location().protocol().unwrap_or_default() != "http:"
        || !["localhost", "127.0.0.1"]
            .contains(&window().location().hostname().unwrap_or_default().as_str())
    {
        return;
    }
    spawn_local(async move {
        if let Ok(value) = api::request("GET", &manager_endpoint(), None).await {
            let mut state = app.borrow_mut();
            state.workflows.manager_pending = value["operation"]["state"] == "applying";
            state.cache.insert("manager".into(), value);
            let visible = matches!(state.route.as_str(), "host" | "images") && state.form.is_none();
            drop(state);
            if visible {
                render_content(&app);
            }
        }
    });
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
