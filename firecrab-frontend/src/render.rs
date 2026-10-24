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
            let _ = write!(
                nav,
                "<li><a class=\"sidebar-item {}\" href=\"#/{id}\" {} aria-label=\"{}\"><span class=\"sidebar-label\">{}</span>{}</a></li>",
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
        "secrets" => button("New secret", "create", "secrets"),
        _ => String::new(),
    };
    let panel = crate::details::panel(app);
    let panel_width = app.panel_width;
    let panel_hidden = if app.panel_open && app.route != "vms/new" {
        ""
    } else {
        "hidden"
    };
    let panel_toggle = format!(
        "<button type=\"button\" class=\"btn\" id=\"panel-toggle\" data-action=\"panel-toggle\">{}</button>",
        crate::i18n::t(if app.panel_open {
            "Hide details"
        } else {
            "Show details"
        })
    );
    let category = match section {
        "vms" | "services" | "pools" | "schedules" => tr(app, "Compute", "컴퓨팅"),
        "networks" | "pipe" | "security-groups" => tr(app, "Network", "네트워크"),
        "storages" | "images" | "kernels" | "secrets" => tr(app, "Data", "데이터"),
        _ => tr(app, "System", "시스템"),
    };
    let meters = host_meters(app);
    let search_label = tr(app, "Search and go", "검색 및 이동");
    let panel_label = panel_label(app);
    format!(
        "<div class=\"app-frame\"><button class=\"icon-btn nav-toggle\" aria-label=\"Open menu\" data-action=\"menu\">☰</button><nav class=\"sidebar\" aria-label=\"{}\"><div class=\"sidebar-brand\"><span class=\"sidebar-logo\"><img src=\"/firecrab.png\" width=\"20\" height=\"20\" alt=\"\"></span><span class=\"sidebar-brand-text\"><span class=\"sidebar-name\">firecrab</span><span class=\"sidebar-host\" id=\"sidebar-platform\">private microvm cloud</span></span></div><button class=\"sidebar-search\" aria-label=\"{search_label}\" data-action=\"palette\">{} <kbd>⌘ K</kbd></button><div class=\"sidebar-groups\">{nav}</div><section class=\"host-card\" id=\"host-status\" aria-label=\"Host status\">{meters}</section><div class=\"sidebar-foot\"><span id=\"update-indicator\"></span><a href=\"#/host\" class=\"btn\">{}</a><select id=\"language\" aria-label=\"Language\"><option value=\"en\" {}>English</option><option value=\"ko\" {}>한국어</option></select></div></nav><div class=\"app-workspace\"><main class=\"screen\"><header class=\"screen-head screen-header\"><div class=\"screen-heading\"><span class=\"screen-group\">{category}</span><h1>{title}</h1><span id=\"api-status\"></span></div><div class=\"wasm-actions\">{actions}{}{panel_toggle}</div></header><div id=\"messages\" aria-live=\"polite\">{}</div><div id=\"toolbar\">{toolbar}</div><div class=\"wasm-layout\"><div id=\"content\" class=\"wasm-content screen-main screen-body\" data-loaded=\"{}\">{}</div><aside id=\"detail-panel\" aria-label=\"{panel_label}\" class=\"wasm-panel detail-panel\" style=\"width:{panel_width}px\" {panel_hidden}>{panel}</aside></div></main></div></div><div id=\"menu-root\"></div><div id=\"palette-root\"></div><div id=\"tooltip-root\"></div><div id=\"modal-root\"></div>",
        tr(app, "Main navigation", "주요 메뉴"),
        tr(app, "Search and go", "검색 및 이동"),
        tr(app, "Host and updates", "호스트 및 업데이트"),
        if app.locale == "en" { "selected" } else { "" },
        if app.locale == "ko" { "selected" } else { "" },
        button(tr(app, "Refresh", "새로 고침"), "refresh", ""),
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
        "pools" | "schedules" | "security-groups" | "secrets" => crate::advanced::content(app),
        "services" => resource_table(
            app,
            &app.resource,
            &[
                ("name", "Name"),
                ("image", "Image"),
                ("port", "Port"),
                ("replicas", "Replicas"),
            ],
        ),
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
    let action = if vm.extra.get("purpose").and_then(Value::as_str) == Some("pool") {
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
            if app.route == "services" {
                tr(
                    app,
                    "No service configurations yet.",
                    "서비스 구성이 없습니다.",
                )
            } else {
                tr(app, "No items yet.", "항목이 없습니다.")
            }
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
    for row in rows {
        let id = ["id", "alias", "version", "provider", "registry"]
            .iter()
            .find_map(|key| row[key].as_str())
            .unwrap_or("");
        let selected = app.detail.as_ref().is_some_and(|detail| {
            ["id", "alias", "version", "provider", "registry"]
                .iter()
                .any(|key| detail[key] == id)
        });
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
            if app.route == "services" {
                let name = text(row, "name");
                let edit = if app.locale == "ko" {
                    format!("{name} 수정")
                } else {
                    format!("Edit {name}")
                };
                let run = if app.locale == "ko" {
                    format!("{name} 실행")
                } else {
                    format!("Run {name}")
                };
                format!(
                    "{}{}{}{}",
                    button(tr(app, "Details", "상세"), "detail", id),
                    button(&edit, "resource-edit", id),
                    button(&run, "service-run", id).replace(
                        "type=\"button\"",
                        if app.form.is_some() {
                            "type=\"button\" disabled"
                        } else {
                            "type=\"button\""
                        }
                    ),
                    request(
                        &format!("Delete {name}"),
                        "DELETE",
                        &format!("/api/micro-services/{}", encode(id)),
                        Value::Null
                    )
                )
            } else {
                button(tr(app, "Details", "상세"), "detail", id)
            }
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
        return "<p role=\"status\">Not deployed</p>".into();
    }
    let phase = text(value, "phase");
    let label = match phase {
        "running" => "Running",
        "stopped" => "Stopped",
        "failed" => "Deployment failed",
        _ => phase,
    };
    let mut out = format!("<p role=\"status\">{}</p>", escape(crate::i18n::t(label)));
    if value["domainRouteOwned"] == true
        && ["running", "degraded"].contains(&phase)
        && !text(&value["spec"], "domain").is_empty()
    {
        let domain = text(&value["spec"], "domain");
        out.push_str(&format!("<p>Domain routing is ready.</p><a class=\"btn\" href=\"https://{}\" target=\"_blank\" rel=\"noreferrer\">{}</a>",escape(domain),crate::i18n::t("Open domain")));
    }
    if !text(value, "error").is_empty() {
        out.push_str(&format!(
            "<p role=\"alert\">{}</p>",
            escape(text(value, "error"))
        ));
    }
    out.push_str("<table class=\"wasm-table\"><thead><tr><th>VM</th><th>IPv4</th><th>Port</th><th>Health</th></tr></thead><tbody>");
    for instance in list(&value["instances"]) {
        out.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            escape(text(instance, "vmId")),
            escape(text(instance, "ipv4")),
            display(&instance["hostPort"]),
            if instance["healthy"] == true {
                "Healthy"
            } else {
                "Unhealthy"
            }
        ));
    }
    out.push_str("</tbody></table>");
    out.push_str(&format!(
        "<pre class=\"wasm-log\">{}</pre>",
        escape(
            &list(&value["log"])
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        )
    ));
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

