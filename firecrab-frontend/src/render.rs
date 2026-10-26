use crate::{
    App,
    api::encode,
    model::{VIEWS, Vm, escape, list, text},
};
use serde_json::{Value, json};
use std::fmt::Write;

pub fn button(label: &str, action: &str, id: &str) -> String {
    format!(
        "<button type=\"button\" class=\"btn\" data-action=\"{}\" data-id=\"{}\">{}</button>",
        escape(action),
        escape(id),
        escape(crate::i18n::t(label))
    )
}

pub enum Icon {
    Add,
    Refresh,
    Reveal,
    Hide,
    Edit,
    Delete,
    Close,
    Key,
    Sparkles,
    Connected,
}

impl Icon {
    pub(crate) fn svg(&self) -> String {
        let path = match self {
            Self::Add => "M12 5v14M5 12h14",
            Self::Refresh => "M20 7v5h-5M4 17v-5h5m-5 0a8 8 0 0 1 14-6l2 2M4 16l2 2a8 8 0 0 0 14-6",
            Self::Reveal => {
                "M2 12s3-7 10-7 10 7 10 7-3 7-10 7S2 12 2 12Zm13 0a3 3 0 1 1-6 0 3 3 0 0 1 6 0Z"
            }
            Self::Hide => {
                "m3 3 18 18M10.6 10.6a2 2 0 0 0 2.8 2.8M9.9 5.2A12 12 0 0 1 12 5c7 0 10 7 10 7a18 18 0 0 1-3.1 4.1M6.2 6.2A18 18 0 0 0 2 12s3 7 10 7a12 12 0 0 0 5.8-1.8"
            }
            Self::Edit => "m16 3 5 5M4 20l4-1 13-13a2.8 2.8 0 0 0-4-4L4 15l-1 6Z",
            Self::Delete => "M3 6h18M9 6V3h6v3M5 6l1 15h12l1-15M10 10v7m4-7v7",
            Self::Close => "m6 6 12 12M18 6 6 18",
            Self::Key => {
                "M21 8a5 5 0 1 1-10 0 5 5 0 0 1 10 0ZM12.5 11.5 3 21H2v-3l3-3h3v-3l4.5-.5M16 7h.01"
            }
            Self::Sparkles => {
                "m12 3 2.5 6.5L21 12l-6.5 2.5L12 21l-2.5-6.5L3 12l6.5-2.5L12 3ZM20 2v4M18 4h4"
            }
            Self::Connected => "M21 12a9 9 0 1 1-9-9 9 9 0 0 1 9 9ZM8 12l3 3 5-6",
        };
        format!(
            "<svg viewBox=\"0 0 24 24\" aria-hidden=\"true\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\" stroke-linejoin=\"round\"><path d=\"{path}\"/></svg>"
        )
    }
}

pub fn icon_button(label: &str, action: &str, id: &str, icon: Icon) -> String {
    let label = escape(crate::i18n::t(label));
    format!(
        "<button type=\"button\" class=\"icon-btn\" aria-label=\"{label}\" title=\"{label}\" data-action=\"{}\" data-id=\"{}\">{}</button>",
        escape(action),
        escape(id),
        icon.svg()
    )
}

pub fn icon_request(label: &str, path: &str) -> String {
    let label = escape(crate::i18n::t(label));
    format!(
        "<button type=\"button\" class=\"icon-btn danger\" aria-label=\"{label}\" title=\"{label}\" data-action=\"request\" data-method=\"DELETE\" data-path=\"{}\" data-body=\"null\">{}</button>",
        escape(path),
        Icon::Delete.svg()
    )
}
fn vm_icon_button(label: &str, action: &str, id: &str) -> String {
    let path = match action {
        "vm-start" => "M6 3l14 9-14 9z",
        "vm-stop" => "M6 6h12v12H6z",
        "vm-pause" => "M7 4h3v16H7zM14 4h3v16h-3z",
        "vm-resume" => "M6 3l14 9-14 9z",
        _ => "M4 11h1v2H4zM11 11h1v2h-1zM18 11h1v2h-1z",
    };
    format!(
        "<button type=\"button\" class=\"icon-btn\" aria-label=\"{}\" title=\"{}\" data-action=\"{}\" data-id=\"{}\"><svg viewBox=\"0 0 24 24\" aria-hidden=\"true\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"><path d=\"{path}\"/></svg></button>",
        escape(crate::i18n::t(label)),
        escape(crate::i18n::t(label)),
        escape(action),
        escape(id)
    )
}

pub fn request(label: &str, method: &str, path: &str, body: Value) -> String {
    format!(
        "<button type=\"button\" class=\"btn {}\" data-action=\"request\" data-method=\"{}\" data-path=\"{}\" data-body=\"{}\">{}</button>",
        if method == "DELETE" { "danger" } else { "" },
        method,
        escape(path),
        escape(&body.to_string()),
        escape(crate::i18n::t(label))
    )
}
fn tr<'a>(app: &App, en: &'a str, ko: &'a str) -> &'a str {
    if app.locale == "ko" { ko } else { en }
}
fn view<'a>(app: &App, en: &'a str, ko: &'a str) -> &'a str {
    tr(app, en, ko)
}

fn navigation_icon(name: &str) -> String {
    let path = match name {
        "vms" => {
            "M5 3h14a2 2 0 0 1 2 2v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2Zm0 10h14a2 2 0 0 1 2 2v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4a2 2 0 0 1 2-2ZM7 7h.01M7 17h.01M11 7h6M11 17h6"
        }
        "pools" => "m12 3 10 5-10 5L2 8l10-5ZM2 12l10 5 10-5M2 16l10 5 10-5",
        "schedules" => {
            "M8 2v4M16 2v4M3 10h18M5 4h14a2 2 0 0 1 2 2v13a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2ZM12 13v3l2 1"
        }
        "services" => "m12 3 9 5v8l-9 5-9-5V8l9-5Zm0 10 9-5M12 13 3 8M12 13v8M7.5 5.5l9 5v5",
        "cluster" => "M3 3h6v4H3zM15 3h6v4h-6zM9 17h6v4H9zM6 7v5h12V7M12 12v5",
        "networks" => "M9 3h6v6H9zM3 15h6v6H3zM15 15h6v6h-6zM12 9v3M6 15v-3h12v3",
        "security-groups" => {
            "M12 3c3 2 6 3 8 3v6c0 5-5 8-8 10-3-2-8-5-8-10V6c2 0 5-1 8-3Zm-4 9 3 3 5-6"
        }
        "pipe" => "M3 7h18m-4-4 4 4-4 4M21 17H3m4-4-4 4 4 4",
        "queues" => "M4 5h16M4 10h12M4 15h8M4 20h8M15 17h6m-3-3 3 3-3 3",
        "secrets" => {
            "M21 8a5 5 0 1 1-10 0 5 5 0 0 1 10 0ZM12.5 11.5 3 21H2v-3l3-3h3v-3l4.5-.5M16 7h.01"
        }
        "storages" => {
            "M21 5c0 2.2-4 4-9 4S3 7.2 3 5s4-4 9-4 9 1.8 9 4ZM3 5v14c0 2.2 4 4 9 4s9-1.8 9-4V5M3 12c0 2.2 4 4 9 4s9-1.8 9-4"
        }
        "volumes" => {
            "M5 3h14l3 12H2L5 3ZM2 15v4a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-4M6 18h.01M10 18h.01"
        }
        "images" => {
            "M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2ZM7.5 7.5h.01M3 17l6-6 4 4 3-3 5 5"
        }
        "kernels" => {
            "M6 6h12v12H6zM9 9h6v6H9zM9 2v4M15 2v4M9 18v4M15 18v4M2 9h4M2 15h4M18 9h4M18 15h4"
        }
        "shells" => "m4 5 7 7-7 7M13 19h7",
        "host" => {
            "M4 3h16a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2ZM12 17v4M8 21h8"
        }
        "search" => "M21 21l-5-5M18 10a8 8 0 1 1-16 0 8 8 0 0 1 16 0Z",
        "menu" => "M4 6h16M4 12h16M4 18h16",
        _ => return String::new(),
    };
    format!(
        "<svg width=\"16\" height=\"16\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.75\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\"><path d=\"{path}\"/></svg>"
    )
}

