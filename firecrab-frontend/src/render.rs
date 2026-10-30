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
    Key,
    Logs,
    Events,
    Alerts,
    Export,
    Save,
    Play,
    Refresh,
    Add,
    Reveal,
    Hide,
    Edit,
    Delete,
    Close,
    AiKey,
    Start,
    Stop,
    Pause,
    Retry,
    Restart,
    Log,
    Copy,
    Download,
    Upload,
    More,
    Terminal,
    Ssh,
    Deploy,
    Rollback,
    Details,
    Attach,
    Detach,
    Drain,
    Open,
}

impl Icon {
    pub fn svg(&self) -> String {
        let path = match self {
            Self::Key => {
                "M21 2l-2 2m-7.6 7.6a5.5 5.5 0 1 1-7.8 7.8 5.5 5.5 0 0 1 7.8-7.8ZM11.4 11.6 19 4m-3 3 3 3 3-3-3-3"
            }
            Self::Logs => "M4 4h16v16H4zM7 8h10M7 12h10M7 16h7",
            Self::Events => "M4 6h2M9 6h11M4 12h2M9 12h11M4 18h2M9 18h11",
            Self::Alerts => "M18 8a6 6 0 0 0-12 0c0 7-2 7-2 9h16c0-2-2-2-2-9ZM10 20h4",
            Self::Export => "M12 15V3m-4 4 4-4 4 4M5 17v3h14v-3",
            Self::Save => {
                "M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h12l4 4v12a2 2 0 0 1-2 2ZM7 3v6h10V3M7 21v-8h10v8"
            }
            Self::Play => "m8 5 12 7-12 7Z",
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
            Self::AiKey => {
                "M21 8a5 5 0 1 1-10 0 5 5 0 0 1 10 0ZM12.5 11.5 3 21H2v-3l3-3h3v-3l4.5-.5M16 7h.01"
            }
            Self::Start => "M8 5v14l11-7z",
            Self::Stop => "M7 7h10v10H7z",
            Self::Pause => "M8 5v14M16 5v14",
            Self::Retry | Self::Restart => "M3 12a9 9 0 1 0 3-6.7L3 8M3 3v5h5",
            Self::Log => {
                "M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8ZM14 2v6h6M8 13h8M8 17h5"
            }
            Self::Copy => "M9 9h11v11H9zM5 15H4V4h11v1",
            Self::Download => "M12 3v12m-5-5 5 5 5-5M5 21h14",
            Self::Upload => "M12 21V9m-5 5 5-5 5 5M5 3h14",
            Self::More => {
                "M4 12a1 1 0 1 0 2 0 1 1 0 1 0-2 0M11 12a1 1 0 1 0 2 0 1 1 0 1 0-2 0M18 12a1 1 0 1 0 2 0 1 1 0 1 0-2 0"
            }
            Self::Terminal => "m4 17 6-6-6-6M12 19h8",
            Self::Ssh => {
                "M15.5 7.5 19 4m-2 2 2 2M10.5 11.5a4.5 4.5 0 1 1-6.4 6.4 4.5 4.5 0 0 1 6.4-6.4Zm0 0L15.5 6.5"
            }
            Self::Deploy => {
                "M5 15c-1.5 1.3-2 5-2 5s3.7-.5 5-2c.7-.8.7-2.1-.1-2.9a2.2 2.2 0 0 0-2.9-.1ZM12 15l-3-3a22 22 0 0 1 2-3.9A12.9 12.9 0 0 1 22 2c0 2.7-.8 7.5-6 11a22.4 22.4 0 0 1-4 2Z"
            }
            Self::Rollback => "M3 7v6h6M21 17a9 9 0 0 0-15-6.7L3 13",
            Self::Details => "M3 4h18v16H3zM15 4v16",
            Self::Attach => {
                "m21 12-9 9a6 6 0 0 1-8.5-8.5l9-9a4 4 0 0 1 5.7 5.7l-9 9a2 2 0 0 1-2.8-2.8L15 7"
            }
            Self::Detach => "M8 12h8M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0Z",
            Self::Drain => "M9 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h4M16 17l5-5-5-5M21 12H9",
            Self::Open => {
                "M15 3h6v6M10 14 21 3M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"
            }
        };
        format!(
            "<svg viewBox=\"0 0 24 24\" aria-hidden=\"true\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.75\" stroke-linecap=\"round\" stroke-linejoin=\"round\"><path d=\"{path}\"/></svg>"
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

/// An icon button that shows or hides something: `expanded` is its state, `controls` the id of
/// what it shows.
pub fn icon_toggle(
    label: &str,
    action: &str,
    expanded: bool,
    controls: &str,
    icon: Icon,
) -> String {
    let label = escape(crate::i18n::t(label));
    format!(
        "<button type=\"button\" class=\"icon-btn\" aria-label=\"{label}\" title=\"{label}\" aria-expanded=\"{expanded}\" aria-controls=\"{}\" data-action=\"{}\" data-id=\"\">{}</button>",
        escape(controls),
        escape(action),
        icon.svg()
    )
}

/// Icon-only counterpart of `request`: same data attributes, the label sits in `aria-label` and `title`.
pub fn icon_request_with(label: &str, method: &str, path: &str, body: Value, icon: Icon) -> String {
    let label = escape(crate::i18n::t(label));
    format!(
        "<button type=\"button\" class=\"icon-btn{}\" aria-label=\"{label}\" title=\"{label}\" data-action=\"request\" data-method=\"{}\" data-path=\"{}\" data-body=\"{}\">{}</button>",
        if method == "DELETE" { " danger" } else { "" },
        method,
        escape(path),
        escape(&body.to_string()),
        icon.svg()
    )
}

pub fn icon_request(label: &str, path: &str) -> String {
    icon_request_with(label, "DELETE", path, Value::Null, Icon::Delete)
}

/// Icon-only link. `new_tab` marks a link that leaves the dashboard.
pub fn icon_link(label: &str, href: &str, new_tab: bool, icon: Icon) -> String {
    let label = escape(crate::i18n::t(label));
    format!(
        "<a class=\"icon-btn\" href=\"{}\"{} aria-label=\"{label}\" title=\"{label}\">{}</a>",
        escape(href),
        if new_tab {
            " target=\"_blank\" rel=\"noreferrer\""
        } else {
            ""
        },
        icon.svg()
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

pub fn navigation_icon(name: &str) -> String {
    let path = match name {
        "vms" => {
            "M5 3h14a2 2 0 0 1 2 2v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2Zm0 10h14a2 2 0 0 1 2 2v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4a2 2 0 0 1 2-2ZM7 7h.01M7 17h.01M11 7h6M11 17h6"
        }
        "pools" => "m12 3 10 5-10 5L2 8l10-5ZM2 12l10 5 10-5M2 16l10 5 10-5",
        "schedules" => {
            "M8 2v4M16 2v4M3 10h18M5 4h14a2 2 0 0 1 2 2v13a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2ZM12 13v3l2 1"
        }
        "services" => "m12 3 9 5v8l-9 5-9-5V8l9-5Zm0 10 9-5M12 13 3 8M12 13v8M7.5 5.5l9 5v5",
        "stacks" => "m12 3 9 5-9 5-9-5 9-5ZM3 12l9 5 9-5M3 16l9 5 9-5",
        "cluster" => "M4 4h6v6H4zM14 4h6v6h-6zM4 14h6v6H4zM14 14h6v6h-6z",
        "networks" => "M9 3h6v6H9zM3 15h6v6H3zM15 15h6v6h-6zM12 9v3M6 15v-3h12v3",
        "security" | "security-groups" => {
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
            "M22 12H2M5.5 5.1 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.5-6.9A2 2 0 0 0 16.8 4H7.2a2 2 0 0 0-1.7 1.1ZM6 16h.01M10 16h.01"
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
        "memory" => "M4 6h16v12H4zM8 9v6M12 9v6M16 9v6M8 3v3M16 3v3M8 18v3M16 18v3M1 10h3M20 10h3",
        _ => return String::new(),
    };
    format!(
        "<svg width=\"16\" height=\"16\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.75\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\"><path d=\"{path}\"/></svg>"
    )
}

/// The value of an empty cell.
const NONE: &str = "<span class=\"none\">—</span>";

// The list parts below are for the screen tasks that follow; until a screen calls one it has no caller.
// Text parameters are plain text and are escaped here. `parts`, `actions` and `action` are markup.

/// A status as a 7px dot and text. `tone`: `ok`, `busy`, `error`, `off` or `none` (a ring).
pub fn status_dot(tone: &str, label: &str) -> String {
    format!(
        "<span class=\"status\" data-tone=\"{}\"><span class=\"status-dot\" aria-hidden=\"true\"></span>{}</span>",
        escape(tone),
        escape(label)
    )
}

/// The first cell of a table row: a name with an optional line under it.
/// `sub_tone` is `""`, `"error"` or `"busy"`. An empty `sub` leaves the line out.
pub fn name_cell(title: &str, sub: &str, sub_tone: &str, mono: bool) -> String {
    cell_name(
        &format!(
            "<b class=\"{}\">{}</b>",
            if mono { "mono" } else { "" },
            escape(title)
        ),
        sub,
        "",
        sub_tone,
    )
}

/// A name cell for text that a narrow column cuts short: the name and the line under it carry
/// the whole text as their tooltip.
pub fn name_cell_cut(title: &str, sub: &str, mono: bool) -> String {
    cell_name(
        &format!(
            "<b class=\"{}\" title=\"{}\">{}</b>",
            if mono { "mono" } else { "" },
            escape(title),
            escape(title)
        ),
        sub,
        sub,
        "",
    )
}

/// A name cell whose name is a button that opens the row's panel (`data-action="detail"`): the
/// keyboard reaches the panel through it, as a click on the row does. `label` is its accessible
/// name. `sub_title` is the whole text of a `sub` that was cut short; it becomes the line's tooltip.
pub fn name_cell_open(title: &str, sub: &str, sub_title: &str, label: &str, id: &str) -> String {
    cell_name(
        &format!(
            "<b><button type=\"button\" class=\"cell-open\" data-action=\"detail\" data-id=\"{}\" aria-label=\"{}\" title=\"{}\">{}</button></b>",
            escape(id),
            escape(label),
            escape(title),
            escape(title)
        ),
        sub,
        sub_title,
        "",
    )
}

/// `name` is the cell's bold first line (markup); the line under it is plain text.
fn cell_name(name: &str, sub: &str, sub_title: &str, sub_tone: &str) -> String {
    let sub = if sub.is_empty() {
        String::new()
    } else {
        format!(
            "<span class=\"cell-sub\" data-tone=\"{}\"{}>{}</span>",
            escape(sub_tone),
            if sub_title.is_empty() {
                String::new()
            } else {
                format!(" title=\"{}\"", escape(sub_title))
            },
            escape(sub)
        )
    };
    format!("<div class=\"cell-name\">{name}{sub}</div>")
}

/// Tags such as the services a rule applies to. No items gives a dash.
pub fn chips(items: &[String]) -> String {
    if items.is_empty() {
        return NONE.into();
    }
    let items = items
        .iter()
        .map(|item| format!("<span class=\"chip\">{}</span>", escape(item)))
        .collect::<String>();
    format!("<div class=\"chips\">{items}</div>")
}

/// A list with nothing in it. `icon` is a `navigation_icon` name; `action` is the first action.
pub fn empty_state(icon: &str, title: &str, description: &str, action: &str) -> String {
    format!(
        "<div class=\"empty-state\" role=\"status\"><span class=\"empty-state-icon\" aria-hidden=\"true\">{}</span><strong class=\"empty-state-title\">{}</strong><p>{}</p>{action}</div>",
        navigation_icon(icon),
        escape(title),
        escape(description)
    )
}

/// One line for the state of the screen's single subject. `tone`: `ok`, `busy` or `error`.
pub fn status_line(
    tone: &str,
    name: &str,
    parts: &[String],
    actions: &str,
    message: &str,
) -> String {
    format!(
        "<div class=\"status-line\" data-tone=\"{}\"><b>{}</b>{}<span class=\"status-line-message\">{}</span><span class=\"spacer\"></span>{actions}</div>",
        escape(tone),
        escape(name),
        parts.join("<span class=\"sep\">·</span>"),
        escape(message)
    )
}

/// Underline tabs that split one screen's lists. `tabs` holds the id, the translated name and the count.
/// A click runs the `subtab` action with `{screen}:{id}`.
pub fn subtabs(screen: &str, label: &str, tabs: &[(&str, &str, usize)], active: &str) -> String {
    let mut out = format!(
        "<div class=\"subtabs\" role=\"tablist\" aria-label=\"{}\">",
        escape(label)
    );
    for (id, name, count) in tabs {
        let _ = write!(
            out,
            "<button type=\"button\" role=\"tab\" class=\"subtab\" aria-selected=\"{}\" data-action=\"subtab\" data-id=\"{}:{}\">{} <em>{count}</em></button>",
            *id == active,
            escape(screen),
            escape(id),
            escape(name)
        );
    }
    out.push_str("</div>");
    out
}

/// The tab the user chose on `screen`, or `default`.
pub fn active_subtab<'a>(app: &'a App, screen: &str, default: &'a str) -> &'a str {
    app.subtabs.get(screen).map_or(default, String::as_str)
}

/// A byte count for people: `512 B`, `1.5 GiB`. One decimal below 10, none from 10.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 3] = ["KiB", "MiB", "GiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    // 1023.9 KiB rounds to 1024: show it as 1.0 MiB.
    while unit + 1 < UNITS.len() && value.round() >= 1024.0 {
        value /= 1024.0;
        unit += 1;
    }
    let number = if value < 9.95 {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    };
    format!("{number} {}", UNITS[unit])
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
                        match app.vms.iter().filter(|vm| vm.state == "error").count() {
                            0 => String::new(),
                            count => count.to_string(),
                        }
                    )
                } else {
                    String::new()
                }
            );
        }
        nav.push_str("</ul></div>");
    }
    let (parent, title) = crumbs(app);
    let parent = parent.map_or_else(String::new, |parent| {
        format!("<span class=\"screen-parent\">{parent}</span>")
    });
    let (summary, toolbar) = if section == "vms" && app.route == "vms" {
        (
            if app.loaded {
                vm_summary(app)
            } else {
                String::new()
            },
            vm_toolbar(app),
        )
    } else {
        (String::new(), String::new())
    };
    let actions = match section {
        // The create page is the target of this button.
        "vms" if app.route == "vms/new" => String::new(),
        "vms" => format!(
            "<a class=\"btn primary\" id=\"vm-list-add\" href=\"#/vms/new\">{}</a>",
            tr(app, "New MicroVM", "새 MicroVM")
        ),
        // The create page is the target of this button.
        "services" if app.route == "services/new" => String::new(),
        "services" => format!(
            "<a class=\"btn primary\" id=\"service-list-add\" href=\"#/services/new\">{}</a>",
            tr(app, "New MicroService", "새 MicroService")
        ),
        "networks" | "storages" | "shells" => button(
            match section {
                "networks" => tr(app, "New network", "새 네트워크"),
                "storages" => tr(app, "Register disk location", "저장 위치 등록"),
                _ => tr(app, "New shell", "새 Shell"),
            },
            "create",
            section,
        ),
        "images" => button(
            tr(app, "Import OCI image", "OCI 이미지 가져오기"),
            "create",
            "oci",
        ),
        "ai-sandbox" => format!(
            "<button type=\"button\" class=\"btn primary\" data-action=\"sb-new\" data-id=\"\">{}</button>",
            tr(app, "New project", "새 프로젝트")
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
    let panel_hidden = if crate::details::panel_visible(app) {
        ""
    } else {
        "hidden"
    };
    let category = match section {
        "vms" | "services" | "stacks" | "pools" | "schedules" | "cluster" => {
            tr(app, "Compute", "컴퓨팅")
        }
        "networks" | "pipe" | "security-groups" => tr(app, "Network", "네트워크"),
        "storages" | "volumes" | "images" | "kernels" | "secrets" | "queues" => {
            tr(app, "Data", "데이터")
        }
        "ai-sandbox" | "security" => "AI",
        _ => tr(app, "System", "시스템"),
    };
    let meters = host_meters(app);
    let search_label = tr(app, "Search and go", "검색 및 이동");
    let panel_label = panel_label(app);
    let menu_icon = navigation_icon("menu");
    let menu_label = tr(app, "Open menu", "메뉴 열기");
    let search_icon = navigation_icon("search");
    format!(
        "<div class=\"app-frame\"><button class=\"icon-btn nav-toggle\" aria-label=\"{menu_label}\" id=\"nav-toggle\" aria-controls=\"main-navigation\" aria-expanded=\"false\" data-action=\"menu\">{menu_icon}</button><div class=\"nav-backdrop\" data-action=\"menu-close\" aria-hidden=\"true\"></div><nav id=\"main-navigation\" class=\"sidebar\" aria-label=\"{}\"><div class=\"sidebar-brand\"><img class=\"sidebar-logo\" src=\"/firecrab.png\" width=\"22\" height=\"22\" alt=\"\"><span class=\"sidebar-brand-text\"><span class=\"sidebar-name\">firecrab</span><span class=\"sidebar-host\" id=\"sidebar-platform\">private microvm cloud</span></span></div><button class=\"sidebar-search\" aria-label=\"{search_label}\" data-action=\"palette\">{search_icon}<span class=\"sidebar-label\">{search_label}</span><kbd>⌘ K</kbd></button><div class=\"sidebar-groups\">{nav}</div><div class=\"sidebar-foot\"><span id=\"update-indicator\"></span><section class=\"host-status\" id=\"host-status\" aria-label=\"Host status\">{meters}</section><div class=\"sidebar-language\"><select id=\"language\" aria-label=\"Language\"><option value=\"en\" {}>English</option><option value=\"ko\" {}>한국어</option></select></div></div></nav><div class=\"app-workspace\"><main class=\"screen\"><header class=\"screen-header\"><div class=\"screen-heading\"><span class=\"screen-group\">{category}</span>{parent}<h1>{title}</h1></div><div class=\"screen-header-end\">{actions}</div></header><div id=\"messages\" aria-live=\"polite\">{}</div><div class=\"wasm-layout\"><div class=\"screen-column\"><div id=\"screen-summary\">{summary}</div><div id=\"toolbar\">{toolbar}</div><div id=\"content\" class=\"wasm-content screen-main screen-body\" data-loaded=\"{}\">{}</div></div><aside id=\"detail-panel\" aria-label=\"{panel_label}\" class=\"wasm-panel detail-panel\" style=\"width:{panel_width}px\" {panel_hidden}>{panel}</aside></div></main></div></div><div id=\"menu-root\"></div><div id=\"palette-root\"></div><div id=\"tooltip-root\"></div><div id=\"modal-root\"></div>",
        tr(app, "Main navigation", "주요 메뉴"),
        if app.locale == "en" { "selected" } else { "" },
        if app.locale == "ko" { "selected" } else { "" },
        messages(app),
        app.loaded,
        content(app)
    )
}

fn crumbs(app: &App) -> (Option<&'static str>, &'static str) {
    match app.route.as_str() {
        "vms/new" => (Some("MicroVM"), tr(app, "New MicroVM", "새 MicroVM")),
        "services/new" => (
            Some("MicroService"),
            tr(app, "New MicroService", "새 MicroService"),
        ),
        route => {
            if let Some((_, label, _)) = route
                .strip_prefix("pipe/")
                .and_then(|id| PIPE_CARDS.iter().find(|(card, _, _)| *card == id))
            {
                return (Some("Pipe"), crate::i18n::t(label));
            }
            let section = route.split('/').next().unwrap_or("vms");
            let title = VIEWS
                .iter()
                .find(|(id, _, _, _)| *id == section)
                .map_or("MicroVM", |(_, _, en, ko)| tr(app, en, ko));
            (None, title)
        }
    }
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
        // The page head is ours; the form brings only its sections and footer.
        return format!(
            "<div class=\"form-page\"><header>{}<p>{}</p></header>{}</div>",
            form.heading(),
            tr(
                app,
                "Pick an image and resources. The MicroVM starts in a few seconds.",
                "이미지와 사양을 고르면 몇 초 안에 시작합니다."
            ),
            form.html(app.busy, &app.field_errors)
        );
    }
    if app.route == "services/new"
        && let Some(form) = &app.form
    {
        return format!(
            "<div class=\"wizard-page\"><header>{}<p>{}</p></header>{}</div>",
            form.heading(),
            tr(
                app,
                "Pick an app and how it runs. It deploys without downtime.",
                "앱과 실행 방식을 고르면 무중단으로 배포합니다."
            ),
            crate::advanced::services::html(app, form)
        );
    }
    if !app.loaded {
        return format!(
            "<div class=\"empty-state\">{}</div>",
            tr(app, "Loading…", "불러오는 중…")
        );
    }
    let mut html = match app.route.as_str() {
        "vms" => format!(
            "<div class=\"vm-screen\"><div id=\"vm-scroll\" class=\"vm-list-scroll\"><table class=\"vm-list wasm-table\" aria-label=\"{}\"><colgroup><col><col class=\"vm-col-state\"><col class=\"vm-col-ip col-secondary\"><col class=\"vm-col-cpu col-secondary\"><col class=\"vm-col-mem col-secondary\"><col class=\"vm-col-actions\"></colgroup><thead><tr><th>{}</th><th>{}</th><th class=\"vm-col-ip col-secondary\">IPv4</th><th class=\"col-secondary\">CPU</th><th class=\"vm-col-mem col-secondary\">{}</th><th><span class=\"sr-only\">Actions</span></th></tr></thead><tbody id=\"vm-rows\">{}</tbody></table><div id=\"vm-empty\" {}>{}</div></div><footer class=\"vm-list-footer\"><span id=\"vm-result-count\" role=\"status\" aria-live=\"polite\"></span><span id=\"vm-filter-reset\" hidden>{}</span><span class=\"vm-list-hint\">{}</span></footer></div>",
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
            vm_empty(app),
            button(
                tr(app, "Clear filters", "필터 초기화"),
                "vm-clear-filters",
                ""
            ),
            tr(
                app,
                "Select a row to inspect · ↑ ↓ to navigate",
                "행을 선택해 상세 보기 · ↑ ↓로 이동"
            )
        ),
        "pools" | "schedules" | "security-groups" | "secrets" | "queues" | "cluster" => {
            crate::advanced::content(app)
        }
        "services" => services(app),
        "security" => service_security(app),
        "stacks" => crate::stacks::content(app),
        "networks" => resource_table(
            app,
            &app.resource,
            &[
                ("name", "Name"),
                ("gateway", "Gateway"),
                ("internetEnabled", "Internet"),
                ("_vms", "VMs"),
            ],
        ),
        "storages" => storage_locations(app),
        "volumes" => crate::volumes::content(app),
        "images" => resource_table(
            app,
            &app.resource,
            &[
                ("alias", "Image"),
                ("installed", "Installed"),
                ("sizeBytes", "Size"),
            ],
        ),
        "kernels" => {
            crate::kernels::queue(app)
                + &resource_table(
                    app,
                    &app.resource,
                    &[
                        ("version", "Version"),
                        ("installed", "Status"),
                        ("architecture", "Architecture"),
                    ],
                )
        }
        "shells" => resource_table(
            app,
            &app.resource,
            &[("name", "Name"), ("latestVersion", "Latest revision")],
        ),
        "pipe/storage" => crate::advanced::storage::content(app),
        "pipe/ai" => crate::advanced::ai::content(app),
        "ai-sandbox" => crate::sandbox::content(app),
        "host" => host(app),
        "pipe" => pipe_overview(app),
        route if route.starts_with("pipe/") => pipe(app, route.trim_start_matches("pipe/")),
        _ => String::new(),
    };
    // Images are three tabs: what is installed, the packages to register, and the registry images.
    let images_tab = if app.route == "images" {
        Some(active_subtab(app, "images", "installed"))
    } else {
        None
    };
    if app.route == "images" && images_tab != Some("installed") {
        html.clear();
    }
    if app.route == "images" && images_tab == Some("pulled") {
        html.push_str(&crate::advanced::images::sources(app));
    }
    if app.route == "pipe/registry" {
        html.push_str(&format!(
            "<div id=\"workflow-root\">{}</div>",
            crate::workflows::registry_html(app)
        ));
    }
    if app.route == "images"
        && images_tab == Some("packages")
        && let Some(catalog) = app.cache.get("/api/microregistry")
    {
        html.push_str(&format!(
            "<section class=\"wasm-details\"><h2>{}</h2>",
            crate::i18n::t("Available packages")
        ));
        let packages: Vec<_> = list(&catalog["images"])
            .iter()
            .filter(|image| !default_placeholder(image))
            .collect();
        if packages.is_empty() {
            html.push_str(&format!(
                "<p>{}</p>",
                tr(
                    app,
                    "No packages available.",
                    "등록할 수 있는 패키지가 없다."
                )
            ));
        }
        for image in packages {
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
        if app.route == "images" && job.0.starts_with("/api/oci/import/") {
            html = crate::oci_progress::html(&job.1) + &html;
        } else {
            html.push_str(&format!(
                "<section class=\"wasm-details\"><h2>{}</h2>{}{}</section>",
                crate::i18n::t("Progress"),
                properties(&job.1),
                button("Refresh progress", "job-refresh", &job.0)
            ));
        }
    }
    if app.route == "images" {
        let ko = app.locale == "ko";
        let installed = list(&app.resource)
            .iter()
            .filter(|row| visible_image(row))
            .count();
        let packages = app.cache.get("/api/microregistry").map_or(0, |catalog| {
            list(&catalog["images"])
                .iter()
                .filter(|image| !default_placeholder(image))
                .count()
        });
        let pulled = list(app.cache.get("pulled-images").unwrap_or(&Value::Null)).len();
        html = format!(
            "{}{html}",
            subtabs(
                "images",
                if ko {
                    "이미지 구역"
                } else {
                    "Image sections"
                },
                &[
                    (
                        "installed",
                        if ko { "설치됨" } else { "Installed" },
                        installed
                    ),
                    ("packages", if ko { "패키지" } else { "Packages" }, packages),
                    (
                        "pulled",
                        if ko {
                            "레지스트리 이미지"
                        } else {
                            "Registry images"
                        },
                        pulled
                    ),
                ],
                images_tab.unwrap_or("installed"),
            )
        );
    }
    // Every Pipe page but the database tables sits beside the Pipe sub-navigation.
    if app.route == "pipe" || (app.route.starts_with("pipe/") && app.route != "pipe/database") {
        let head = app
            .route
            .strip_prefix("pipe/")
            .and_then(|id| PIPE_CARDS.iter().find(|(card, _, _)| *card == id))
            .map_or_else(String::new, |(_, label, description)| {
                format!(
                    "<header class=\"pipe-page-head\"><h2>{}</h2><p>{}</p></header>",
                    crate::i18n::t(label),
                    crate::i18n::t(description)
                )
            });
        html = format!(
            "<div class=\"pipe-layout\">{}<div class=\"pipe-main\">{head}{html}</div></div>",
            pipe_subnav(app)
        );
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
            "<button type=\"button\" role=\"tab\" aria-selected=\"{}\" tabindex=\"{}\" aria-controls=\"vm-scroll\" class=\"vm-filter\" data-action=\"filter\" data-id=\"{id}\">{} <span class=\"vm-filter-count\">{count}</span></button>",
            app.filter == id,
            if app.filter == id { "0" } else { "-1" },
            tr(app, en, ko)
        );
    }
    let _ = write!(
        out,
        "</div><label class=\"vm-search\">{}<input type=\"search\" id=\"vm-search\" aria-label=\"{}\" placeholder=\"{}\" value=\"{}\"></label></div>",
        navigation_icon("search"),
        tr(app, "Filter MicroVMs", "MicroVM 필터"),
        tr(app, "Name, IP, or image", "이름, IP, 이미지"),
        escape(&app.query)
    );
    out
}

/// A memory size in GiB: whole numbers stay whole, others get one decimal.
pub(crate) fn gib(mib: u64) -> String {
    let value = mib as f64 / 1024.0;
    if (value - value.round()).abs() < 0.05 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

/// The strip above the list: four numbers. Memory and disk get a bar when the host total is known.
pub fn vm_summary(app: &App) -> String {
    let running = app.vms.iter().filter(|vm| vm.state == "running").count();
    let cpu: u64 = app.vms.iter().map(|vm| u64::from(vm.cpu)).sum();
    let memory: u64 = app.vms.iter().map(|vm| u64::from(vm.ram)).sum();
    let disk: u64 = app.vms.iter().map(|vm| u64::from(vm.disk_gb)).sum();
    let host = app.cache.get("/api/host").unwrap_or(&Value::Null);
    let memory_total = host["memoryTotalMib"]
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.0);
    let disk_total = host["diskTotalGib"]
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.0);
    // Each item: label, value markup, and the share of the host that is allocated.
    let items = [
        (
            tr(app, "Running", "실행 중"),
            format!("{running}<small>/ {}</small>", app.vms.len()),
            None,
        ),
        (
            tr(app, "vCPU allocated", "vCPU 할당"),
            cpu.to_string(),
            None,
        ),
        (
            tr(app, "Memory allocated", "메모리 할당"),
            match memory_total {
                Some(total) => format!("{}<small>/ {:.0} GiB</small>", gib(memory), total / 1024.0),
                None => format!("{}<small>GiB</small>", gib(memory)),
            },
            memory_total.map(|total| memory as f64 / total),
        ),
        (
            tr(app, "Disk allocated", "디스크 할당"),
            match disk_total {
                Some(total) => format!("{disk}<small>/ {total:.0} GB</small>"),
                None => format!("{disk}<small>GB</small>"),
            },
            disk_total.map(|total| disk as f64 / total),
        ),
    ];
    let mut out = "<dl class=\"vm-summary\" id=\"vm-summary\">".to_string();
    for (label, value, share) in items {
        let bar = share
            .map(|share| {
                let percent = (share * 100.0).clamp(0.0, 100.0);
                format!(
                    "<span class=\"vm-summary-bar{}\" aria-hidden=\"true\"><i style=\"width:{percent:.1}%\"></i></span>",
                    if percent >= 80.0 { " warn" } else { "" }
                )
            })
            .unwrap_or_default();
        let _ = write!(
            out,
            "<div class=\"vm-summary-item\"><dt>{label}</dt><dd><span class=\"vm-summary-value\">{value}</span>{bar}</dd></div>"
        );
    }
    out.push_str("</dl>");
    out
}

