use crate::{
    App, Shared, api, document, element,
    i18n::t,
    model::{VIEWS, escape, list, text},
    render, set_html, window,
};
use serde_json::{Value, json};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::spawn_local;
use web_sys::{Element, KeyboardEvent, PointerEvent};
fn button(label: &str, action: &str, id: &str) -> String {
    render::button(label, action, id)
}
/// A panel section: a title row (with an optional aside, such as icon buttons) and its body.
pub fn section(title: &str, aside: &str, body: &str) -> String {
    let title = escape(t(title));
    format!(
        "<section class=\"detail-sec\" aria-label=\"{title}\"><div class=\"detail-sec-head\"><h3>{title}</h3>{aside}</div>{body}</section>"
    )
}
/// The flat head of every detail panel. `kind` and `title` are plain text; `meta` and `actions`
/// are markup. The close button always follows the actions.
pub fn panel_head(kind: &str, title: &str, mono: bool, meta: &str, actions: &str) -> String {
    let close = render::icon_button("Close detail", "close-detail", "", render::Icon::Close);
    let title = escape(title);
    format!(
        "<header class=\"detail-head\"><div class=\"detail-top\"><span class=\"detail-kind\">{}</span><div class=\"detail-top-actions\">{actions}{close}</div></div><h2 class=\"detail-title{}\" title=\"{title}\">{title}</h2><div class=\"detail-meta\">{meta}</div></header>",
        escape(t(kind)),
        if mono { " mono" } else { "" }
    )
}
/// The tabs of the MicroVM panel and of the MicroService panel: the id and the name.
const VM_TABS: [(&str, &str); 5] = [
    ("overview", "Overview"),
    ("network", "Network"),
    ("volumes", "Volumes"),
    ("env", "Environment"),
    ("logs", "Log"),
];
const SHELL_TABS: [(&str, &str); 2] = [("content", "Content"), ("revisions", "Revisions")];
const SERVICE_TABS: [(&str, &str); 4] = [
    ("overview", "Overview"),
    ("instances", "Instances"),
    ("releases", "Releases"),
    ("settings", "Settings"),
];
/// The tabs a tab button belongs to, from its id (`{prefix}-tab-{tab}`): the prefix and the tabs.
fn panel_tabs(button_id: &str) -> Option<(&'static str, &'static [(&'static str, &'static str)])> {
    if button_id.starts_with("vm-tab-") {
        Some(("vm", &VM_TABS))
    } else if button_id.starts_with("service-tab-") {
        Some(("service", &SERVICE_TABS))
    } else if button_id.starts_with("shell-tab-") {
        Some(("shell", &SHELL_TABS))
    } else {
        None
    }
}
/// The underline tabs of a panel. `prefix` names the ids (`{prefix}-tab-{tab}`, `{prefix}-tab-panel`).
/// `counts` gives a tab a count after its name, as the sub-tabs of a list screen have; a tab that
/// is not in it has none (a count that is not known yet is left out, not shown as zero).
fn tab_list(
    prefix: &str,
    label: &str,
    tabs: &[(&str, &str)],
    active: &str,
    counts: &[(&str, usize)],
) -> String {
    let mut out =
        format!("<div role=\"tablist\" class=\"tabs detail-tabs\" aria-label=\"{label}\">");
    for (id, name) in tabs {
        let count = counts
            .iter()
            .find(|(tab, _)| tab == id)
            .map_or_else(String::new, |(_, count)| format!(" <em>{count}</em>"));
        out.push_str(&format!("<button class=\"tab\" role=\"tab\" type=\"button\" aria-selected=\"{}\" aria-controls=\"{prefix}-tab-panel\" id=\"{prefix}-tab-{id}\" tabindex=\"{}\" data-action=\"detail-tab\" data-id=\"{id}\">{}{count}</button>", active == *id, if active == *id { 0 } else { -1 }, t(name)));
    }
    out.push_str("</div>");
    out
}
/// What the open tab shows.
fn tab_panel(prefix: &str, active: &str, body: &str) -> String {
    format!(
        "<div role=\"tabpanel\" id=\"{prefix}-tab-panel\" aria-labelledby=\"{prefix}-tab-{}\">{body}</div>",
        escape(active)
    )
}
/// The panel's kind line: the current screen's menu label.
fn kind(app: &App) -> &'static str {
    let screen = app.route.split('/').next().unwrap_or("vms");
    let (_, _, en, ko) = VIEWS
        .iter()
        .find(|view| view.0 == screen)
        .unwrap_or(&VIEWS[0]);
    if app.locale == "ko" { ko } else { en }
}
/// A label and its value.
fn property(label: &str, value: &str) -> String {
    property_row(label, value, false)
}
/// Like `property`, with an icon right after the value that copies it. For values worth copying:
/// IDs, addresses, host names, images and keys. An empty value has no icon.
fn copyable(label: &str, value: &str) -> String {
    property_row(label, value, true)
}
fn property_row(label: &str, value: &str, copy: bool) -> String {
    let copy = if copy && !value.is_empty() {
        let name = escape(&crate::i18n::copy_name(label));
        format!(
            "<button type=\"button\" class=\"icon-btn prop-copy\" aria-label=\"{name}\" title=\"{name}\" data-action=\"copy-text\" data-id=\"{}\">{}</button>",
            escape(value),
            render::Icon::Copy.svg()
        )
    } else {
        String::new()
    };
    format!(
        "<div class=\"wasm-property\"><dt>{}</dt><dd><span class=\"prop-value\" title=\"{}\">{}</span>{copy}</dd></div>",
        escape(t(label)),
        escape(value),
        escape(value)
    )
}
pub const PANEL_MIN_WIDTH: u32 = 440;
pub const PANEL_MAX_WIDTH: u32 = 960;
pub const PANEL_DEFAULT_WIDTH: u32 = 576;

/// Leave room for the larger sidebar and list when the panel sits beside them.
pub fn panel_max_width() -> u32 {
    let viewport = window()
        .inner_width()
        .ok()
        .and_then(|value| value.as_f64())
        .unwrap_or(1440.0);
    if viewport >= 1440.0 {
        (viewport - 832.0).clamp(f64::from(PANEL_MIN_WIDTH), f64::from(PANEL_MAX_WIDTH)) as u32
    } else {
        PANEL_MAX_WIDTH
    }
}