pub fn page(app: &App) -> String {
    if let Some(id) = app.route.strip_prefix("console/") {
        return console(app, id);
    }
    let section = app.route.split('/').next().unwrap_or("vms");
    let mut nav = String::new();
    for (group, en, ko) in [
        ("compute", "Compute", "컴퓨팅"),
        ("network", "Network", "네트워크"),
        ("data", "Data", "데이터"),
        ("system", "System", "시스템"),
        ("ai", "AI", "AI"),
    ] {
        let _ = write!(
            nav,
            "<div class=\"sidebar-group\"><div class=\"sidebar-group-label\">{}</div><ul>",
            tr(app, en, ko)
        );
        for (id, category, en, ko) in VIEWS
            .iter()
            .filter(|(_, category, _, _)| *category == group)
        {
            let _ = category;
            let nav_icon = if *id == "ai-sandbox" {
                crate::sandbox::icon("bot")
            } else {
                navigation_icon(id)
            };
            let _ = write!(
                nav,
                "<li><a class=\"sidebar-item {}\" href=\"#/{id}\" {} aria-label=\"{}\">{nav_icon}<span class=\"sidebar-label\">{}</span>{}</a></li>",
                if *id == section { "active" } else { "" },
                if *id == section {
                    "aria-current=\"page\""
                } else {
                    ""
                },
                tr(app, en, ko),
                tr(app, en, ko),
                if *id == "vms" {
                    format!(
                        "<span class=\"sidebar-count\">{}</span><span class=\"sidebar-alert\">{}</span>",
                        app.vms.len(),
                        app.vms.iter().filter(|vm| vm.state == "error").count()
                    )
                } else {
                    String::new()
                }
            );
        }
        nav.push_str("</ul></div>");
    }
    let title = VIEWS
        .iter()
        .find(|(id, _, _, _)| *id == section)
        .map(|(_, _, en, ko)| tr(app, en, ko))
        .unwrap_or("MicroVM");
    let toolbar = if section == "vms" && app.route == "vms" {
        vm_toolbar(app)
    } else {
        String::new()
    };
    let actions = match section {
        "vms" => format!(
            "<a class=\"btn primary\" id=\"vm-list-add\" href=\"#/vms/new\">{}</a>",
            tr(app, "New MicroVM", "새 MicroVM")
        ),
        "services" | "networks" | "storages" | "shells" => button(
            if section == "services" {
                tr(app, "New MicroService", "새 MicroService")
            } else {
                tr(app, "Create", "생성")
            },
            "create",
            section,
        ),
        "images" => button(
            tr(app, "Import OCI image", "OCI 이미지 가져오기"),
            "create",
            "oci",
        ),
        "pools" => button("New pool", "create", "pools"),
        "schedules" => button("New schedule", "create", "schedules"),
        "security-groups" => button("New security group", "create", "security-groups"),
        "secrets" => icon_button("New secret", "create", "secrets", Icon::Add),
        "volumes" => button("Create volume", "create", "volumes"),
        _ => String::new(),
    };
    let panel = crate::details::panel(app);
    let panel_width = app.panel_width;
    let panel_hidden = if app.panel_open
        && !["vms/new", "queues", "cluster", "ai-sandbox"].contains(&app.route.as_str())
    {
        ""
    } else {
        "hidden"
    };
    let panel_toggle = if ["cluster", "ai-sandbox", "services"].contains(&section) {
        String::new()
    } else if section == "secrets" {
        let label = crate::i18n::t(if app.panel_open {
            "Hide details"
        } else {
            "Show details"
        });
        format!(
            "<button type=\"button\" class=\"btn\" id=\"panel-toggle\" data-action=\"panel-toggle\" title=\"{label}\" aria-expanded=\"{}\">{}</button>",
            app.panel_open,
            crate::i18n::t("Details")
        )
    } else {
        format!(
            "<button type=\"button\" class=\"btn\" id=\"panel-toggle\" data-action=\"panel-toggle\">{}</button>",
            crate::i18n::t(if app.panel_open {
                "Hide details"
            } else {
                "Show details"
            })
        )
    };
    let category = match section {
        "vms" | "services" | "pools" | "schedules" | "cluster" => tr(app, "Compute", "컴퓨팅"),
        "networks" | "pipe" | "security-groups" => tr(app, "Network", "네트워크"),
        "storages" | "volumes" | "images" | "kernels" | "secrets" | "queues" => {
            tr(app, "Data", "데이터")
        }
        "ai-sandbox" => "AI",
        _ => tr(app, "System", "시스템"),
    };
    let meters = host_meters(app);
    let search_label = tr(app, "Search and go", "검색 및 이동");
    let panel_label = panel_label(app);
    let menu_icon = navigation_icon("menu");
    let search_icon = navigation_icon("search");
    let host_icon = navigation_icon("host");
    let host_label = tr(app, "Host and updates", "호스트 및 업데이트");
    format!(
        "<div class=\"app-frame\"><button class=\"icon-btn nav-toggle\" aria-label=\"Open menu\" data-action=\"menu\">{menu_icon}</button><nav class=\"sidebar\" aria-label=\"{}\"><div class=\"sidebar-brand\"><span class=\"sidebar-logo\"><img src=\"/firecrab.png\" width=\"20\" height=\"20\" alt=\"\"></span><span class=\"sidebar-brand-text\"><span class=\"sidebar-name\">firecrab</span><span class=\"sidebar-host\" id=\"sidebar-platform\">private microvm cloud</span></span></div><button class=\"sidebar-search\" aria-label=\"{search_label}\" data-action=\"palette\">{search_icon}<span class=\"sidebar-label\">{search_label}</span><kbd>⌘ K</kbd></button><div class=\"sidebar-groups\">{nav}</div><section class=\"host-card\" id=\"host-status\" aria-label=\"Host status\">{meters}</section><div class=\"sidebar-foot\"><span id=\"update-indicator\"></span><a href=\"#/host\" class=\"btn\" aria-label=\"{host_label}\" title=\"{host_label}\">{host_icon}<span class=\"sidebar-label\">{host_label}</span></a><div class=\"sidebar-language\"><select id=\"language\" aria-label=\"Language\"><option value=\"en\" {}>English</option><option value=\"ko\" {}>한국어</option></select></div></div></nav><div class=\"app-workspace\"><main class=\"screen\"><header class=\"screen-head screen-header\"><div class=\"screen-heading\"><span class=\"screen-group\">{category}</span><h1>{title}</h1><span id=\"api-status\"></span></div><div class=\"wasm-actions\">{actions}{}{panel_toggle}</div></header><div id=\"messages\" aria-live=\"polite\">{}</div><div id=\"toolbar\">{toolbar}</div><div class=\"wasm-layout\"><div id=\"content\" class=\"wasm-content screen-main screen-body\" data-loaded=\"{}\">{}</div><aside id=\"detail-panel\" aria-label=\"{panel_label}\" class=\"wasm-panel detail-panel\" style=\"width:{panel_width}px\" {panel_hidden}>{panel}</aside></div></main></div></div><div id=\"menu-root\"></div><div id=\"palette-root\"></div><div id=\"tooltip-root\"></div><div id=\"modal-root\"></div>",
        tr(app, "Main navigation", "주요 메뉴"),
        if app.locale == "en" { "selected" } else { "" },
        if app.locale == "ko" { "selected" } else { "" },
        if section == "ai-sandbox" {
            String::new()
        } else if section == "secrets" {
            icon_button(
                tr(app, "Refresh", "새로 고침"),
                "refresh",
                "",
                Icon::Refresh,
            )
        } else {
            button(tr(app, "Refresh", "새로 고침"), "refresh", "")
        },
        messages(app),
        app.loaded,
        content(app)
    )
}