/// What `#vm-empty` holds: the first-MicroVM prompt, or a miss of the filter.
pub fn vm_empty(app: &App) -> String {
    if app.vms.is_empty() {
        empty_state(
            "vms",
            tr(app, "Create your first MicroVM", "첫 MicroVM을 만듭니다"),
            tr(
                app,
                "Pick an image and resources. It starts in a few seconds.",
                "이미지와 사양을 고르면 몇 초 안에 시작합니다.",
            ),
            &format!(
                "<a class=\"btn primary\" href=\"#/vms/new\">{}</a>",
                tr(app, "New MicroVM", "새 MicroVM")
            ),
        )
    } else {
        empty_state(
            "search",
            tr(app, "No matching MicroVMs", "검색 결과 없음"),
            tr(
                app,
                "No MicroVMs match the filter.",
                "조건에 맞는 MicroVM이 없습니다.",
            ),
            &button(
                tr(app, "Clear filters", "필터 초기화"),
                "vm-clear-filters",
                "",
            ),
        )
    }
}

pub fn vm_result_count(app: &App, count: usize) -> String {
    if app.locale == "ko" {
        format!("전체 {}개 중 {count}개 표시", app.vms.len())
    } else {
        format!("{count} of {} MicroVMs", app.vms.len())
    }
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
        vm.memory_used_percent
            .filter(|v| v.is_finite())
            .map(|value| {
                format!(
                    "<span class=\"vm-meter{}\"><i style=\"width:{}%\"></i></span><span class=\"vm-usage-value\">{value:.1}%</span>",
                    if value >= 80.0 { " warn" } else { "" },
                    value.clamp(0.0, 100.0)
                )
            })
            .unwrap_or("—".into())
    } else {
        "—".into()
    };
    let action = if matches!(
        vm.extra.get("purpose").and_then(Value::as_str),
        Some("pool" | "queue")
    ) {
        String::new()
    } else if vm.state == "paused" {
        icon_button("Resume", "vm-resume", &vm.id, Icon::Start)
    } else if vm.state == "running" {
        icon_button(tr(app, "Stop", "중지"), "vm-stop", &vm.id, Icon::Stop)
            + &icon_button("Pause", "vm-pause", &vm.id, Icon::Pause)
    } else if vm.editable() {
        icon_button(tr(app, "Start", "시작"), "vm-start", &vm.id, Icon::Start)
    } else {
        String::new()
    };
    let terminal = if vm.state == "running" {
        icon_link(
            "Terminal (new tab)",
            &format!("#/console/{}", encode(&vm.id)),
            true,
            Icon::Terminal,
        )
    } else {
        let label = escape(crate::i18n::t("Terminal (new tab)"));
        format!(
            "<button type=\"button\" class=\"icon-btn\" disabled aria-label=\"{label}\" title=\"{label}\">{}</button>",
            Icon::Terminal.svg()
        )
    };
    let (subtitle, tone) = vm_subtitle(vm);
    let _ = write!(
        out,
        "<tr data-id=\"{id}\" aria-selected=\"{selected}\" class=\"{}\"><td><button type=\"button\" class=\"vm-row-name\" data-action=\"detail\" data-id=\"{id}\">{}</button><div class=\"vm-row-sub{tone}\">{subtitle}</div></td><td><span class=\"badge {state} state-label vm-status-label\" data-state=\"{state}\" tabindex=\"0\" aria-label=\"VM-STATUS: {state_label}\" data-vm-tooltip=\"{id}\" title=\"{state_label}\">{state_label}</span></td><td class=\"vm-col-ip col-secondary\"><span class=\"mono\">{}</span></td><td class=\"col-secondary\"><span class=\"vm-usage\">{cpu}</span></td><td class=\"vm-col-mem col-secondary\"><span class=\"vm-usage\">{memory}</span></td><td class=\"vm-row-actions\">{action}{terminal}{}</td></tr>",
        if selected { "selected" } else { "" },
        escape(&vm.name),
        escape(vm.ipv4.as_deref().unwrap_or("—")),
        icon_button("Actions", "vm-menu", &vm.id, Icon::More)
    );
    out
}
/// The line under a row's name and its tone class: the failure, the start step, or the spec.
fn vm_subtitle(vm: &Vm) -> (String, &'static str) {
    if let Some(failed) = list(vm.extra.get("startupTimeline").unwrap_or(&Value::Null))
        .iter()
        .rev()
        .find(|run| run["outcome"] == "failed")
    {
        let tone = if vm.state == "error" { " error" } else { "" };
        return (escape(text(failed, "detail")), tone);
    }
    let step = vm
        .extra
        .get("startupStep")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !step.is_empty() {
        return (
            format!("{}…", escape(crate::details::step_label(step))),
            " busy",
        );
    }
    let memory = if vm.ram.is_multiple_of(1024) {
        format!("{} GiB", vm.ram / 1024)
    } else {
        format!("{} MiB", vm.ram)
    };
    (
        format!(
            "{} · {} vCPU · {memory} · {} GiB",
            escape(&vm.template),
            vm.cpu,
            vm.disk_gb
        ),
        "",
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
        "<svg class=\"sparkline\" width=\"56\" height=\"18\" aria-hidden=\"true\"><polyline points=\"{points}\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/></svg>"
    )
}

