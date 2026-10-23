use crate::{
    Shared, api, document, element,
    forms::Form,
    i18n::t,
    model::{escape, list, text},
    render, render_content, render_messages, set_html, window,
};
use serde_json::{Value, json};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{Element, FormData, HtmlFormElement};

#[derive(Default)]
pub struct State {
    pub registry: Registry,
    pub zones: Value,
    pub update_open: bool,
    pub update_running: bool,
    pub update_pid: Option<u64>,
    pub update_after: f64,
    pub update_progress: Value,
    pub update_polling: bool,
    pub ssh_open: bool,
    pub ssh_vm: Value,
    pub ssh_key: Option<String>,
    pub ssh_shown: bool,
    pub ssh_busy: bool,
    pub manager_pending: bool,
    pub manager_polling: bool,
    pub manager_operation: Option<String>,
    pub manager_previous: String,
}
#[derive(Default)]
pub struct Registry {
    pub host: String,
    pub repository: String,
    pub repositories: Vec<String>,
    pub tags: Vec<String>,
    pub tag: String,
    pub next_page: Option<u64>,
    pub cursor: Option<String>,
    pub next_last: Option<String>,
    pub loading: bool,
    pub tag_loading: bool,
    pub error: String,
    pub tag_error: String,
    pub inventory: Vec<Value>,
    pub inventory_error: bool,
    pub epoch: u64,
    pub tag_epoch: u64,
}
fn button(label: &str, action: &str, id: &str) -> String {
    render::button(label, action, id)
}
fn input(key: &str) -> String {
    element(key)
        .and_then(|e| e.dyn_into::<web_sys::HtmlInputElement>().ok())
        .map(|e| e.value())
        .unwrap_or_default()
}
fn read_form() -> Option<FormData> {
    element("editor")
        .and_then(|e| e.dyn_into::<HtmlFormElement>().ok())
        .and_then(|form| FormData::new_with_form(&form).ok())
}
fn values(form: &Form, data: &FormData) -> std::collections::BTreeMap<String, String> {
    form.fields
        .iter()
        .map(|field| {
            let value = if field.kind == "multi" {
                data.get_all(field.key)
                    .iter()
                    .filter_map(|v| v.as_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                data.get(field.key).as_string().unwrap_or_default()
            };
            (field.key.into(), value)
        })
        .collect()
}
pub fn prepare_form(app: &Shared) {
    let mut state = app.borrow_mut();
    let Some(mut form) = state.form.take() else {
        return;
    };
    let empty = Value::Null;
    let shells = state.cache.get("/api/shells").unwrap_or(&empty);
    let networks = state.cache.get("/api/micro-networks").unwrap_or(&empty);
    let storage = state.cache.get("/api/storage").unwrap_or(&empty);
    form.choose("shellIds", shells, "id", "name");
    form.choose("microNetworkId", networks, "id", "name");
    form.choose("storageRoot", storage, "path", "path");
    if let Some(kernels) = state.cache.get("/api/kernels") {
        let kernels = if kernels.is_array() {
            kernels
        } else {
            &kernels["kernels"]
        };
        form.choose("kernelVersion", kernels, "version", "version");
    }
    if let Some(network) = state.cache.get("/api/network") {
        let interfaces = json!(
            list(&network["interfaces"])
                .iter()
                .filter_map(Value::as_str)
                .map(|name| json!({"name":name}))
                .collect::<Vec<_>>()
        );
        form.choose("uplink", &interfaces, "name", "name");
    }
    if let Some(field) = form.fields.iter_mut().find(|field| field.key == "shellIds") {
        field.kind = "multi";
        field.label = "Shells";
    }
    if let Some(field) = form
        .fields
        .iter_mut()
        .find(|field| field.key == "microNetworkId")
        && !field.required
    {
        field.kind = "nullable-select";
        field.label = "Network ID (optional)";
    }
    if ["Quick tunnel", "Firecrab address"].contains(&form.title.as_str()) {
        let apps = app_addresses(&state.vms);
        form.choose("upstreams", &apps, "id", "name");
        if let Some(field) = form
            .fields
            .iter_mut()
            .find(|field| field.key == "upstreams")
        {
            field.kind = "multi";
            let retained: Vec<_> = field.value.lines().map(str::to_owned).collect();
            for retained in retained {
                if !field.options.iter().any(|(key, _)| *key == retained) {
                    field.options.push((retained.clone(), retained));
                }
            }
        }
        let routes = json!(
            list(&state.resource["config"]["routes"])
                .iter()
                .map(|route| json!({"id":route["host"],"name":route["host"]}))
                .collect::<Vec<_>>()
        );
        form.choose("host", &routes, "id", "name");
        if let Some(field) = form.fields.iter_mut().find(|field| field.key == "host") {
            field.kind = "nullable-select";
        }
    }
    state.form = Some(form);
}
fn app_addresses(vms: &[crate::model::Vm]) -> Value {
    let mut values = Vec::new();
    for vm in vms.iter().filter(|vm| vm.state == "running") {
        let Some(ip) = &vm.ipv4 else {
            continue;
        };
        for port in list(vm.extra.get("portForwards").unwrap_or(&Value::Null))
            .iter()
            .filter(|port| port["protocol"] == "tcp")
        {
            if let Some(port) = port["guestPort"].as_u64() {
                values.push(
                    json!({"id":format!("{ip}:{port}"),"name":format!("{} · TCP {port}",vm.name)}),
                );
            }
        }
    }
    json!(values)
}
pub fn sync_form() {
    if let (Some(mode), Some(interface)) = (
        element("field-networkMode")
            .and_then(|el| el.dyn_into::<web_sys::HtmlSelectElement>().ok()),
        element("field-networkInterface")
            .and_then(|el| el.dyn_into::<web_sys::HtmlSelectElement>().ok()),
    ) {
        let bridged = mode.value() == "bridged";
        if let Some(field) = interface
            .closest(".field")
            .ok()
            .flatten()
            .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
        {
            field.set_hidden(!bridged);
        }
        if !bridged {
            interface.set_value("");
        } else if interface.value().is_empty()
            && let Ok(Some(option)) = interface.query_selector("option[value]:not([value=\"\"])")
        {
            interface.set_value(&option.get_attribute("value").unwrap_or_default());
        }
    }

    let kind = element("field-targetType")
        .and_then(|e| e.dyn_into::<web_sys::HtmlSelectElement>().ok())
        .map(|e| e.value());
    if let Some(kind) = kind {
        for (key, show) in [("upstreams", kind == "apps"), ("host", kind != "apps")] {
            if let Some(field) = element(&format!("field-{key}"))
                .and_then(|e| e.closest(".field").ok().flatten())
                .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
            {
                field.set_hidden(!show);
            }
        }
    }
}
pub fn after_refresh(app: &Shared) {
    let state = app.borrow();
    if state.route != "pipe/registry" {
        return;
    }
    let host = if !list(&state.resource)
        .iter()
        .any(|row| text(row, "registry") == state.workflows.registry.host)
    {
        list(&state.resource)
            .first()
            .map(|row| text(row, "registry").to_string())
            .or(Some(String::new()))
    } else {
        None
    };
    drop(state);
    if let Some(host) = host {
        if host.is_empty() {
            let epoch = app.borrow().workflows.registry.epoch + 1;
            app.borrow_mut().workflows.registry = Registry {
                epoch,
                ..Registry::default()
            };
            redraw_registry(app);
        } else {
            load_repositories(app.clone(), host, false);
        }
    }
    restore_job(app);
}
pub fn registry_html(app: &crate::App) -> String {
    let r = &app.workflows.registry;
    if r.host.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "<section class=\"wasm-card\" aria-label=\"{}\"><h2>{}</h2>",
        t("Registry"),
        escape(&r.host)
    );
    if !r.error.is_empty() {
        out.push_str(&format!(
            "<p role=\"alert\">{}</p>{}",
            escape(&r.error),
            button("Retry", "registry-refresh", &r.host)
        ));
    }
    if r.loading {
        out.push_str(&format!("<p role=\"status\">{}</p>", t("Loading…")));
    }
    out.push_str(&format!(
        "<label for=\"registry-repository\">{}</label><select id=\"registry-repository\" {}>",
        t("Repository"),
        if r.loading { "disabled" } else { "" }
    ));
    for repository in &r.repositories {
        out.push_str(&format!(
            "<option value=\"{}\" {}>{}</option>",
            escape(repository),
            if *repository == r.repository {
                "selected"
            } else {
                ""
            },
            escape(repository)
        ));
    }
    out.push_str("</select>");
    if !r.loading && r.repositories.is_empty() {
        out.push_str(&format!("<p>{}</p>", t("No repositories found.")));
    }
    if r.next_page.is_some() || r.cursor.is_some() {
        out.push_str(&button("More repositories", "registry-more", &r.host));
    }
    if !r.repository.is_empty() {
        if !r.tag_error.is_empty() {
            out.push_str(&format!(
                "<p role=\"alert\">{}</p>{}",
                escape(&r.tag_error),
                button("Retry", "registry-tags-retry", &r.repository)
            ));
        }
        out.push_str(&format!(
            "<label for=\"registry-tag\">{}</label><select id=\"registry-tag\" {}>",
            t("Tag"),
            if r.tag_loading { "disabled" } else { "" }
        ));
        for tag in &r.tags {
            out.push_str(&format!(
                "<option value=\"{}\" {}>{}</option>",
                escape(tag),
                if *tag == r.tag { "selected" } else { "" },
                escape(tag)
            ));
        }
        out.push_str("</select>");
        if r.tag_loading {
            out.push_str(&format!("<p role=\"status\">{}</p>", t("Loading…")));
        }
        if r.next_last.is_some() {
            out.push_str(&button("More tags", "registry-tags-more", &r.repository));
        }
        let reference = format!("{}/{}:{}", r.host, r.repository, r.tag);
        let pulled = r
            .inventory
            .iter()
            .any(|image| text(image, "reference") == reference);
        out.push_str(&format!(
            "<p class=\"state-badge\">{}</p>",
            t(if r.inventory_error {
                "Status unavailable"
            } else if pulled {
                "Pulled"
            } else {
                "Not pulled"
            })
        ));
        out.push_str(&format!(
            "<button class=\"btn primary\" data-action=\"registry-pull-selected\" {}>{}</button>",
            if r.tag_loading
                || !r.tag_error.is_empty()
                || r.tag.is_empty()
                || !r.tags.contains(&r.tag)
            {
                "disabled"
            } else {
                ""
            },
            t("Pull")
        ));
    }
    out.push_str("</section>");
    out
}
fn redraw_registry(app: &Shared) {
    set_html("workflow-root", &registry_html(&app.borrow()));
}
fn load_repositories(app: Shared, host: String, more: bool) {
    let mut state = app.borrow_mut();
    let r = &mut state.workflows.registry;
    if r.loading && r.host == host {
        return;
    }
    if r.host != host {
        let epoch = r.epoch + 1;
        *r = Registry {
            host: host.clone(),
            epoch,
            ..Registry::default()
        };
    }
    if !more {
        r.repositories.clear();
        r.next_page = None;
        r.cursor = None;
        r.repository.clear();
        r.tags.clear();
        r.tag.clear();
        r.tag_epoch += 1;
    }
    r.loading = true;
    r.error.clear();
    let epoch = r.epoch;
    let page = if more { r.next_page.unwrap_or(1) } else { 1 };
    let cursor = if more { r.cursor.clone() } else { None };
    drop(state);
    redraw_registry(&app);
    spawn_local(async move {
        let mut path = format!(
            "/api/pipe/registries/{}/repositories?page={page}",
            api::encode(&host)
        );
        if let Some(cursor) = cursor {
            path.push_str(&format!("&cursor={}", api::encode(&cursor)));
        }
        let result = api::request("GET", &path, None).await;
        let inventory = api::request("GET", "/api/pipe/registry-images", None).await;
        let mut state = app.borrow_mut();
        let r = &mut state.workflows.registry;
        if r.epoch != epoch || r.host != host {
            return;
        }
        r.loading = false;
        match inventory {
            Ok(images) => {
                r.inventory = list(&images).to_vec();
                r.inventory_error = false;
            }
            Err(_) => r.inventory_error = true,
        }
        match result {
            Err(error) => r.error = error,
            Ok(value) => {
                for repository in list(&value["repositories"])
                    .iter()
                    .filter_map(Value::as_str)
                {
                    if !r.repositories.iter().any(|entry| entry == repository) {
                        r.repositories.push(repository.into());
                    }
                }
                r.next_page = value["nextPage"].as_u64();
                r.cursor = value["nextCursor"]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned);
                if value["listingSupported"] == false {
                    for image in &r.inventory {
                        let reference = text(image, "reference");
                        if let Some(tail) = reference.strip_prefix(&format!("{host}/")) {
                            let repository = tail
                                .rsplit_once(':')
                                .map(|(name, _)| name)
                                .unwrap_or(tail)
                                .split('@')
                                .next()
                                .unwrap_or(tail);
                            if !r.repositories.iter().any(|entry| entry == repository) {
                                r.repositories.push(repository.into());
                            }
                        }
                    }
                    if r.repositories.is_empty() {
                        r.error = t("Catalog listing is not supported by this registry.").into();
                    }
                }
            }
        }
        let first = if r.repository.is_empty() {
            r.repositories.first().cloned()
        } else {
            None
        };
        drop(state);
        redraw_registry(&app);
        if let Some(repository) = first {
            load_tags(app, repository, false);
        }
    });
}
fn load_tags(app: Shared, repository: String, more: bool) {
    let mut state = app.borrow_mut();
    let r = &mut state.workflows.registry;
    if r.tag_loading && r.repository == repository {
        return;
    }
    if !more {
        r.repository = repository.clone();
        r.tags.clear();
        r.tag.clear();
        r.next_last = None;
    }
    r.tag_epoch += 1;
    r.tag_loading = true;
    r.tag_error.clear();
    let tag_epoch = r.tag_epoch;
    let epoch = r.epoch;
    let host = r.host.clone();
    let last = if more { r.next_last.clone() } else { None };
    drop(state);
    redraw_registry(&app);
    spawn_local(async move {
        let mut path = format!(
            "/api/pipe/registry-tags?reference={}",
            api::encode(&format!("{host}/{repository}:latest"))
        );
        if let Some(last) = last {
            path.push_str(&format!("&last={}", api::encode(&last)));
        }
        let result = api::request("GET", &path, None).await;
        let mut state = app.borrow_mut();
        let r = &mut state.workflows.registry;
        if r.epoch != epoch || r.tag_epoch != tag_epoch || r.repository != repository {
            return;
        }
        r.tag_loading = false;
        match result {
            Err(error) => r.tag_error = error,
            Ok(value) => {
                for tag in list(&value["tags"]).iter().filter_map(Value::as_str) {
                    if !r.tags.iter().any(|entry| entry == tag) {
                        r.tags.push(tag.into());
                    }
                }
                r.next_last = value["nextLast"]
                    .as_str()
                    .filter(|last| !last.is_empty())
                    .map(str::to_owned);
                if r.tag.is_empty() {
                    r.tag = r.tags.first().cloned().unwrap_or_default();
                }
            }
        }
        drop(state);
        redraw_registry(&app);
    });
}
pub fn restore_job(app: &Shared) {
    if app.borrow().job.is_some() {
        return;
    }
    let saved = window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|storage| storage.get_item("firecrab.registry-job").ok().flatten())
        .and_then(|saved| serde_json::from_str::<Value>(&saved).ok());
    if let Some(saved) = saved {
        let id = text(&saved, "id");
        if !id.is_empty() {
            app.borrow_mut().job = Some((
                format!("/api/pipe/registry-transfers/{}", api::encode(id)),
                json!({"status":"running","alias":id}),
            ));
            crate::poll_job(app.clone());
        }
    }
}
pub fn remember_job(alias: &str, reference: &str) {
    if let Ok(Some(storage)) = window().local_storage() {
        let _ = storage.set_item(
            "firecrab.registry-job",
            &json!({"kind":"transfer","id":alias,"reference":reference}).to_string(),
        );
    }
}
pub fn job_finished(app: &Shared) {
    if let Ok(Some(storage)) = window().local_storage() {
        let _ = storage.remove_item("firecrab.registry-job");
    }
    let app = app.clone();
    spawn_local(async move {
        let result = api::request("GET", "/api/pipe/registry-images", None).await;
        let mut state = app.borrow_mut();
        match result {
            Ok(value) => {
                state.workflows.registry.inventory = list(&value).to_vec();
                state.workflows.registry.inventory_error = false;
            }
            Err(_) => state.workflows.registry.inventory_error = true,
        }
        drop(state);
        redraw_registry(&app);
    });
}
pub fn change(app: &Shared, target: &Element) -> bool {
    let id = target.id();
    if id == "registry-repository" {
        let value = target
            .clone()
            .dyn_into::<web_sys::HtmlSelectElement>()
            .ok()
            .map(|input| input.value())
            .unwrap_or_default();
        load_tags(app.clone(), value, false);
        return true;
    }
    if id == "registry-tag" {
        app.borrow_mut().workflows.registry.tag = target
            .clone()
            .dyn_into::<web_sys::HtmlSelectElement>()
            .ok()
            .map(|input| input.value())
            .unwrap_or_default();
        redraw_registry(app);
        return true;
    }
    if id == "field-targetType" || id == "field-networkMode" {
        sync_form();
        return false;
    }
    if id == "field-zoneId" {
        select_zone(app);
        return false;
    }
    false
}
fn inspect_form(app: Shared, kind: &str) {
    let Some(data) = read_form() else {
        return;
    };
    let Some(form) = app.borrow().form.clone() else {
        return;
    };
    let values = values(&form, &data);
    let (method, path, body) = match kind {
        "zones" => (
            "POST",
            "/api/pipe/cloudflare/zones".into(),
            Some(json!({"apiToken":values.get("apiToken")})),
        ),
        "database" => {
            let body = match form.payload(&values) {
                Ok(body) => body,
                Err(error) => {
                    app.borrow_mut().error = error;
                    render_messages(&app);
                    return;
                }
            };
            ("POST", "/api/pipe/databases/test".into(), Some(body))
        }
        _ => (
            "GET",
            format!(
                "/api/oci/inspect?reference={}",
                api::encode(
                    values
                        .get("reference")
                        .map(String::as_str)
                        .unwrap_or_default()
                )
            ),
            None,
        ),
    };
    let generation = app.borrow().generation;
    let form_path = form.path.clone();
    let kind = kind.to_string();
    spawn_local(async move {
        let result = api::request(method, &path, body.as_ref()).await;
        let mut state = app.borrow_mut();
        if state.generation != generation
            || !state
                .form
                .as_ref()
                .is_some_and(|form| form.path == form_path)
        {
            return;
        }
        if kind == "zones"
            && input("field-apiToken") != values.get("apiToken").cloned().unwrap_or_default()
        {
            return;
        }
        match result {
            Err(error) => {
                state.error = error;
                drop(state);
                render_messages(&app);
            }
            Ok(value) => {
                if kind == "zones" {
                    state.workflows.zones = value.clone();
                    if let Some(form) = state.form.as_mut() {
                        for field in &mut form.fields {
                            field.value = values.get(field.key).cloned().unwrap_or_default();
                        }
                        form.choose("zoneId", &value, "id", "name");
                        if let Some(field) =
                            form.fields.iter_mut().find(|field| field.key == "zoneId")
                        {
                            field.value = list(&value)
                                .first()
                                .map(|zone| text(zone, "id").to_string())
                                .unwrap_or_default();
                        }
                    }
                    drop(state);
                    crate::render_editor(&app);
                    select_zone(&app);
                } else {
                    drop(state);
                    set_html(
                        "form-result",
                        &if kind == "database" {
                            format!("<p role=\"status\">{}</p>", t("Connected"))
                        } else {
                            render::properties(&value)
                        },
                    );
                }
            }
        }
    });
}
pub fn form_input(app: &Shared, target: &Element) {
    if target.id() != "field-apiToken" {
        return;
    }
    let mut state = app.borrow_mut();
    state.workflows.zones = Value::Null;
    if let Some(form) = state.form.as_mut() {
        for field in form
            .fields
            .iter_mut()
            .filter(|field| ["zoneId", "hostname"].contains(&field.key))
        {
            field.options.clear();
            field.value.clear();
        }
    }
    drop(state);
    set_html("field-zoneId", "");
    set_html("field-hostname", "");
}
fn select_zone(app: &Shared) {
    let zone = element("field-zoneId")
        .and_then(|e| e.dyn_into::<web_sys::HtmlSelectElement>().ok())
        .map(|e| e.value())
        .unwrap_or_default();
    let mut state = app.borrow_mut();
    let name = list(&state.workflows.zones)
        .iter()
        .find(|entry| text(entry, "id") == zone)
        .map(|entry| text(entry, "name"))
        .unwrap_or("");
    let hosts: Vec<String> = list(&state.resource["config"]["routes"])
        .iter()
        .map(|route| text(route, "host"))
        .filter(|host| !name.is_empty() && (*host == name || host.ends_with(&format!(".{name}"))))
        .map(str::to_owned)
        .collect();
    let selected = hosts.first().cloned().unwrap_or_default();
    if let Some(form) = state.form.as_mut() {
        for field in &mut form.fields {
            if field.key == "zoneId" {
                field.value = zone.clone();
            }
            if field.key == "hostname" {
                field.options = hosts
                    .iter()
                    .map(|host| (host.clone(), host.clone()))
                    .collect();
                field.value = selected.clone();
                field.translate_options = false;
            }
        }
    }
    drop(state);
    set_html(
        "field-hostname",
        &hosts
            .iter()
            .map(|host| {
                format!(
                    "<option value=\"{}\">{}</option>",
                    escape(host),
                    escape(host)
                )
            })
            .collect::<Vec<_>>()
            .join(""),
    );
}
pub async fn copy(text: &str) -> Result<(), String> {
    let navigator = window().navigator();
    if let Ok(value) = js_sys::Reflect::get(&navigator, &JsValue::from_str("clipboard"))
        && !value.is_null()
        && !value.is_undefined()
        && let Ok(clipboard) = value.dyn_into::<web_sys::Clipboard>()
        && JsFuture::from(clipboard.write_text(text)).await.is_ok()
    {
        return Ok(());
    }
    let area = document()
        .create_element("textarea")
        .map_err(api::error)?
        .dyn_into::<web_sys::HtmlTextAreaElement>()
        .map_err(api::error)?;
    area.set_value(text);
    let _ = area.set_attribute("style", "position:fixed;left:-9999px");
    document()
        .body()
        .ok_or("Document body is unavailable.")?
        .append_child(&area)
        .map_err(api::error)?;
    area.select();
    let result = js_sys::Reflect::get(&document(), &JsValue::from_str("execCommand"))
        .and_then(|f| f.dyn_into::<js_sys::Function>())
        .and_then(|f| f.call1(&document(), &JsValue::from_str("copy")))
        .map_err(api::error);
    area.remove();
    if result
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        Ok(())
    } else {
        Err("Clipboard copy failed.".into())
    }
}
pub fn copy_action(app: Shared, value: String) {
    spawn_local(async move {
        match copy(&value).await {
            Ok(()) => app.borrow_mut().notice = t("Copied").into(),
            Err(error) => app.borrow_mut().error = error,
        }
        render_messages(&app);
    });
}