pub fn panel_label(app: &App) -> &'static str {
    crate::i18n::t(match app.route.as_str() {
        "pools" => "Pool detail",
        "schedules" => "Schedule detail",
        "security-groups" => "Security group detail",
        "secrets" => "Secret detail",
        "vms" => "MicroVM detail",
        "services" => "MicroService detail",
        "networks" => "MicroNetwork detail",
        "storages" => "MicroStorage detail",
        "images" => "Image detail",
        "kernels" => "Kernel detail",
        "shells" => "Shell detail",
        _ => "Details",
    })
}

pub fn messages(app: &App) -> String {
    let mut out = String::new();
    for error in [&app.error, &app.connection_error]
        .into_iter()
        .enumerate()
        .filter(|(index, error)| {
            !error.is_empty()
                && (*index == 0 || *error != &app.error)
                && (app.field_errors.is_empty() || *error != &app.error)
        })
    {
        let (_, error) = error;
        let _ = write!(
            out,
            "<div class=\"wasm-message wasm-error\" role=\"alert\">{}</div>",
            escape(error)
        );
    }
    if !app.notice.is_empty() {
        let _ = write!(
            out,
            "<div class=\"wasm-message\" role=\"status\">{}</div>",
            escape(crate::i18n::t(&app.notice))
        );
    }
    if app.busy {
        out.push_str(&format!(
            "<div class=\"wasm-message\" role=\"status\">{}</div>",
            crate::i18n::t("Working…")
        ));
    }
    out
}

pub fn content(app: &App) -> String {
    if app.route == "vms/new"
        && let Some(form) = &app.form
    {
        return form.html(app.busy, &app.field_errors);
    }
    if !app.loaded {
        return format!(
            "<div class=\"empty-state\">{}</div>",
            tr(app, "Loading…", "불러오는 중…")
        );
    }
    let mut html = match app.route.as_str() {
        "vms" => format!(
            "<div class=\"vm-screen\">{}<div id=\"vm-scroll\" class=\"vm-list-scroll\"><table class=\"vm-list\" aria-label=\"{}\"><colgroup><col><col class=\"vm-col-state\"><col class=\"vm-col-ip\"><col class=\"vm-col-cpu\"><col class=\"vm-col-mem\"><col class=\"vm-col-actions\"></colgroup><thead><tr><th>{}</th><th>{}</th><th class=\"vm-col-ip\">IPv4</th><th>CPU</th><th class=\"vm-col-mem\">{}</th><th><span class=\"sr-only\">Actions</span></th></tr></thead><tbody id=\"vm-rows\">{}</tbody></table><p id=\"vm-empty\" class=\"empty-state\" {}>{}</p></div></div>",
            vm_summary(app),
            tr(app, "MicroVMs", "MicroVM 목록"),
            tr(app, "Name", "이름"),
            tr(app, "State", "상태"),
            tr(app, "Memory", "메모리"),
            vm_rows(app),
            if app.vms.iter().any(|vm| vm.matches(&app.filter, &app.query)) {
                "hidden"
            } else {
                ""
            },
            tr(
                app,
                "No MicroVMs match the filter.",
                "조건에 맞는 MicroVM이 없습니다."
            )
        ),
        "pools" | "schedules" | "security-groups" | "secrets" | "queues" | "cluster" => {
            crate::advanced::content(app)
        }
        "services" => services(app),
        "networks" => resource_table(
            app,
            &app.resource,
            &[
                ("name", "Name"),
                ("subnetCidr", "IPv4 subnet"),
                ("gateway", "Gateway"),
                ("internetEnabled", "Internet"),
            ],
        ),
        "storages" => resource_table(
            app,
            &app.resource,
            &[("name", "Name"), ("path", "Directory")],
        ),
        "volumes" => crate::volumes::content(app),
        "images" => resource_table(
            app,
            &app.resource,
            &[
                ("alias", "Image"),
                ("installed", "Installed"),
                ("sizeBytes", "Size (bytes)"),
            ],
        ),
        "kernels" => resource_table(
            app,
            &app.resource,
            &[
                ("version", "Version"),
                ("installed", "Installed"),
                ("architecture", "Architecture"),
            ],
        ),
        "shells" => resource_table(
            app,
            &app.resource,
            &[
                ("name", "Name"),
                ("description", "Description"),
                ("latestVersion", "Latest revision"),
            ],
        ),
        "pipe/storage" => crate::advanced::storage::content(app),
        "pipe/ai" => crate::advanced::ai::content(app),
        "ai-sandbox" => crate::sandbox::content(app),
        "host" => host(app),
        "pipe" => pipe_cards(app),
        route if route.starts_with("pipe/") => pipe(app, route.trim_start_matches("pipe/")),
        _ => String::new(),
    };
    if app.route == "images" {
        html.push_str(&crate::advanced::images::sources(app));
    }
    if app.route == "pipe/registry" {
        html.push_str(&format!(
            "<div id=\"workflow-root\">{}</div>",
            crate::workflows::registry_html(app)
        ));
    }
    if app.route == "images"
        && let Some(catalog) = app.cache.get("/api/microregistry")
    {
        html.push_str(&format!(
            "<section class=\"wasm-details\"><h2>{}</h2>",
            crate::i18n::t("Available packages")
        ));
        for image in list(&catalog["images"]) {
            html.push_str(&format!(
                "<p>{} · {} {}</p>",
                escape(text(image, "alias")),
                escape(text(image, "version")),
                request(
                    "Register",
                    "POST",
                    "/api/microregistry/register",
                    json!({"alias":image["alias"],"version":image["version"]})
                )
            ));
        }
        html.push_str("</section>");
    }
    if let Some(job) = &app.job {
        html.push_str(&format!(
            "<section class=\"wasm-details\"><h2>{}</h2>{}{}</section>",
            crate::i18n::t("Progress"),
            properties(&job.1),
            button("Refresh progress", "job-refresh", &job.0)
        ));
    }
    html
}

pub fn vm_toolbar(app: &App) -> String {
    let mut out="<div class=\"vm-toolbar\"><div class=\"vm-filters\" role=\"tablist\" aria-label=\"MicroVM state\">".to_string();
    for (id, en, ko) in [
        ("all", "All", "전체"),
        ("running", "Running", "실행 중"),
        ("paused", "Paused", "일시정지됨"),
        ("stopped", "Stopped", "중지됨"),
        ("error", "Error", "오류"),
    ] {
        let count = app
            .vms
            .iter()
            .filter(|vm| id == "all" || vm.state == id)
            .count();
        let _ = write!(
            out,
            "<button type=\"button\" role=\"tab\" aria-selected=\"{}\" class=\"vm-filter\" data-action=\"filter\" data-id=\"{id}\">{} <span class=\"vm-filter-count\">{count}</span></button>",
            app.filter == id,
            tr(app, en, ko)
        );
    }
    let _ = write!(
        out,
        "</div><label class=\"vm-search\"><span aria-hidden=\"true\">⌕</span><input type=\"search\" id=\"vm-search\" aria-label=\"{}\" placeholder=\"{}\" value=\"{}\"></label></div>",
        tr(app, "Filter MicroVMs", "MicroVM 필터"),
        tr(app, "Name, IP, or image", "이름, IP, 이미지"),
        escape(&app.query)
    );
    out
}

