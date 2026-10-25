use crate::{
    App, Shared, api, document, element,
    i18n::t,
    model::{escape, list, text},
    render, set_html, window,
};
use serde_json::{Value, json};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::spawn_local;
use web_sys::{Element, KeyboardEvent, PointerEvent};
fn button(label: &str, action: &str, id: &str) -> String {
    render::button(label, action, id)
}
fn section(title: &str, body: String) -> String {
    format!(
        "<section class=\"detail-section\" role=\"region\" aria-label=\"{label}\"><h3>{}</h3>{body}</section>",
        escape(t(title)),
        label = escape(t(title))
    )
}
fn property(label: &str, value: &str) -> String {
    format!(
        "<div class=\"wasm-property\"><dt>{}</dt><dd><span class=\"prop-value\" title=\"{}\">{}</span>{}</dd></div>",
        escape(t(label)),
        escape(value),
        escape(value),
        if value.is_empty() {
            String::new()
        } else {
            button("Copy", "copy-text", value)
        }
    )
}
pub fn panel(app: &App) -> String {
    let mut out = format!(
        "<button class=\"wasm-panel-resize\" role=\"separator\" aria-orientation=\"vertical\" aria-label=\"{}\" aria-valuemin=\"320\" aria-valuemax=\"720\" aria-valuenow=\"{}\" id=\"panel-resize\"></button>",
        t("Resize detail panel"),
        app.panel_width
    );
    if app.route == "vms/new" {
        return String::new();
    }
    out.push_str(&button("Close detail", "panel-toggle", ""));
    if let Some(form) = &app.form {
        out.push_str(&format!(
            "<section role=\"region\" aria-label=\"Detail\" class=\"wasm-panel-body detail-body\">{}</section>",
            if form.return_route == "services" { crate::advanced::services::html(app, form) } else { form.html(app.busy, &app.field_errors) } + &if form.title == "Deploy MicroService" { format!("<div id=\"service-status\">{}</div><div id=\"deploy-panel\">{}</div>", render::deployment(app.service_deployment.as_ref()), crate::deploy::html(app)) } else { String::new() }
        ));
        if form.title == "Deploy MicroService" && !text(&form.base, "domain").is_empty() {
            if let Some(pipe) = app.cache.get("/api/pipe")
                && !text(&pipe["config"]["tunnel"], "id").is_empty()
            {
                out.push_str(&format!("<p>For public HTTPS, point this domain's DNS CNAME to <code>{}.cfargotunnel.com</code>. Keep Cloudflare proxy enabled.</p>",escape(text(&pipe["config"]["tunnel"],"id"))));
            } else {
                out.push_str("<p>For public HTTPS, connect your domain in <a href=\"#/pipe/named-tunnel\">Pipe Tunnel</a>.</p>");
            }
        }
        return out;
    }
    let Some(data) = &app.detail else {
        out.push_str(&format!(
            "<div class=\"empty-state\">{}</div>",
            t("Select an item to see its details.")
        ));
        return out;
    };
    let title = ["name", "alias", "version", "registry", "provider"]
        .iter()
        .find_map(|key| data[key].as_str())
        .unwrap_or(t("Details"));
    out.push_str(&format!("<section role=\"region\" aria-label=\"Detail\" class=\"wasm-panel-body detail-body\"><header class=\"wasm-actions detail-head\"><h2 class=\"detail-title\">{}{}</h2>{}</header>{}",escape(title),if text(data,"purpose")=="pool"{" · Pool"}else{""},button("Close","close-detail",""),render::detail_actions(app,data)));
    if app.route == "vms" {
        out.push_str(
            "<div role=\"tablist\" class=\"tabs\" aria-label=\"MicroVM detail sections\">",
        );
        for (id, label) in [
            ("overview", "Overview"),
            ("network", "Network"),
            ("env", "Environment"),
            ("logs", "Log"),
        ] {
            out.push_str(&format!("<button role=\"tab\" type=\"button\" aria-selected=\"{}\" aria-controls=\"vm-tab-panel\" id=\"vm-tab-{id}\" data-action=\"detail-tab\" data-id=\"{id}\">{}</button>",app.detail_tab==id,t(label)));
        }
        out.push_str("</div>");
        let body = match app.detail_tab.as_str() {
            "env" => environment(app, data),
            "network" => network(data),
            "logs" => logs(app, data),
            _ => overview(data) + &crate::advanced::schedules::vm_schedules(app, data),
        };
        out.push_str(&format!(
            "<div role=\"tabpanel\" id=\"vm-tab-panel\" aria-labelledby=\"vm-tab-{}\">{body}</div>",
            escape(&app.detail_tab)
        ));
    } else {
        if app.route == "pipe/auth" && text(data, "provider") == "firecrab" {
            let expires = data["expiresAt"]
                .as_f64()
                .filter(|value| value.is_finite())
                .map(|millis| {
                    js_sys::Date::new(&JsValue::from_f64(millis))
                        .to_iso_string()
                        .as_string()
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            out.push_str(&format!(
                "<dl>{}{}{}</dl>",
                property("Account", text(data, "account")),
                property("Account ID", text(data, "accountId")),
                property("Token expires", &expires)
            ));
        } else if let Some(body) = crate::advanced::detail(app, data) {
            out.push_str(&body);
        } else {
            out.push_str(&render::properties(data));
        }
        if app.route == "storages" {
            out.push_str(&format!(
                "<p>{} / {} GiB</p>",
                data["availableGib"], data["totalGib"]
            ));
        }
        if app.route == "services" {
            out.push_str(&format!(
                "<div id=\"service-status\">{}</div><div id=\"deploy-panel\">{}</div>",
                render::deployment(app.service_deployment.as_ref()),
                crate::deploy::html(app)
            ));
        }
        let related: Vec<_> = app
            .vms
            .iter()
            .filter(|vm| match app.route.as_str() {
                "networks" => vm.extra.get("microNetworkId") == Some(&data["id"]),
                "images" => vm.template == text(data, "alias"),
                "storages" => vm.extra.get("storageRoot") == Some(&data["path"]),
                "shells" => list(vm.extra.get("shellRefs").unwrap_or(&Value::Null))
                    .iter()
                    .any(|entry| entry["shellId"] == data["id"]),
                _ => false,
            })
            .collect();
        if app.route == "networks" || app.route == "storages" || app.route == "images" {
            let title = match app.route.as_str() {
                "networks" => "Member VMs",
                "storages" => "VMs",
                _ => "VMs using it",
            };
            let members = if data["vms"].is_array() {
                list(&data["vms"])
                    .iter()
                    .map(|vm| button(text(vm, "name"), "navigate-vm", text(vm, "id")))
                    .collect::<String>()
            } else {
                related
                    .iter()
                    .map(|vm| button(&vm.name, "navigate-vm", &vm.id))
                    .collect::<String>()
            };
            out.push_str(&section(title, members));
        }
        if app.route == "shells" {
            let revisions = list(&data["revisions"])
                .iter()
                .map(|revision| format!("<li>v{}</li>", revision["version"]))
                .collect::<String>();
            out.push_str(&section("Revisions", format!("<ul>{revisions}</ul>")));
        }
        if !related.is_empty() {
            out.push_str(&section(
                "MicroVM",
                related
                    .iter()
                    .map(|vm| button(&vm.name, "navigate-vm", &vm.id))
                    .collect::<Vec<_>>()
                    .join(""),
            ));
        }
    }
    out.push_str(&format!(
        "<div id=\"detail-result\">{}</div></section>",
        app.detail_result
    ));
    out
}
pub fn overview(data: &Value) -> String {
    let mut props = String::from("<dl class=\"props\">");
    for (label, key) in [
        ("ID", "id"),
        ("Name", "name"),
        ("State", "state"),
        ("Image", "template"),
        ("Version", "templateVersion"),
        ("MicroNetwork", "microNetworkId"),
        ("Hostname", "hostname"),
        ("SSH host key", "sshHostFingerprint"),
        ("MicroStorage", "storageRoot"),
    ] {
        props.push_str(&property(label, text(data, key)));
    }
    for (label, key) in [
        ("CPU", "cpu"),
        ("Memory (MiB)", "ram"),
        ("Disk (GB)", "diskGb"),
    ] {
        props.push_str(&property(label, &data[key].to_string()));
    }
    props.push_str("</dl>");
    let mut out = section("Properties", props);
    if ["paused", "pausing", "resuming"].contains(&text(data, "state")) {
        out.push_str(&format!(
            "<p>{}</p>",
            t("Memory stays allocated while the VM is paused. Resume to reconnect.")
        ));
    }
    out.push_str(&section("Startup", timeline(data)));
    if data["reconciliation"].is_object() {
        out.push_str(&section(
            "API-STATUS",
            reconciliation(&data["reconciliation"]),
        ));
    }
    let cpu = data["cpuUsagePercent"]
        .as_f64()
        .map(|value| format!("{value:.1}%"))
        .unwrap_or("—".into());
    let memory = data["memoryUsedPercent"]
        .as_f64()
        .map(|value| format!("{value:.1}%"))
        .unwrap_or("—".into());
    out.push_str(&section(
        "Usage",
        format!(
            "<p>CPU {cpu} · {} {memory}</p>{}{}",
            t("Memory"),
            chart(&data["usageHistory"], "cpuUsagePercent"),
            chart(&data["usageHistory"], "memoryUsedPercent")
        ),
    ));
    out
}
fn chart(history: &Value, key: &str) -> String {
    let values: Vec<_> = list(history)
        .iter()
        .filter_map(|entry| entry[key].as_f64())
        .filter(|value| value.is_finite())
        .collect();
    if values.len() < 2 {
        return String::new();
    }
    let points = values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            format!(
                "{:.1},{:.1}",
                index as f64 * 240.0 / (values.len() - 1) as f64,
                40.0 - value.clamp(0.0, 100.0) * 0.4
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "<svg viewBox=\"0 0 240 44\" aria-label=\"{}\" role=\"img\"><polyline points=\"{points}\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/></svg>",
        escape(key)
    )
}
pub fn step_label(step: &str) -> &str {
    t(match step {
        "preparingDisk" => "Preparing disk",
        "generatingConfig" => "Generating configuration",
        "startingProcess" => "Starting process",
        "configuringNetwork" => "Checking network",
        _ => step,
    })
}
pub fn timeline(data: &Value) -> String {
    let mut out = "<ol class=\"wasm-timeline\">".to_string();
    for step in [
        "preparingDisk",
        "generatingConfig",
        "startingProcess",
        "configuringNetwork",
    ] {
        let run = list(&data["startupTimeline"])
            .iter()
            .rev()
            .find(|run| run["step"] == step);
        let outcome =
            run.map(|run| text(run, "outcome"))
                .unwrap_or(if data["startupStep"] == step {
                    "running"
                } else {
                    "pending"
                });
        let duration = run
            .and_then(|run| {
                Some((run["endedAtMs"].as_f64()? - run["startedAtMs"].as_f64()?).max(0.0))
            })
            .map(|millis| format!("{:.0}s", millis / 1000.0))
            .unwrap_or_default();
        let detail = run
            .map(|run| failure_detail(text(run, "detail")))
            .unwrap_or_default();
        out.push_str(&format!(
            "<li class=\"{}\"><strong>{}</strong><span>{} {duration}</span><p>{}</p></li>",
            escape(outcome),
            escape(step_label(step)),
            escape(t(outcome)),
            escape(&detail)
        ));
    }
    out.push_str("</ol>");
    out
}
fn failure_detail(detail: &str) -> String {
    if detail.contains("File exists")
        && let Some(tail) = detail.split("IPv4 lease ").nth(1)
        && let Some(ip) = tail.split_whitespace().next()
        && ip.parse::<std::net::Ipv4Addr>().is_ok()
    {
        return if t("Name") == "이름" {
            format!(
                "IP {ip}은 기존 MicroVM이 사용 중이다. 기존 VM을 중지하거나 복구한 뒤 다시 시도한다."
            )
        } else {
            format!("IP {ip} is used by an existing MicroVM. Stop or recover that VM and retry.")
        };
    }
    detail.into()
}
fn reconciliation(result: &Value) -> String {
    let (label, en, ko) = match text(result, "outcome") {
        "reconnected" => (
            "Reconnected",
            "The API reconnected to the surviving VM. Any pending stop was resumed.",
            "API가 실행 중이던 VM에 다시 연결했다. 진행 중이던 종료 작업도 이어서 처리했다.",
        ),
        "gone" => (
            "VM not found",
            "No running VM or exit record was found. The VM was marked stopped.",
            "실행 중인 VM이나 종료 기록이 없어 중지 상태로 변경했다.",
        ),
        "mismatched" => (
            "Connection mismatch",
            "The API could not control the VM and attempted to stop its service.",
            "API가 VM을 제어할 수 없어 서비스 중지를 시도했다.",
        ),
        "networkFailed" => (
            "Network recovery failed",
            "The VM reconnected, but restoring its network configuration failed.",
            "VM에 다시 연결했지만 네트워크 설정 복구에 실패했다.",
        ),
        "interrupted" => (
            "Startup interrupted",
            "The API restart interrupted VM startup. The VM was marked error.",
            "API 재시작으로 VM 시작이 중단되었다. VM을 오류 상태로 변경했다.",
        ),
        _ => (
            "Exited while API offline",
            "The VM exited while the API was offline. Its exit status was recorded.",
            "API가 중단된 동안 VM이 종료되었다. 종료 상태를 기록했다.",
        ),
    };
    let time = result["checkedAtMs"]
        .as_f64()
        .map(|millis| {
            js_sys::Date::new(&JsValue::from_f64(millis))
                .to_iso_string()
                .as_string()
                .unwrap_or_default()
        })
        .unwrap_or_default();
    format!(
        "<dl>{}{}{}{}</dl>",
        property("API", "Firecrab API · firecrab-api"),
        property("Status", t(label)),
        property("Info", if t("Name") == "이름" { ko } else { en }),
        property("Checked at", &time)
    ) + &if text(result, "detail").is_empty() {
        String::new()
    } else {
        format!(
            "<p class=\"wasm-diagnostic\">{}</p>",
            escape(text(result, "detail"))
        )
    }
}
fn network(data: &Value) -> String {
    let mut out = "<dl>".to_string();
    for (label, key) in [
        ("IPv4", "ipv4"),
        ("IPv6", "ipv6"),
        ("MAC", "mac"),
        ("Hostname", "hostname"),
        ("Network", "microNetworkId"),
        ("Egress policy", "egressPolicy"),
    ] {
        out.push_str(&property(label, text(data, key)));
    }
    out.push_str("</dl>");
    out.push_str(&section(
        "Port forwarding",
        render::properties(&json!({"portForwards":data["portForwards"]})),
    ));
    out
}
fn environment(app: &App, data: &Value) -> String {
    let mut out = button(
        if app.show_env {
            "Hide values"
        } else {
            "Show values"
        },
        "env-reveal",
        "",
    );
    out.push_str("<dl>");
    if let Some(env) = data["env"].as_object() {
        for (key, value) in env {
            let secret = ["TOKEN", "SECRET", "PASSWORD"]
                .iter()
                .any(|part| crate::model::uppercase(key).contains(part));
            let value = if secret && !app.show_env {
                "••••••••"
            } else {
                value.as_str().unwrap_or("")
            };
            out.push_str(&format!(
                "<div class=\"wasm-property\"><dt>{}</dt><dd><span class=\"kv-value\">{}</span></dd></div>",
                escape(key),
                escape(value)
            ));
        }
    }
    out.push_str("</dl>");
    out
}
fn logs(app: &App, data: &Value) -> String {
    format!(
        "<div class=\"wasm-actions\">{}{}</div><pre class=\"wasm-log detail-log\">{}</pre>",
        button("Copy log", "copy-vm-log", text(data, "id")),
        button("Save log", "export-vm-log", text(data, "id")),
        escape(if app.detail_log_loaded {
            &app.detail_log
        } else {
            t("Loading…")
        })
    )
}
pub fn export_bundle(vm: &Value, id: &str, server_log: &str, terminal: Option<&str>) -> String {
    let mut text = format!(
        "# firecrab VM log — {}\n# id: {id}\n# state: {}\n# exported: {}\n\n## startup timeline\n",
        text(vm, "name"),
        text(vm, "state"),
        js_sys::Date::new_0().to_iso_string()
    );
    if list(&vm["startupTimeline"]).is_empty() {
        text.push_str("(no timeline)\n");
    }
    for run in list(&vm["startupTimeline"]) {
        text.push_str(&format!(
            "{}\t{}\t{}\n",
            step_label(crate::model::text(run, "step")),
            crate::model::text(run, "outcome"),
            crate::model::text(run, "detail")
        ));
    }
    text.push_str(&format!(
        "\n## console log (server)\n{}",
        server_log.trim_end()
    ));
    if let Some(terminal) = terminal {
        text.push_str(&format!(
            "\n\n## terminal buffer (session)\n{}",
            terminal.trim_end()
        ));
    }
    text
}
pub fn load_log(app: Shared) {
    let state = app.borrow();
    let Some(vm) = &state.detail else {
        return;
    };
    if state.detail_tab != "logs" || !state.panel_open {
        return;
    }
    let id = text(vm, "id").to_string();
    let generation = state.detail_generation;
    drop(state);
    spawn_local(async move {
        let result = api::request("GET", &format!("/api/vms/{}/log", api::encode(&id)), None).await;
        let mut state = app.borrow_mut();
        if state.detail_generation != generation || state.detail_tab != "logs" {
            return;
        }
        match result {
            Ok(value) => {
                state.detail_log = text(&value, "consoleLog").into();
                state.detail_log_loaded = true;
                if value["truncated"] == true {
                    state.detail_log.push_str("\n[truncated]");
                }
            }
            Err(error) => state.error = error,
        }
        drop(state);
        crate::render_detail(&app);
    });
}
pub fn tick(app: &Shared) {
    let mut state = app.borrow_mut();
    let deploy_form = state
        .form
        .as_ref()
        .is_some_and(|form| form.title == "Deploy MicroService");
    if state.busy
        || state.form.is_some() && !deploy_form
        || !state.panel_open
        || !["vms", "services"].contains(&state.route.as_str())
        || state.detail_polling == Some(state.detail_generation)
    {
        return;
    }
    let Some(detail) = &state.detail else {
        return;
    };
    let id = text(detail, "id").to_string();
    let service = state.route == "services";
    let generation = state.detail_generation;
    let route_generation = state.generation;
    let logs = state.detail_tab == "logs";
    state.detail_polling = Some(generation);
    drop(state);
    let app = app.clone();
    spawn_local(async move {
        let path = if service {
            format!("/api/micro-services/{}/deployment", api::encode(&id))
        } else {
            format!("/api/vms/{}", api::encode(&id))
        };
        let result = api::request("GET", &path, None).await;
        let mut state = app.borrow_mut();
        if state.detail_polling == Some(generation) {
            state.detail_polling = None;
        }
        if state.detail_generation != generation || state.generation != route_generation {
            return;
        }
        let deploy_form = state
            .form
            .as_ref()
            .is_some_and(|form| form.title == "Deploy MicroService");
        if state.form.is_some() && !deploy_form {
            return;
        }
        if let Ok(value) = result {
            if service {
                state.service_deployment = Some(value);
            } else if let Some(base) = state.detail.as_mut().and_then(Value::as_object_mut)
                && let Some(vm) = value.as_object()
            {
                base.extend(vm.clone());
            }
        }
        let status = render::deployment(state.service_deployment.as_ref());
        drop(state);
        if deploy_form {
            set_html("service-status", &status);
            set_html("deploy-panel", &crate::deploy::html(&app.borrow()));
            crate::advanced::services::sync(&app);
        } else {
            crate::render_detail(&app);
        }
        if logs {
            load_log(app);
        }
    });
}
pub fn dispatch(app: &Shared, action: &str, id: &str) -> bool {
    match action {
        "detail-tab" => {
            let mut state = app.borrow_mut();
            if state.dirty {
                return true;
            }
            state.detail_tab = id.into();
            drop(state);
            crate::render_detail(app);
            if id == "logs" {
                load_log(app.clone());
            }
        }
        "logs" => {
            app.borrow_mut().detail_tab = "logs".into();
            crate::render_detail(app);
            load_log(app.clone());
        }
        "env-reveal" => {
            let mut state = app.borrow_mut();
            state.show_env = !state.show_env;
            drop(state);
            crate::render_detail(app);
        }
        "panel-toggle" => {
            if app.borrow().panel_open && !crate::allow_discard(app) {
                return true;
            }
            set_html("palette-root", "");
            let mut state = app.borrow_mut();
            state.panel_open = !state.panel_open;
            let open = state.panel_open;
            drop(state);
            if let Ok(Some(storage)) = window().local_storage() {
                let _ =
                    storage.set_item("firecrab.detailPanel", if open { "open" } else { "closed" });
            }
            crate::render_detail(app);
        }
        "navigate-vm" | "palette-vm" => {
            set_html("palette-root", "");
            if app.borrow().route == "vms" {
                crate::show_detail(app.clone(), id.into());
            } else {
                app.borrow_mut().pending_vm = Some(id.into());
                let _ = window().location().set_hash("/vms");
            }
        }
        "copy-vm-log" | "export-vm-log" => {
            let vm = app.borrow().detail.clone().unwrap_or(Value::Null);
            let id = id.to_string();
            let copy = action == "copy-vm-log";
            let app = app.clone();
            spawn_local(async move {
                let result =
                    api::request("GET", &format!("/api/vms/{}/log", api::encode(&id)), None).await;
                let text = export_bundle(
                    &vm,
                    &id,
                    result
                        .as_ref()
                        .map(|log| crate::model::text(log, "consoleLog"))
                        .unwrap_or(""),
                    None,
                );
                if copy {
                    crate::workflows::copy_action(app, text);
                } else {
                    crate::download(
                        &format!(
                            "firecrab-vm-{}.txt",
                            safe_name(crate::model::text(&vm, "name"))
                        ),
                        &text,
                    );
                }
            });
        }
        "palette-view" => {
            set_html("palette-root", "");
            let _ = window().location().set_hash(&format!("/{id}"));
        }
        "palette-new" => {
            set_html("palette-root", "");
            let _ = window().location().set_hash("/vms/new");
        }
        _ => return false,
    }
    true
}
pub fn safe_name(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || "._-".contains(ch) {
                ch
            } else {
                '-'
            }
        })
        .take(64)
        .collect();
    if safe.is_empty() { "vm".into() } else { safe }
}
pub fn palette_items(app: &App) -> String {
    let query = crate::model::lowercase(app.palette_query.trim());
    let mut entries = Vec::new();
    for (id, _, en, ko) in crate::model::VIEWS {
        let label = if app.locale == "ko" { *ko } else { *en };
        if query.is_empty()
            || crate::model::lowercase(label).contains(&query)
            || crate::model::lowercase(en).contains(&query)
        {
            entries.push((label.to_string(), "palette-view", id.to_string()));
        }
    }
    for (label, action) in [
        ("New MicroVM", "palette-new"),
        (
            if app.panel_open {
                "Hide details"
            } else {
                "Show details"
            },
            "panel-toggle",
        ),
    ] {
        if query.is_empty()
            || crate::model::lowercase(t(label)).contains(&query)
            || action == "panel-toggle" && "toggle detail".contains(&query)
        {
            entries.push((t(label).to_string(), action, String::new()));
        }
    }
    for vm in &app.vms {
        if entries.len() >= 75 {
            break;
        }
        if query.is_empty() || vm.matches("all", &query) {
            entries.push((
                format!(
                    "MicroVM · {} · {}",
                    vm.name,
                    vm.ipv4.as_deref().unwrap_or("")
                ),
                "palette-vm",
                vm.id.clone(),
            ));
        }
    }
    if entries.is_empty() {
        return format!("<p role=\"status\">{}</p>", t("No results"));
    }
    entries.iter().enumerate().map(|(index,(label,action,id))|format!("<button type=\"button\" role=\"option\" id=\"palette-option-{index}\" aria-selected=\"{}\" class=\"palette-item\" data-action=\"{action}\" data-id=\"{}\">{}</button>",app.palette_index==index,escape(id),escape(label))).collect::<Vec<_>>().join("")
}
pub fn palette_input(app: &Shared, query: String) {
    let mut state = app.borrow_mut();
    state.palette_query = query;
    state.palette_index = 0;
    drop(state);
    set_html("palette-items", &palette_items(&app.borrow()));
}
pub fn palette_key(app: &Shared, event: &KeyboardEvent) -> bool {
    if element("palette-query").is_none() {
        return false;
    }
    if event.key() == "Escape" {
        set_html("palette-root", "");
        return true;
    }
    if !["ArrowDown", "ArrowUp", "Enter"].contains(&event.key().as_str()) {
        return false;
    }
    event.prevent_default();
    let Ok(items) = document().query_selector_all(".palette-item") else {
        return true;
    };
    if items.length() == 0 {
        return true;
    }
    if event.key() == "Enter" {
        let index = app.borrow().palette_index;
        if let Some(item) = items
            .item(index as u32)
            .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
        {
            item.click();
        }
        return true;
    }
    let mut state = app.borrow_mut();
    state.palette_index = if event.key() == "ArrowDown" {
        (state.palette_index + 1) % items.length() as usize
    } else {
        (state.palette_index + items.length() as usize - 1) % items.length() as usize
    };
    let selected = state.palette_index;
    drop(state);
    for index in 0..items.length() {
        if let Some(item) = items.item(index).and_then(|e| e.dyn_into::<Element>().ok()) {
            let _ = item.set_attribute(
                "aria-selected",
                if index as usize == selected {
                    "true"
                } else {
                    "false"
                },
            );
        }
    }
    if let Some(query) = element("palette-query") {
        let _ = query.set_attribute(
            "aria-activedescendant",
            &format!("palette-option-{selected}"),
        );
    }
    true
}
pub fn resize_key(app: &Shared, event: &KeyboardEvent) -> bool {
    if !document()
        .active_element()
        .is_some_and(|e| e.id() == "panel-resize")
    {
        return false;
    }
    let width = app.borrow().panel_width;
    let width = match event.key().as_str() {
        "ArrowLeft" => (width + 16).min(720),
        "ArrowRight" => width.saturating_sub(16).max(320),
        "Home" => 320,
        "End" => 720,
        _ => return false,
    };
    event.prevent_default();
    app.borrow_mut().panel_width = width;
    if let Some(panel) = element("detail-panel") {
        let _ = panel.set_attribute("style", &format!("width:{width}px"));
    }
    if let Some(handle) = element("panel-resize") {
        let _ = handle.set_attribute("aria-valuenow", &width.to_string());
    }
    if let Ok(Some(storage)) = window().local_storage() {
        let _ = storage.set_item("firecrab.detailPanelWidth", &width.to_string());
    }
    true
}
pub fn install(app: &Shared) -> Result<(), JsValue> {
    let responsive = app.clone();
    let resized = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        if responsive.borrow().form.is_none() {
            crate::render_detail(&responsive);
        }
    });
    window().add_event_listener_with_callback("resize", resized.as_ref().unchecked_ref())?;
    resized.forget();
    let reset = app.clone();
    let double = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
        if event
            .target()
            .and_then(|el| el.dyn_into::<Element>().ok())
            .is_some_and(|el| el.id() == "panel-resize")
        {
            reset.borrow_mut().panel_width = 384;
            if let Ok(Some(storage)) = window().local_storage() {
                let _ = storage.set_item("firecrab.detailPanelWidth", "384");
            }
            crate::render_detail(&reset);
        }
    });
    document().add_event_listener_with_callback("dblclick", double.as_ref().unchecked_ref())?;
    double.forget();
    let moved = app.clone();
    let down = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        if event
            .target()
            .and_then(|e| e.dyn_into::<Element>().ok())
            .is_some_and(|e| e.id() == "panel-resize")
        {
            event.prevent_default();
            let mut state = moved.borrow_mut();
            state.panel_dragging = true;
            state.panel_drag_x = event.client_x();
            state.panel_drag_width = state.panel_width;
        }
    });
    document().add_event_listener_with_callback("pointerdown", down.as_ref().unchecked_ref())?;
    down.forget();
    let moved = app.clone();
    let motion = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        if moved.borrow().panel_dragging {
            resize(&moved, event.client_x());
        }
    });
    document().add_event_listener_with_callback("pointermove", motion.as_ref().unchecked_ref())?;
    motion.forget();
    let released = app.clone();
    let up = Closure::<dyn FnMut(PointerEvent)>::new(move |_| {
        let mut state = released.borrow_mut();
        if state.panel_dragging {
            state.panel_dragging = false;
            if let Ok(Some(storage)) = window().local_storage() {
                let _ =
                    storage.set_item("firecrab.detailPanelWidth", &state.panel_width.to_string());
            }
        }
    });
    document().add_event_listener_with_callback("pointerup", up.as_ref().unchecked_ref())?;
    up.forget();
    let hovered = app.clone();
    let hover = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
        let target = event
            .target()
            .and_then(|e| e.dyn_into::<Element>().ok())
            .and_then(|e| e.closest("[data-vm-tooltip]").ok().flatten());
        if let Some(target) = target {
            tooltip(&hovered, &target);
        } else {
            set_html("tooltip-root", "");
        }
    });
    document().add_event_listener_with_callback("pointerover", hover.as_ref().unchecked_ref())?;
    document().add_event_listener_with_callback("focusin", hover.as_ref().unchecked_ref())?;
    hover.forget();
    Ok(())
}
fn resize(app: &Shared, x: i32) {
    let viewport = window()
        .inner_width()
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(1440.0);
    let max = if viewport >= 1280.0 {
        (viewport - 480.0 - 236.0).clamp(320.0, 720.0) as i32
    } else {
        720
    };
    let state = app.borrow();
    let width = (state.panel_drag_width as i32 + state.panel_drag_x - x).clamp(320, max) as u32;
    drop(state);
    app.borrow_mut().panel_width = width;
    if let Some(panel) = element("detail-panel") {
        let _ = panel.set_attribute("style", &format!("width:{width}px"));
    }
    if let Some(handle) = element("panel-resize") {
        let _ = handle.set_attribute("aria-valuenow", &width.to_string());
    }
}
fn tooltip(app: &Shared, target: &Element) {
    let id = target.get_attribute("data-vm-tooltip").unwrap_or_default();
    let state = app.borrow();
    let Some(vm) = state.vms.iter().find(|vm| vm.id == id) else {
        return;
    };
    let rect = target.get_bounding_client_rect();
    let width = window()
        .inner_width()
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(1440.0);
    let left = rect.left().clamp(8.0, (width - 320.0).max(8.0));
    let body = format!(
        "<h3>VM-STATUS</h3><dl>{}{}{}</dl>",
        property("Name", &vm.name),
        property("ID", &vm.id),
        property("Status", t(&vm.state))
    ) + &format!(
        "<p>{}</p>",
        escape(t(match vm.state.as_str() {
            "created" => "The VM is ready to start.",
            "running" => "The VM is running.",
            "starting" => "The VM is starting.",
            "stopping" => "The VM is stopping.",
            "stopped" => "The VM is stopped.",
            _ => "The VM encountered an error. Open details for diagnostics.",
        }))
    );

    set_html(
        "tooltip-root",
        &format!(
            "<div role=\"tooltip\" class=\"wasm-tooltip\" style=\"left:{left}px;top:{}px\">{body}</div>",
            rect.bottom() + 6.0
        ),
    );
}