fn services(app: &App) -> String {
    let rows = list(&app.resource);
    if rows.is_empty() {
        return empty_state(
            "services",
            tr(app, "No MicroServices yet", "아직 MicroService가 없습니다"),
            tr(
                app,
                "Run a container image on MicroVMs and deploy without downtime.",
                "컨테이너 이미지를 MicroVM에서 실행하고, 무중단으로 배포합니다.",
            ),
            &format!(
                "<a class=\"btn primary\" href=\"#/services/new\">{}</a>",
                tr(app, "New MicroService", "새 MicroService")
            ),
        );
    }
    let mut out = format!(
        "<div class=\"service-list\"><table class=\"wasm-table service-table\" aria-label=\"MicroServices\"><colgroup><col><col class=\"service-col-port\"><col class=\"service-col-replicas\"><col class=\"service-col-domain col-secondary\"><col class=\"service-col-actions\"></colgroup><thead><tr><th id=\"service-column-0\">{}</th><th id=\"service-column-2\">{}</th><th id=\"service-column-3\">{}</th><th id=\"service-column-4\" class=\"col-secondary\">{}</th><th class=\"row-actions\"><span class=\"sr-only\">{}</span></th></tr></thead><tbody>",
        tr(app, "Service", "서비스"),
        crate::i18n::t("Port"),
        crate::i18n::t("Replicas"),
        crate::i18n::t("Domain"),
        crate::i18n::t("Actions")
    );
    for row in rows {
        let id = text(row, "id");
        let name = text(row, "name");
        let selected = app.detail.as_ref().is_some_and(|detail| detail["id"] == id);
        // Row controls are named for the service, so a list of them reads one by one.
        let named = |en: &str, ko: &str| {
            if app.locale == "ko" {
                format!("{name} {ko}")
            } else {
                format!("{en} {name}")
            }
        };
        let (open, deploy, actions, edit, delete) = (
            named("Open", "열기"),
            named("Deploy", "배포"),
            if app.locale == "ko" {
                format!("{name} 작업")
            } else {
                format!("Actions for {name}")
            },
            named("Edit", "수정"),
            named("Delete", "삭제"),
        );
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
        let domain = match text(row, "domain") {
            "" => NONE.to_string(),
            domain => format!(
                "<span class=\"mono\" title=\"{0}\">{0}</span>",
                escape(domain)
            ),
        };
        let more = Icon::More.svg();
        let _ = write!(
            out,
            "<tr data-id=\"{}\" aria-selected=\"{selected}\"><td headers=\"service-column-0\" aria-label=\"{}\">{}</td><td headers=\"service-column-2\"><span class=\"mono\">{}</span></td><td headers=\"service-column-3\">{}</td><td headers=\"service-column-4\" class=\"col-secondary\">{domain}</td><td class=\"row-actions\">{}<details class=\"service-row-menu\"><summary class=\"icon-btn\" aria-label=\"{}\" title=\"{}\">{more}</summary><div class=\"service-row-actions\">{}{}</div></details></td></tr>",
            escape(id),
            escape(name),
            name_cell_open(name, &image_label, image, &open, id),
            display(&row["port"]),
            display(&row["replicas"]),
            icon_button(&deploy, "service-run", id, Icon::Deploy),
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

/// Columns that give way on a narrow screen (spec §8).
fn secondary(key: &str) -> &'static str {
    if matches!(
        key,
        "gateway" | "_vms" | "architecture" | "sizeBytes" | "description" | "path"
    ) {
        " class=\"col-secondary\""
    } else {
        ""
    }
}
/// One cell of a resource table: people-readable values, never `true`, `false` or raw bytes.
fn table_cell(app: &App, row: &Value, key: &str, first: bool) -> String {
    let ko = app.locale == "ko";
    match (app.route.as_str(), key) {
        (_, "_vms") => {
            // Networks count the VMs on them.
            let id = text(row, "id");
            app.vms
                .iter()
                .filter(|vm| vm.extra.get("microNetworkId").and_then(Value::as_str) == Some(id))
                .count()
                .to_string()
        }
        ("networks", "internetEnabled") => {
            if row[key] == true {
                status_dot("ok", if ko { "허용" } else { "Allowed" })
            } else {
                status_dot("off", if ko { "격리" } else { "Isolated" })
            }
        }
        ("networks", "name") => {
            let sub = [text(row, "subnetCidr"), text(row, "ipv6AddressMode")]
                .iter()
                .filter(|part| !part.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(" · ");
            name_cell(text(row, "name"), &sub, "", false)
        }
        ("storages", "name") => name_cell(text(row, "name"), text(row, "path"), "", false),
        (_, "installed") => {
            if app.route == "kernels" {
                return escape(crate::i18n::t(crate::kernels::status(app, row)));
            }
            if row[key] == true {
                status_dot("ok", if ko { "설치됨" } else { "Installed" })
            } else {
                status_dot(
                    "none",
                    if ko {
                        "설치 안 됨"
                    } else {
                        "Not installed"
                    },
                )
            }
        }
        (_, "sizeBytes") => row[key].as_u64().map_or(NONE.to_string(), human_bytes),
        (_, "latestVersion") => match row[key].as_u64() {
            Some(version) => format!("<span class=\"mono\">v{version}</span>"),
            None => NONE.to_string(),
        },
        ("shells", "name") => name_cell(text(row, "name"), text(row, "description"), "", false),
        _ => match &row[key] {
            Value::String(title) if first && !title.is_empty() => name_cell(title, "", "", false),
            value => display(value),
        },
    }
}

fn storage_locations(app: &App) -> String {
    format!(
        "<section class=\"storage-explainer\" aria-labelledby=\"storage-purpose\"><h2 id=\"storage-purpose\">{}</h2><p>{}</p><p>{}</p><a href=\"#/volumes\">{}</a></section>{}",
        tr(
            app,
            "Host folders for VM disk files",
            "VM 디스크 파일을 저장하는 호스트 폴더"
        ),
        tr(
            app,
            "MicroVM operating-system disks and MicroBlockStorage data disks are saved in these folders.",
            "MicroVM의 운영체제 디스크와 MicroBlockStorage의 데이터 디스크 파일을 이 위치에 저장한다."
        ),
        tr(
            app,
            "Register a folder, then select it when creating a MicroVM or MicroBlockStorage.",
            "폴더를 등록한 뒤 MicroVM 또는 MicroBlockStorage를 만들 때 저장 위치로 선택한다."
        ),
        tr(
            app,
            "Create a data disk to attach inside a VM → MicroBlockStorage",
            "VM 안에 연결할 데이터 디스크 만들기 → MicroBlockStorage"
        ),
        resource_table(app, &app.resource, &[("name", "Disk location")])
    )
}

fn default_placeholder(image: &Value) -> bool {
    matches!(
        image["alias"].as_str(),
        Some("alpine-3.24.1" | "rocky-9.8" | "ubuntu-26.04")
    )
}

fn visible_image(image: &Value) -> bool {
    !default_placeholder(image) || image["installed"] == true || image["packageStaged"] == true
}

fn resource_table(app: &App, resource: &Value, columns: &[(&str, &str)]) -> String {
    let rows = if resource.is_array() {
        list(resource)
    } else if resource["kernels"].is_array() {
        list(&resource["kernels"])
    } else {
        &[]
    };
    let rows: Vec<&Value> = rows
        .iter()
        .filter(|row| app.route != "images" || visible_image(row))
        .collect();
    if rows.is_empty() {
        let route = app.route.split('/').next().unwrap_or_default();
        let (title, text, action) = match app.route.as_str() {
            "networks" => (
                tr(app, "No MicroNetworks yet", "아직 MicroNetwork가 없습니다"),
                tr(
                    app,
                    "Private networks that MicroVMs use to talk to each other.",
                    "MicroVM끼리 통신하는 사설 네트워크입니다.",
                ),
                button(tr(app, "New network", "새 네트워크"), "create", "networks"),
            ),
            "storages" => (
                tr(
                    app,
                    "No disk locations registered",
                    "등록한 VM 디스크 저장소가 없다",
                ),
                tr(
                    app,
                    "Register a host folder to choose where new VM and data disks are saved.",
                    "호스트 폴더를 등록한 뒤 새 VM과 데이터 디스크의 저장 위치로 선택한다.",
                ),
                button(
                    tr(app, "Register disk location", "저장 위치 등록"),
                    "create",
                    "storages",
                ),
            ),
            "images" => (
                tr(app, "No images installed", "설치된 이미지가 없습니다"),
                tr(
                    app,
                    "Import an OCI image or register an available package.",
                    "OCI 이미지를 가져오거나 사용 가능한 패키지를 등록합니다.",
                ),
                String::new(),
            ),
            "kernels" => (
                tr(app, "No kernels installed", "설치된 커널이 없습니다"),
                tr(
                    app,
                    "Linux kernels that boot MicroVMs. The default kernel comes with Firecrab.",
                    "MicroVM을 부팅하는 Linux 커널입니다. 기본 커널은 Firecrab에 들어 있습니다.",
                ),
                String::new(),
            ),
            "pipe/registry" => (
                tr(
                    app,
                    "No registries connected",
                    "연결한 레지스트리가 없습니다",
                ),
                tr(
                    app,
                    "Connect a registry to pull private images.",
                    "레지스트리를 연결하면 비공개 이미지를 가져올 수 있습니다.",
                ),
                String::new(),
            ),
            "shells" => (
                tr(app, "No shells yet", "아직 Shell이 없습니다"),
                tr(
                    app,
                    "Shell scripts that run when a VM starts. Every change is kept as a revision.",
                    "VM이 시작할 때 실행하는 스크립트입니다. 바꿀 때마다 리비전으로 남습니다.",
                ),
                button(tr(app, "New shell", "새 Shell"), "create", "shells"),
            ),
            _ => (
                tr(app, "No items yet", "항목이 없습니다"),
                tr(
                    app,
                    "Items you add will appear here.",
                    "추가한 항목이 여기에 표시됩니다.",
                ),
                String::new(),
            ),
        };
        return empty_state(route, title, text, &action);
    }
    // Kernels are read only: the row opens the panel and there is nothing to act on.
    let actions = app.route != "kernels";
    let kind = app.route.trim_end_matches('s');
    let mut out =
        format!("<div class=\"table-scroll\"><table class=\"wasm-table {kind}-table\"><thead><tr>");
    for (index, (key, label)) in columns.iter().enumerate() {
        let _ = write!(
            out,
            "<th id=\"{kind}-column-{index}\"{}>{}</th>",
            secondary(key),
            crate::i18n::t(label)
        );
    }
    if actions {
        let _ = write!(
            out,
            "<th class=\"row-actions\"><span class=\"sr-only\">{}</span></th>",
            crate::i18n::t("Actions")
        );
    }
    out.push_str("</tr></thead><tbody>");
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
            let cell = table_cell(app, row, key, index == 0);
            let _ = write!(
                out,
                "<td headers=\"{kind}-column-{index}\"{}>{cell}</td>",
                secondary(key)
            );
        }
        if actions {
            let _ = write!(
                out,
                "<td class=\"row-actions\">{}</td>",
                icon_button("Details", "detail", id, Icon::Details)
            );
        }
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table></div>");
    out
}

/// The deployment phase as people read it: a label for every phase the API has. A phase this
/// list does not know yet reads as words ("warming_up" is "Warming up"), never as the raw value.
pub fn phase_label(phase: &str) -> String {
    let label = match phase {
        "queued" => "Queued",
        "importing" => "Preparing image",
        "starting" => "Starting replicas",
        "checking_health" => "Checking health",
        "connecting_domain" => "Connecting domain",
        "switching_traffic" => "Switching Nginx traffic",
        "draining" => "Draining previous requests",
        "rolling_back" => "Restoring previous release",
        "running" => "Running",
        "degraded" => "Degraded",
        "stopping" => "Stopping service",
        "stopped" => "Stopped",
        "failed" => "Deployment failed",
        "cancelled" => "Cancelled",
        other => {
            let mut words = other.replace('_', " ");
            if let Some(first) = words.chars().next() {
                words.replace_range(..first.len_utf8(), &first.to_uppercase().to_string());
            }
            return words;
        }
    };
    crate::i18n::t(label).to_owned()
}

/// The `status_dot` tone of a deployment phase.
pub fn phase_tone(phase: &str) -> &'static str {
    match phase {
        "running" => "ok",
        "failed" | "degraded" => "error",
        _ if crate::deploy::busy_phase(phase) => "busy",
        _ => "off",
    }
}

/// The deployment as one block: the summary, then the instances. The Deploy form shows this.
pub fn deployment(value: Option<&Value>) -> String {
    format!(
        "{}{}",
        deployment_summary(value),
        deployment_instances(value)
    )
}

/// Where the deployment stands: the phase, how many instances are healthy, where it answers and
/// what went wrong.
pub fn deployment_summary(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return format!("<p role=\"status\">{}</p>", crate::i18n::t("Loading…"));
    };
    if value.is_null() {
        return format!(
            "<p class=\"service-state\" role=\"status\">{}</p>",
            status_dot("off", crate::i18n::t("Not deployed"))
        );
    }
    let phase = text(value, "phase");
    let instances = list(&value["instances"]);
    let healthy = instances
        .iter()
        .filter(|instance| instance["healthy"] == true)
        .count();
    // The replicas the service asks for; a snapshot without them counts its instances.
    let replicas = match value["spec"]["replicas"].as_u64() {
        Some(replicas) if replicas > 0 => replicas as usize,
        _ => instances.len(),
    };
    let mut out = format!(
        "<div class=\"service-runtime\"><p class=\"service-state\" data-phase=\"{}\" role=\"status\">{}</p>",
        escape(phase),
        status_dot(phase_tone(phase), &phase_label(phase))
    );
    if !instances.is_empty() {
        out.push_str(&format!(
            "<span class=\"service-health\">{healthy}/{replicas} {}</span>",
            crate::i18n::t("healthy")
        ));
    }
    out.push_str("</div>");
    // An update says where it goes from and to; a going deployment has a bar of its replicas.
    if value["update"].is_object() && value["update"]["completed"] != true {
        let strategy = match text(&value["update"], "strategy") {
            "blue_green" => crate::i18n::t("Blue-green"),
            "rolling" => crate::i18n::t("Rolling"),
            _ => "",
        };
        let release = |id: &str| {
            format!(
                "<span class=\"mono\" title=\"{}\">{}</span>",
                escape(id),
                escape(&crate::deploy::short(id))
            )
        };
        let _ = write!(
            out,
            "<p class=\"service-update\">{} → {}{}</p>",
            release(text(&value["update"]["previous"], "id")),
            release(text(value, "id")),
            if strategy.is_empty() {
                String::new()
            } else {
                format!(" · {strategy}")
            }
        );
    }
    if crate::deploy::busy(value) && replicas > 0 {
        let ready = healthy.min(replicas);
        let cells: String = (0..replicas)
            .map(|index| {
                format!(
                    "<i data-state=\"{}\"></i>",
                    match index.cmp(&ready) {
                        std::cmp::Ordering::Less => "ok",
                        std::cmp::Ordering::Equal => "busy",
                        std::cmp::Ordering::Greater => "off",
                    }
                )
            })
            .collect();
        let _ = write!(
            out,
            "<div class=\"service-progress\" role=\"progressbar\" aria-label=\"{}\" aria-valuemin=\"0\" aria-valuenow=\"{ready}\" aria-valuemax=\"{replicas}\">{cells}</div>",
            crate::i18n::t("Healthy replicas")
        );
    }
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
    out
}

/// The VMs that run the service: id, address, host port and health. No instances, no table.
pub fn deployment_instances(value: Option<&Value>) -> String {
    let instances = value.map_or(&[][..], |value| list(&value["instances"]));
    if instances.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "<div class=\"table-scroll\"><table class=\"wasm-table service-instances\"><colgroup><col><col class=\"instance-col-ip\"><col class=\"instance-col-port\"><col class=\"instance-col-health\"></colgroup><thead><tr><th>VM</th><th>IPv4</th><th>{}</th><th>{}</th></tr></thead><tbody>",
        crate::i18n::t("Port"),
        crate::i18n::t("Health")
    );
    for instance in instances {
        let vm = text(instance, "vmId");
        let (tone, label) = if instance["healthy"] == true {
            ("ok", "Healthy")
        } else {
            ("error", "Unhealthy")
        };
        let _ = write!(
            out,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            name_cell_cut(vm, "", true),
            match text(instance, "ipv4") {
                "" => NONE.to_string(),
                ip => format!("<span class=\"mono\">{}</span>", escape(ip)),
            },
            display(&instance["hostPort"]),
            status_dot(tone, crate::i18n::t(label))
        );
    }
    out.push_str("</tbody></table></div>");
    out
}