pub fn vm_summary(app: &App) -> String {
    let running = app.vms.iter().filter(|vm| vm.state == "running").count();
    let cpu: u64 = app.vms.iter().map(|vm| u64::from(vm.cpu)).sum();
    let memory: u64 = app.vms.iter().map(|vm| u64::from(vm.ram)).sum();
    let disk: u64 = app.vms.iter().map(|vm| u64::from(vm.disk_gb)).sum();
    let mut out = "<dl class=\"vm-summary\" id=\"vm-summary\">".to_string();
    for (label, value) in [
        (
            tr(app, "Running", "실행 중"),
            format!("{running} / {}", app.vms.len()),
        ),
        (tr(app, "vCPU allocated", "vCPU 할당"), cpu.to_string()),
        (
            tr(app, "Memory allocated", "메모리 할당"),
            match app
                .cache
                .get("/api/host")
                .and_then(|h| h["memoryTotalMib"].as_f64())
            {
                Some(total) => format!("{:.0} / {:.0} GiB", memory as f64 / 1024.0, total / 1024.0),
                None => format!("{:.0} GiB", memory as f64 / 1024.0),
            },
        ),
        (
            tr(app, "Disk allocated", "디스크 할당"),
            match app
                .cache
                .get("/api/host")
                .and_then(|h| h["diskTotalGib"].as_f64())
            {
                Some(total) => format!("{disk} / {total:.0} GB"),
                None => format!("{disk} GB"),
            },
        ),
    ] {
        let _ = write!(
            out,
            "<div class=\"vm-summary-item\"><dt>{label}</dt><dd><span class=\"vm-summary-value\">{value}</span></dd></div>"
        );
    }
    out.push_str("</dl>");
    out
}
pub fn vm_rows(app: &App) -> String {
    let matches: Vec<_> = app
        .vms
        .iter()
        .filter(|vm| vm.matches(&app.filter, &app.query))
        .collect();
    let limit = if app.vms.len() > crate::vm_list::WINDOW_THRESHOLD {
        64
    } else {
        matches.len()
    };
    matches
        .into_iter()
        .take(limit)
        .map(|vm| vm_row(app, vm))
        .collect()
}
pub fn vm_row(app: &App, vm: &Vm) -> String {
    let mut out = String::with_capacity(1600);
    let selected = app.detail.as_ref().is_some_and(|d| text(d, "id") == vm.id);
    let id = escape(&vm.id);
    let state = escape(&vm.state);
    let state_label = match vm.state.as_str() {
        "created" => tr(app, "Created", "생성됨"),
        "running" => tr(app, "Running", "실행 중"),
        "stopped" => tr(app, "Stopped", "중지됨"),
        "starting" => tr(app, "Starting", "시작 중"),
        "stopping" => tr(app, "Stopping", "중지 중"),
        "paused" => tr(app, "Paused", "일시 중지됨"),
        "pausing" => tr(app, "Pausing", "일시 중지 중"),
        "resuming" => tr(app, "Resuming", "재개 중"),
        _ => tr(app, "Error", "오류"),
    };
    let cpu = if vm.state == "running" {
        vm.cpu_usage_percent
            .map(|value| {
                format!(
                    "{}<span class=\"vm-usage-value\">{value:.1}%</span>",
                    sparkline(vm)
                )
            })
            .unwrap_or("—".into())
    } else {
        "—".into()
    };
    let memory = if vm.state == "running" {
        vm.memory_used_percent.filter(|v|v.is_finite()).map(|value|format!("<span class=\"vm-meter\"><i style=\"width:{}%\"></i></span><span class=\"vm-usage-value\">{value:.1}%</span>",value.clamp(0.0,100.0))).unwrap_or("—".into())
    } else {
        "—".into()
    };
    let action = if matches!(
        vm.extra.get("purpose").and_then(Value::as_str),
        Some("pool" | "queue")
    ) {
        String::new()
    } else if vm.state == "paused" {
        vm_icon_button("Resume", "vm-resume", &vm.id)
    } else if vm.state == "running" {
        vm_icon_button(tr(app, "Stop", "중지"), "vm-stop", &vm.id)
            + &vm_icon_button("Pause", "vm-pause", &vm.id)
    } else if vm.editable() {
        vm_icon_button(tr(app, "Start", "시작"), "vm-start", &vm.id)
    } else {
        String::new()
    };
    let terminal = if vm.state == "running" {
        format!(
            "<a class=\"icon-btn\" href=\"#/console/{}\" target=\"_blank\" rel=\"noopener noreferrer\" aria-label=\"Terminal (new tab)\">▣</a>",
            encode(&vm.id)
        )
    } else {
        "<button class=\"icon-btn\" disabled aria-label=\"Terminal (new tab)\">▣</button>".into()
    };
    let _ = write!(
        out,
        "<tr data-id=\"{id}\" aria-selected=\"{selected}\" class=\"{}\"><td><button type=\"button\" class=\"vm-row-name\" data-action=\"detail\" data-id=\"{id}\">{}</button><div class=\"vm-row-sub\">{}</div></td><td><span class=\"badge {state} state-label vm-status-label\" data-state=\"{state}\" tabindex=\"0\" aria-label=\"VM-STATUS: {state_label}\" data-vm-tooltip=\"{id}\" title=\"{state_label}\">{state_label}</span></td><td class=\"vm-col-ip\"><span class=\"mono\">{}</span></td><td><span class=\"vm-usage\">{cpu}</span></td><td class=\"vm-col-mem\"><span class=\"vm-usage\">{memory}</span></td><td class=\"vm-row-actions\">{action}{terminal}{}</td></tr>",
        if selected { "selected" } else { "" },
        escape(&vm.name),
        vm_subtitle(vm),
        escape(vm.ipv4.as_deref().unwrap_or("—")),
        vm_icon_button("Actions", "vm-menu", &vm.id)
    );
    out
}
fn vm_subtitle(vm: &Vm) -> String {
    if let Some(failed) = list(vm.extra.get("startupTimeline").unwrap_or(&Value::Null))
        .iter()
        .rev()
        .find(|run| run["outcome"] == "failed")
    {
        return escape(text(failed, "detail"));
    }
    let step = vm
        .extra
        .get("startupStep")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !step.is_empty() {
        return format!("{}…", escape(crate::details::step_label(step)));
    }
    let memory = if vm.ram.is_multiple_of(1024) {
        format!("{} GiB", vm.ram / 1024)
    } else {
        format!("{} MiB", vm.ram)
    };
    format!(
        "{} · {} vCPU · {memory} · {} GiB",
        escape(&vm.template),
        vm.cpu,
        vm.disk_gb
    )
}
fn sparkline(vm: &Vm) -> String {
    let values: Vec<f64> = vm
        .usage_history
        .iter()
        .filter_map(|sample| sample["cpuUsagePercent"].as_f64())
        .filter(|v| v.is_finite())
        .collect();
    if values.len() < 2 {
        return String::new();
    }
    let max = values.iter().copied().fold(1.0, f64::max);
    let points = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            format!(
                "{:.1},{:.1}",
                i as f64 * 56.0 / (values.len() - 1) as f64,
                17.0 - v / max * 16.0
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "<svg width=\"56\" height=\"18\" aria-hidden=\"true\"><polyline points=\"{points}\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/></svg>"
    )
}