/// Routes that use the whole workspace, so they never get a side panel.
pub const NO_PANEL_ROUTES: [&str; 4] = ["vms/new", "services/new", "pipe/database", "ai-sandbox"];
/// The panel shows only for a selection or an open form.
pub fn panel_visible(app: &App) -> bool {
    app.panel_open
        && (app.detail.is_some() || app.form.is_some())
        && !NO_PANEL_ROUTES.contains(&app.route.as_str())
}
/// What the panel shows: the screen, the selected item, the open tab and the form. A render of
/// the same view (a poll) keeps the body's scroll position; another view starts at the top.
pub fn panel_view(app: &App) -> String {
    let item = app.detail.as_ref().map_or_else(String::new, |detail| {
        ["id", "alias", "version", "provider", "registry"]
            .map(|key| text(detail, key))
            .join("\0")
    });
    let form = app.form.as_ref().map_or("", |form| form.title.as_str());
    format!("{}\0{item}\0{}\0{form}", app.route, app.detail_tab)
}
pub fn panel(app: &App) -> String {
    let mut out = format!(
        "<button class=\"wasm-panel-resize\" role=\"separator\" aria-orientation=\"vertical\" aria-label=\"{}\" aria-valuemin=\"{PANEL_MIN_WIDTH}\" aria-valuemax=\"{PANEL_MAX_WIDTH}\" aria-valuenow=\"{}\" id=\"panel-resize\"></button>",
        t("Resize detail panel"),
        app.panel_width
    );
    if app.route == "vms/new" {
        return String::new();
    }
    if app.route == "services" && app.detail.is_some() && app.form.is_none() {
        out.push_str(&service_panel(app));
        return out;
    }
    if app.route == "shells" && app.detail.is_some() && app.form.is_none() {
        out.push_str(&shell_panel(app));
        return out;
    }
    if let Some(form) = &app.form {
        // The panel head shows the form title, so the form has no heading of its own.
        let mut body = if form.return_route == "services" {
            crate::advanced::services::html(app, form)
        } else {
            form.html(app.busy, &app.field_errors)
        };
        if form.title == "Deploy MicroService" {
            // Where the service stands, above the form that deploys it.
            body = format!(
                "<div id=\"service-status\" class=\"service-status-block\">{}</div>{body}",
                render::deployment(app.service_deployment.as_ref())
            );
        }
        // A service's form says which service it is for.
        let meta = if app.route == "services" {
            app.detail
                .as_ref()
                .map_or_else(String::new, |data| escape(text(data, "name")))
        } else {
            String::new()
        };
        out.push_str(&format!(
            "<section role=\"region\" aria-label=\"Detail\" class=\"wasm-panel-body detail-body detail-form\">{}<div class=\"detail-content lined\">{body}</div></section>",
            panel_head(kind(app), t(&form.title), false, &meta, "")
        ));
        return out;
    }
    let Some(data) = &app.detail else {
        return out;
    };
    let mut title = ["name", "alias", "version", "registry", "provider"]
        .iter()
        .find_map(|key| data[key].as_str())
        .unwrap_or(t("Details"))
        .to_string();
    title.push_str(match text(data, "purpose") {
        "pool" => " · Pool",
        "queue" => " · Queue",
        _ => "",
    });
    let mut meta = String::new();
    if app.route == "vms" {
        let state = text(data, "state");
        let label = match state {
            "running" => "Running",
            "stopped" => "Stopped",
            "paused" => "Paused",
            "created" => "Created",
            "error" => "Error",
            "starting" => "Starting",
            "pausing" => "Pausing",
            "resuming" => "Resuming",
            _ => state,
        };
        meta = format!(
            "<span class=\"badge {}\">{}</span>",
            escape(state),
            escape(t(label))
        );
        if let Some(ip) = data["ipv4"].as_str().filter(|ip| !ip.is_empty()) {
            meta.push_str(&format!(
                "<span class=\"sep\">·</span><span class=\"mono\">{}</span>",
                escape(ip)
            ));
        }
    }
    out.push_str(&format!(
        "<section role=\"region\" aria-label=\"Detail\" class=\"wasm-panel-body detail-body\">{}",
        panel_head(
            kind(app),
            &title,
            false,
            &meta,
            &render::detail_actions(app, data)
        )
    ));
    if app.route == "vms" {
        out.push_str(&tab_list(
            "vm",
            "MicroVM detail sections",
            &VM_TABS,
            &app.detail_tab,
            &[],
        ));
        out.push_str("<div class=\"detail-content\">");
        let body = match app.detail_tab.as_str() {
            "env" => environment(app, data),
            "network" => network(data),
            "volumes" => crate::volumes::panel(app, data),
            "logs" => logs(app, data),
            _ => overview(data) + &crate::advanced::schedules::vm_schedules(app, data),
        };
        out.push_str(&tab_panel("vm", &app.detail_tab, &body));
    } else {
        out.push_str("<div class=\"detail-content lined\">");
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
                "<dl class=\"props\">{}{}{}</dl>",
                property("Account", text(data, "account")),
                copyable("Account ID", text(data, "accountId")),
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
            out.push_str(&section(title, "", &members));
        }
        if app.route == "shells" {
            let revisions = list(&data["revisions"])
                .iter()
                .map(|revision| format!("<li>v{}</li>", revision["version"]))
                .collect::<String>();
            out.push_str(&section("Revisions", "", &format!("<ul>{revisions}</ul>")));
        }
        if !related.is_empty() {
            out.push_str(&section(
                "MicroVM",
                "",
                &related
                    .iter()
                    .map(|vm| button(&vm.name, "navigate-vm", &vm.id))
                    .collect::<Vec<_>>()
                    .join(""),
            ));
        }
    }
    out.push_str(&format!(
        "<div id=\"detail-result\" class=\"detail-result\">{}</div></div></section>",
        app.detail_result
    ));
    out
}

/// The panel of a selected shell: the head, the Content and Revisions tabs. A form replaces it.
fn shell_panel(app: &App) -> String {
    let data = app.detail.as_ref().unwrap_or(&Value::Null);
    let tab = if app.detail_tab == "revisions" {
        "revisions"
    } else {
        "content"
    };
    let revisions = list(&data["revisions"]);
    let body = if tab == "revisions" {
        shell_revisions(data)
    } else {
        shell_content(data)
    };
    let meta = match data["latestVersion"].as_u64() {
        Some(version) => format!("<span class=\"mono\">v{version}</span>"),
        None => String::new(),
    };
    format!(
        "<section role=\"region\" aria-label=\"Detail\" class=\"wasm-panel-body detail-body shell-panel\">{}{}<div class=\"detail-content\">{}<div id=\"detail-result\" class=\"detail-result\">{}</div></div></section>",
        panel_head(
            kind(app),
            text(data, "name"),
            false,
            &meta,
            &render::detail_actions(app, data)
        ),
        tab_list(
            "shell",
            "Shell detail sections",
            &SHELL_TABS,
            tab,
            &[("revisions", revisions.len())]
        ),
        tab_panel("shell", tab, &body),
        app.detail_result
    )
}
/// The latest script in a code block, with a copy icon.
fn shell_content(data: &Value) -> String {
    let content = text(data, "latestContent");
    let description = text(data, "description");
    let mut out = String::new();
    if !description.is_empty() {
        out.push_str(&format!(
            "<p class=\"detail-note\">{}</p>",
            escape(description)
        ));
    }
    let copy = if content.is_empty() {
        String::new()
    } else {
        render::icon_button("Copy", "copy-text", content, render::Icon::Copy)
    };
    let script = if content.is_empty() {
        format!("<p class=\"detail-note\">{}</p>", t("No script yet."))
    } else {
        format!("<pre class=\"codeblock\">{}</pre>", escape(content))
    };
    out.push_str(&section("Latest revision", &copy, &script));
    out
}
/// Every revision, each with the button that shows it.
fn shell_revisions(data: &Value) -> String {
    let id = text(data, "id");
    let rows = list(&data["revisions"])
        .iter()
        .map(|revision| {
            format!(
                "<li><span class=\"mono detail-list-main\">v{}</span>{}</li>",
                revision["version"],
                render::button(
                    &format!("Revision {}", revision["version"]),
                    "shell-revision",
                    &format!("{id}/{}", text(revision, "id"))
                )
            )
        })
        .collect::<String>();
    if rows.is_empty() {
        format!("<p class=\"detail-note\">{}</p>", t("No revisions yet."))
    } else {
        section(
            "Revisions",
            "",
            &format!("<ul class=\"detail-list\">{rows}</ul>"),
        )
    }
}