pub fn dispatch(app: &Shared, action: &str, id: &str) -> bool {
    match action {
        "registry-repositories" | "registry-refresh" => {
            load_repositories(app.clone(), id.into(), false)
        }
        "registry-more" => load_repositories(app.clone(), id.into(), true),
        "registry-tags-retry" => load_tags(app.clone(), id.into(), false),
        "registry-tags-more" => load_tags(app.clone(), id.into(), true),
        "registry-pull-selected" => {
            let state = app.borrow();
            let r = &state.workflows.registry;
            if r.tag_loading || !r.tag_error.is_empty() || !r.tags.contains(&r.tag) {
                return true;
            }
            let reference = format!("{}/{}:{}", r.host, r.repository, r.tag);
            drop(state);
            crate::mutate(
                app.clone(),
                "POST".into(),
                "/api/pipe/registry-transfers".into(),
                Some(json!({"action":"pull","reference":reference})),
                None,
            );
        }
        "load-zones" => inspect_form(app.clone(), "zones"),
        "database-test" => inspect_form(app.clone(), "database"),
        "oci-inspect" => inspect_form(app.clone(), "oci"),
        "edit-quick-tunnel" | "edit-firecrab-tunnel" => {
            if !crate::allow_discard(app) {
                return true;
            }
            let mut state = app.borrow_mut();
            let key = if action == "edit-quick-tunnel" {
                "quickTunnels"
            } else {
                "firecrabTunnels"
            };
            let item = list(&state.resource[key])
                .iter()
                .find(|item| {
                    text(
                        item,
                        if key == "quickTunnels" {
                            "id"
                        } else {
                            "subdomain"
                        },
                    ) == id
                })
                .cloned();
            if let Some(mut item) = item {
                if item["target"]["type"] == "quick_tunnel" {
                    let saved = list(&state.resource["quickTunnels"])
                        .iter()
                        .find(|entry| entry["id"] == item["target"]["id"])
                        .map(|entry| entry["target"].clone())
                        .unwrap_or(Value::Null);
                    item["target"] = saved;
                    if item["target"]["type"] == "routes" {
                        item["target"]["type"] = json!("nginx");
                    }
                }
                state.form = crate::forms::resource_form(
                    if key == "quickTunnels" {
                        "quick-tunnel"
                    } else {
                        "firecrab-tunnel"
                    },
                    &item,
                    id,
                );
                state.dirty = false;
                state.panel_open = true;
                drop(state);
                prepare_form(app);
                render_content(app);
            }
        }
        "copy-text" => copy_action(app.clone(), id.into()),
        _ => return false,
    }
    true
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn command(label: &str, value: &str) -> String {
    format!(
        "<div class=\"console-ssh-block\"><div class=\"console-ssh-block-head\"><span class=\"console-ssh-block-label\">{}</span>{}</div><pre class=\"console-ssh-code\"><code>{}</code></pre></div>",
        escape(label),
        button(&format!("Copy {label}"), "copy-text", value),
        escape(value)
    )
}
pub fn ssh_html(state: &State) -> String {
    let vm = &state.ssh_vm;
    if vm.is_null() {
        return format!("<p>{}</p>", t("Loading…"));
    }
    let id = text(vm, "id");
    let filename = format!(
        "firecrab-{}.pem",
        crate::details::safe_name(text(vm, "name"))
    );
    let masked = "-----BEGIN OPENSSH PRIVATE KEY-----\n••••••••••••••••••••••••••••••••••••••••••••\n-----END OPENSSH PRIVATE KEY-----";
    let key = if state.ssh_shown {
        state.ssh_key.as_deref().unwrap_or(masked)
    } else {
        masked
    };
    let masked_class = if state.ssh_shown { "" } else { " is-masked" };
    let mut out = format!(
        "<div class=\"console-ssh\"><h2>SSH · {}</h2><section class=\"ssh-step\"><h3><span aria-hidden=\"true\">1 · </span>{}</h3><div class=\"wasm-actions\">{}{}{}</div><pre id=\"ssh-key\" class=\"console-ssh-code{masked_class}\">{}</pre>{}</section><section class=\"ssh-step\"><h3><span aria-hidden=\"true\">2 · </span>{}</h3>",
        escape(text(vm, "name")),
        t("Private key"),
        button(
            if state.ssh_shown {
                "Hide key"
            } else {
                "Show key"
            },
            "ssh-show-key",
            id
        ),
        button("Copy key", "ssh-copy-key", id),
        button("Download SSH key", "ssh-download-key", id),
        escape(key),
        command(
            "wget",
            &format!(
                "wget -O {filename} {} && chmod 600 {filename}",
                shell_quote(&format!(
                    "{}/api/vms/{}/ssh-key",
                    window().location().origin().unwrap_or_default(),
                    api::encode(id)
                ))
            )
        ),
        t("Connect")
    );
    let ipv4 = text(vm, "ipv4");
    let ipv6 = text(vm, "ipv6");
    if ipv4.parse::<std::net::Ipv4Addr>().is_ok() {
        out.push_str(&command("IPv4", &format!("ssh -i {filename} root@{ipv4}")));
        out.push_str(&command(
            "proxy jump",
            &format!("ssh -J <hostUser>@<hostIP> -i {filename} root@{ipv4}"),
        ));
    }
    if ipv6.parse::<std::net::Ipv6Addr>().is_ok() {
        out.push_str(&command(
            "IPv6",
            &format!("ssh -6 -i {filename} root@{ipv6}"),
        ));
    }
    if ipv4.is_empty() && ipv6.is_empty() {
        out.push_str(&format!("<p>{}</p>", t("No address yet — start the VM.")));
    }
    let forwarded = list(&vm["portForwards"])
        .iter()
        .find(|rule| rule["guestPort"] == 22 && rule["protocol"] == "tcp");
    if let Some(forwarded) = forwarded {
        out.push_str(&command(
            "port forward",
            &format!(
                "ssh -p {} -i {filename} root@<hostIP>",
                forwarded["hostPort"]
            ),
        ));
        out.push_str(&button("Remove SSH port forward", "ssh-port-remove", id));
    } else {
        out.push_str(&format!("<label for=\"ssh-host-port\">{}</label><input id=\"ssh-host-port\" type=\"number\" min=\"1\" max=\"65535\" value=\"22022\">{}",t("Host port"),button("Create SSH port forward","ssh-port-create",id)));
    }
    out.push_str(&format!(
        "</section><section class=\"ssh-step\"><h3><span aria-hidden=\"true\">3 · </span>{}</h3>",
        t("Verify host key")
    ));
    let expected = text(vm, "sshHostFingerprint");
    if !expected.is_empty() {
        out.push_str(&format!("<p>{}</p>", escape(expected)));
    }
    for (ip, switch, label) in [(ipv4, "", "check ipv4"), (ipv6, "-6 ", "check ipv6")] {
        if ip.parse::<std::net::IpAddr>().is_ok() {
            let scan =
                format!("ssh-keyscan {switch}-t ed25519 {ip} 2>/dev/null | ssh-keygen -lf -");
            out.push_str(&command(
                label,
                &if expected.is_empty() {
                    scan
                } else {
                    format!(
                        "{scan} | grep -qF -- {} && echo MATCH || echo MISMATCH",
                        shell_quote(expected)
                    )
                },
            ));
        }
    }
    out.push_str("</section></div>");
    out
}
pub fn redraw_ssh(app: &Shared) {
    let port = input("ssh-host-port");
    let focused = document()
        .active_element()
        .is_some_and(|e| e.id() == "ssh-host-port");
    let state = app.borrow();
    let body = ssh_html(&state.workflows);
    if state.workflows.ssh_open {
        let html = format!(
            "<dialog open class=\"wasm-modal\" aria-modal=\"true\" aria-label=\"{}\">{}{}{}</dialog>",
            t("SSH connection"),
            button("Close", "ssh-close", ""),
            if state.error.is_empty() {
                String::new()
            } else {
                format!("<p role=\"alert\">{}</p>", escape(&state.error))
            },
            body
        );
        set_html("modal-root", &html);
    } else if state
        .console
        .as_ref()
        .is_some_and(|console| console.surface == "ssh")
    {
        set_html("console-ssh", &body);
    }
    if !port.is_empty()
        && let Some(input) =
            element("ssh-host-port").and_then(|e| e.dyn_into::<web_sys::HtmlInputElement>().ok())
    {
        input.set_value(&port);
        if focused {
            let _ = input.focus();
        }
    }
}
fn open_ssh(app: Shared, id: String) {
    let mut state = app.borrow_mut();
    let vm = state
        .detail
        .as_ref()
        .filter(|vm| text(vm, "id") == id)
        .cloned()
        .or_else(|| {
            state
                .vms
                .iter()
                .find(|vm| vm.id == id)
                .and_then(|vm| serde_json::to_value(vm).ok())
        })
        .unwrap_or(Value::Null);
    state.workflows.ssh_vm = vm;
    state.workflows.ssh_key = None;
    state.workflows.ssh_shown = false;
    state.workflows.ssh_open = true;
    drop(state);
    redraw_ssh(&app);
}
fn ssh_key(app: Shared, id: String, action: String) {
    if action == "ssh-show-key" && app.borrow().workflows.ssh_shown {
        app.borrow_mut().workflows.ssh_shown = false;
        redraw_ssh(&app);
        return;
    }
    if app.borrow().workflows.ssh_busy {
        return;
    }
    app.borrow_mut().workflows.ssh_busy = true;
    spawn_local(async move {
        let result = api::text_response(&format!("/api/vms/{}/ssh-key", api::encode(&id))).await;
        let payload = {
            let mut state = app.borrow_mut();
            state.workflows.ssh_busy = false;
            if text(&state.workflows.ssh_vm, "id") != id
                || (!state.workflows.ssh_open
                    && !state
                        .console
                        .as_ref()
                        .is_some_and(|console| console.id == id && console.surface == "ssh"))
            {
                return;
            }
            match result {
                Err(error) => {
                    state.error = error;
                    None
                }
                Ok((key, filename)) => {
                    let filename = filename.unwrap_or_else(|| {
                        format!(
                            "firecrab-{}.pem",
                            crate::details::safe_name(text(&state.workflows.ssh_vm, "name"))
                        )
                    });
                    if action != "ssh-download-key" {
                        state.workflows.ssh_key = Some(key.clone());
                        state.workflows.ssh_shown = action == "ssh-show-key";
                    }
                    Some((key, filename))
                }
            }
        };
        if let Some((key, filename)) = payload {
            if action == "ssh-download-key" {
                crate::download(&filename, &key);
            }
            if action == "ssh-copy-key" {
                match copy(&key).await {
                    Ok(()) => app.borrow_mut().notice = t("Copied").into(),
                    Err(error) => app.borrow_mut().error = error,
                }
            }
        }
        redraw_ssh(&app);
        crate::render_messages(&app);
    });
}
fn ssh_port(app: Shared, id: String, remove: bool) {
    let state = app.borrow();
    let mut forwards = list(&state.workflows.ssh_vm["portForwards"]).to_vec();
    drop(state);
    if remove {
        forwards.retain(|rule| !(rule["guestPort"] == 22 && rule["protocol"] == "tcp"));
    } else {
        let port = input("ssh-host-port")
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0);
        let Some(port) = port else {
            app.borrow_mut().error = "Host port must be 1–65535.".into();
            redraw_ssh(&app);
            return;
        };
        if forwards
            .iter()
            .any(|rule| rule["hostPort"] == port && rule["protocol"] == "tcp")
        {
            app.borrow_mut().error = "Host TCP port is already forwarded.".into();
            redraw_ssh(&app);
            return;
        }
        forwards.push(json!({"hostPort":port,"guestPort":22,"protocol":"tcp"}));
    }
    spawn_local(async move {
        let result = api::request(
            "PUT",
            &format!("/api/vms/{}/port-forwards", api::encode(&id)),
            Some(&json!({"portForwards":forwards})),
        )
        .await;
        let mut state = app.borrow_mut();
        if text(&state.workflows.ssh_vm, "id") != id {
            return;
        }
        match result {
            Ok(vm) => {
                state.workflows.ssh_vm = vm;
                state.error.clear();
            }
            Err(error) => state.error = error,
        }
        drop(state);
        redraw_ssh(&app);
    });
}
pub fn ssh_dispatch(app: &Shared, action: &str, id: &str) -> bool {
    match action {
        "ssh" => open_ssh(app.clone(), id.into()),
        "ssh-close" => {
            let mut state = app.borrow_mut();
            state.workflows.ssh_open = false;
            state.workflows.ssh_vm = Value::Null;
            state.workflows.ssh_key = None;
            state.workflows.ssh_shown = false;
            drop(state);
            set_html("modal-root", "");
        }
        "ssh-show-key" | "ssh-copy-key" | "ssh-download-key" => {
            if text(&app.borrow().workflows.ssh_vm, "id") != id {
                open_ssh(app.clone(), id.into());
            }
            ssh_key(app.clone(), id.into(), action.into())
        }
        "ssh-port-create" | "ssh-port-remove" => {
            ssh_port(app.clone(), id.into(), action == "ssh-port-remove")
        }
        _ => return false,
    }
    true
}
pub fn close_on_escape(app: &Shared) -> bool {
    if app.borrow().workflows.ssh_open {
        ssh_dispatch(app, "ssh-close", "");
        return true;
    }
    false
}