fn services(app: &App) -> String {
    let rows = list(&app.resource);
    if rows.is_empty() {
        return format!(
            "<div class=\"empty-state\">{}</div>",
            tr(
                app,
                "No service configurations yet.",
                "서비스 구성이 없습니다."
            )
        );
    }
    let mut out = format!(
        "<div class=\"service-list\"><table class=\"wasm-table service-table\" aria-label=\"MicroServices\"><thead><tr><th id=\"service-column-0\">{}</th><th id=\"service-column-2\">{}</th><th id=\"service-column-3\">{}</th><th><span class=\"sr-only\">{}</span></th></tr></thead><tbody>",
        tr(app, "Service", "서비스"),
        crate::i18n::t("Port"),
        crate::i18n::t("Replicas"),
        crate::i18n::t("Actions")
    );
    for row in rows {
        let id = text(row, "id");
        let name = text(row, "name");
        let selected = app.detail.as_ref().is_some_and(|detail| detail["id"] == id);
        let open = if app.locale == "ko" {
            format!("{name} 열기")
        } else {
            format!("Open {name}")
        };
        let actions = if app.locale == "ko" {
            format!("{name} 작업")
        } else {
            format!("Actions for {name}")
        };
        let edit = if app.locale == "ko" {
            format!("{name} 수정")
        } else {
            format!("Edit {name}")
        };
        let delete = if app.locale == "ko" {
            format!("{name} 삭제")
        } else {
            format!("Delete {name}")
        };
        let image = text(row, "image");
        let image_name = image.rsplit('/').next().unwrap_or(image);
        let image_label = if let Some((name, digest)) = image_name.split_once("@sha256:") {
            format!(
                "{name}@sha256:{}…",
                digest.chars().take(12).collect::<String>()
            )
        } else {
            image_name.to_string()
        };
        let _ = write!(
            out,
            "<tr data-id=\"{}\" aria-selected=\"{selected}\"><td headers=\"service-column-0\" aria-label=\"{}\"><button type=\"button\" class=\"service-name\" data-action=\"service-run\" data-id=\"{}\" aria-label=\"{}\" title=\"{}\">{}</button><span class=\"service-image\" title=\"{}\">{}</span></td><td headers=\"service-column-2\">{}</td><td headers=\"service-column-3\">{}</td><td><details class=\"service-row-menu\"><summary aria-label=\"{}\" title=\"{}\"><svg viewBox=\"0 0 24 24\" width=\"18\" height=\"18\" aria-hidden=\"true\"><circle cx=\"5\" cy=\"12\" r=\"1.5\" fill=\"currentColor\"/><circle cx=\"12\" cy=\"12\" r=\"1.5\" fill=\"currentColor\"/><circle cx=\"19\" cy=\"12\" r=\"1.5\" fill=\"currentColor\"/></svg></summary><div class=\"service-row-actions\">{}{}</div></details></td></tr>",
            escape(id),
            escape(name),
            escape(id),
            escape(&open),
            escape(name),
            escape(name),
            escape(image),
            escape(&image_label),
            display(&row["port"]),
            display(&row["replicas"]),
            escape(&actions),
            escape(&actions),
            button(&edit, "resource-edit", id),
            request(
                &delete,
                "DELETE",
                &format!("/api/micro-services/{}", encode(id)),
                Value::Null
            )
        );
    }
    out.push_str("</tbody></table></div>");
    out
}

fn resource_table(app: &App, resource: &Value, columns: &[(&str, &str)]) -> String {
    let rows = if resource.is_array() {
        list(resource)
    } else if resource["kernels"].is_array() {
        list(&resource["kernels"])
    } else {
        &[]
    };
    if rows.is_empty() {
        return format!(
            "<div class=\"empty-state\">{}</div>",
            tr(app, "No items yet.", "항목이 없습니다.")
        );
    }
    let kind = app.route.trim_end_matches('s');
    let mut out =
        format!("<div class=\"table-scroll\"><table class=\"wasm-table {kind}-table\"><thead><tr>");
    for (index, (_, label)) in columns.iter().enumerate() {
        let _ = write!(
            out,
            "<th id=\"{kind}-column-{index}\">{}</th>",
            crate::i18n::t(label)
        );
    }
    out.push_str(&format!(
        "<th>{}</th></tr></thead><tbody>",
        crate::i18n::t("Actions")
    ));
    let identity = if app.route == "pipe/registry" {
        ["registry", "id", "alias", "version", "provider"]
    } else {
        ["id", "alias", "version", "provider", "registry"]
    };
    for row in rows {
        let id = identity
            .iter()
            .find_map(|key| row[key].as_str())
            .unwrap_or("");
        let selected = app
            .detail
            .as_ref()
            .is_some_and(|detail| identity.iter().any(|key| detail[key] == id));
        let _ = write!(
            out,
            "<tr data-id=\"{}\" aria-selected=\"{selected}\">",
            escape(id)
        );
        for (index, (key, _)) in columns.iter().enumerate() {
            let _ = write!(
                out,
                "<td headers=\"{kind}-column-{index}\">{}</td>",
                display(&row[key])
            );
        }
        let _ = write!(
            out,
            "<td>{}</td></tr>",
            button(tr(app, "Details", "상세"), "detail", id)
        );
    }
    out.push_str("</tbody></table></div>");
    out
}

pub fn deployment(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return format!("<p role=\"status\">{}</p>", crate::i18n::t("Loading…"));
    };
    if value.is_null() {
        return format!(
            "<p class=\"service-state\" role=\"status\">{}</p>",
            crate::i18n::t("Not deployed")
        );
    }
    let phase = text(value, "phase");
    let label = match phase {
        "running" => "Running",
        "stopped" => "Stopped",
        "failed" => "Deployment failed",
        "cancelled" => "Cancelled",
        "queued" => "Queued",
        "switching_traffic" => "Switching Nginx traffic",
        "draining" => "Draining previous requests",
        "rolling_back" => "Restoring previous release",
        _ => phase,
    };
    let instances = list(&value["instances"]);
    let healthy = instances
        .iter()
        .filter(|instance| instance["healthy"] == true)
        .count();
    let mut out = format!(
        "<div class=\"service-runtime\"><p class=\"service-state\" data-phase=\"{}\" role=\"status\">{}</p>",
        escape(phase),
        escape(crate::i18n::t(label))
    );
    if !instances.is_empty() {
        out.push_str(&format!(
            "<span class=\"service-health\">{healthy}/{} {}</span>",
            instances.len(),
            crate::i18n::t("healthy")
        ));
    }
    out.push_str("</div>");
    if value["internalRouteOwned"] == true && ["running", "degraded"].contains(&phase) {
        out.push_str(&format!("<p><code>http://{}.internal</code></p><p>Available to apps in the same MicroNetwork.</p>", escape(text(&value["spec"], "name"))));
    }
    if value["domainRouteOwned"] == true
        && ["running", "degraded"].contains(&phase)
        && !text(&value["spec"], "domain").is_empty()
    {
        let domain = text(&value["spec"], "domain");
        out.push_str(&format!("<a class=\"service-endpoint\" href=\"https://{}\" target=\"_blank\" rel=\"noreferrer\" aria-label=\"{}\">{} ↗</a>",escape(domain),crate::i18n::t("Open domain"),escape(domain)));
    } else if ["running", "degraded"].contains(&phase) {
        let endpoints = instances
            .iter()
            .filter_map(|instance| {
                let port = instance["hostPort"].as_u64().filter(|port| *port > 0)?;
                let ip = text(instance, "ipv4");
                if ip.is_empty() {
                    return None;
                }
                Some(format!(
                    "<span class=\"service-endpoint mono\">{}:{port}</span>",
                    escape(ip)
                ))
            })
            .collect::<String>();
        out.push_str(&endpoints);
    }
    if !text(value, "error").is_empty() {
        out.push_str(&format!(
            "<p role=\"alert\">{}</p>",
            escape(text(value, "error"))
        ));
    }
    if !instances.is_empty() {
        out.push_str(&format!("<details class=\"service-disclosure\" id=\"service-instances\"><summary id=\"service-instances-toggle\">{} <span>{}</span></summary><table class=\"wasm-table service-instances\"><thead><tr><th>VM</th><th>IPv4</th><th>{}</th><th>{}</th></tr></thead><tbody>", crate::i18n::t("Instances"), instances.len(), crate::i18n::t("Port"), crate::i18n::t("Health")));
        for instance in instances {
            out.push_str(&format!(
            "<tr><td><span class=\"service-instance-id\" title=\"{}\">{}</span></td><td>{}</td><td>{}</td><td>{}</td></tr>",
            escape(text(instance, "vmId")),
            escape(text(instance, "vmId")),
            escape(text(instance, "ipv4")),
            display(&instance["hostPort"]),
            if instance["healthy"] == true {
                crate::i18n::t("Healthy")
            } else {
                crate::i18n::t("Unhealthy")
            }
        ));
        }
        out.push_str("</tbody></table></details>");
    }
    if !list(&value["log"]).is_empty() {
        out.push_str(&format!(
        "<details class=\"service-disclosure\" id=\"service-log\"><summary id=\"service-log-toggle\">{}</summary><pre class=\"wasm-log\">{}</pre></details>",
        crate::i18n::t("Deployment log"),
        escape(
            &list(&value["log"])
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        )
    ));
    }
    out
}
fn display(value: &Value) -> String {
    match value {
        Value::Null => "—".into(),
        Value::String(value) => escape(value),
        Value::Bool(value) => crate::i18n::t(if *value { "Yes" } else { "No" }).into(),
        Value::Number(value) => value.to_string(),
        Value::Array(values) => values.iter().map(display).collect::<Vec<_>>().join(", "),
        Value::Object(_) => properties(value),
    }
}
pub fn properties(value: &Value) -> String {
    let Some(object) = value.as_object() else {
        return display(value);
    };
    let mut out = "<dl>".to_string();
    for (key, value) in object {
        if matches!(
            key.as_str(),
            "secret"
                | "token"
                | "secretAccessKey"
                | "accessKeyId"
                | "apiToken"
                | "credentialsFile"
                | "secrets"
        ) {
            continue;
        }
        if key == "usageHistory" {
            continue;
        }
        let _ = write!(
            out,
            "<dt>{}</dt><dd>{}</dd>",
            escape(crate::i18n::t(&friendly(key))),
            display(value)
        );
    }
    out.push_str("</dl>");
    out
}
fn friendly(key: &str) -> String {
    let mut result = String::new();
    for (i, ch) in key.chars().enumerate() {
        if i == 0 {
            result.push(ch.to_ascii_uppercase());
        } else {
            if ch.is_uppercase() {
                result.push(' ');
            }
            result.push(ch);
        }
    }
    result
}