/// The panel of a selected MicroService: the head, four tabs and the open tab. A form replaces it.
fn service_panel(app: &App) -> String {
    let data = app.detail.as_ref().unwrap_or(&Value::Null);
    let tab = SERVICE_TABS
        .iter()
        .map(|(id, _)| *id)
        .find(|id| *id == app.detail_tab)
        .unwrap_or("overview");
    let body = match tab {
        "instances" => service_instances(app),
        "releases" => service_releases(app),
        "settings" => service_settings(app, data),
        _ => service_overview(app, data),
    };
    format!(
        "<section role=\"region\" aria-label=\"Detail\" class=\"wasm-panel-body detail-body service-panel\">{}{}<div class=\"detail-content\">{}</div></section>",
        panel_head(
            kind(app),
            text(data, "name"),
            false,
            &service_meta(app),
            &render::detail_actions(app, data)
        ),
        tab_list(
            "service",
            "MicroService detail sections",
            &SERVICE_TABS,
            tab,
            &service_counts(app)
        ),
        tab_panel("service", tab, &body)
    )
}

/// The head's line: the phase, the active release and the strategy of the last update. Each part
/// is left out until the host has said it.
fn service_meta(app: &App) -> String {
    let Some(deployment) = &app.service_deployment else {
        return String::new();
    };
    if deployment.is_null() {
        return render::status_dot("off", t("Not deployed"));
    }
    let phase = text(deployment, "phase");
    let mut out = render::status_dot(render::phase_tone(phase), &render::phase_label(phase));
    let id = text(deployment, "id");
    if !id.is_empty() {
        out.push_str(&format!(
            "<span class=\"sep\">·</span><span class=\"mono\" title=\"{}\">{}</span>",
            escape(id),
            escape(&crate::deploy::short(id))
        ));
    }
    let strategy = match text(&deployment["update"], "strategy") {
        "blue_green" => "Blue-green",
        "rolling" => "Rolling",
        _ => "",
    };
    if !strategy.is_empty() {
        out.push_str(&format!(
            "<span class=\"sep\">·</span><span>{}</span>",
            t(strategy)
        ));
    }
    out
}

/// The count of each tab that has one: the VMs of the deployment, once the host has said.
fn service_counts(app: &App) -> Vec<(&'static str, usize)> {
    app.service_deployment
        .as_ref()
        .filter(|deployment| deployment.is_object())
        .map_or_else(Vec::new, |deployment| {
            vec![("instances", list(&deployment["instances"]).len())]
        })
}

/// A label and a value that is markup, already escaped (chips).
fn property_html(label: &str, value: &str) -> String {
    format!(
        "<div class=\"wasm-property\"><dt>{}</dt><dd>{value}</dd></div>",
        escape(t(label))
    )
}

/// The Queues, the S3 connection and the Secrets the service is linked to, as chips. A name the
/// screen has not loaded yet is the alias of the Queue or the short id of the connection.
fn connections(app: &App, data: &Value) -> String {
    let name_of = |path: &str, id: &str| {
        app.cache
            .get(path)
            .and_then(|rows| list(rows).iter().find(|row| row["id"] == id))
            .map(|row| text(row, "name"))
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    };
    let mut chips = Vec::new();
    for (alias, binding) in data["queues"].as_object().into_iter().flatten() {
        let name =
            name_of("/api/queues", text(binding, "queueId")).unwrap_or_else(|| alias.clone());
        chips.push(format!("Queue {name}"));
    }
    let storage = text(data, "storageConnectionId");
    if !storage.is_empty() {
        let name =
            name_of("/api/pipe/storage", storage).unwrap_or_else(|| crate::deploy::short(storage));
        chips.push(format!("S3 {name}"));
    }
    let secrets = data["secrets"]
        .as_object()
        .map_or(0, |secrets| secrets.len());
    if secrets > 0 {
        chips.push(format!("Secret {secrets}"));
    }
    render::chips(&chips)
}