pub fn detail_actions(app: &App, data: &Value) -> String {
    let id = text(data, "id");
    let encoded = encode(id);
    let section = app.route.as_str();
    let mut out = format!(
        "<fieldset class=\"wasm-actions\" style=\"border:0;padding:0\" {}>",
        if app.detail_loading { "disabled" } else { "" }
    );
    match section {
        "vms" => {
            let state = text(data, "state");
            let pool = text(data, "purpose") == "pool";
            let editable = !pool && matches!(state, "created" | "stopped" | "error");
            if editable {
                out.push_str(&button("Start", "vm-start", id));
                out.push_str(&button("Resources", "edit", "vm-resources"));
                out.push_str(&button("Storage", "edit", "vm-storage"));
                out.push_str(&request(
                    "Delete",
                    "DELETE",
                    &format!("/api/vms/{encoded}"),
                    Value::Null,
                ));
            }
            if !pool && state == "running" {
                out.push_str(&button("Pause", "vm-pause", id));
                out.push_str(&button("Stop", "vm-stop", id));
            }
            if !pool && (editable || state == "running") {
                for (kind, label) in [
                    ("vm-egress", "Egress"),
                    ("vm-env", "Environment"),
                    ("vm-ports", "Port forwarding"),
                    ("vm-shells", "Shells"),
                ] {
                    out.push_str(&button(label, "edit", kind));
                }
            }
            if !pool && ["pausing", "resuming"].contains(&state) {
                out.push_str(&button("Retry pause", "vm-pause", id));
                out.push_str(&button("Retry resume", "vm-resume", id));
            }
            if !pool && ["paused", "pausing", "resuming"].contains(&state) {
                if state == "paused" {
                    out.push_str(&button("Resume", "vm-resume", id));
                }
                out.push_str(&button("Stop", "vm-stop", id));
            }
            if !pool {
                out.push_str(&format!("<a class=\"btn\" href=\"#/schedules?vm={encoded}&new=1\" aria-label=\"Add schedule for {}\">Add schedule</a>",escape(text(data,"name"))));
            }
            if pool {
                out.push_str("<span class=\"badge\">Pool</span>");
            }
            out.push_str(&button("Logs", "logs", id));
            out.push_str(&button("Download SSH key", "ssh-download-key", id));
            out.push_str(&button("SSH connection", "ssh", id));
        }
        "pools" | "schedules" | "security-groups" | "secrets" => {
            out.push_str(&crate::advanced::actions(app, data))
        }
        "services" => {
            out.push_str(&button("Edit", "edit", "services"));
            out.push_str(&button("Deploy", "edit", "service-deploy"));
            if app.service_deployment.as_ref().is_some_and(|value| {
                matches!(
                    text(value, "phase"),
                    "queued" | "starting" | "running" | "draining"
                )
            }) {
                out.push_str(&request(
                    "Stop service",
                    "POST",
                    &format!("/api/micro-services/{encoded}/stop"),
                    Value::Null,
                ));
            }
            out.push_str(&request(
                "Delete",
                "DELETE",
                &format!("/api/micro-services/{encoded}"),
                Value::Null,
            ));
            out.push_str(&button("Deployment status", "deployment", id));
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
                "Install",
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
            "<a class=\"wasm-card\" href=\"#/pipe/{id}\"><h2>{}</h2><p>{}</p></a>",
            crate::i18n::t(label),
            crate::i18n::t(description)
        );
    }
    out.push_str("</div>");
    let _ = app;
    out
}
fn pipe(app: &App, card: &str) -> String {
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
                "Reinstall the Firecrab host bundle to restore the Cloudflare connector.",
                "Cloudflare 연결기를 복구하려면 Firecrab 호스트 번들을 다시 설치한다."
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