fn vm_detail_actions(app: &App, data: &Value) -> String {
    let id = text(data, "id");
    let encoded = encode(id);
    let state = text(data, "state");
    let queue = text(data, "purpose") == "queue";
    let pool = queue || text(data, "purpose") == "pool";
    let editable = !pool && matches!(state, "created" | "stopped" | "error");
    let mut primary = String::new();
    if editable {
        primary.push_str(&button("Start", "vm-start", id));
    } else if state == "running" {
        primary.push_str(&button("SSH connection", "ssh", id));
    }
    if !pool {
        match state {
            "running" => primary.push_str(&button("Pause", "vm-pause", id)),
            "paused" => primary.push_str(&button("Resume", "vm-resume", id)),
            "pausing" => primary.push_str(&button("Retry pause", "vm-pause", id)),
            "resuming" => primary.push_str(&button("Retry resume", "vm-resume", id)),
            _ => {}
        }
        if matches!(state, "running" | "paused" | "pausing" | "resuming") {
            primary.push_str(&button("Stop", "vm-stop", id));
        }
    }
    if queue {
        primary.push_str("<a class=\"btn\" href=\"#/queues\">Queue</a>");
    }
    let mut menu = String::new();
    if !pool && state == "pausing" {
        menu.push_str(&button("Retry resume", "vm-resume", id));
    } else if !pool && state == "resuming" {
        menu.push_str(&button("Retry pause", "vm-pause", id));
    }
    if editable {
        menu.push_str(&button("Resources", "edit", "vm-resources"));
        menu.push_str(&button("Storage", "edit", "vm-storage"));
    }
    if !pool && (editable || state == "running") {
        for (kind, label) in [
            ("vm-egress", "Egress"),
            ("vm-env", "Environment"),
            ("vm-ports", "Port forwarding"),
            ("vm-shells", "Shells"),
        ] {
            menu.push_str(&button(label, "edit", kind));
        }
    }
    if !pool {
        menu.push_str(&format!(
            "<a class=\"btn\" href=\"#/schedules?vm={encoded}&new=1\" aria-label=\"{} {}\">{}</a>",
            crate::i18n::t("Add schedule"),
            escape(text(data, "name")),
            crate::i18n::t("Add schedule")
        ));
    }
    if state != "running" {
        menu.push_str(&button("SSH connection", "ssh", id));
    }
    menu.push_str(&button("Download SSH key", "ssh-download-key", id));
    if editable {
        menu.push_str("<div class=\"vm-action-divider\"></div>");
        menu.push_str(&request(
            "Delete",
            "DELETE",
            &format!("/api/vms/{encoded}"),
            Value::Null,
        ));
    }
    format!(
        "<fieldset class=\"detail-actions vm-detail-actions\" {}><legend class=\"sr-only\">{}</legend>{primary}<details id=\"vm-more-actions\" class=\"vm-more-actions\"><summary id=\"vm-more-summary\" class=\"btn\" aria-label=\"{}\">{}<svg viewBox=\"0 0 16 16\" width=\"12\" height=\"12\" aria-hidden=\"true\"><path d=\"m4 6 4 4 4-4\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/></svg></summary><div class=\"vm-action-menu\" role=\"group\" aria-label=\"{}\">{menu}</div></details></fieldset>",
        if app.detail_loading || app.busy {
            "disabled"
        } else {
            ""
        },
        crate::i18n::t("Actions"),
        crate::i18n::t("More VM actions"),
        crate::i18n::t("More"),
        crate::i18n::t("VM settings")
    )
}