/// The deployment's own log lines as text. No lines, no output.
pub fn deployment_log(value: Option<&Value>) -> String {
    let lines = value.map_or(&[][..], |value| list(&value["log"]));
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "<pre class=\"wasm-log\">{}</pre>",
        escape(
            &lines
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        )
    )
}
/// A value as cell text. A missing or empty value is a dash.
/// `true` and `false` read Yes and No only where a screen has no better label.
fn display(value: &Value) -> String {
    match value {
        Value::Null => NONE.into(),
        Value::String(value) if value.is_empty() => NONE.into(),
        Value::String(value) => escape(value),
        Value::Bool(value) => crate::i18n::t(if *value { "Yes" } else { "No" }).into(),
        Value::Number(value) => value.to_string(),
        Value::Array(values) if values.is_empty() => NONE.into(),
        Value::Array(values) => values.iter().map(display).collect::<Vec<_>>().join(", "),
        Value::Object(object) if object.is_empty() => NONE.into(),
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

/// The panel head's actions (spec §7.1). Running: stop, pause, console, SSH. Stopped: start, delete.
/// A paused or half-way VM keeps resume or retry beside stop. The rest sits in the More menu.
fn vm_detail_actions(app: &App, data: &Value) -> String {
    let id = text(data, "id");
    let encoded = encode(id);
    let state = text(data, "state");
    let queue = text(data, "purpose") == "queue";
    let pool = queue || text(data, "purpose") == "pool";
    let editable = !pool && matches!(state, "created" | "stopped" | "error");
    let mut primary = String::new();
    if !pool {
        let stop = icon_button("Stop", "vm-stop", id, Icon::Stop);
        match state {
            "running" => {
                primary.push_str(&stop);
                primary.push_str(&icon_button("Pause", "vm-pause", id, Icon::Pause));
            }
            "paused" => {
                primary.push_str(&icon_button("Resume", "vm-resume", id, Icon::Start));
                primary.push_str(&stop);
            }
            "pausing" => {
                primary.push_str(&icon_button("Retry pause", "vm-pause", id, Icon::Retry));
                primary.push_str(&stop);
            }
            "resuming" => {
                primary.push_str(&icon_button("Retry resume", "vm-resume", id, Icon::Retry));
                primary.push_str(&stop);
            }
            _ if editable => primary.push_str(&icon_button("Start", "vm-start", id, Icon::Start)),
            _ => {}
        }
    }
    if state == "running" {
        // The same page as the row's terminal link, in a new tab.
        primary.push_str(&icon_link(
            "Open console",
            &format!("#/console/{encoded}"),
            true,
            Icon::Terminal,
        ));
        primary.push_str(&icon_button("SSH connection", "ssh", id, Icon::Ssh));
    }
    if editable {
        primary.push_str(&icon_request("Delete", &format!("/api/vms/{encoded}")));
    }
    if queue {
        primary.push_str(&icon_link("Queue", "#/queues", false, Icon::Open));
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
    format!(
        "<fieldset class=\"detail-actions vm-detail-actions\" {}><legend class=\"sr-only\">{}</legend>{primary}<details id=\"vm-more-actions\" class=\"vm-more-actions\"><summary id=\"vm-more-summary\" class=\"icon-btn\" aria-label=\"{more}\" title=\"{more}\">{}</summary><div class=\"vm-action-menu\" role=\"group\" aria-label=\"{}\">{menu}</div></details></fieldset>",
        if app.detail_loading || app.busy {
            "disabled"
        } else {
            ""
        },
        crate::i18n::t("Actions"),
        Icon::More.svg(),
        crate::i18n::t("VM settings"),
        more = crate::i18n::t("More VM actions")
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
            let deployment = app.service_deployment.as_ref();
            let owns = deployment.is_some_and(crate::advanced::services::owns_resources);
            if owns {
                // The rocket is always the first icon. Once the service runs it updates it: that
                // is the form of the Releases tab.
                let update = icon_button("Deploy update", "deploy-update", "", Icon::Deploy);
                out.push_str(&if crate::deploy::can_deploy_update(app) {
                    update
                } else {
                    update.replace("type=\"button\"", "type=\"button\" disabled")
                });
            } else if deployment.is_some() {
                out.push_str(&icon_button(
                    "Deploy",
                    "edit",
                    "service-deploy",
                    Icon::Deploy,
                ));
            }
            // A release to go back to exists once the service has run behind its domain.
            if deployment.is_some_and(crate::deploy::has_releases) {
                out.push_str(&icon_button(
                    "Roll back",
                    "detail-tab",
                    "releases",
                    Icon::Rollback,
                ));
            }
            // Stop is in More, with the same name and id. The menu id is the one every panel's
            // More menu shares: it keeps its open state through a render and closes on Escape or
            // a click elsewhere.
            let stop = if owns {
                let stop = request(
                    "Stop service",
                    "POST",
                    &format!("/api/micro-services/{encoded}/stop"),
                    Value::Null,
                );
                let hold = app.busy || deployment.is_some_and(crate::deploy::busy);
                stop.replace(
                    "type=\"button\"",
                    &format!(
                        "type=\"button\" id=\"service-stop\"{}",
                        if hold { " disabled" } else { "" }
                    ),
                )
            } else {
                String::new()
            };
            let more = crate::i18n::t("More");
            let _ = write!(
                out,
                "<details id=\"vm-more-actions\" class=\"vm-more-actions\"><summary id=\"vm-more-summary\" class=\"icon-btn\" aria-label=\"{more}\" title=\"{more}\">{}</summary><div class=\"vm-action-menu\" role=\"group\" aria-label=\"{}\">{stop}{}{}</div></details>",
                Icon::More.svg(),
                crate::i18n::t("MicroService settings"),
                button("Edit configuration", "resource-edit", id),
                request(
                    "Delete",
                    "DELETE",
                    &format!("/api/micro-services/{encoded}"),
                    Value::Null
                )
            );
        }
        "networks" => {
            out.push_str(&icon_button("Edit", "edit", "networks", Icon::Edit));
            out.push_str(&icon_request(
                "Delete",
                &format!("/api/micro-networks/{encoded}"),
            ));
        }
        "storages" => out.push_str(&icon_request(
            "Delete",
            &format!("/api/micro-storages/{encoded}"),
        )),
        "shells" => {
            out.push_str(&icon_button(
                "Publish revision",
                "edit",
                "shells",
                Icon::Edit,
            ));
            out.push_str(&icon_request("Delete", &format!("/api/shells/{encoded}")));
        }
        "images" => {
            let alias = encode(text(data, "alias"));
            for (action, label) in [("package", "Download package"), ("install", "Install")] {
                out.push_str(&icon_request_with(
                    label,
                    "POST",
                    &format!("/api/images/{alias}/{action}"),
                    Value::Null,
                    Icon::Download,
                ));
            }
            out.push_str(&icon_button("Kernel", "edit", "image-kernel", Icon::Edit));
            out.push_str(&icon_request(
                "Delete image",
                &format!("/api/images/{alias}"),
            ));
            out.push_str(&icon_request(
                "Delete package",
                &format!("/api/images/{alias}/package"),
            ));
        }
        "kernels" => {
            let version = encode(text(data, "version"));
            let install = icon_request_with(
                "Build kernel",
                "POST",
                &format!("/api/kernels/{version}/install"),
                Value::Null,
                Icon::Download,
            );
            let enabled = crate::kernels::can_install(app, data);
            out.push_str(&if enabled {
                install
            } else {
                install.replacen("<button ", "<button disabled ", 1)
            });
            if data["installed"] == true && enabled && data["inUse"] != true {
                out.push_str(&icon_request("Delete", &format!("/api/kernels/{version}")));
            }
        }
        "pipe/auth" => {
            if text(data, "provider") == "firecrab" {
                out.push_str(&icon_link(
                    "Firecrab Console",
                    "https://console.firecrab.dev/#account",
                    true,
                    Icon::Open,
                ));
            }
            if list(&data["services"])
                .iter()
                .any(|service| service == "storage")
            {
                out.push_str(&icon_link("S3", "#/pipe/storage", false, Icon::Open));
            }
            out.push_str(&icon_button(
                "Update credentials",
                "create",
                text(data, "provider"),
                Icon::Edit,
            ));
            out.push_str(&icon_request(
                "Disconnect",
                &format!("/api/pipe/auth/{}", encode(text(data, "provider"))),
            ));
        }
        "pipe/registry" => {
            out.push_str(&icon_button("Update token", "edit", "registry", Icon::Edit));
            out.push_str(&icon_request(
                "Disconnect",
                &format!("/api/pipe/registries/{}", encode(text(data, "registry"))),
            ));
            out.push_str(&icon_button(
                "Repositories",
                "registry-repositories",
                text(data, "registry"),
                Icon::Open,
            ));
        }
        "pipe/database" => {
            out.push_str(&icon_request(
                "Disconnect",
                &format!("/api/pipe/databases/{encoded}"),
            ));
            out.push_str(&icon_button("Tables", "database-tables", id, Icon::Open));
        }
        _ => {}
    }
    out.push_str("</fieldset>");
    out
}
/// A line icon for a Pipe page, at `size` px. Every page has its own shape (spec §4.9) and none
/// repeats a sidebar icon.
fn pipe_icon(id: &str, size: u32) -> String {
    let path = match id {
        "overview" => "M3 4h6v6H3zM3 14h6v6H3zM14 5h7M14 9h7M14 15h7M14 19h7",
        "auth" => "M16 7a4 4 0 1 1-8 0 4 4 0 0 1 8 0ZM4 21v-2a6 6 0 0 1 6-6h4a6 6 0 0 1 6 6v2",
        "ai" => "m12 3 2.5 6.5L21 12l-6.5 2.5L12 21l-2.5-6.5L3 12l6.5-2.5L12 3ZM20 2v4M18 4h4",
        "storage" => {
            "M3 7h6l2 2h10v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v2"
        }
        "route" => "M16 3h5v5M8 3H3v5M12 22v-8.3a4 4 0 0 0-1.2-2.9L3 3M15 9l6-6",
        "registry" => "M3 3h18v5H3zM5 8v11a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8M10 12h4",
        "database" => "M3 3h18v18H3zM3 9h18M3 15h18M9 3v18",
        "quick-tunnel" => "m13 2-9 12h7l-1 8 10-12h-7l1-8Z",
        "firecrab-tunnel" => {
            "M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20ZM2 12h20M12 2a15 15 0 0 1 0 20M12 2a15 15 0 0 0 0 20"
        }
        "named-tunnel" => "M17.5 19H7a5 5 0 1 1 1-9.9A6 6 0 0 1 19.5 11 4 4 0 0 1 17.5 19Z",
        _ => return String::new(),
    };
    format!(
        "<svg width=\"{size}\" height=\"{size}\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.75\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\"><path d=\"{path}\"/></svg>"
    )
}

/// The Pipe pages by group, in the order of the sub-navigation: (id, group).
const PIPE_GROUPS: [(&str, &[&str]); 3] = [
    ("Connections", &["auth", "ai"]),
    (
        "Public addresses",
        &["firecrab-tunnel", "quick-tunnel", "named-tunnel", "route"],
    ),
    ("Data", &["storage", "registry", "database"]),
];

/// The status of a Pipe page for the overview: a tone and a label, or `None` while unknown.
/// List-backed pages count their connections.
pub fn pipe_status(id: &str, data: Option<&Value>, ko: bool) -> Option<(&'static str, String)> {
    let count = |data: Option<&Value>| data.and_then(|v| v.as_array()).map(Vec::len);
    match id {
        "ai" | "storage" | "registry" | "database" | "auth" => count(data).map(|n| match n {
            0 => (
                "none",
                if ko {
                    "연결 안 됨"
                } else {
                    "Not connected"
                }
                .to_string(),
            ),
            n if ko => ("ok", format!("{n}개 연결됨")),
            n => ("ok", format!("{n} connected")),
        }),
        "route" => data
            .and_then(|v| v["config"]["routes"].as_array())
            .map(|r| match r.len() {
                0 => (
                    "none",
                    if ko { "라우트 없음" } else { "No routes" }.to_string(),
                ),
                n if ko => ("ok", format!("라우트 {n}개")),
                n => ("ok", format!("{n} routes")),
            }),
        "named-tunnel" => data.map(|v| {
            if v["config"]["tunnel"].is_null() {
                (
                    "none",
                    if ko {
                        "연결 안 됨"
                    } else {
                        "Not connected"
                    }
                    .to_string(),
                )
            } else {
                ("ok", if ko { "연결됨" } else { "Connected" }.to_string())
            }
        }),
        _ => None,
    }
}

const PIPE_CARDS: [(&str, &str, &str); 9] = [
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
];

/// The sub-navigation of every Pipe page: Overview, then the pages by group.
fn pipe_subnav(app: &App) -> String {
    let link = |id: &str, href: &str, label: &str| {
        let current = if id == "overview" {
            app.route == "pipe"
        } else {
            app.route.strip_prefix("pipe/") == Some(id)
        };
        format!(
            "<a class=\"pipe-subnav-link{}\" href=\"{href}\"{}>{}<span>{}</span></a>",
            if current { " active" } else { "" },
            if current {
                " aria-current=\"page\""
            } else {
                ""
            },
            pipe_icon(id, 16),
            crate::i18n::t(label)
        )
    };
    let mut out = format!(
        "<nav class=\"pipe-subnav\" aria-label=\"Pipe\">{}",
        link("overview", "#/pipe", "Overview")
    );
    for (group, pages) in PIPE_GROUPS {
        let _ = write!(
            out,
            "<div class=\"pipe-subnav-group\"><div class=\"pipe-subnav-label\">{}</div>",
            crate::i18n::t(group)
        );
        for id in pages {
            if let Some((_, label, _)) = PIPE_CARDS.iter().find(|(card, _, _)| card == id) {
                out.push_str(&link(id, &format!("#/pipe/{id}"), label));
            }
        }
        out.push_str("</div>");
    }
    out.push_str("</nav>");
    out
}
/// The overview of Pipe: every page as a row with what it holds.
fn pipe_overview(app: &App) -> String {
    let ko = app.locale == "ko";
    let mut out = String::new();
    for (group, pages) in PIPE_GROUPS {
        let _ = write!(
            out,
            "<section class=\"pipe-group\"><h2>{}</h2>",
            crate::i18n::t(group)
        );
        for id in pages {
            let Some((_, label, description)) = PIPE_CARDS.iter().find(|(card, _, _)| card == id)
            else {
                continue;
            };
            let data = match *id {
                "auth" => app.cache.get("/api/pipe/auth"),
                "ai" => app.cache.get("/api/pipe/ai/connections"),
                "storage" => app.cache.get("/api/pipe/storage"),
                "registry" => app.cache.get("/api/pipe/registries"),
                "database" => app.cache.get("/api/pipe/databases"),
                _ => app.cache.get("/api/pipe").or(Some(&app.resource)),
            };
            let status = pipe_status(id, data, ko)
                .map_or_else(String::new, |(tone, label)| status_dot(tone, &label));
            let _ = write!(
                out,
                "<a class=\"pipe-row\" href=\"#/pipe/{id}\"><span class=\"pipe-row-icon\">{}</span><span class=\"pipe-row-text\"><b>{}</b><span>{}</span></span><span class=\"pipe-row-status\">{status}</span><span class=\"pipe-row-chevron\" aria-hidden=\"true\">›</span></a>",
                pipe_icon(id, 16),
                crate::i18n::t(label),
                crate::i18n::t(description)
            );
        }
        out.push_str("</section>");
    }
    out
}
/// A Pipe service (ingress or the tunnel connector) as one status line with its controls.
fn service_line(
    app: &App,
    name: &str,
    service: &str,
    unit: Option<&Value>,
    detail: &str,
) -> String {
    let ko = app.locale == "ko";
    let active = unit.is_some_and(|unit| unit["activeState"] == "active");
    let missing = unit.is_none_or(|unit| unit["loadState"] == "not-found");
    let state = if active {
        if ko { "실행 중" } else { "Running" }
    } else if missing {
        if ko {
            "설치 안 됨"
        } else {
            "Not installed"
        }
    } else if ko {
        "중지됨"
    } else {
        "Stopped"
    };
    let control = |label: &str, action: &str, icon: Icon| {
        icon_request_with(
            &format!("{label} {service}"),
            "POST",
            "/api/pipe/service",
            json!({"service": service, "action": action}),
            icon,
        )
    };
    let mut actions = String::new();
    if active {
        actions.push_str(&control("Stop", "stop", Icon::Stop));
        actions.push_str(&control("Restart", "restart", Icon::Restart));
    } else if !missing {
        actions.push_str(&control("Start", "start", Icon::Start));
    }
    let mut parts = vec![state.to_string()];
    if !detail.is_empty() {
        parts.push(format!("<span class=\"mono\">{}</span>", escape(detail)));
    }
    status_line(if active { "ok" } else { "" }, name, &parts, &actions, "")
}
/// The public address of a tunnel: a link that opens it, with the tunnel's id under it.
fn tunnel_address(url: &str, sub: &str) -> String {
    format!(
        "<div class=\"cell-name\"><b class=\"mono\"><a href=\"{0}\" target=\"_blank\" rel=\"noreferrer\">{0}</a></b>{1}</div>",
        escape(url),
        if sub.is_empty() {
            String::new()
        } else {
            format!(
                "<span class=\"cell-sub\" data-tone=\"\">{}</span>",
                escape(sub)
            )
        }
    )
}
/// The addresses of Firecrab or the quick tunnels as a table: the URL, where it goes, the state
/// and the controls. `kind` is `quick` or `firecrab`.
fn tunnel_table(app: &App, rows: &[Value], kind: &str) -> String {
    let ko = app.locale == "ko";
    let quick = kind == "quick";
    if rows.is_empty() {
        let (title, text, action) = if quick {
            (
                tr(app, "No quick tunnels", "빠른 터널이 없습니다"),
                tr(
                    app,
                    "Temporary public URLs for apps. They change when the tunnel restarts.",
                    "앱을 임시 공개 URL로 엽니다. 터널을 다시 시작하면 주소가 바뀝니다.",
                ),
                button(
                    tr(app, "Create tunnel", "터널 만들기"),
                    "create",
                    "quick-tunnel",
                ),
            )
        } else {
            (
                tr(app, "No published addresses", "공개한 주소가 없습니다"),
                tr(
                    app,
                    "Publish an app at a fixed address under firecrab.dev.",
                    "firecrab.dev 아래 고정 주소로 앱을 공개합니다.",
                ),
                button(
                    tr(app, "Connect address", "주소 연결"),
                    "create",
                    "firecrab-tunnel",
                ),
            )
        };
        return empty_state("pipe", title, text, &action);
    }
    let mut out = format!(
        "<div class=\"table-scroll\"><table class=\"wasm-table tunnel-table\" aria-label=\"{}\"><thead><tr><th>{}</th><th class=\"col-secondary\">{}</th>{}<th class=\"row-actions\"><span class=\"sr-only\">{}</span></th></tr></thead><tbody>",
        if quick {
            tr(app, "Quick tunnels", "빠른 터널")
        } else {
            tr(app, "Firecrab addresses", "Firecrab 주소")
        },
        tr(app, "Address", "주소"),
        tr(app, "Target", "대상"),
        if quick {
            format!("<th>{}</th>", tr(app, "Status", "상태"))
        } else {
            String::new()
        },
        crate::i18n::t("Actions")
    );
    for row in rows {
        let url = text(row, "publicUrl");
        let target_value = if row["target"].is_object() {
            &row["target"]
        } else {
            row
        };
        let upstreams = list(&target_value["upstreams"])
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let target = if !upstreams.is_empty() {
            upstreams
        } else if !text(target_value, "host").is_empty() {
            text(target_value, "host").to_string()
        } else {
            text(target_value, "type").to_string()
        };
        let id = text(row, "id");
        let subdomain = text(row, "subdomain");
        let mut actions = icon_button(
            if quick { "Copy URL" } else { "Copy address" },
            "copy-text",
            url,
            Icon::Copy,
        );
        let status = if quick {
            let active = row["unit"]["activeState"] == "active" || row["activeState"] == "active";
            for (label, action, icon, show) in [
                ("Stop", "stop", Icon::Stop, active),
                ("Restart", "restart", Icon::Restart, active),
                ("Start", "start", Icon::Start, !active),
            ] {
                if show {
                    actions.push_str(&icon_request_with(
                        label,
                        "POST",
                        &format!("/api/pipe/quick-tunnels/{}/service", encode(id)),
                        json!({"action": action}),
                        icon,
                    ));
                }
            }
            format!(
                "<td>{}</td>",
                status_dot(
                    if active { "ok" } else { "off" },
                    if active {
                        if ko { "실행 중" } else { "Running" }
                    } else if ko {
                        "중지됨"
                    } else {
                        "Stopped"
                    }
                )
            )
        } else {
            String::new()
        };
        actions.push_str(&icon_button(
            "Edit",
            if quick {
                "edit-quick-tunnel"
            } else {
                "edit-firecrab-tunnel"
            },
            if quick { id } else { subdomain },
            Icon::Edit,
        ));
        actions.push_str(&icon_request_with(
            "Delete",
            "DELETE",
            &if quick {
                format!("/api/pipe/quick-tunnels/{}", encode(id))
            } else {
                format!("/api/pipe/firecrab-tunnels/{}", encode(subdomain))
            },
            Value::Null,
            Icon::Delete,
        ));
        out.push_str(&format!(
            "<tr><td>{}</td><td class=\"col-secondary\"><span class=\"mono\">{}</span></td>{status}<td class=\"row-actions\">{actions}</td></tr>",
            tunnel_address(
                url,
                match (quick, text(row, "name")) {
                    (true, "") => id,
                    (true, name) => name,
                    _ => "",
                }
            ),
            escape(&target)
        ));
    }
    out.push_str("</tbody></table></div>");
    out
}
fn pipe(app: &App, card: &str) -> String {
    if card == "database" {
        return crate::databases::content(app);
    }
    let mut out = "<div class=\"wasm-actions\">".to_string();
    match card {
        "auth" => {
            out.push_str(&button("Connect Firecrab", "create", "firecrab"));
            out.push_str(&button("Connect GitHub", "create", "github"));
            out.push_str(&button("Connect AWS", "create", "aws"));
        }
        "route" => {
            out.push_str(&button("Edit routes", "create", "route"));
            out.push_str(&request(
                "Disable Pipe",
                "POST",
                "/api/pipe/disable",
                Value::Null,
            ));
        }
        "registry" => {
            out.push_str(&button("Connect registry", "create", "registry"));
            out.push_str(&icon_button(
                "Pull image",
                "create",
                "registry-pull",
                Icon::Download,
            ));
            out.push_str(&icon_button(
                "Cached images",
                "registry-images",
                "",
                Icon::Logs,
            ));
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
        "auth" => {
            out.push_str(&crate::auth_setup::overview(app.locale == "ko"));
            out.push_str(&resource_table(
                app,
                &app.resource,
                &[
                    ("provider", "Provider"),
                    ("account", "Account"),
                    ("registry", "Registry"),
                ],
            ));
        }
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
        "quick-tunnel" => out.push_str(&tunnel_table(
            app,
            list(&app.resource["quickTunnels"]),
            "quick",
        )),
        "firecrab-tunnel" => out.push_str(&tunnel_table(
            app,
            list(&app.resource["firecrabTunnels"]),
            "firecrab",
        )),
        "named-tunnel" => {
            let tunnel = &app.resource["config"]["tunnel"];
            if tunnel.is_null() {
                out.push_str(&empty_state(
                    "pipe",
                    tr(app, "Connect your domain", "도메인을 연결합니다"),
                    tr(
                        app,
                        "Publish apps on a domain in your Cloudflare account.",
                        "Cloudflare 계정의 도메인으로 앱을 공개합니다.",
                    ),
                    "",
                ));
            } else {
                let hostname = text(tunnel, "hostname");
                out.push_str(&service_line(
                    app,
                    "Cloudflare tunnel",
                    "tunnel",
                    list(&app.resource["units"]).get(1),
                    hostname,
                ));
                out.push_str(&request(
                    "Disconnect Cloudflare",
                    "POST",
                    "/api/pipe/cloudflare/disconnect",
                    Value::Null,
                ));
            }
        }
        "route" => {
            let units = list(&app.resource["units"]);
            out.push_str(&service_line(
                app,
                "Ingress",
                "ingress",
                units.first(),
                text(&app.resource["config"], "listen"),
            ));
            out.push_str(&service_line(app, "Tunnel", "tunnel", units.get(1), ""));
            let routes = list(&app.resource["config"]["routes"]);
            if routes.is_empty() {
                out.push_str(&empty_state(
                    "pipe",
                    tr(app, "No routes", "라우트가 없습니다"),
                    tr(
                        app,
                        "Send incoming requests to apps by host name and path.",
                        "들어오는 요청을 호스트 이름과 경로에 따라 앱으로 보냅니다.",
                    ),
                    "",
                ));
            } else {
                out.push_str(&format!(
                    "<div class=\"table-scroll\"><table class=\"wasm-table route-table\" aria-label=\"{}\"><thead><tr><th>{}</th><th>{}</th></tr></thead><tbody>",
                    tr(app, "Routes", "라우트"),
                    tr(app, "Host", "호스트"),
                    tr(app, "Target", "대상")
                ));
                for route in routes {
                    let upstreams = list(&route["upstreams"])
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ");
                    out.push_str(&format!(
                        "<tr><td>{}</td><td><span class=\"mono\">{}</span></td></tr>",
                        name_cell(text(route, "host"), text(route, "pathPrefix"), "", true),
                        escape(&upstreams)
                    ));
                }
                out.push_str("</tbody></table></div>");
            }
        }
        _ => out.push_str(&properties(&app.resource)),
    }
    out
}
/// What the sidebar's host status link shows. `update_host` writes these into the
/// existing nodes on every poll, so the link keeps keyboard focus.
pub struct HostStatus {
    pub ok: bool,
    pub head: &'static str,
    pub load: String,
    pub memory: String,
    pub disk: String,
}
fn managed_platform(host: &Value) -> bool {
    matches!(text(&host["platform"], "os"), "macos" | "windows")
}
pub fn host_status(app: &App) -> HostStatus {
    let host = app.cache.get("/api/host").unwrap_or(&Value::Null);
    let percent = |total: &str, available: &str| -> Option<f64> {
        match (host[total].as_f64(), host[available].as_f64()) {
            (Some(total), Some(available))
                if total.is_finite() && total > 0.0 && available.is_finite() =>
            {
                Some((1.0 - available / total).clamp(0.0, 1.0) * 100.0)
            }
            _ => None,
        }
    };
    let memory = percent("memoryTotalMib", "memoryAvailableMib");
    let disk = percent("diskTotalGib", "diskAvailableGib");
    let load = host["loadAverage1m"]
        .as_f64()
        .filter(|v| v.is_finite())
        .map(|v| format!("{v:.2}"))
        .unwrap_or_else(|| "—".into());
    let percent_text = |value: Option<f64>| {
        value
            .map(|v| format!("{v:.0}%"))
            .unwrap_or_else(|| "—".into())
    };
    HostStatus {
        ok: host.is_object(),
        head: if host.is_object() && managed_platform(host) {
            tr(app, "Management VM healthy", "관리 VM 정상")
        } else if host.is_object() {
            tr(app, "Host healthy", "호스트 정상")
        } else {
            tr(app, "Host status unavailable", "호스트 상태 확인 불가")
        },
        load,
        memory: percent_text(memory),
        disk: percent_text(disk),
    }
}
pub fn host_meters(app: &App) -> String {
    let status = host_status(app);
    format!(
        "<a class=\"host-status-link\" href=\"#/host\"><span class=\"host-status-head\"><span class=\"host-dot{}\" id=\"host-dot\" aria-hidden=\"true\"></span><span id=\"host-health\">{}</span></span><span class=\"host-status-values\">{} <span class=\"host-meter-value\" id=\"host-load\">{}</span> · {} <span class=\"host-meter-value\" id=\"host-memory\">{}</span> · {} <span class=\"host-meter-value\" id=\"host-disk\">{}</span></span></a>",
        if status.ok { " ok" } else { "" },
        status.head,
        tr(app, "Load", "부하"),
        status.load,
        tr(app, "Memory", "메모리"),
        status.memory,
        tr(app, "Disk", "디스크"),
        status.disk
    )
}
/// How long the host has been up, for people: "less than a minute", "12 min", "3 h 5 min", "2 d 4 h".
pub fn format_uptime(seconds: u64, ko: bool) -> String {
    let (minutes, hours, days) = (seconds / 60, seconds / 3600, seconds / 86_400);
    if seconds < 60 {
        (if ko {
            "1분 미만"
        } else {
            "less than a minute"
        })
        .into()
    } else if hours == 0 {
        if ko {
            format!("{minutes}분")
        } else {
            format!("{minutes} min")
        }
    } else if days == 0 {
        let (h, m) = (hours, minutes % 60);
        if ko {
            format!("{h}시간 {m}분")
        } else {
            format!("{h} h {m} min")
        }
    } else {
        let (d, h) = (days, hours % 24);
        if ko {
            format!("{d}일 {h}시간")
        } else {
            format!("{d} d {h} h")
        }
    }
}

/// A section of the host screen: the title and what it is at the left, the values at the right.
fn host_section(title: &str, description: &str, class: &str, aria: &str, body: &str) -> String {
    format!(
        "<section class=\"host-section {class}\"{aria}><div class=\"host-section-head\"><h3>{title}</h3><p>{description}</p></div><dl class=\"host-values\">{body}</dl></section>"
    )
}
fn host_row(label: &str, value: &str, class: &str, title: &str) -> String {
    format!(
        "<dt>{}</dt><dd class=\"{class}\" title=\"{}\">{}</dd>",
        escape(crate::i18n::t(label)),
        escape(title),
        escape(value)
    )
}
/// The screen of the machine that runs Firecrab: its resources, then its parts and versions.
fn host(app: &App) -> String {
    let ko = app.locale == "ko";
    let host = &app.resource;
    let platform = &host["platform"];
    let title = [text(platform, "name"), text(platform, "version")]
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    let title = if title.is_empty() {
        crate::i18n::t("Host").to_string()
    } else {
        title
    };
    let mib = |key: &str| host[key].as_u64().unwrap_or(0);
    let gib_of = |key: &str| host[key].as_u64().unwrap_or(0);
    let mut out = format!(
        "<div class=\"host-screen\"><h2 class=\"host-title\">{}</h2>",
        escape(&title)
    );
    // The resources in one line, like the summary of the MicroVM list.
    let memory_used = mib("memoryTotalMib").saturating_sub(mib("memoryAvailableMib"));
    let disk_used = gib_of("diskTotalGib").saturating_sub(gib_of("diskAvailableGib"));
    let item = |label: &str, value: String| {
        format!(
            "<div><dt>{}</dt><dd>{}</dd></div>",
            escape(crate::i18n::t(label)),
            value
        )
    };
    let resources = format!(
        "<dl class=\"host-resources\">{}{}{}{}</dl>",
        item(
            "Uptime",
            escape(&format_uptime(
                host["uptimeSeconds"].as_u64().unwrap_or(0),
                ko
            ))
        ),
        item(
            "Load",
            host["loadAverage1m"]
                .as_f64()
                .map_or("—".into(), |load| format!("{load:.2}"))
        ),
        item(
            "Memory",
            format!(
                "{} <small>/ {} GiB</small>",
                gib(memory_used),
                gib(mib("memoryTotalMib"))
            )
        ),
        item(
            "Disk",
            format!(
                "{disk_used} <small>/ {} GiB</small>",
                gib_of("diskTotalGib")
            )
        ),
    );
    let managed = managed_platform(host);
    if !managed {
        out.push_str(&format!(
            "<section aria-label=\"{}\">{resources}</section>",
            tr(app, "Host resource usage", "호스트 사용량")
        ));
    }
    // The outer platform and the managed Linux system are separate machines.
    let mut platform_rows = String::new();
    for (label, key) in [
        ("OS", "os"),
        ("Architecture", "architecture"),
        ("Virtualization", "virtualization"),
    ] {
        if !text(platform, key).is_empty() {
            platform_rows.push_str(&host_row(label, text(platform, key), "", ""));
        }
    }
    let mut system_rows = String::new();
    for (key, value) in platform.as_object().into_iter().flatten() {
        if !matches!(
            key.as_str(),
            "os" | "name" | "version" | "architecture" | "virtualization"
        ) && let Some(value) = value.as_str().filter(|v| !v.is_empty())
        {
            let row = host_row(key, value, "", "");
            if managed {
                system_rows.push_str(&row);
            } else {
                platform_rows.push_str(&row);
            }
        }
    }
    out.push_str(&host_section(
        tr(app, "Host system", "호스트 시스템"),
        tr(
            app,
            "The machine running Firecrab",
            "Firecrab을 실행하는 호스트",
        ),
        "host-info",
        "",
        &platform_rows,
    ));
    if managed && !system_rows.is_empty() {
        out.push_str(&host_section(
            tr(app, "Management Linux", "관리 Linux 환경"),
            tr(
                app,
                "The Linux environment MicroVMs run in",
                "MicroVM이 실행되는 Linux 환경",
            ),
            "host-info",
            "",
            &system_rows,
        ));
    }
    // Firecrab: its versions, services and folders.
    let info = app.cache.get("/api/info").unwrap_or(&Value::Null);
    let firecrab = if info.is_null() || info["status"] == "unavailable" {
        format!(
            "<p class=\"host-note\">{}</p>",
            view(
                app,
                "Could not load the Firecrab info.",
                "Firecrab 정보를 불러오지 못했습니다.",
            )
        )
    } else {
        let mut rows = String::new();
        for (label, key) in [
            ("Version", "version"),
            ("Firecracker", "firecrackerVersion"),
            ("API service", "apiService"),
            ("Helper service", "netHelperService"),
            ("Install prefix", "prefix"),
            ("Data directory", "datadir"),
            ("Config directory", "confdir"),
            ("systemd directory", "unitdir"),
            ("API address", "apiBase"),
        ] {
            let value = text(info, key);
            let (shown, class) = match (key, value) {
                ("firecrackerVersion", "") => (view(app, "Unavailable", "확인 불가"), ""),
                (key, "" | "unknown") if key.ends_with("Service") => {
                    (view(app, "Unavailable", "확인 불가"), "")
                }
                (key, "active") if key.ends_with("Service") => {
                    (view(app, "Running", "실행 중"), "")
                }
                (key, "failed") if key.ends_with("Service") => {
                    (view(app, "Failed", "실패"), "error")
                }
                (_, value) => (value, ""),
            };
            let unit = if key == "netHelperService" {
                text(info, "helperUnit")
            } else {
                ""
            };
            let mono =
                if key.ends_with("Service") || key == "version" || key == "firecrackerVersion" {
                    class.to_string()
                } else {
                    format!("{class} mono")
                };
            rows.push_str(&host_row(label, shown, mono.trim(), unit));
        }
        rows
    };
    out.push_str(&host_section(
        "Firecrab",
        tr(app, "Versions, services and folders", "버전, 서비스, 폴더"),
        "host-info",
        " role=\"region\" aria-label=\"Firecrab\"",
        &firecrab,
    ));
    // Updates: one line and the way to check.
    if let Some(update) = app.cache.get("/api/update") {
        let current = [text(update, "currentVersion"), text(update, "current")]
            .into_iter()
            .find(|v| !v.is_empty())
            .unwrap_or_else(|| text(info, "version"));
        let latest = [text(update, "latestVersion"), text(update, "latest")]
            .into_iter()
            .find(|v| !v.is_empty())
            .unwrap_or_default();
        let state = if update["updateAvailable"] == true {
            format!(
                "{} · {latest}",
                tr(app, "Update available", "업데이트 있음")
            )
        } else {
            format!("{} · {current}", tr(app, "Up to date", "최신 버전"))
        };
        let _ = write!(
            out,
            "{}",
            host_section(
                crate::i18n::t("Updates"),
                tr(app, "Firecrab releases", "Firecrab 릴리스"),
                "",
                "",
                &format!(
                    "<dt>{}</dt><dd>{} {}</dd>",
                    tr(app, "Status", "상태"),
                    escape(&state),
                    button("Check for updates", "update-open", "")
                )
            )
        );
    } else {
        out.push_str(&button("Updates", "update-open", ""));
    }
    let manager = app.cache.get("manager");
    if managed || manager.is_some() {
        out.push_str("<section class=\"wasm-details host-manager\" aria-label=\"microManager\"><h2>microManager</h2>");
        if managed {
            let _ = write!(
                out,
                "<h3>{}</h3><p class=\"host-note\">{}</p>{resources}",
                tr(app, "Current usage", "현재 사용량"),
                tr(
                    app,
                    "Usage of the Linux management VM, not the physical host.",
                    "관리 Linux VM의 사용량이다. 호스트 사용량은 포함하지 않는다."
                )
            );
        }
    }
    if let Some(manager) = manager {
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
            .map(|value| {
                format!(
                    "<p>{value} GiB {}</p>",
                    tr(app, "host disk available", "호스트 디스크 사용 가능")
                )
            })
            .unwrap_or_default();
        let _ = write!(
            out,
            "<h3>{}</h3>{}{available}{}{}{}",
            tr(app, "Configured resources", "설정된 리소스"),
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
    if managed || manager.is_some() {
        out.push_str("</section>");
    }
    out.push_str("</div>");
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

#[cfg(test)]
mod icon_button_tests {
    use super::*;
    use serde_json::json;

    fn app(route: &str) -> App {
        App {
            route: route.into(),
            ..Default::default()
        }
    }

    fn vm(state: &str) -> Vm {
        Vm {
            id: "vm-1".into(),
            name: "api".into(),
            state: state.into(),
            ..Default::default()
        }
    }

    // Every control must keep its name for assistive tools and for the tooltip.
    fn assert_icon_only(html: &str, label: &str) {
        assert!(
            html.contains(&format!("aria-label=\"{label}\"")),
            "{label}: {html}"
        );
        assert!(
            html.contains(&format!("title=\"{label}\"")),
            "{label}: {html}"
        );
        assert!(html.contains("<svg"), "{label}: {html}");
        assert!(!html.contains(&format!(">{label}<")), "{label}: {html}");
    }

    #[test]
    fn icons_share_one_stroke_width() {
        for icon in [Icon::Add, Icon::Close, Icon::Start, Icon::Open] {
            assert!(icon.svg().contains("stroke-width=\"1.75\""));
        }
        assert!(Icon::Stop.svg().contains("<path d=\"M7 7h10v10H7z\"/>"));
    }

    #[test]
    fn requests_keep_their_data_as_icon_buttons() {
        let stop = icon_request_with(
            "Stop service",
            "POST",
            "/api/micro-services/s1/stop",
            Value::Null,
            Icon::Stop,
        );
        assert_icon_only(&stop, "Stop service");
        assert!(
            stop.contains("data-method=\"POST\"")
                && stop.contains("data-path=\"/api/micro-services/s1/stop\"")
        );
        assert!(!stop.contains("danger"));
        let delete = icon_request("Delete", "/api/shells/s1");
        assert_icon_only(&delete, "Delete");
        assert!(
            delete.contains("class=\"icon-btn danger\"") && delete.contains("data-body=\"null\"")
        );
    }

    #[test]
    fn links_become_icon_links() {
        let console = icon_link(
            "Firecrab Console",
            "https://console.firecrab.dev/#account",
            true,
            Icon::Open,
        );
        assert_icon_only(&console, "Firecrab Console");
        assert!(console.contains("target=\"_blank\"") && console.contains("rel=\"noreferrer\""));
        assert!(!icon_link("S3", "#/pipe/storage", false, Icon::Open).contains("target="));
    }

    #[test]
    fn vm_rows_use_icons_for_every_action() {
        let html = vm_row(&app("vms"), &vm("running"));
        for label in ["Stop", "Pause", "Terminal (new tab)", "Actions"] {
            assert_icon_only(&html, label);
        }
        assert!(html.contains("href=\"#/console/vm-1\""));
        assert!(!html.contains('▣'));
        let stopped = vm_row(&app("vms"), &vm("stopped"));
        assert_icon_only(&stopped, "Start");
        assert!(
            stopped.contains(
                "disabled aria-label=\"Terminal (new tab)\" title=\"Terminal (new tab)\""
            )
        );
    }

    #[test]
    fn the_vm_panel_head_has_icon_actions_and_a_labelled_menu_button() {
        let running = vm_detail_actions(
            &app("vms"),
            &json!({"id": "vm-1", "name": "api", "state": "running"}),
        );
        let (head, _menu) = running.split_once("<details").unwrap();
        for label in ["Stop", "Pause", "Open console", "SSH connection"] {
            assert_icon_only(head, label);
        }
        assert!(!head.contains("class=\"btn\""));
        assert!(running.contains("<summary id=\"vm-more-summary\" class=\"icon-btn\" aria-label=\"More VM actions\" title=\"More VM actions\"><svg"));
        let stopped = vm_detail_actions(
            &app("vms"),
            &json!({"id": "vm-1", "name": "api", "state": "stopped"}),
        );
        assert_icon_only(stopped.split_once("<details").unwrap().0, "Start");
        let pausing = vm_detail_actions(
            &app("vms"),
            &json!({"id": "vm-1", "name": "api", "state": "pausing"}),
        );
        assert_icon_only(pausing.split_once("<details").unwrap().0, "Retry pause");
    }

    #[test]
    fn a_running_vm_head_reads_stop_pause_console_ssh_more() {
        let html = vm_detail_actions(
            &app("vms"),
            &json!({"id": "vm 1", "name": "api", "state": "running"}),
        );
        let at = |needle: &str| {
            html.find(needle)
                .unwrap_or_else(|| panic!("{needle}: {html}"))
        };
        let order = [
            at("aria-label=\"Stop\""),
            at("aria-label=\"Pause\""),
            at("aria-label=\"Open console\""),
            at("aria-label=\"SSH connection\""),
            at("<details id=\"vm-more-actions\""),
        ];
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{html}");
        // The console link opens the same page as the row's terminal link.
        assert!(
            html.contains("<a class=\"icon-btn\" href=\"#/console/vm%201\" target=\"_blank\" rel=\"noreferrer\" aria-label=\"Open console\""),
            "{html}"
        );
        // A running VM cannot be deleted or started, and SSH is not repeated in the menu.
        let (head, menu) = html.split_once("<details").unwrap();
        assert!(
            !head.contains("Delete") && !menu.contains("Delete"),
            "{html}"
        );
        assert!(!head.contains("aria-label=\"Start\""), "{html}");
        assert!(!menu.contains("SSH connection"), "{html}");
    }

    #[test]
    fn a_stopped_vm_head_has_start_and_delete_and_the_menu_has_no_delete() {
        for state in ["created", "stopped", "error"] {
            let html = vm_detail_actions(
                &app("vms"),
                &json!({"id": "vm 1", "name": "api", "state": state}),
            );
            let (head, menu) = html.split_once("<details").unwrap();
            assert_icon_only(head, "Start");
            assert_icon_only(head, "Delete");
            assert!(
                head.contains("class=\"icon-btn danger\"")
                    && head.contains("data-action=\"request\" data-method=\"DELETE\" data-path=\"/api/vms/vm%201\""),
                "{state}: {head}"
            );
            assert!(head.find("aria-label=\"Start\"") < head.find("aria-label=\"Delete\""));
            assert!(!head.contains("Open console") && !head.contains("aria-label=\"Stop\""));
            assert!(
                !menu.contains("Delete") && !menu.contains("vm-action-divider"),
                "{state}: {menu}"
            );
            assert!(
                menu.contains("Resources") && menu.contains("Download SSH key"),
                "{state}: {menu}"
            );
        }
    }

    #[test]
    fn paused_and_pending_vms_keep_resume_retry_and_stop_in_the_head() {
        for (state, label) in [
            ("paused", "Resume"),
            ("pausing", "Retry pause"),
            ("resuming", "Retry resume"),
        ] {
            let html = vm_detail_actions(
                &app("vms"),
                &json!({"id": "vm-1", "name": "api", "state": state}),
            );
            let head = html.split_once("<details").unwrap().0;
            assert_icon_only(head, label);
            assert_icon_only(head, "Stop");
            assert!(
                head.find(&format!("aria-label=\"{label}\"")) < head.find("aria-label=\"Stop\"")
            );
            for hidden in ["Delete", "Open console", "aria-label=\"Start\""] {
                assert!(!head.contains(hidden), "{state}: {head}");
            }
        }
    }

    #[test]
    fn pool_and_queue_vms_hide_the_lifecycle_actions() {
        for (purpose, state) in [
            ("pool", "stopped"),
            ("pool", "running"),
            ("queue", "stopped"),
        ] {
            let html = vm_detail_actions(
                &app("vms"),
                &json!({"id": "vm-1", "name": "api", "state": state, "purpose": purpose}),
            );
            let head = html.split_once("<details").unwrap().0;
            for hidden in ["Start", "Stop", "Pause", "Delete"] {
                assert!(
                    !head.contains(&format!("aria-label=\"{hidden}\"")),
                    "{purpose} {state}: {head}"
                );
            }
            assert!(!html.contains("Delete"), "{purpose} {state}: {html}");
        }
        let queue = vm_detail_actions(
            &app("vms"),
            &json!({"id": "vm-1", "name": "q", "state": "stopped", "purpose": "queue"}),
        );
        assert!(queue.contains(
            "<a class=\"icon-btn\" href=\"#/queues\" aria-label=\"Queue\" title=\"Queue\">"
        ));
    }

    #[test]
    fn generic_panel_actions_are_icon_buttons() {
        let data = json!({"id": "n1", "alias": "ubuntu", "version": "1"});
        for (route, labels) in [
            ("networks", vec!["Edit", "Delete"]),
            ("storages", vec!["Delete"]),
            ("shells", vec!["Publish revision", "Delete"]),
            (
                "images",
                vec![
                    "Download package",
                    "Install",
                    "Kernel",
                    "Delete image",
                    "Delete package",
                ],
            ),
            ("kernels", vec!["Build kernel", "Delete"]),
        ] {
            let mut state = app(route);
            let mut data = data.clone();
            if route == "kernels" {
                data["installed"] = true.into();
                state
                    .kernels
                    .installs
                    .insert("1".into(), json!({"status":"succeeded"}));
            }
            let html = detail_actions(&state, &data);
            for label in labels {
                assert_icon_only(&html, label);
            }
            assert!(!html.contains("class=\"btn"), "{route}: {html}");
        }
    }

    #[test]
    fn resource_tables_show_an_icon_only_details_button() {
        let html = resource_table(
            &app("networks"),
            &json!([{"id": "n1", "name": "prod"}]),
            &[("name", "Name")],
        );
        assert_icon_only(&html, "Details");
        assert!(html.contains("data-action=\"detail\" data-id=\"n1\""));
    }
}

#[cfg(test)]
mod list_part_tests {
    use super::*;

    fn app(route: &str) -> App {
        App {
            route: route.into(),
            ..Default::default()
        }
    }

    #[test]
    fn formats_bytes_for_people() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(12_582_912), "12 MiB");
        assert_eq!(human_bytes(1_610_612_736), "1.5 GiB");
    }

    #[test]
    fn rounds_bytes_into_the_next_unit() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1_048_575), "1.0 MiB");
        assert_eq!(human_bytes(10_485_759), "10 MiB");
        assert_eq!(human_bytes(2 * 1024 * 1024 * 1024 * 1024), "2048 GiB");
    }

    #[test]
    fn renders_status_and_name_cells() {
        assert_eq!(
            status_dot("ok", "Running"),
            "<span class=\"status\" data-tone=\"ok\"><span class=\"status-dot\" aria-hidden=\"true\"></span>Running</span>"
        );
        assert!(name_cell("orders", "", "", false).contains("<b class=\"\">orders</b>"));
        assert!(!name_cell("orders", "", "", false).contains("cell-sub"));
        assert!(name_cell("web", "Build failed", "error", false).contains("data-tone=\"error\""));
    }

    #[test]
    fn name_cells_mark_mono_names_and_escape_text() {
        let html = name_cell("<b>a</b>", "x & y", "busy", true);
        assert!(
            html.contains("<b class=\"mono\">&lt;b&gt;a&lt;/b&gt;</b>"),
            "{html}"
        );
        assert!(html.contains("<span class=\"cell-sub\" data-tone=\"busy\">x &amp; y</span>"));
    }

    #[test]
    fn renders_a_dash_for_empty_chips() {
        assert_eq!(chips(&[]), "<span class=\"none\">—</span>");
        assert_eq!(
            chips(&["api".into(), "<db>".into()]),
            "<div class=\"chips\"><span class=\"chip\">api</span><span class=\"chip\">&lt;db&gt;</span></div>"
        );
    }

    #[test]
    fn renders_an_empty_state_with_an_icon_tile() {
        let html = empty_state("networks", "No networks", "Add one.", "<a>New</a>");
        assert!(
            html.starts_with("<div class=\"empty-state\" role=\"status\">"),
            "{html}"
        );
        assert!(html.contains("<span class=\"empty-state-icon\" aria-hidden=\"true\"><svg"));
        assert!(html.contains("<strong class=\"empty-state-title\">No networks</strong><p>Add one.</p><a>New</a></div>"));
    }

    #[test]
    fn renders_a_status_line() {
        let html = status_line(
            "busy",
            "Engine",
            &["Starting".into(), "<span class=\"mono\">5672</span>".into()],
            "<i></i>",
            "Wait & see",
        );
        assert_eq!(
            html,
            "<div class=\"status-line\" data-tone=\"busy\"><b>Engine</b>Starting<span class=\"sep\">·</span><span class=\"mono\">5672</span><span class=\"status-line-message\">Wait &amp; see</span><span class=\"spacer\"></span><i></i></div>"
        );
    }

    #[test]
    fn renders_subtabs_with_counts() {
        let html = subtabs(
            "cluster",
            "Cluster sections",
            &[("hosts", "Hosts", 2), ("placements", "Placements", 0)],
            "placements",
        );
        assert!(html.starts_with(
            "<div class=\"subtabs\" role=\"tablist\" aria-label=\"Cluster sections\">"
        ));
        assert!(html.contains("role=\"tab\" class=\"subtab\" aria-selected=\"false\" data-action=\"subtab\" data-id=\"cluster:hosts\">Hosts <em>2</em></button>"), "{html}");
        assert!(html.contains("aria-selected=\"true\" data-action=\"subtab\" data-id=\"cluster:placements\">Placements <em>0</em></button>"), "{html}");
    }

    #[test]
    fn remembers_the_active_subtab_per_screen() {
        let mut state = app("cluster");
        assert_eq!(active_subtab(&state, "cluster", "hosts"), "hosts");
        state.subtabs.insert("cluster".into(), "services".into());
        assert_eq!(active_subtab(&state, "cluster", "hosts"), "services");
        assert_eq!(active_subtab(&state, "images", "installed"), "installed");
    }

    #[test]
    fn shows_a_dash_for_empty_values() {
        for empty in [json!(null), json!(""), json!([]), json!({})] {
            assert_eq!(display(&empty), "<span class=\"none\">—</span>", "{empty}");
        }
        assert_eq!(display(&json!("a<b")), "a&lt;b");
        assert_eq!(display(&json!(true)), "Yes");
        assert_eq!(display(&json!(0)), "0");
    }

    #[test]
    fn resource_tables_lead_with_a_name_cell_and_row_actions() {
        let html = resource_table(
            &app("networks"),
            &json!([{"id": "n1", "name": "prod", "gateway": null, "subnetCidr": "10.0.0.0/24"}]),
            &[("name", "Name"), ("gateway", "Gateway")],
        );
        assert!(
            html.contains("<table class=\"wasm-table network-table\">"),
            "{html}"
        );
        assert!(html.contains("<td headers=\"network-column-0\"><div class=\"cell-name\"><b class=\"\">prod</b><span class=\"cell-sub\" data-tone=\"\">10.0.0.0/24</span></div></td>"), "{html}");
        assert!(
            html.contains("<td headers=\"network-column-1\" class=\"col-secondary\"><span class=\"none\">—</span></td>"),
            "{html}"
        );
        assert!(html.contains("<td class=\"row-actions\"><button"), "{html}");
        assert!(
            html.contains("<th class=\"row-actions\"><span class=\"sr-only\">Actions</span></th>"),
            "{html}"
        );
    }

    #[test]
    fn kernel_tables_have_no_actions_column() {
        let html = resource_table(
            &app("kernels"),
            &json!([{"version": "6.12.1", "installed": true}]),
            &[("version", "Version"), ("installed", "Installed")],
        );
        assert!(!html.contains("row-actions"), "{html}");
        assert!(!html.contains("Actions"), "{html}");
        assert!(!html.contains("data-action=\"detail\""), "{html}");
        assert_eq!(html.matches("<th ").count(), 2, "{html}");
    }

    #[test]
    fn image_table_hides_uninstalled_default_images_but_keeps_usable_images() {
        let columns = &[("alias", "Image"), ("installed", "Installed")];
        let defaults = json!([
            {"alias": "alpine-3.24.1", "installed": false},
            {"alias": "rocky-9.8", "installed": false},
            {"alias": "ubuntu-26.04", "installed": false}
        ]);
        let html = resource_table(&app("images"), &defaults, columns);
        assert!(html.contains("No images installed"), "{html}");
        assert!(!html.contains("alpine-3.24.1"), "{html}");
        assert!(!html.contains("rocky-9.8"), "{html}");
        assert!(!html.contains("ubuntu-26.04"), "{html}");

        let images = json!([
            {"alias": "alpine-3.24.1", "installed": true},
            {"alias": "rocky-9.8", "installed": false, "packageStaged": true},
            {"alias": "custom", "installed": true},
            {"alias": "ubuntu-26.04", "installed": false}
        ]);
        let html = resource_table(&app("images"), &images, columns);
        assert!(html.contains("alpine-3.24.1"), "{html}");
        assert!(html.contains("rocky-9.8"), "{html}");
        assert!(html.contains("custom"), "{html}");
        assert!(!html.contains("ubuntu-26.04"), "{html}");
    }

    #[test]
    fn empty_resource_tables_show_the_shared_empty_state() {
        let html = resource_table(&app("storages"), &json!([]), &[("name", "Name")]);
        assert!(
            html.starts_with("<div class=\"empty-state\" role=\"status\">"),
            "{html}"
        );
        assert!(html.contains("empty-state-title"), "{html}");
        assert!(!html.contains("<table"), "{html}");
        let korean = resource_table(
            &App {
                route: "storages".into(),
                locale: "ko".into(),
                ..Default::default()
            },
            &json!([]),
            &[("name", "Name")],
        );
        assert!(
            korean.contains("등록한 VM 디스크 저장소가 없다"),
            "{korean}"
        );
    }
}

#[cfg(test)]
mod vm_list_markup_tests {
    use super::*;
    use serde_json::json;

    fn app(locale: &str) -> App {
        App {
            route: "vms".into(),
            locale: locale.into(),
            ..Default::default()
        }
    }

    fn vm(state: &str, ram: u32) -> Vm {
        Vm {
            id: "vm-1".into(),
            name: "api".into(),
            state: state.into(),
            cpu: 2,
            ram,
            disk_gb: 8,
            ..Default::default()
        }
    }

    #[test]
    fn summary_items_hold_a_label_a_value_and_at_most_a_bar() {
        let mut state = app("en");
        state.vms = vec![vm("running", 4096), vm("stopped", 4096)];
        state.cache.insert(
            "/api/host".into(),
            json!({"memoryTotalMib": 65536.0, "diskTotalGib": 1000.0}),
        );
        let html = vm_summary(&state);
        assert_eq!(
            html.matches("<div class=\"vm-summary-item\">").count(),
            4,
            "{html}"
        );
        for part in [
            "<dt>Running</dt><dd><span class=\"vm-summary-value\">1<small>/ 2</small></span></dd>",
            "<dt>vCPU allocated</dt><dd><span class=\"vm-summary-value\">4</span></dd>",
            "<dt>Memory allocated</dt><dd><span class=\"vm-summary-value\">8<small>/ 64 GiB</small></span><span class=\"vm-summary-bar\" aria-hidden=\"true\"><i style=\"width:12.5%\"></i></span></dd>",
            "<dt>Disk allocated</dt><dd><span class=\"vm-summary-value\">16<small>/ 1000 GB</small></span><span class=\"vm-summary-bar\" aria-hidden=\"true\"><i style=\"width:1.6%\"></i></span></dd>",
        ] {
            assert!(html.contains(part), "{part}\n{html}");
        }
        assert!(
            !html.contains("<svg") && !html.contains("vm-summary-note"),
            "{html}"
        );
    }

    #[test]
    fn summary_without_a_host_total_has_no_slash_and_no_bar() {
        let mut state = app("en");
        state.vms = vec![vm("running", 4096)];
        let html = vm_summary(&state);
        assert!(
            html.contains("<span class=\"vm-summary-value\">4<small>GiB</small></span>"),
            "{html}"
        );
        assert!(
            html.contains("<span class=\"vm-summary-value\">8<small>GB</small></span>"),
            "{html}"
        );
        assert!(!html.contains("vm-summary-bar"), "{html}");
        // Only the running count has a total of its own.
        assert_eq!(html.matches("<small>/").count(), 1, "{html}");
    }

    #[test]
    fn a_summary_bar_turns_amber_from_80_percent() {
        let mut state = app("en");
        state.vms = vec![vm("running", 52430)];
        state.cache.insert(
            "/api/host".into(),
            json!({"memoryTotalMib": 65536.0, "diskTotalGib": 1000.0}),
        );
        let html = vm_summary(&state);
        assert!(
            html.contains(
                "<span class=\"vm-summary-bar warn\" aria-hidden=\"true\"><i style=\"width:80.0%\">"
            ),
            "{html}"
        );
        assert_eq!(html.matches("vm-summary-bar warn").count(), 1, "{html}");
    }

    #[test]
    fn memory_sizes_keep_one_decimal_only_when_they_need_it() {
        assert_eq!(gib(6144), "6");
        assert_eq!(gib(4608), "4.5");
        assert_eq!(gib(512), "0.5");
        assert_eq!(gib(0), "0");
        let mut state = app("en");
        state.vms = vec![vm("running", 1536)];
        assert!(
            vm_summary(&state)
                .contains("<span class=\"vm-summary-value\">1.5<small>GiB</small></span>")
        );
    }

    #[test]
    fn the_summary_reads_in_korean() {
        let mut state = app("ko");
        state.vms = vec![vm("running", 1024)];
        let html = vm_summary(&state);
        for label in ["실행 중", "vCPU 할당", "메모리 할당", "디스크 할당"] {
            assert!(
                html.contains(&format!("<dt>{label}</dt>")),
                "{label}: {html}"
            );
        }
    }

    #[test]
    fn the_empty_list_uses_the_shared_empty_state() {
        let html = vm_empty(&app("en"));
        assert!(html.starts_with("<div class=\"empty-state\" role=\"status\"><span class=\"empty-state-icon\" aria-hidden=\"true\"><svg"), "{html}");
        assert!(html.contains("<strong class=\"empty-state-title\">Create your first MicroVM</strong><p>Pick an image and resources. It starts in a few seconds.</p><a class=\"btn primary\" href=\"#/vms/new\">New MicroVM</a></div>"), "{html}");
        let korean = vm_empty(&app("ko"));
        assert!(korean.contains("첫 MicroVM을 만듭니다"), "{korean}");
        assert!(
            korean.contains("<p>이미지와 사양을 고르면 몇 초 안에 시작합니다.</p>"),
            "{korean}"
        );
        assert!(korean.contains(">새 MicroVM</a>"), "{korean}");
    }

    #[test]
    fn a_filter_with_no_match_offers_to_clear_the_filters() {
        let mut state = app("en");
        state.vms = vec![vm("running", 1024)];
        let html = vm_empty(&state);
        assert!(
            html.starts_with("<div class=\"empty-state\" role=\"status\">"),
            "{html}"
        );
        assert!(html.contains("No matching MicroVMs"), "{html}");
        assert!(html.contains("data-action=\"vm-clear-filters\""), "{html}");
        assert!(!html.contains("href=\"#/vms/new\""), "{html}");
    }

    #[test]
    fn the_list_marks_its_secondary_columns_for_narrow_screens() {
        let mut state = app("en");
        state.loaded = true;
        let table = content(&state);
        assert!(
            table.contains("<table class=\"vm-list wasm-table\""),
            "{table}"
        );
        assert!(
            table.contains("<th class=\"vm-col-ip col-secondary\">IPv4</th>"),
            "{table}"
        );
        assert!(
            table.contains("<th class=\"vm-col-mem col-secondary\">Memory</th>"),
            "{table}"
        );
        assert!(
            table.contains("<col class=\"vm-col-ip col-secondary\">")
                && table.contains("<col class=\"vm-col-mem col-secondary\">"),
            "{table}"
        );
        assert!(
            table.contains("<th class=\"col-secondary\">CPU</th>")
                && table.contains("<col class=\"vm-col-cpu col-secondary\">"),
            "{table}"
        );
        // The name, the state and the row actions never hide.
        assert!(table.contains("<th>Name</th><th>State</th>"), "{table}");
        assert!(
            table.contains("<th><span class=\"sr-only\">Actions</span></th>"),
            "{table}"
        );
        assert!(!table.contains("vm-overview"), "{table}");
        let row = vm_row(&state, &vm("running", 1024));
        assert!(
            row.contains("<td class=\"vm-col-ip col-secondary\">"),
            "{row}"
        );
        assert!(
            row.contains("<td class=\"vm-col-mem col-secondary\">"),
            "{row}"
        );
        assert!(
            row.contains("<td class=\"col-secondary\"><span class=\"vm-usage\">"),
            "{row}"
        );
        assert!(row.contains("<td class=\"vm-row-actions\">"), "{row}");
    }

    #[test]
    fn the_terminal_link_is_an_icon_link_that_opens_a_new_tab() {
        let row = vm_row(&app("en"), &vm("running", 1024));
        assert!(
            row.contains("<a class=\"icon-btn\" href=\"#/console/vm-1\" target=\"_blank\" rel=\"noreferrer\" aria-label=\"Terminal (new tab)\" title=\"Terminal (new tab)\"><svg"),
            "{row}"
        );
    }

    #[test]
    fn a_memory_meter_turns_amber_from_80_percent() {
        let busy = Vm {
            memory_used_percent: Some(80.0),
            ..vm("running", 1024)
        };
        let calm = Vm {
            memory_used_percent: Some(79.9),
            ..vm("running", 1024)
        };
        assert!(vm_row(&app("en"), &busy).contains("<span class=\"vm-meter warn\">"));
        assert!(vm_row(&app("en"), &calm).contains("<span class=\"vm-meter\">"));
    }

    #[test]
    fn the_second_line_is_red_after_a_failure_and_amber_while_starting() {
        let failed = Vm {
            extra: json!({"startupTimeline": [{"outcome": "failed", "detail": "no rootfs"}]})
                .as_object()
                .cloned()
                .unwrap(),
            ..vm("error", 1024)
        };
        let starting = Vm {
            extra: json!({"startupStep": "startingProcess"})
                .as_object()
                .cloned()
                .unwrap(),
            ..vm("starting", 1024)
        };
        let plain = vm_row(&app("en"), &vm("stopped", 1024));
        assert!(
            vm_row(&app("en"), &failed).contains("<div class=\"vm-row-sub error\">no rootfs</div>")
        );
        assert!(vm_row(&app("en"), &starting).contains("<div class=\"vm-row-sub busy\">"));
        assert!(plain.contains("<div class=\"vm-row-sub\">"));
    }

    #[test]
    fn the_new_microvm_page_has_its_own_head_and_one_heading() {
        let page = App {
            route: "vms/new".into(),
            form: Some(crate::forms::vm_create(&Default::default())),
            ..Default::default()
        };
        let html = content(&page);
        assert_eq!(html.matches("<h2").count(), 1, "{html}");
        assert!(html.starts_with("<div class=\"form-page\"><header><h2>New MicroVM</h2><p>Pick an image and resources. The MicroVM starts in a few seconds.</p></header><form id=\"editor\""), "{html}");
        assert!(html.ends_with("</form></div>"), "{html}");
    }

    #[test]
    fn the_new_microvm_page_head_reads_in_korean() {
        let page = App {
            route: "vms/new".into(),
            locale: "ko".into(),
            form: Some(crate::forms::vm_create(&Default::default())),
            ..Default::default()
        };
        crate::i18n::set("ko");
        let html = content(&page);
        crate::i18n::set("en");
        assert!(html.starts_with("<div class=\"form-page\"><header><h2>새 MicroVM</h2><p>이미지와 사양을 고르면 몇 초 안에 시작합니다.</p></header>"), "{html}");
    }

    #[test]
    fn the_new_microvm_page_has_no_header_button_to_itself() {
        let mut state = app("en");
        state.route = "vms/new".into();
        let html = page(&state);
        assert!(!html.contains("id=\"vm-list-add\""), "{html}");
        assert!(!html.contains("href=\"#/vms/new\""), "{html}");
        // The list keeps its primary action.
        state.route = "vms".into();
        assert!(page(&state).contains(
            "<a class=\"btn primary\" id=\"vm-list-add\" href=\"#/vms/new\">New MicroVM</a>"
        ));
    }
}

#[cfg(test)]
mod service_list_tests {
    use super::*;
    use serde_json::json;

    fn app(locale: &str) -> App {
        App {
            route: "services".into(),
            locale: locale.into(),
            loaded: true,
            ..Default::default()
        }
    }

    fn orders() -> Value {
        json!({"id": "s1", "name": "orders", "image": "ghcr.io/team/orders:1.0", "port": 8080, "replicas": 3, "domain": "orders.example.com"})
    }

    fn worker() -> Value {
        json!({"id": "s2", "name": "worker", "image": "worker:2.0", "port": 9000, "replicas": 2, "domain": null})
    }

    fn running() -> Value {
        json!({
            "id": "dep-1", "serviceId": "s1", "phase": "running", "domainRouteOwned": true,
            "spec": {"domain": "orders.example.com"},
            "instances": [
                {"vmId": "vm-aaaa", "ipv4": "172.30.0.2", "hostPort": 30100, "healthy": true},
                {"vmId": "vm-bbbb", "ipv4": "172.30.0.3", "hostPort": 30101, "healthy": false}
            ],
            "log": ["queued", "healthy"], "error": null
        })
    }

    #[test]
    fn the_table_reads_name_port_replicas_domain_and_row_actions() {
        let mut state = app("en");
        state.resource = json!([orders(), worker()]);
        let html = services(&state);
        for head in ["Service", "Port", "Replicas", "Domain"] {
            assert!(html.contains(&format!(">{head}</th>")), "{head}: {html}");
        }
        assert!(!html.contains(">State<"), "no state column: {html}");
        assert!(
            html.contains("<th class=\"row-actions\"><span class=\"sr-only\">Actions</span></th>"),
            "{html}"
        );
        // The name is a Task 7 name cell; the line under it is the image, cut short, with the whole reference as its tooltip.
        assert!(html.contains("<div class=\"cell-name\"><b><button type=\"button\" class=\"cell-open\" data-action=\"detail\" data-id=\"s1\" aria-label=\"Open orders\" title=\"orders\">orders</button></b><span class=\"cell-sub\" data-tone=\"\" title=\"ghcr.io/team/orders:1.0\">orders:1.0</span></div>"), "{html}");
        assert!(
            !html.contains("service-image") && !html.contains("service-name"),
            "{html}"
        );
        // The port and replica cells keep the ids the headers point at.
        assert!(
            html.contains("<th id=\"service-column-2\">Port</th>"),
            "{html}"
        );
        assert!(
            html.contains("<td headers=\"service-column-2\"><span class=\"mono\">8080</span></td>"),
            "{html}"
        );
        assert!(
            html.contains("<td headers=\"service-column-3\">3</td>"),
            "{html}"
        );
        // A domain is mono; no domain is a dash.
        assert!(
            html.contains(
                "<span class=\"mono\" title=\"orders.example.com\">orders.example.com</span>"
            ),
            "{html}"
        );
        assert!(html.contains("<td headers=\"service-column-4\" class=\"col-secondary\"><span class=\"none\">—</span></td>"), "{html}");
    }

    #[test]
    fn a_row_has_a_deploy_icon_and_a_menu_with_edit_and_delete() {
        let mut state = app("en");
        state.resource = json!([orders()]);
        let html = services(&state);
        let (_, actions) = html
            .split_once("<td class=\"row-actions\">")
            .expect("actions");
        assert!(actions.contains("aria-label=\"Deploy orders\" title=\"Deploy orders\" data-action=\"service-run\" data-id=\"s1\""), "{actions}");
        assert!(actions.contains("<summary class=\"icon-btn\" aria-label=\"Actions for orders\" title=\"Actions for orders\"><svg"), "{actions}");
        // Today's menu: Edit and Delete, named for the service.
        assert!(
            actions.contains("data-action=\"resource-edit\" data-id=\"s1\">Edit orders</button>"),
            "{actions}"
        );
        assert!(
            actions.contains("data-path=\"/api/micro-services/s1\"")
                && actions.contains(">Delete orders</button>"),
            "{actions}"
        );
        // The deploy icon comes first; the menu follows.
        assert!(actions.find("Deploy orders").unwrap() < actions.find("<details").unwrap());
        assert!(!actions.contains(">Deploy orders<"), "icon only: {actions}");
    }

    #[test]
    fn the_selected_row_is_marked_and_the_domain_column_is_secondary() {
        let mut state = app("en");
        state.resource = json!([orders(), worker()]);
        state.detail = Some(orders());
        let html = services(&state);
        assert!(
            html.contains("<tr data-id=\"s1\" aria-selected=\"true\">"),
            "{html}"
        );
        assert!(
            html.contains("<tr data-id=\"s2\" aria-selected=\"false\">"),
            "{html}"
        );
        assert!(
            html.contains("<col class=\"service-col-domain col-secondary\">"),
            "{html}"
        );
        assert!(
            html.contains("<th id=\"service-column-4\" class=\"col-secondary\">Domain</th>"),
            "{html}"
        );
        // Port and replicas stay on a phone.
        assert!(
            !html.contains("<th id=\"service-column-2\" class=\"col-secondary\""),
            "{html}"
        );
    }

    #[test]
    fn a_digest_image_is_cut_short_and_the_title_keeps_all_of_it() {
        let mut state = app("en");
        let image = format!(
            "118167167440.dkr.ecr.ap-northeast-2.amazonaws.com/ecr/nginx@sha256:{}",
            "a".repeat(64)
        );
        state.resource =
            json!([{"id": "s1", "name": "web", "image": image, "port": 80, "replicas": 1}]);
        let html = services(&state);
        assert!(
            html.contains(&format!(
                "title=\"{image}\">nginx@sha256:aaaaaaaaaaaa…</span>"
            )),
            "{html}"
        );
    }

    #[test]
    fn the_table_and_its_row_actions_read_in_korean() {
        let mut state = app("ko");
        state.resource = json!([orders()]);
        crate::i18n::set("ko");
        let html = services(&state);
        crate::i18n::set("en");
        for text in [
            ">서비스</th>",
            ">포트</th>",
            ">복제 수</th>",
            ">도메인</th>",
            "aria-label=\"orders 배포\"",
            "aria-label=\"orders 열기\"",
            "aria-label=\"orders 작업\"",
        ] {
            assert!(html.contains(text), "{text}: {html}");
        }
    }

    #[test]
    fn an_empty_list_explains_the_screen_and_offers_the_first_service() {
        let html = services(&app("en"));
        assert!(html.starts_with("<div class=\"empty-state\" role=\"status\"><span class=\"empty-state-icon\" aria-hidden=\"true\"><svg"), "{html}");
        assert!(html.contains("<strong class=\"empty-state-title\">No MicroServices yet</strong><p>Run a container image on MicroVMs and deploy without downtime.</p><a class=\"btn primary\" href=\"#/services/new\">New MicroService</a></div>"), "{html}");
        let korean = services(&app("ko"));
        assert!(korean.contains("아직 MicroService가 없습니다"), "{korean}");
        assert!(
            korean
                .contains("<p>컨테이너 이미지를 MicroVM에서 실행하고, 무중단으로 배포합니다.</p>"),
            "{korean}"
        );
        assert!(korean.contains(">새 MicroService</a>"), "{korean}");
    }

    #[test]
    fn the_deployment_splits_into_a_summary_and_an_instances_table() {
        let value = running();
        let summary = deployment_summary(Some(&value));
        assert!(summary.contains("<p class=\"service-state\" data-phase=\"running\" role=\"status\"><span class=\"status\" data-tone=\"ok\">"), "{summary}");
        assert!(
            summary.contains("<span class=\"service-health\">1/2 healthy</span>"),
            "{summary}"
        );
        assert!(summary.contains("aria-label=\"Open domain\""), "{summary}");
        assert!(
            !summary.contains("<table") && !summary.contains("<details"),
            "{summary}"
        );
        let instances = deployment_instances(Some(&value));
        assert!(
            instances.starts_with(
                "<div class=\"table-scroll\"><table class=\"wasm-table service-instances\">"
            ),
            "{instances}"
        );
        // The VM id is a mono name cell with its whole text as the tooltip; health is a status dot.
        assert!(instances.contains("<td><div class=\"cell-name\"><b class=\"mono\" title=\"vm-aaaa\">vm-aaaa</b></div></td>"), "{instances}");
        assert!(instances.contains("<span class=\"status\" data-tone=\"ok\"><span class=\"status-dot\" aria-hidden=\"true\"></span>Healthy</span>"), "{instances}");
        assert!(instances.contains("data-tone=\"error\"><span class=\"status-dot\" aria-hidden=\"true\"></span>Unhealthy</span>"), "{instances}");
        assert!(!instances.contains("<details"), "{instances}");
        // The Deploy form shows both, as one block.
        assert_eq!(deployment(Some(&value)), format!("{summary}{instances}"));
    }

    /// Every phase the API type `ServicePhase` has, with its English and Korean label.
    const PHASES: [(&str, &str, &str); 13] = [
        ("queued", "Queued", "대기"),
        ("importing", "Preparing image", "이미지 준비 중"),
        ("starting", "Starting replicas", "복제본 시작 중"),
        ("checking_health", "Checking health", "상태 확인 중"),
        ("connecting_domain", "Connecting domain", "도메인 연결 중"),
        (
            "switching_traffic",
            "Switching Nginx traffic",
            "Nginx 트래픽 전환 중",
        ),
        (
            "draining",
            "Draining previous requests",
            "이전 요청 정리 중",
        ),
        (
            "rolling_back",
            "Restoring previous release",
            "이전 릴리스 복원 중",
        ),
        ("running", "Running", "실행 중"),
        ("degraded", "Degraded", "일부 비정상"),
        ("stopping", "Stopping service", "서비스 중지 중"),
        ("stopped", "Stopped", "중지됨"),
        ("failed", "Deployment failed", "배포 실패"),
    ];

    #[test]
    fn every_phase_has_an_english_and_a_korean_label() {
        for (phase, english, _) in PHASES {
            assert_eq!(phase_label(phase), english, "{phase}");
        }
        crate::i18n::set("ko");
        let korean: Vec<_> = PHASES
            .iter()
            .map(|(phase, _, _)| phase_label(phase))
            .collect();
        crate::i18n::set("en");
        assert_eq!(korean, PHASES.map(|(_, _, korean)| korean));
    }

    #[test]
    fn an_unknown_phase_reads_as_words_never_as_snake_case() {
        assert_eq!(phase_label("warming_up_caches"), "Warming up caches");
        assert_eq!(phase_label("paused"), "Paused");
        assert_eq!(phase_label(""), "");
        assert!(!phase_label("a_b_c").contains('_'));
    }

    #[test]
    fn every_phase_has_a_tone() {
        for (phase, tone) in [
            ("running", "ok"),
            ("failed", "error"),
            ("degraded", "error"),
            ("draining", "busy"),
            ("queued", "busy"),
            ("importing", "busy"),
            ("starting", "busy"),
            ("checking_health", "busy"),
            ("connecting_domain", "busy"),
            ("switching_traffic", "busy"),
            ("rolling_back", "busy"),
            ("stopping", "busy"),
            ("stopped", "off"),
            ("", "off"),
        ] {
            assert_eq!(phase_tone(phase), tone, "{phase}");
        }
    }

    #[test]
    fn a_deployment_without_instances_has_no_table_and_a_missing_one_says_so() {
        let value = json!({"phase": "stopped", "instances": []});
        assert_eq!(deployment_instances(Some(&value)), "");
        assert_eq!(deployment_instances(None), "");
        assert!(deployment_summary(Some(&json!(null))).contains("Not deployed"));
        assert!(deployment_summary(None).contains("Loading…"));
        // The failure message is an alert in the summary.
        let failed =
            json!({"phase": "failed", "instances": [], "error": "App health check failed"});
        let summary = deployment_summary(Some(&failed));
        assert!(summary.contains("data-tone=\"error\""), "{summary}");
        assert!(
            summary.contains("<p role=\"alert\">App health check failed</p>"),
            "{summary}"
        );
    }

    #[test]
    fn the_log_block_is_the_lines_as_text() {
        let log = deployment_log(Some(&running()));
        assert_eq!(log, "<pre class=\"wasm-log\">queued\nhealthy</pre>");
        assert_eq!(deployment_log(Some(&json!({"log": []}))), "");
        assert_eq!(deployment_log(None), "");
    }

    #[test]
    fn the_head_icons_are_deploy_roll_back_and_more_with_stop_inside_the_menu() {
        let mut state = app("en");
        state.detail = Some(orders());
        // Not deployed: Deploy opens the Deploy form. No release to go back to, so no Roll back, and nothing to stop.
        state.service_deployment = Some(json!(null));
        let html = detail_actions(&state, &orders());
        assert!(html.contains("aria-label=\"Deploy\" title=\"Deploy\" data-action=\"edit\" data-id=\"service-deploy\""), "{html}");
        assert!(
            !html.contains("Roll back") && !html.contains("Stop service"),
            "{html}"
        );
        // Running with a domain: the first icon is still the rocket. It opens the update form of
        // the Releases tab, and says so. Roll back opens the Releases tab.
        state.service_deployment = Some(running());
        let html = detail_actions(&state, &orders());
        let (head, menu) = html.split_once("<details").unwrap();
        assert!(
            head.contains(
                "aria-label=\"Deploy update\" title=\"Deploy update\" data-action=\"deploy-update\""
            ),
            "{head}"
        );
        assert!(
            !head.contains("aria-label=\"Deploy\"") && !head.contains("Stop service"),
            "{head}"
        );
        assert!(
            head.find("Deploy update").unwrap() < head.find("aria-label=\"Roll back\"").unwrap()
        );
        assert!(head.contains("aria-label=\"Roll back\" title=\"Roll back\" data-action=\"detail-tab\" data-id=\"releases\""), "{head}");
        // More holds Stop service first (it keeps its id), then today's edit and delete.
        assert!(menu.contains("<summary id=\"vm-more-summary\" class=\"icon-btn\" aria-label=\"More\" title=\"More\"><svg"), "{menu}");
        let stop = menu.find("id=\"service-stop\"").expect("Stop in More");
        assert!(menu.contains("data-action=\"request\" data-method=\"POST\" data-path=\"/api/micro-services/s1/stop\""), "{menu}");
        assert!(menu[stop..].contains(">Stop service</button>"), "{menu}");
        assert!(
            stop < menu.find("Edit configuration").unwrap()
                && menu.find("Edit configuration").unwrap()
                    < menu.find(">Delete</button>").unwrap(),
            "{menu}"
        );
        assert!(
            menu.contains(
                "data-action=\"resource-edit\" data-id=\"s1\">Edit configuration</button>"
            ),
            "{menu}"
        );
        // Stopped: Stop leaves the menu, and Deploy is back.
        let mut stopped = running();
        stopped["phase"] = json!("stopped");
        stopped["instances"] = json!([]);
        stopped["domainRouteOwned"] = json!(false);
        state.service_deployment = Some(stopped);
        let html = detail_actions(&state, &orders());
        assert!(
            html.contains("aria-label=\"Deploy\"") && !html.contains("Stop service"),
            "{html}"
        );
    }

    #[test]
    fn the_update_icon_and_stop_wait_while_a_deployment_is_going_or_there_is_no_domain() {
        let mut state = app("en");
        state.detail = Some(orders());
        // Going: neither the update nor Stop can start another operation.
        let mut going = running();
        going["phase"] = json!("draining");
        state.service_deployment = Some(going);
        let html = detail_actions(&state, &orders());
        assert!(
            html.contains(
                "<button type=\"button\" disabled class=\"icon-btn\" aria-label=\"Deploy update\""
            ),
            "{html}"
        );
        assert!(html.contains("id=\"service-stop\" disabled"), "{html}");
        // Running without a domain: there is no Nginx route to update through. The icon is there, disabled; no releases.
        let mut plain = running();
        plain["spec"]["domain"] = json!("");
        state.service_deployment = Some(plain);
        let html = detail_actions(&state, &worker());
        assert!(
            html.contains(
                "<button type=\"button\" disabled class=\"icon-btn\" aria-label=\"Deploy update\""
            ),
            "{html}"
        );
        assert!(!html.contains("Roll back"), "{html}");
        assert!(
            html.contains("id=\"service-stop\" class=\"btn \""),
            "{html}"
        );
        // A request in flight holds the update, as it holds Stop.
        state.service_deployment = Some(running());
        state.busy = true;
        assert!(detail_actions(&state, &orders()).contains(
            "<button type=\"button\" disabled class=\"icon-btn\" aria-label=\"Deploy update\""
        ));
    }

    #[test]
    fn an_update_reads_from_release_to_release_with_a_bar_while_it_goes() {
        let update = json!({"strategy": "blue_green", "completed": false, "previous": {"id": "r2222222-aaaa"}});
        let mut value = running();
        value["id"] = json!("r3333333-aaaa");
        value["phase"] = json!("switching_traffic");
        value["update"] = update;
        value["spec"]["replicas"] = json!(3);
        let html = deployment_summary(Some(&value));
        // The phase, then the health out of the replicas the service asks for.
        assert!(
            html.contains("<span class=\"service-health\">1/3 healthy</span>"),
            "{html}"
        );
        assert!(html.contains("<p class=\"service-update\"><span class=\"mono\" title=\"r2222222-aaaa\">r2222222</span> → <span class=\"mono\" title=\"r3333333-aaaa\">r3333333</span> · Blue-green</p>"), "{html}");
        // The bar is a progressbar, with one cell per replica: healthy, the next one in progress, the rest waiting.
        assert!(html.contains("<div class=\"service-progress\" role=\"progressbar\" aria-label=\"Healthy replicas\" aria-valuemin=\"0\" aria-valuenow=\"1\" aria-valuemax=\"3\"><i data-state=\"ok\"></i><i data-state=\"busy\"></i><i data-state=\"off\"></i></div>"), "{html}");
        assert_eq!(html.matches("role=\"status\"").count(), 1, "{html}");
        // Not going: no bar. Not updating: no update line.
        let steady = deployment_summary(Some(&running()));
        assert!(
            !steady.contains("progressbar") && !steady.contains("service-update"),
            "{steady}"
        );
        let mut first = running();
        first["phase"] = json!("starting");
        first["spec"]["replicas"] = json!(2);
        let first = deployment_summary(Some(&first));
        assert!(
            first.contains("aria-valuenow=\"1\" aria-valuemax=\"2\"")
                && !first.contains("service-update"),
            "{first}"
        );
        // More healthy VMs than replicas (the extra one of a rolling update) never overfills the bar.
        let mut extra = value.clone();
        extra["spec"]["replicas"] = json!(1);
        assert!(
            deployment_summary(Some(&extra)).contains("aria-valuenow=\"1\" aria-valuemax=\"1\"")
        );
        // Korean.
        crate::i18n::set("ko");
        let korean = deployment_summary(Some(&value));
        crate::i18n::set("en");
        assert!(
            korean.contains("aria-label=\"정상 복제본\"")
                && korean.contains("· 블루그린</p>")
                && korean.contains("1/3 정상"),
            "{korean}"
        );
    }

    #[test]
    fn an_icon_toggle_says_whether_it_is_open() {
        let closed = icon_toggle(
            "Deployment log",
            "deploy-log",
            false,
            "service-log",
            Icon::Log,
        );
        assert_eq!(
            closed,
            format!(
                "<button type=\"button\" class=\"icon-btn\" aria-label=\"Deployment log\" title=\"Deployment log\" aria-expanded=\"false\" aria-controls=\"service-log\" data-action=\"deploy-log\" data-id=\"\">{}</button>",
                Icon::Log.svg()
            )
        );
        assert!(
            icon_toggle(
                "Deployment log",
                "deploy-log",
                true,
                "service-log",
                Icon::Log
            )
            .contains("aria-expanded=\"true\"")
        );
    }
}

#[cfg(test)]
mod uptime_tests {
    use super::format_uptime;

    #[test]
    fn reads_uptime_for_people() {
        assert_eq!(format_uptime(30, false), "less than a minute");
        assert_eq!(format_uptime(30, true), "1분 미만");
        assert_eq!(format_uptime(12 * 60, false), "12 min");
        assert_eq!(format_uptime(3 * 3600 + 5 * 60, false), "3 h 5 min");
        assert_eq!(format_uptime(3 * 3600 + 5 * 60, true), "3시간 5분");
        assert_eq!(format_uptime(2 * 86_400 + 4 * 3600, false), "2 d 4 h");
        assert_eq!(format_uptime(2 * 86_400 + 4 * 3600, true), "2일 4시간");
    }
}

fn service_security(app: &App) -> String {
    let title = tr(app, "Serverless security", "서버리스 보안");
    let explanation = tr(
        app,
        "CVE scanning, threat detection, and AI analysis are not available yet. These services have not been scanned.",
        "CVE 검사, 위협 탐지와 AI 분석을 아직 사용할 수 없다. 아래 서비스는 검사되지 않았다.",
    );
    let mut out = format!(
        "<section class=\"security-page\"><h2>{}</h2><p role=\"status\">{}</p>",
        escape(title),
        escape(explanation)
    );
    let services = list(&app.resource["services"]);
    if services.is_empty() {
        out.push_str(&format!(
            "<p>{}</p>",
            tr(
                app,
                "No service deployment records.",
                "서비스 배포 기록이 없다."
            )
        ));
    } else {
        out.push_str(&format!("<div class=\"table-scroll\"><table class=\"wasm-table\"><thead><tr><th>{}</th><th>{}</th><th>{}</th><th>{}</th></tr></thead><tbody>", tr(app, "Service", "서비스"), tr(app, "Deployment", "배포"), tr(app, "Image digest", "이미지 식별값"), tr(app, "Coverage", "검사 상태")));
        for service in services {
            let digest = text(service, "digest");
            out.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td class=\"mono\">{}</td><td>{}</td></tr>",
                escape(text(service, "name")),
                escape(text(service, "deploymentId")),
                escape(if digest.is_empty() { "—" } else { digest }),
                tr(app, "Not scanned", "검사되지 않음")
            ));
        }
        out.push_str("</tbody></table></div>");
    }
    out.push_str("</section>");
    out
}