/// What the service is doing now (with the deployment log behind an icon), what it is made of,
/// and its latest releases.
fn service_overview(app: &App, data: &Value) -> String {
    let deployment = app.service_deployment.as_ref();
    let status = render::deployment_summary(deployment);
    let mut out = if deployment.is_some_and(Value::is_object) {
        // The log is a block under the line, shut until the icon opens it.
        let log = render::deployment_log(deployment);
        let open = crate::deploy::log_open(app) && !log.is_empty();
        let mut toggle = if log.is_empty() {
            String::new()
        } else {
            render::icon_toggle(
                "Deployment log",
                "deploy-log",
                open,
                "service-log",
                render::Icon::Log,
            )
        };
        // A queued deployment can be cancelled before it starts.
        if deployment.is_some_and(crate::deploy::cancellable) {
            toggle.insert_str(0, &crate::deploy::cancel_button(app));
        }
        let log = if open {
            format!("<div id=\"service-log\">{log}</div>")
        } else {
            String::new()
        };
        section(
            "Deployment",
            &toggle,
            &format!("<div id=\"service-status\">{status}</div>{log}"),
        )
    } else {
        // Loading, or never deployed: the line alone.
        format!("<div id=\"service-status\" class=\"service-status-block\">{status}</div>")
    };
    let raw = |key: &str| match &data[key] {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let or_dash = |value: String| {
        if value.is_empty() {
            "—".into()
        } else {
            value
        }
    };
    let size = if raw("cpu").is_empty() && raw("ram").is_empty() {
        "—".into()
    } else {
        crate::forms::size_label(&raw("cpu"), &raw("ram"))
    };
    out.push_str(&section(
        "Properties",
        "",
        &format!(
            "<dl class=\"props\">{}{}{}{}{}{}</dl>",
            copyable("Image", &or_dash(raw("image"))),
            property("Port", &or_dash(raw("port"))),
            property("Replicas", &or_dash(raw("replicas"))),
            property("Health path", &or_dash(raw("healthPath"))),
            property("Size", &size),
            property_html("Connections", &connections(app, data))
        ),
    ));
    let releases = crate::deploy::recent_releases(app);
    if !releases.is_empty() {
        out.push_str(&section("Recent releases", "", &releases));
    }
    out
}

/// The VMs that run the service.
fn service_instances(app: &App) -> String {
    let table = render::deployment_instances(app.service_deployment.as_ref());
    let body = if table.is_empty() {
        format!(
            "<p class=\"detail-note\">{}</p>",
            t(if app.service_deployment.is_some() {
                "No instances are running."
            } else {
                "Loading…"
            })
        )
    } else {
        table
    };
    // Logs, events, alerts and OTLP export belong to the instances that produce them.
    format!(
        "<div id=\"service-instances\">{body}</div>{}",
        crate::observability::html(app)
    )
}

/// Update, roll back and the releases. The polls and the release list redraw `#deploy-panel` alone.
fn service_releases(app: &App) -> String {
    let body = if app.service_deployment.is_some() {
        crate::deploy::html(app)
    } else {
        format!("<p role=\"status\">{}</p>", t("Loading…"))
    };
    format!("<div id=\"deploy-panel\">{body}</div>")
}

/// Edit, delete and, for a service with a domain, how to point its DNS at the tunnel.
fn service_settings(app: &App, data: &Value) -> String {
    let id = text(data, "id");
    let mut out = section(
        "Configuration",
        &render::icon_button(
            "Edit configuration",
            "resource-edit",
            id,
            render::Icon::Edit,
        ),
        &format!(
            "<p class=\"detail-note\">{}</p>",
            t("Change the image, port, size or connections. Changes apply on the next deployment.")
        ),
    );
    out.push_str(&section(
        "Delete",
        &render::icon_request(
            "Delete",
            &format!("/api/micro-services/{}", api::encode(id)),
        ),
        &format!(
            "<p class=\"detail-note\">{}</p>",
            t("Remove this MicroService's configuration.")
        ),
    ));
    if !text(data, "domain").is_empty() {
        let note = match app.cache.get("/api/pipe") {
            Some(pipe) if !text(&pipe["config"]["tunnel"], "id").is_empty() => format!(
                "{} <code>{}.cfargotunnel.com</code>. {}",
                t("For public HTTPS, point this domain's DNS CNAME to"),
                escape(text(&pipe["config"]["tunnel"], "id")),
                t("Keep Cloudflare proxy enabled.")
            ),
            _ => format!(
                "{} <a href=\"#/pipe/named-tunnel\">Pipe Tunnel</a>.",
                t("For public HTTPS, connect your domain in")
            ),
        };
        out.push_str(&section(
            "Domain",
            "",
            &format!("<p class=\"detail-note\">{note}</p>"),
        ));
    }
    out
}

// Polling replaces these blocks. Keep the focus on the control the person is using.
pub fn set_service_html(id: &str, html: &str) {
    let Some(root) = element(id) else {
        return;
    };
    let current = root.inner_html();
    if current == html {
        return;
    }
    // Normalize browser serialization, including boolean attributes like disabled. An open More
    // menu is the person's state, not the markup's: the new markup is compared with it open, so a
    // poll that changed nothing replaces nothing (the menu's focused button stays the same node).
    if let Ok(parsed) = document().create_element("div") {
        parsed.set_inner_html(html);
        if root
            .query_selector("#vm-more-actions[open]")
            .ok()
            .flatten()
            .is_some()
            && let Ok(Some(menu)) = parsed.query_selector("#vm-more-actions")
        {
            let _ = menu.set_attribute("open", "");
        }
        if current == parsed.inner_html() {
            return;
        }
    }
    let focused = document()
        .active_element()
        .filter(|el| root.contains(Some(el)))
        .map(|el| el.id());
    root.set_inner_html(html);
    if let Some(control) = focused
        .and_then(|id| element(&id))
        .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
    {
        let options = web_sys::FocusOptions::new();
        options.set_prevent_scroll(true);
        let _ = control.focus_with_options(&options);
    }
}
pub fn overview(data: &Value) -> String {
    let mut props = String::from("<dl class=\"props\">");
    // The last column marks the values worth copying.
    for (label, key, copy) in [
        ("ID", "id", true),
        ("Name", "name", false),
        ("State", "state", false),
        ("Image", "template", true),
        ("Version", "templateVersion", false),
        ("MicroNetwork", "microNetworkId", false),
        ("Hostname", "hostname", true),
        ("SSH host key", "sshHostFingerprint", true),
        ("MicroStorage", "storageRoot", false),
    ] {
        props.push_str(&property_row(label, text(data, key), copy));
    }
    for (label, key) in [
        ("CPU", "cpu"),
        ("Memory (MiB)", "ram"),
        ("Disk (GB)", "diskGb"),
    ] {
        props.push_str(&property(label, &data[key].to_string()));
    }
    props.push_str("</dl>");
    let mut out = section("Properties", "", &props);
    if ["paused", "pausing", "resuming"].contains(&text(data, "state")) {
        out.push_str(&format!(
            "<p>{}</p>",
            t("Memory stays allocated while the VM is paused. Resume to reconnect.")
        ));
    }
    out.push_str(&section("Startup", "", &timeline(data)));
    if data["reconciliation"].is_object() {
        out.push_str(&section(
            "API-STATUS",
            "",
            &reconciliation(&data["reconciliation"]),
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
        "",
        &format!(
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
        "<dl class=\"props\">{}{}{}{}</dl>",
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
    let mut out = "<dl class=\"props\">".to_string();
    for (label, key, copy) in [
        ("IPv4", "ipv4", true),
        ("IPv6", "ipv6", true),
        ("MAC", "mac", false),
        ("Hostname", "hostname", true),
        ("Network", "microNetworkId", false),
        ("Egress policy", "egressPolicy", false),
    ] {
        out.push_str(&property_row(label, text(data, key), copy));
    }
    out.push_str("</dl>");
    out.push_str(&section(
        "Port forwarding",
        "",
        &render::properties(&json!({"portForwards":data["portForwards"]})),
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
    out.push_str("<dl class=\"props\">");
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
    let auto_editing = crate::document().active_element().is_some_and(|element| {
        element.id().starts_with("deploy-auto-")
            || element.id().starts_with("deploy-ci-")
            || element.id() == "service-observe-endpoint"
    });
    if auto_editing
        || state.busy
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
        let status = deploy_form.then(|| render::deployment(state.service_deployment.as_ref()));
        drop(state);
        if service {
            crate::deploy::refresh(&app);
        }
        if let Some(status) = status {
            set_service_html("service-status", &status);
            crate::advanced::services::sync(&app);
        } else {
            crate::render_detail(&app);
        }
        if service {
            crate::deploy::refresh_automation(&app);
        }
        if logs {
            load_log(app);
        }
    });
}
pub fn tab_key(app: &Shared, event: &KeyboardEvent) -> bool {
    let Some(target) = event
        .target()
        .and_then(|target| target.dyn_into::<Element>().ok())
    else {
        return false;
    };
    let Some((prefix, tabs)) = panel_tabs(&target.id()) else {
        return false;
    };
    if target.get_attribute("role").as_deref() != Some("tab") {
        return false;
    }
    let index = tabs
        .iter()
        .position(|(id, _)| target.id() == format!("{prefix}-tab-{id}"))
        .unwrap_or(0);
    let next = match event.key().as_str() {
        "ArrowRight" => (index + 1) % tabs.len(),
        "ArrowLeft" => (index + tabs.len() - 1) % tabs.len(),
        "Home" => 0,
        "End" => tabs.len() - 1,
        _ => return false,
    };
    event.prevent_default();
    dispatch(app, "detail-tab", tabs[next].0);
    if let Some(control) = element(&format!("{prefix}-tab-{}", tabs[next].0))
        .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
    {
        let _ = control.focus();
    }
    true
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
            if id == "volumes" {
                crate::volumes::load(app);
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
    if query.is_empty() || crate::model::lowercase(t("New MicroVM")).contains(&query) {
        entries.push((t("New MicroVM").to_string(), "palette-new", String::new()));
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
        "ArrowLeft" => (width + 24).min(panel_max_width()),
        "ArrowRight" => width.saturating_sub(24).max(PANEL_MIN_WIDTH),
        "Home" => PANEL_MIN_WIDTH,
        "End" => panel_max_width(),
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
            reset.borrow_mut().panel_width = PANEL_DEFAULT_WIDTH;
            if let Ok(Some(storage)) = window().local_storage() {
                let _ = storage.set_item(
                    "firecrab.detailPanelWidth",
                    &PANEL_DEFAULT_WIDTH.to_string(),
                );
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
    let max = panel_max_width() as i32;
    let state = app.borrow();
    let width = (state.panel_drag_width as i32 + state.panel_drag_x - x)
        .clamp(PANEL_MIN_WIDTH as i32, max) as u32;
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

#[cfg(test)]
mod panel_visibility_tests {
    use super::*;
    use serde_json::json;

    fn app(route: &str) -> App {
        App {
            route: route.into(),
            panel_open: true,
            ..Default::default()
        }
    }

    #[test]
    fn hidden_without_selection_or_form() {
        assert!(!panel_visible(&app("vms")));
        assert!(!panel_visible(&app("pipe")));
    }

    #[test]
    fn shown_for_a_selection_or_a_form() {
        let mut selected = app("cluster");
        selected.detail = Some(json!({"id": "h1"}));
        assert!(panel_visible(&selected));
        let mut editing = app("networks");
        editing.form = Some(crate::forms::vm_create(&Default::default()));
        assert!(panel_visible(&editing));
    }

    #[test]
    fn never_shown_on_workspace_or_page_routes() {
        for route in NO_PANEL_ROUTES {
            let mut state = app(route);
            state.detail = Some(json!({"id": "x"}));
            assert!(!panel_visible(&state), "{route}");
        }
    }
}

#[cfg(test)]
mod panel_view_tests {
    use super::*;
    use serde_json::json;

    fn selected(route: &str, id: &str, tab: &str) -> App {
        App {
            route: route.into(),
            panel_open: true,
            detail: Some(json!({"id": id})),
            detail_tab: tab.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_poll_of_the_same_view_has_the_same_key() {
        let mut poll = selected("vms", "vm-1", "overview");
        poll.detail = Some(json!({"id": "vm-1", "state": "running", "cpuUsagePercent": 12}));
        assert_eq!(
            panel_view(&selected("vms", "vm-1", "overview")),
            panel_view(&poll)
        );
    }

    #[test]
    fn another_item_tab_screen_or_form_is_another_view() {
        let base = panel_view(&selected("vms", "vm-1", "overview"));
        assert_ne!(base, panel_view(&selected("vms", "vm-2", "overview")));
        assert_ne!(base, panel_view(&selected("vms", "vm-1", "env")));
        assert_ne!(base, panel_view(&selected("networks", "vm-1", "overview")));
        let mut editing = selected("vms", "vm-1", "overview");
        editing.form = Some(crate::forms::vm_create(&Default::default()));
        assert_ne!(base, panel_view(&editing));
    }

    #[test]
    fn items_without_an_id_are_told_apart_by_their_alias() {
        let mut ubuntu = selected("images", "", "overview");
        ubuntu.detail = Some(json!({"alias": "ubuntu"}));
        let mut debian = selected("images", "", "overview");
        debian.detail = Some(json!({"alias": "debian"}));
        assert_ne!(panel_view(&ubuntu), panel_view(&debian));
    }
}

#[cfg(test)]
mod property_copy_tests {
    use super::*;

    #[test]
    fn a_plain_property_has_no_button() {
        let html = property("State", "running");
        assert_eq!(
            html,
            "<div class=\"wasm-property\"><dt>State</dt><dd><span class=\"prop-value\" title=\"running\">running</span></dd></div>"
        );
    }

    #[test]
    fn a_copyable_property_has_an_icon_right_after_the_value() {
        let html = copyable("ID", "vm 1<2>");
        assert!(
            html.contains("<span class=\"prop-value\" title=\"vm 1&lt;2&gt;\">vm 1&lt;2&gt;</span><button type=\"button\" class=\"icon-btn prop-copy\" aria-label=\"Copy ID\" title=\"Copy ID\" data-action=\"copy-text\" data-id=\"vm 1&lt;2&gt;\"><svg"),
            "{html}"
        );
        assert!(!html.contains(">Copy<"), "{html}");
        assert!(html.ends_with("</svg></button></dd></div>"), "{html}");
    }

    #[test]
    fn an_empty_value_has_nothing_to_copy() {
        assert!(!copyable("Hostname", "").contains("<button"));
    }

    #[test]
    fn the_copy_name_reads_in_korean() {
        crate::i18n::set("ko");
        assert_eq!(crate::i18n::copy_name("ID"), "ID 복사");
        assert_eq!(crate::i18n::copy_name("Image"), "이미지 복사");
        assert!(copyable("ID", "x").contains("aria-label=\"ID 복사\""));
        crate::i18n::set("en");
        assert_eq!(crate::i18n::copy_name("Image"), "Copy Image");
    }

    fn copy_labels(html: &str) -> Vec<String> {
        html.split("aria-label=\"")
            .skip(1)
            .filter_map(|part| part.split('"').next())
            .filter(|label| label.starts_with("Copy "))
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn the_overview_copies_only_ids_images_hosts_and_keys() {
        let data = json!({
            "id": "vm-1", "name": "api", "state": "running", "template": "ubuntu-24.04",
            "templateVersion": "2026.09.1", "microNetworkId": "default", "hostname": "api",
            "sshHostFingerprint": "SHA256:abc", "storageRoot": "default",
            "cpu": 2, "ram": 1024, "diskGb": 8
        });
        assert_eq!(
            copy_labels(&overview(&data)),
            [
                "Copy ID",
                "Copy Image",
                "Copy Hostname",
                "Copy SSH host key"
            ]
        );
        // A VM that has no host key yet has no icon for it.
        let fresh = json!({"id": "vm-1", "name": "api", "state": "stopped", "template": "alpine"});
        assert_eq!(copy_labels(&overview(&fresh)), ["Copy ID", "Copy Image"]);
    }

    #[test]
    fn the_network_tab_copies_addresses_and_the_hostname() {
        let data = json!({
            "ipv4": "10.42.0.2", "ipv6": "fd42::2", "mac": "02:00:00:00:00:01",
            "hostname": "api", "microNetworkId": "default", "egressPolicy": "internet"
        });
        assert_eq!(
            copy_labels(&network(&data)),
            ["Copy IPv4", "Copy IPv6", "Copy Hostname"]
        );
    }
}

#[cfg(test)]
mod panel_markup_tests {
    use super::*;

    #[test]
    fn head_has_kind_title_meta_and_the_close_button_after_the_actions() {
        let html = panel_head(
            "Queue",
            "orders <1>",
            true,
            "<span>ready</span>",
            "<i>A</i>",
        );
        assert!(html.starts_with(
            "<header class=\"detail-head\"><div class=\"detail-top\"><span class=\"detail-kind\">Queue</span><div class=\"detail-top-actions\"><i>A</i><button type=\"button\" class=\"icon-btn\" aria-label=\"Close detail\""
        ));
        assert!(html.contains(
            "<h2 class=\"detail-title mono\" title=\"orders &lt;1&gt;\">orders &lt;1&gt;</h2>"
        ));
        assert!(html.ends_with("<div class=\"detail-meta\"><span>ready</span></div></header>"));
    }

    #[test]
    fn head_title_is_not_mono_unless_asked() {
        let html = panel_head("MicroVM", "api", false, "", "");
        assert!(html.contains("<h2 class=\"detail-title\" title=\"api\">api</h2>"));
        assert!(html.ends_with("<div class=\"detail-meta\"></div></header>"));
    }

    fn editing(route: &str) -> App {
        App {
            route: route.into(),
            panel_open: true,
            form: Some(crate::forms::vm_create(&Default::default())),
            ..Default::default()
        }
    }

    #[test]
    fn a_form_panel_has_one_heading() {
        let html = panel(&editing("networks"));
        assert_eq!(html.matches("<h2").count(), 1, "{html}");
        assert!(html.contains("class=\"detail-title\""));
    }

    #[test]
    fn a_service_form_panel_has_the_panel_head_and_names_the_service() {
        let service = json!({"id": "s1", "name": "orders", "port": 8080});
        let mut deploy = App {
            route: "services".into(),
            panel_open: true,
            detail: Some(service.clone()),
            form: crate::forms::resource_form("service-deploy", &service, "s1"),
            ..Default::default()
        };
        // Every form in the panel has the panel head: the kind, the form's title, the service.
        for (form, title) in [
            ("service-deploy", "Deploy MicroService"),
            ("services", "Edit MicroService"),
        ] {
            deploy.form = crate::forms::resource_form(form, &service, "s1");
            let html = panel(&deploy);
            assert_eq!(html.matches("<h2").count(), 1, "{html}");
            assert!(
                html.contains(&format!(
                    "<h2 class=\"detail-title\" title=\"{title}\">{title}</h2>"
                )),
                "{html}"
            );
            assert!(
                html.contains("<span class=\"detail-kind\">MicroService</span>"),
                "{html}"
            );
            assert!(
                html.contains("<div class=\"detail-meta\">orders</div>"),
                "{html}"
            );
            // Not the tabs, and no second heading under the head.
            assert!(
                !html.contains("role=\"tablist\"") && !html.contains("service-form-title"),
                "{html}"
            );
        }
    }

    #[test]
    fn every_form_footer_has_a_ghost_cancel_before_the_submit_button() {
        let plain = panel(&editing("networks"));
        let secret = crate::forms::Form::new("New secret", "/x".into(), "POST", vec![], "secrets")
            .html(false, &Default::default());
        for html in [plain, secret] {
            let footer = html.split("form-footer\">").nth(1).expect("footer");
            let cancel = footer
                .find("<button class=\"btn ghost\" type=\"button\" data-action=\"cancel\">Cancel</button>")
                .expect("ghost cancel");
            assert!(cancel < footer.find("type=\"submit\"").expect("submit"));
        }
    }

    #[test]
    fn section_has_a_title_row_with_an_aside_and_the_body() {
        assert_eq!(
            section("Properties", "<i>x</i>", "<p>b</p>"),
            "<section class=\"detail-sec\" aria-label=\"Properties\"><div class=\"detail-sec-head\"><h3>Properties</h3><i>x</i></div><p>b</p></section>"
        );
    }
}

#[cfg(test)]
mod service_panel_tests {
    use super::*;
    use serde_json::json;

    fn service() -> Value {
        json!({"id": "s1", "name": "orders", "image": "ghcr.io/team/orders:1.0", "port": 8080, "replicas": 3,
            "cpu": 2, "ram": 1024, "healthPath": "/health", "domain": "orders.example.com",
            "env": {"PRIVATE": "never-shown"}, "secrets": {"A": "x", "B": "y"}})
    }

    fn running() -> Value {
        json!({"id": "dep-12345678", "serviceId": "s1", "phase": "running", "domainRouteOwned": true,
            "spec": {"domain": "orders.example.com", "image": "orders:1.0"},
            "instances": [{"vmId": "vm-1", "ipv4": "172.30.0.2", "hostPort": 30100, "healthy": true}],
            "log": ["All replicas answered"], "error": null,
            "update": {"strategy": "blue_green", "completed": true}})
    }

    fn open(tab: &str) -> App {
        App {
            route: "services".into(),
            panel_open: true,
            detail: Some(service()),
            detail_tab: tab.into(),
            service_deployment: Some(running()),
            deploy: crate::deploy::State::loaded(
                "s1",
                json!([
                    {"id": "dep-12345678", "spec": {"image": "orders:1.0"}, "digest": "sha256:aa"},
                    {"id": "dep-00000002", "spec": {"image": "orders:0.9"}, "digest": "sha256:bb"}
                ]),
            ),
            ..Default::default()
        }
    }

    #[test]
    fn the_panel_has_a_head_and_four_tabs_and_no_disclosures() {
        let html = panel(&open("overview"));
        assert_eq!(html.matches("<h2").count(), 1, "{html}");
        assert!(
            html.contains("<span class=\"detail-kind\">MicroService</span>"),
            "{html}"
        );
        assert!(
            html.contains("<h2 class=\"detail-title\" title=\"orders\">orders</h2>"),
            "{html}"
        );
        assert!(html.contains("role=\"tablist\" class=\"tabs detail-tabs\" aria-label=\"MicroService detail sections\""), "{html}");
        for (id, name) in [
            ("overview", "Overview"),
            ("instances", "Instances"),
            ("releases", "Releases"),
            ("settings", "Settings"),
        ] {
            let selected = id == "overview";
            // Only the Instances tab has a count; it is the number of VMs, in the tab's own <em>.
            let count = if id == "instances" { " <em>1</em>" } else { "" };
            assert!(html.contains(&format!("aria-selected=\"{selected}\" aria-controls=\"service-tab-panel\" id=\"service-tab-{id}\" tabindex=\"{}\" data-action=\"detail-tab\" data-id=\"{id}\">{name}{count}</button>", if selected { 0 } else { -1 })), "{id}: {html}");
        }
        assert!(html.contains("<div role=\"tabpanel\" id=\"service-tab-panel\" aria-labelledby=\"service-tab-overview\">"), "{html}");
        for gone in [
            "service-disclosure",
            "service-panel-head",
            "service-panel-body",
            "service-form-title",
        ] {
            assert!(!html.contains(gone), "{gone}: {html}");
        }
        // The one fold left is the More menu in the head.
        assert_eq!(html.matches("<details").count(), 1, "{html}");
        assert!(html.contains("<details id=\"vm-more-actions\""), "{html}");
    }

    #[test]
    fn the_head_line_reads_phase_release_and_strategy() {
        let html = panel(&open("overview"));
        assert!(html.contains("<div class=\"detail-meta\"><span class=\"status\" data-tone=\"ok\"><span class=\"status-dot\" aria-hidden=\"true\"></span>Running</span><span class=\"sep\">·</span><span class=\"mono\" title=\"dep-12345678\">dep-1234</span><span class=\"sep\">·</span><span>Blue-green</span></div>"), "{html}");
        // No update has run: the strategy is not known and is left out. Not deployed: the phase only.
        let mut first = open("overview");
        first.service_deployment.as_mut().unwrap()["update"] = json!(null);
        assert!(panel(&first).contains("dep-1234</span></div>"));
        let mut none = open("overview");
        none.service_deployment = Some(json!(null));
        assert!(panel(&none).contains("<div class=\"detail-meta\"><span class=\"status\" data-tone=\"off\"><span class=\"status-dot\" aria-hidden=\"true\"></span>Not deployed</span></div>"));
        // Still loading: nothing to say yet.
        let mut loading = open("overview");
        loading.service_deployment = None;
        assert!(panel(&loading).contains("<div class=\"detail-meta\"></div>"));
    }

    #[test]
    fn the_overview_has_the_deployment_the_properties_and_the_recent_releases() {
        let html = panel(&open("overview"));
        // The deployment is the first section; its status is the one status of the Overview.
        assert!(html.contains("<section class=\"detail-sec\" aria-label=\"Deployment\"><div class=\"detail-sec-head\"><h3>Deployment</h3>"), "{html}");
        assert!(
            html.contains("<div id=\"service-status\"><div class=\"service-runtime\">"),
            "{html}"
        );
        assert_eq!(html.matches("role=\"status\"").count(), 1, "{html}");
        let at = |needle: &str| {
            html.find(needle)
                .unwrap_or_else(|| panic!("{needle}: {html}"))
        };
        assert!(at("aria-label=\"Deployment\"") < at("id=\"service-status\""));
        assert!(at("id=\"service-status\"") < at("aria-label=\"Properties\""));
        assert!(at("aria-label=\"Properties\"") < at("aria-label=\"Recent releases\""));
        // The properties: image (with a copy icon), port, replicas, health path, size and connections.
        assert!(html.contains("<dt>Image</dt><dd><span class=\"prop-value\" title=\"ghcr.io/team/orders:1.0\">ghcr.io/team/orders:1.0</span><button type=\"button\" class=\"icon-btn prop-copy\" aria-label=\"Copy Image\""), "{html}");
        let properties = html
            .split("aria-label=\"Properties\"")
            .nth(1)
            .unwrap()
            .split("</section>")
            .next()
            .unwrap();
        let labels: Vec<_> = properties
            .split("<dt>")
            .skip(1)
            .map(|row| row.split("</dt>").next().unwrap())
            .collect();
        assert_eq!(
            labels,
            [
                "Image",
                "Port",
                "Replicas",
                "Health path",
                "Size",
                "Connections"
            ],
            "{properties}"
        );
        for (label, value) in [
            ("Port", "8080"),
            ("Replicas", "3"),
            ("Health path", "/health"),
            ("Size", "2 vCPU · 1 GiB"),
        ] {
            assert!(
                properties.contains(&format!(
                    "<dt>{label}</dt><dd><span class=\"prop-value\" title=\"{value}\">"
                )),
                "{label}: {properties}"
            );
        }
        assert!(
            !html.contains("Secret references")
                && !html.contains("Memory (MiB)")
                && !html.contains("<dt>CPU</dt>"),
            "{html}"
        );
        // The log is behind the icon, and shut: no inline log, nothing else of it in the Overview.
        assert!(
            !html.contains("wasm-log") && !html.contains("id=\"service-log\""),
            "{html}"
        );
        // Values of the environment are never part of the overview.
        assert!(!html.contains("never-shown"), "{html}");
        // The instances, the release tab and the settings are not here.
        assert!(
            !html.contains("service-instances") && !html.contains("id=\"deploy-panel\""),
            "{html}"
        );
    }

    #[test]
    fn the_log_opens_from_an_icon_in_the_deployment_section() {
        let mut state = open("overview");
        let html = panel(&state);
        let head = html
            .split("aria-label=\"Deployment\"")
            .nth(1)
            .unwrap()
            .split("</div>")
            .next()
            .unwrap();
        assert!(head.contains("aria-label=\"Deployment log\" title=\"Deployment log\" aria-expanded=\"false\" aria-controls=\"service-log\" data-action=\"deploy-log\""), "{head}");
        // Open: the lines are a block under the line of the deployment, in the same section.
        state.deploy = crate::deploy::State::loaded("s1", json!([])).with_log_open();
        let html = panel(&state);
        assert!(html.contains("aria-expanded=\"true\""), "{html}");
        let section = html
            .split("aria-label=\"Deployment\"")
            .nth(1)
            .unwrap()
            .split("</section>")
            .next()
            .unwrap();
        assert!(
            section.contains(
                "<div id=\"service-log\"><pre class=\"wasm-log\">All replicas answered</pre></div>"
            ),
            "{section}"
        );
        assert!(
            section.find("service-status").unwrap() < section.find("id=\"service-log\"").unwrap()
        );
        assert_eq!(html.matches("wasm-log").count(), 1, "{html}");
        // The state belongs to the service the panel shows.
        state.deploy = crate::deploy::State::loaded("other", json!([])).with_log_open();
        assert!(!panel(&state).contains("id=\"service-log\""));
        // No lines, no icon and no block.
        let mut quiet = open("overview");
        quiet.deploy = crate::deploy::State::loaded("s1", json!([])).with_log_open();
        quiet.service_deployment.as_mut().unwrap()["log"] = json!([]);
        let html = panel(&quiet);
        assert!(
            !html.contains("Deployment log") && !html.contains("service-log"),
            "{html}"
        );
    }

    #[test]
    fn without_a_deployment_the_overview_has_only_the_status_line() {
        let mut state = open("overview");
        state.service_deployment = Some(json!(null));
        let html = panel(&state);
        assert!(html.contains("<div id=\"service-status\" class=\"service-status-block\"><p class=\"service-state\" role=\"status\">"), "{html}");
        assert!(
            !html.contains("aria-label=\"Deployment\"") && !html.contains("Deployment log"),
            "{html}"
        );
        state.service_deployment = None;
        let html = panel(&state);
        assert!(html.contains("<div id=\"service-status\" class=\"service-status-block\"><p role=\"status\">Loading…</p></div>"), "{html}");
        // No count on the Instances tab while nothing is known, and none for a service that never ran.
        assert!(
            html.contains("data-id=\"instances\">Instances</button>"),
            "{html}"
        );
        state.service_deployment = Some(json!(null));
        assert!(panel(&state).contains("data-id=\"instances\">Instances</button>"));
        // A deployment with no VMs is a count of zero.
        state.service_deployment = Some(json!({"phase": "stopped", "instances": []}));
        assert!(panel(&state).contains("data-id=\"instances\">Instances <em>0</em></button>"));
    }

    #[test]
    fn the_connections_are_chips_with_names_from_what_the_screen_loaded() {
        let mut data = service();
        data["queues"] = json!({"NOTIFICATIONS": {"queueId": "q-1", "access": "consume"}, "AUDIT": {"queueId": "q-unknown", "access": "publish"}});
        data["storageConnectionId"] = json!("abcdef12-0000-4000-8000-000000000000");
        let mut state = open("overview");
        state.detail = Some(data.clone());
        // Still loading: the alias and the short id stand in.
        let waiting = panel(&state);
        let chips = |html: &str| {
            html.split("<dt>Connections</dt><dd>")
                .nth(1)
                .unwrap()
                .split("</dd>")
                .next()
                .unwrap()
                .to_owned()
        };
        assert_eq!(
            chips(&waiting),
            "<div class=\"chips\"><span class=\"chip\">Queue AUDIT</span><span class=\"chip\">Queue NOTIFICATIONS</span><span class=\"chip\">S3 abcdef12</span><span class=\"chip\">Secret 2</span></div>"
        );
        state.cache.insert(
            "/api/queues".into(),
            json!([{"id": "q-1", "name": "notifications"}]),
        );
        state.cache.insert(
            "/api/pipe/storage".into(),
            json!([{"id": "abcdef12-0000-4000-8000-000000000000", "name": "Reports"}]),
        );
        assert_eq!(
            chips(&panel(&state)),
            "<div class=\"chips\"><span class=\"chip\">Queue AUDIT</span><span class=\"chip\">Queue notifications</span><span class=\"chip\">S3 Reports</span><span class=\"chip\">Secret 2</span></div>"
        );
        // Nothing linked: a dash.
        data["queues"] = json!(null);
        data["storageConnectionId"] = json!(null);
        data["secrets"] = json!({});
        state.detail = Some(data);
        assert_eq!(chips(&panel(&state)), "<span class=\"none\">—</span>");
        // Names are text, not markup.
        let mut risky = service();
        risky["queues"] = json!({"<b>": {"queueId": "x"}});
        state.detail = Some(risky);
        assert!(chips(&panel(&state)).contains("Queue &lt;b&gt;"));
    }

    #[test]
    fn the_size_is_one_row_and_a_missing_one_is_a_dash() {
        let mut state = open("overview");
        let mut data = service();
        data["cpu"] = json!(1);
        data["ram"] = json!(512);
        state.detail = Some(data.clone());
        assert!(
            panel(&state).contains(
                "<dt>Size</dt><dd><span class=\"prop-value\" title=\"1 vCPU · 512 MiB\">"
            )
        );
        data["cpu"] = json!(null);
        data["ram"] = json!(null);
        state.detail = Some(data);
        assert!(panel(&state).contains("<dt>Size</dt><dd><span class=\"prop-value\" title=\"—\">"));
    }

    #[test]
    fn recent_releases_show_three_and_archived_ones_offer_roll_back() {
        let mut state = open("overview");
        state.deploy = crate::deploy::State::loaded(
            "s1",
            json!([
                {"id": "dep-12345678", "spec": {"image": "orders:1.0"}, "digest": "sha256:aa"},
                {"id": "dep-00000002", "spec": {"image": "orders:0.9"}, "digest": "sha256:bb"},
                {"id": "dep-00000003", "spec": {"image": "orders:0.8"}, "digest": "sha256:cc"},
                {"id": "dep-00000004", "spec": {"image": "orders:0.7"}, "digest": "sha256:dd"}
            ]),
        );
        let html = panel(&state);
        let (_, section) = html
            .split_once("aria-label=\"Recent releases\"")
            .expect("section");
        let section = section.split("</section>").next().unwrap();
        assert_eq!(section.matches("<li data-release").count(), 3, "{section}");
        assert_eq!(
            section.matches("data-action=\"deploy-select\"").count(),
            2,
            "{section}"
        );
        assert!(!section.contains("<table"), "{section}");
        // Nothing loaded, nothing to list: no section at all.
        state.deploy = Default::default();
        assert!(!panel(&state).contains("Recent releases"));
    }

    #[test]
    fn the_instances_tab_is_the_table_or_a_line_saying_there_are_none() {
        let html = panel(&open("instances"));
        assert!(html.contains("aria-selected=\"true\" aria-controls=\"service-tab-panel\" id=\"service-tab-instances\""), "{html}");
        assert!(html.contains("<div id=\"service-instances\"><div class=\"table-scroll\"><table class=\"wasm-table service-instances\">"), "{html}");
        assert!(!html.contains("id=\"service-status\""), "{html}");
        let mut none = open("instances");
        none.service_deployment = Some(json!({"phase": "stopped", "instances": []}));
        assert!(panel(&none).contains("<div id=\"service-instances\"><p class=\"detail-note\">No instances are running.</p></div>"));
    }

    #[test]
    fn the_releases_tab_holds_the_deploy_panel_and_says_when_there_is_none() {
        let html = panel(&open("releases"));
        assert!(
            html.contains("<div id=\"deploy-panel\"><section aria-label=\"Deploy\">"),
            "{html}"
        );
        assert!(html.contains("id=\"deploy-update\""), "{html}");
        let mut plain = open("releases");
        plain.service_deployment.as_mut().unwrap()["spec"]["domain"] = json!("");
        let html = panel(&plain);
        assert!(html.contains("<div id=\"deploy-panel\"><p class=\"detail-note\">Releases and updates are available for a service that has a domain.</p></div>"), "{html}");
    }

    #[test]
    fn the_settings_tab_edits_deletes_and_explains_the_dns() {
        let mut state = open("settings");
        state.cache.insert(
            "/api/pipe".into(),
            json!({"config": {"tunnel": {"id": "tun-1"}}}),
        );
        let html = panel(&state);
        assert!(html.contains("aria-label=\"Edit configuration\" title=\"Edit configuration\" data-action=\"resource-edit\" data-id=\"s1\""), "{html}");
        assert!(html.contains("class=\"icon-btn danger\" aria-label=\"Delete\" title=\"Delete\" data-action=\"request\" data-method=\"DELETE\" data-path=\"/api/micro-services/s1\""), "{html}");
        assert!(
            html.contains("<code>tun-1.cfargotunnel.com</code>"),
            "{html}"
        );
        // Without a tunnel, the note sends the person to Pipe.
        let mut no_tunnel = open("settings");
        let html = panel(&no_tunnel);
        assert!(
            html.contains("<a href=\"#/pipe/named-tunnel\">Pipe Tunnel</a>"),
            "{html}"
        );
        // Without a domain, there is no DNS to explain.
        no_tunnel.detail.as_mut().unwrap()["domain"] = json!(null);
        let html = panel(&no_tunnel);
        assert!(
            !html.contains("cfargotunnel") && !html.contains("named-tunnel"),
            "{html}"
        );
    }

    #[test]
    fn the_tabs_read_in_korean() {
        crate::i18n::set("ko");
        let html = panel(&open("overview"));
        crate::i18n::set("en");
        for name in ["개요", "인스턴스 <em>1</em>", "릴리스", "설정"] {
            assert!(
                html.contains(&format!(">{name}</button>")),
                "{name}: {html}"
            );
        }
        for text in ["배포 진행", "속성", "크기", "연결", "최근 릴리스"] {
            assert!(html.contains(text), "{text}: {html}");
        }
    }

    #[test]
    fn an_unknown_tab_shows_the_overview() {
        let html = panel(&open("logs"));
        assert!(
            html.contains("aria-labelledby=\"service-tab-overview\""),
            "{html}"
        );
    }

    #[test]
    fn arrow_keys_move_along_the_tabs_of_the_panel_that_is_open() {
        assert_eq!(
            panel_tabs("service-tab-releases").map(|(_, ids)| ids.len()),
            Some(4)
        );
        assert_eq!(
            panel_tabs("vm-tab-logs").map(|(prefix, _)| prefix),
            Some("vm")
        );
        assert!(panel_tabs("tab-x").is_none());
        let ids = |prefix: &str| {
            panel_tabs(&format!("{prefix}-tab-overview"))
                .unwrap()
                .1
                .iter()
                .map(|tab| tab.0)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids("service"),
            ["overview", "instances", "releases", "settings"]
        );
        assert_eq!(ids("vm"), ["overview", "network", "volumes", "env", "logs"]);
    }
}