pub fn detail_actions(app: &App, data: &Value) -> String {
    if app.route == "vms" {
        return vm_detail_actions(app, data);
    }
    let id = text(data, "id");
    let encoded = encode(id);
    let section = app.route.as_str();
    let mut out = format!(
        "<fieldset class=\"wasm-actions\" style=\"border:0;padding:0\" {}>",
        if app.detail_loading { "disabled" } else { "" }
    );
    match section {
        "pools" | "schedules" | "security-groups" | "secrets" => {
            out.push_str(&crate::advanced::actions(app, data))
        }
        "services" => {
            if app
                .service_deployment
                .as_ref()
                .is_some_and(crate::advanced::services::owns_resources)
            {
                let stop = request(
                    "Stop service",
                    "POST",
                    &format!("/api/micro-services/{encoded}/stop"),
                    Value::Null,
                )
                .replace("type=\"button\"", "type=\"button\" id=\"service-stop\"");
                if app.busy
                    || app
                        .service_deployment
                        .as_ref()
                        .is_some_and(crate::deploy::busy)
                {
                    out.push_str(&stop.replace("type=\"button\"", "type=\"button\" disabled"));
                } else {
                    out.push_str(&stop);
                }
            } else if app.service_deployment.is_some() {
                out.push_str(&button("Deploy", "edit", "service-deploy"));
            }
        }
        "networks" => {
            out.push_str(&button("Edit", "edit", "networks"));
            out.push_str(&request(
                "Delete",
                "DELETE",
                &format!("/api/micro-networks/{encoded}"),
                Value::Null,
            ));
        }
        "storages" => out.push_str(&request(
            "Delete",
            "DELETE",
            &format!("/api/micro-storages/{encoded}"),
            Value::Null,
        )),
        "shells" => {
            out.push_str(&button("Publish revision", "edit", "shells"));
            out.push_str(&request(
                "Delete",
                "DELETE",
                &format!("/api/shells/{encoded}"),
                Value::Null,
            ));
            for revision in list(&data["revisions"]) {
                out.push_str(&button(
                    &format!("Revision {}", revision["version"]),
                    "shell-revision",
                    &format!("{id}/{}", text(revision, "id")),
                ));
            }
        }
        "images" => {
            let alias = encode(text(data, "alias"));
            for (action, label) in [("package", "Download package"), ("install", "Install")] {
                out.push_str(&request(
                    label,
                    "POST",
                    &format!("/api/images/{alias}/{action}"),
                    Value::Null,
                ));
            }
            out.push_str(&button("Kernel", "edit", "image-kernel"));
            out.push_str(&request(
                "Delete image",
                "DELETE",
                &format!("/api/images/{alias}"),
                Value::Null,
            ));
            out.push_str(&request(
                "Delete package",
                "DELETE",
                &format!("/api/images/{alias}/package"),
                Value::Null,
            ));
        }
        "kernels" => {
            let version = encode(text(data, "version"));
            out.push_str(&request(
                "Build kernel",
                "POST",
                &format!("/api/kernels/{version}/install"),
                Value::Null,
            ));
            out.push_str(&request(
                "Delete",
                "DELETE",
                &format!("/api/kernels/{version}"),
                Value::Null,
            ));
        }
        "pipe/auth" => {
            if text(data, "provider") == "firecrab" {
                out.push_str("<a class=\"btn\" href=\"https://console.firecrab.dev/#account\" target=\"_blank\" rel=\"noreferrer\">Firecrab Console</a>");
            }
            if list(&data["services"])
                .iter()
                .any(|service| service == "storage")
            {
                out.push_str("<a class=\"btn\" href=\"#/pipe/storage\">S3</a>");
            }
            out.push_str(&button(
                "Update credentials",
                "create",
                text(data, "provider"),
            ));
            out.push_str(&request(
                "Disconnect",
                "DELETE",
                &format!("/api/pipe/auth/{}", encode(text(data, "provider"))),
                Value::Null,
            ));
        }
        "pipe/registry" => {
            out.push_str(&button("Update token", "edit", "registry"));
            out.push_str(&request(
                "Disconnect",
                "DELETE",
                &format!("/api/pipe/registries/{}", encode(text(data, "registry"))),
                Value::Null,
            ));
            out.push_str(&button(
                "Repositories",
                "registry-repositories",
                text(data, "registry"),
            ));
        }
        "pipe/database" => {
            out.push_str(&request(
                "Disconnect",
                "DELETE",
                &format!("/api/pipe/databases/{encoded}"),
                Value::Null,
            ));
            out.push_str(&button("Tables", "database-tables", id));
        }
        _ => {}
    }
    out.push_str("</fieldset>");
    out
}
fn public_address(url: &str) -> String {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return String::new();
    }
    format!(
        "<p class=\"wasm-actions\"><a href=\"{}\" target=\"_blank\" rel=\"noreferrer\">{}</a>{}</p>",
        escape(url),
        escape(url),
        button("Copy", "copy-text", url)
    )
}
fn pipe_icon(id: &str) -> String {
    let path = match id {
        "auth" => "M16 7a4 4 0 1 1-8 0 4 4 0 0 1 8 0ZM4 21v-2a6 6 0 0 1 6-6h4a6 6 0 0 1 6 6v2",
        "ai" => "m12 3 2.5 6.5L21 12l-6.5 2.5L12 21l-2.5-6.5L3 12l6.5-2.5L12 3ZM20 2v4M18 4h4",
        "storage" => {
            "M3 7h6l2 2h10v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v2"
        }
        "route" => "M3 7h18m-4-4 4 4-4 4M21 17H3m4-4-4 4 4 4",
        "registry" => "m12 3 9 5v8l-9 5-9-5V8l9-5Zm0 10 9-5M12 13 3 8M12 13v8M7.5 5.5l9 5v5",
        "database" => {
            "M21 5c0 2.2-4 4-9 4S3 7.2 3 5s4-4 9-4 9 1.8 9 4ZM3 5v14c0 2.2 4 4 9 4s9-1.8 9-4V5M3 12c0 2.2 4 4 9 4s9-1.8 9-4"
        }
        "quick-tunnel" => "m13 2-9 12h7l-1 8 10-12h-7l1-8Z",
        "firecrab-tunnel" => {
            return "<img src=\"/firecrab.png\" width=\"20\" height=\"20\" alt=\"\">".into();
        }
        "named-tunnel" => {
            return "<img src=\"/cloudflare.svg\" width=\"24\" height=\"20\" alt=\"\">".into();
        }
        _ => return String::new(),
    };
    format!(
        "<svg width=\"20\" height=\"20\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.75\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\"><path d=\"{path}\"/></svg>"
    )
}

fn pipe_cards(app: &App) -> String {
    let mut out = "<div class=\"wasm-cards\">".to_string();
    for (id, label, description) in [
        ("auth", "Account", "AWS, GitHub and Firecrab accounts"),
        ("ai", "Pipe AI", "Connect and manage AI accounts"),
        ("storage", "PipeStorage", "Access your files in AWS"),
        ("route", "Routing", "Route traffic to applications"),
        ("registry", "Registry", "Connect and pull OCI images"),
        ("database", "Database", "Connect and browse tables"),
        (
            "firecrab-tunnel",
            "Firecrab address",
            "Publish an application address",
        ),
        (
            "quick-tunnel",
            "Quick tunnel",
            "Temporary public application URLs",
        ),
        ("named-tunnel", "Cloudflare", "Connect a custom domain"),
    ] {
        let _ = write!(
            out,
            "<a class=\"wasm-card pipe-card\" href=\"#/pipe/{id}\"><div class=\"pipe-card-heading\"><span class=\"pipe-card-icon\" aria-hidden=\"true\">{}</span><h2>{}</h2></div><p>{}</p></a>",
            pipe_icon(id),
            crate::i18n::t(label),
            crate::i18n::t(description)
        );
    }
    out.push_str("</div>");
    let _ = app;
    out
}
fn pipe(app: &App, card: &str) -> String {
    if card == "database" {
        return crate::databases::content(app);
    }
    let mut out = "<p><a href=\"#/pipe\">← Pipe</a></p><div class=\"wasm-actions\">".to_string();
    match card {
        "auth" => {
            out.push_str(&button("Connect GitHub", "create", "github"));
            out.push_str(&button("Connect AWS", "create", "aws"));
            out.push_str(&button("Connect Firecrab", "create", "firecrab"));
        }
        "route" => {
            out.push_str(&button("Edit routes", "create", "route"));
            out.push_str(&request(
                "Disable Pipe",
                "POST",
                "/api/pipe/disable",
                Value::Null,
            ));
            for service in ["ingress", "tunnel"] {
                for action in ["start", "stop", "restart"] {
                    out.push_str(&request(
                        &format!("{service}: {action}"),
                        "POST",
                        "/api/pipe/service",
                        json!({"service":service,"action":action}),
                    ));
                }
            }
        }
        "registry" => {
            out.push_str(&button("Connect registry", "create", "registry"));
            out.push_str(&button("Pull image", "create", "registry-pull"));
            out.push_str(&button("Cached images", "registry-images", ""));
        }
        "database" => out.push_str(&button("Connect database", "create", "database")),
        "quick-tunnel" | "firecrab-tunnel" | "named-tunnel" => {
            out.push_str(&button("Connect", "create", card))
        }
        _ => {}
    }
    out.push_str("</div>");
    if app.resource["nginxAvailable"] == false
        && ["route", "named-tunnel", "quick-tunnel", "firecrab-tunnel"].contains(&card)
    {
        out.push_str(&format!(
            "<p role=\"alert\">{}</p>",
            tr(
                app,
                "Reinstall the Firecrab host bundle to restore the ingress engine.",
                "Ingress 엔진을 복구하려면 Firecrab 호스트 번들을 다시 설치한다."
            )
        ));
    }
    if app.resource["cloudflaredAvailable"] == false
        && ["named-tunnel", "quick-tunnel", "firecrab-tunnel"].contains(&card)
    {
        out.push_str(&format!(
            "<p role=\"alert\">{}</p>",
            tr(
                app,
                "Run firecrab pipe install on the API host to install the Cloudflare connector.",
                "API 호스트에서 firecrab pipe install을 실행해 Cloudflare 연결기를 설치한다."
            )
        ));
    }
    match card {
        "auth" => out.push_str(&resource_table(
            app,
            &app.resource,
            &[
                ("provider", "Provider"),
                ("account", "Account"),
                ("registry", "Registry"),
            ],
        )),
        "registry" => out.push_str(&resource_table(
            app,
            &app.resource,
            &[
                ("registry", "Registry"),
                ("username", "Username"),
                ("provider", "Provider"),
            ],
        )),
        "database" => out.push_str(&resource_table(
            app,
            &app.resource,
            &[
                ("name", "Name"),
                ("provider", "Provider"),
                ("engine", "Engine"),
                ("host", "Host"),
            ],
        )),
        "quick-tunnel" => {
            for tunnel in list(&app.resource["quickTunnels"]) {
                let id = encode(text(tunnel, "id"));
                out.push_str(&public_address(text(tunnel, "publicUrl")));
                out.push_str(&button("Edit", "edit-quick-tunnel", text(tunnel, "id")));
                let _ = write!(out, "<section class=\"wasm-card\">{}", properties(tunnel));
                for action in ["start", "stop", "restart"] {
                    out.push_str(&request(
                        action,
                        "POST",
                        &format!("/api/pipe/quick-tunnels/{id}/service"),
                        json!({"action":action}),
                    ));
                }
                out.push_str(&request(
                    "Delete",
                    "DELETE",
                    &format!("/api/pipe/quick-tunnels/{id}"),
                    Value::Null,
                ));
                out.push_str("</section>");
            }
        }
        "firecrab-tunnel" => {
            for address in list(&app.resource["firecrabTunnels"]) {
                out.push_str(&public_address(text(address, "publicUrl")));
                out.push_str(&button(
                    "Edit",
                    "edit-firecrab-tunnel",
                    text(address, "subdomain"),
                ));
                let _ = write!(
                    out,
                    "<section class=\"wasm-card\">{}{}</section>",
                    properties(address),
                    request(
                        "Delete",
                        "DELETE",
                        &format!(
                            "/api/pipe/firecrab-tunnels/{}",
                            encode(text(address, "subdomain"))
                        ),
                        Value::Null
                    )
                );
            }
        }
        "named-tunnel" => {
            let hostname = text(&app.resource["config"]["tunnel"], "hostname");
            if !hostname.is_empty() {
                out.push_str(&public_address(&format!("https://{hostname}")));
            }
            for (action, label) in [("start", "Start"), ("stop", "Stop"), ("restart", "Restart")] {
                out.push_str(&request(
                    label,
                    "POST",
                    "/api/pipe/service",
                    json!({"service":"tunnel","action":action}),
                ));
            }
            out.push_str(&properties(&app.resource));
            out.push_str(&request(
                "Disconnect Cloudflare",
                "POST",
                "/api/pipe/cloudflare/disconnect",
                Value::Null,
            ));
        }
        _ => out.push_str(&properties(&app.resource)),
    }
    out
}
pub fn host_meters(app: &App) -> String {
    let host = app.cache.get("/api/host").unwrap_or(&Value::Null);
    let percent = |total: &str, available: &str| -> String {
        match (host[total].as_f64(), host[available].as_f64()) {
            (Some(total), Some(available)) if total > 0.0 => {
                format!("{:.0}%", (1.0 - available / total).clamp(0.0, 1.0) * 100.0)
            }
            _ => "—".into(),
        }
    };
    let load = host["loadAverage1m"]
        .as_f64()
        .map(|v| format!("{v:.2}"))
        .unwrap_or("—".into());
    format!(
        "<div class=\"host-card-head\">Host</div><div>CPU <span class=\"host-meter-value\" id=\"host-load\">{load}</span></div><div>Memory <span class=\"host-meter-value\">{}</span></div><div>Disk <span class=\"host-meter-value\">{}</span></div>",
        percent("memoryTotalMib", "memoryAvailableMib"),
        percent("diskTotalGib", "diskAvailableGib")
    )
}
fn host(app: &App) -> String {
    let mut out = format!(
        "<section class=\"host-info wasm-details\" aria-label=\"Host info\"><h2>{}</h2>{}</section>",
        view(app, "Host info", "호스트 정보"),
        properties(&app.resource)
    );
    out.push_str("<section class=\"host-info wasm-details\" role=\"region\" aria-label=\"Firecrab\"><h2>Firecrab</h2>");
    let info = app.cache.get("/api/info").unwrap_or(&Value::Null);
    if info.is_null() || info["status"] == "unavailable" {
        out.push_str(view(
            app,
            "Could not load the Firecrab info.",
            "Firecrab 정보를 불러오지 못했습니다.",
        ));
    } else {
        out.push_str("<dl>");
        for (label, key) in [
            ("version", "version"),
            ("firecracker", "firecrackerVersion"),
            ("API service", "apiService"),
            ("helper service", "netHelperService"),
            ("prefix", "prefix"),
            ("datadir", "datadir"),
            ("confdir", "confdir"),
            ("unitdir", "unitdir"),
            ("api", "apiBase"),
        ] {
            let value = text(info, key);
            let value = if value.is_empty() && key == "firecrackerVersion" {
                view(app, "Unavailable", "확인 불가")
            } else if (value.is_empty() || value == "unknown") && key.ends_with("Service") {
                view(app, "unknown", "확인 불가")
            } else {
                value
            };
            let unit = if key == "netHelperService" {
                text(info, "helperUnit")
            } else {
                ""
            };
            let class = if value == "failed" { "error" } else { "" };
            let _ = write!(
                out,
                "<dt>{label}</dt><dd class=\"{class}\" title=\"{}\">{}</dd>",
                escape(unit),
                escape(value)
            );
        }
        out.push_str("</dl>");
    }
    out.push_str("</section>");
    if let Some(value) = app.cache.get("/api/update") {
        let _ = write!(
            out,
            "<section class=\"wasm-details\"><h2>{}</h2>{}</section>",
            crate::i18n::t("Updates"),
            properties(value)
        );
    }
    out.push_str(&button("Updates", "update-open", ""));
    if let Some(manager) = app.cache.get("manager") {
        let pending = app.workflows.manager_pending;
        let mut apply = button(
            if pending {
                "Applying…"
            } else {
                "Apply resources"
            },
            "manager-apply",
            "",
        );
        if pending {
            apply = apply.replace("type=\"button\"", "type=\"button\" disabled");
        }
        let available = manager["host"]["diskAvailableGib"]
            .as_u64()
            .unwrap_or_default();
        let _ = write!(
            out,
            "<section class=\"wasm-details\"><h2>microManager</h2>{}<p>{available} GiB disk available</p>{}{}{}</section>",
            properties(&manager["settings"]),
            if manager["sharedWsl"] == true {
                "<p>CPU, RAM, and networking affect every WSL2 distribution.</p>"
            } else {
                ""
            },
            button("Edit resources", "manager-edit", ""),
            apply
        );
    }
    out
}
fn console(app: &App, id: &str) -> String {
    crate::terminal::html(app, id)
}

pub fn palette(app: &App) -> String {
    format!(
        "<div class=\"wasm-palette-backdrop\" data-action=\"palette-close\"></div><dialog open class=\"wasm-palette\" aria-label=\"{}\"><h2>{}</h2><input id=\"palette-query\" type=\"search\" role=\"combobox\" aria-expanded=\"true\" aria-controls=\"palette-items\" aria-autocomplete=\"list\" aria-label=\"{}\"><div role=\"group\" aria-label=\"{group}\"><div id=\"palette-items\" role=\"listbox\">{}</div></div>{}</dialog>",
        crate::i18n::t("Command palette"),
        crate::i18n::t("Command palette"),
        crate::i18n::t("Search destinations"),
        crate::details::palette_items(app),
        button("Close", "palette-close", ""),
        group = tr(app, "Go to", "이동")
    )
}
