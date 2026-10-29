use super::*;
use std::fmt::Write;

pub fn icon(name: &str) -> String {
    let path = match name {
        "bot" => {
            "M12 8V4H8m0 8h.01M16 12h.01M3 12H1m22 0h-2M7 8h10a4 4 0 0 1 4 4v5a3 3 0 0 1-3 3H6a3 3 0 0 1-3-3v-5a4 4 0 0 1 4-4Z"
        }
        "folder" => {
            "M4 20a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2v3M9 17v-4m0 4a2 2 0 1 0 0 4 2 2 0 0 0 0-4Zm8 0v-4m0 4a2 2 0 1 0 0 4 2 2 0 0 0 0-4Zm-8-4h8"
        }
        "plus" => "M12 5v14M5 12h14",
        "close" => "m6 6 12 12M18 6 6 18",
        "settings" => "M4 7h16M4 17h16M8 4v6m8 4v6",
        "trash" => "M3 6h18M9 6V3h6v3M5 6l1 15h12l1-15M10 10v7m4-7v7",
        "message" => "M21 15a3 3 0 0 1-3 3H6l-4 4V5a3 3 0 0 1 3-3h13a3 3 0 0 1 3 3Z",
        "board" => "M4 4v16M12 4v9M20 4v13",
        "send" => "m22 2-7 20-4-9-9-4 20-7ZM22 2 11 13",
        "square" => "M5 5h14v14H5Z",
        "shield" => "m12 2 8 4v6c0 5-8 10-8 10S4 17 4 12V6Zm-4 9 3 3 5-6",
        "terminal" => "m4 6 5 6-5 6m8 0h8",
        "arrow" => "M7 17 17 7M7 7h10v10",
        "check" => "m5 12 4 4L19 6",
        "circle-check" => "M21 12a9 9 0 1 1-9-9 9 9 0 0 1 9 9ZM8 12l3 3 5-6",
        "circle-x" => "M21 12a9 9 0 1 1-9-9 9 9 0 0 1 9 9ZM9 9l6 6m0-6-6 6",
        "refresh" => "M20 7v5h-5M4 17v-5h5m-5 0a8 8 0 0 1 14-6l2 2M4 16l2 2a8 8 0 0 0 14-6",
        "download" => "M12 3v12m-5-5 5 5 5-5M5 16v5h14v-5",
        "diff" => {
            "M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8ZM14 2v6h6M8 12h8M12 10v4m-4 4h8"
        }
        _ => "M12 3a9 9 0 1 1-9 9",
    };
    format!(
        "<svg viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.65\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\"><path d=\"{path}\"/></svg>"
    )
}
pub(super) fn disabled(value: bool) -> &'static str {
    if value { " disabled" } else { "" }
}
pub(super) fn selected(value: bool) -> &'static str {
    if value { " selected" } else { "" }
}
fn checked(value: bool) -> &'static str {
    if value { " checked" } else { "" }
}
pub(super) fn btn(
    label: &str,
    action: &str,
    id: &str,
    glyph: &str,
    class: &str,
    locked: bool,
) -> String {
    format!(
        "<button type=\"button\" id=\"{}\" class=\"{class}\" aria-label=\"{}\" data-action=\"{action}\" data-id=\"{}\"{}>{}{}</button>",
        escape(&format!("{action}-{id}")),
        escape(label),
        escape(id),
        disabled(locked),
        icon(glyph),
        if class == "icon-btn" {
            String::new()
        } else {
            escape(label)
        }
    )
}
fn state_label<'a>(a: &'a App, task: &Value) -> &'a str {
    match text(task, "state") {
        "waiting" => tr(a, "Waiting", "대기"),
        "accepted" => tr(a, "Preparing", "준비 중"),
        "in_progress" => tr(a, "In progress", "진행 중"),
        "result" => tr(a, "Result", "결과"),
        "merged" => tr(a, "Applied", "반영됨"),
        "failed" => tr(a, "Failed", "실패"),
        "cancelled" => tr(a, "Cancelled", "취소됨"),
        _ => tr(a, "Needs reconciliation", "상태 확인 필요"),
    }
}
fn tab(label: &str, id: &str, action: &str, panel: &str, chosen: bool, glyph: &str) -> String {
    let (caption, count) = label
        .rsplit_once(' ')
        .filter(|(_, n)| n.parse::<usize>().is_ok())
        .map_or((label, String::new()), |(caption, n)| {
            (caption, format!(" <span>{}</span>", escape(n)))
        });
    format!(
        "<button type=\"button\" id=\"{id}\" role=\"tab\" aria-selected=\"{chosen}\" aria-controls=\"{panel}\" tabindex=\"{}\" data-action=\"{action}\">{}{}</button>",
        if chosen { 0 } else { -1 },
        icon(glyph),
        format_args!("{}{count}", escape(caption))
    )
}
pub(super) fn banner(error: &str) -> String {
    if error.is_empty() {
        String::new()
    } else {
        format!(
            "<div class=\"banner error sb-banner\" role=\"alert\">{}</div>",
            escape(error)
        )
    }
}

pub fn content(a: &App) -> String {
    let s = &a.sandbox;
    let mut out = format!(
        "<div class=\"sb-layout\"><aside class=\"sb-projects\" aria-label=\"{}\"><div class=\"sb-projects-title\"><span>{}</span>{}</div><div class=\"sb-project-list\">",
        tr(a, "Sandbox projects", "Sandbox 프로젝트"),
        tr(a, "Projects", "프로젝트"),
        // New project is the primary action of the screen header.
        ""
    );
    for p in s.projects.iter().flatten() {
        let id = text(p, "id");
        let _ = write!(
            out,
            "<button type=\"button\" class=\"sb-project\" data-action=\"sb-project\" data-id=\"{}\"{}>{}<span>{}</span></button>",
            escape(id),
            if id == s.project {
                " aria-current=\"page\""
            } else {
                ""
            },
            icon("folder"),
            escape(text(&p["spec"], "name"))
        );
    }
    if s.projects.as_ref().is_some_and(Vec::is_empty) {
        out.push_str(&format!(
            "<p class=\"sb-hint\">{}</p>",
            tr(
                a,
                "A project keeps your context, model and tasks together.",
                "프로젝트에 지침, 모델, 작업을 함께 저장한다."
            )
        ));
    }
    let _ = write!(
        out,
        "</div><a href=\"#/pipe/ai\" class=\"sb-account-link\">{}<span>Pipe AI</span>{}</a></aside><section class=\"sb-workspace\" aria-label=\"{}\">",
        icon("shield"),
        icon("arrow"),
        if a.route == "ai-canvas" {
            "AI Canvas"
        } else {
            "AI Sandbox"
        }
    );
    if !s.error.is_empty() {
        let _ = write!(
            out,
            "<div class=\"banner error sb-banner\" role=\"alert\">{} {}</div>",
            escape(&s.error),
            btn(
                tr(a, "Retry", "다시 시도"),
                "sb-refresh",
                "",
                "refresh",
                "icon-btn",
                false
            )
        );
    }
    if s.projects.is_none() && s.error.is_empty() {
        let _ = write!(
            out,
            "<div class=\"sb-welcome\">{}</div>",
            tr(a, "Loading projects…", "프로젝트를 불러오는 중…")
        );
    }
    if let Some(p) = project(s) {
        if a.route == "ai-canvas" {
            canvas_workspace(a, p, &mut out);
        } else {
            workspace(a, p, &mut out);
        }
    } else if s.projects.is_some() {
        let _ = write!(
            out,
            "<div class=\"sb-welcome\"><div class=\"sb-welcome-icon\">{}</div><h2>{}</h2><p>{}</p>{}",
            icon("bot"),
            tr(
                a,
                "A place to think. A place to build.",
                "대화에서 시작해, 실행까지."
            ),
            tr(
                a,
                "Choose a connected AI account, bring a project, and work in a MicroVM.",
                "연결한 AI 계정으로 대화하고, 프로젝트를 MicroVM에서 실행한다."
            ),
            btn(
                tr(a, "Create a project", "프로젝트 만들기"),
                "sb-new",
                "welcome",
                "plus",
                "btn primary",
                false
            )
        );
        if s.accounts.is_empty() {
            let _ = write!(
                out,
                "<a href=\"#/pipe/ai\">{} ↗</a>",
                tr(a, "Connect an AI account first", "먼저 AI 계정 연결")
            );
        }
        out.push_str("</div>");
    }
    out.push_str("</section></div>");
    if let Some(e) = &s.editor {
        editor(a, e, &mut out);
    }
    if let Some(r) = &s.review {
        review(a, r, &mut out);
    }
    if let Some(editor) = &s.resource_editor {
        resources::editor(a, editor, &mut out);
    }
    out
}

/// Canvas presents the existing Sandbox conversation and task records together.
fn canvas_workspace(a: &App, p: &Value, out: &mut String) {
    let s = &a.sandbox;
    let locked = s.busy || s.tasks.iter().any(active);
    let target = if !text(&p["spec"], "environmentId").is_empty() {
        tr(a, "Managed environment", "관리형 환경")
    } else {
        a.vms
            .iter()
            .find(|v| v.id == text(&p["spec"], "vmId"))
            .map(|v| v.name.as_str())
            .filter(|name| !name.is_empty())
            .unwrap_or(tr(a, "Conversation workspace", "대화 작업 공간"))
    };
    let _ = write!(
        out,
        "<header class=\"canvas-project-header\"><div class=\"canvas-project-name\">{}<div><p>{}</p><h2>{}</h2><span>{}</span></div></div><div class=\"canvas-project-actions\">{}</div></header>{}<div class=\"canvas-workspace\"><section class=\"canvas-conversation\" aria-label=\"{}\"><header class=\"canvas-panel-header\"><div>{}<h3>{}</h3></div><span>{}</span></header><div id=\"sb-panel\" class=\"canvas-conversation-scroll\">",
        icon("folder"),
        tr(a, "PROJECT", "프로젝트"),
        escape(text(&p["spec"], "name")),
        escape(target),
        btn(
            tr(a, "Project settings", "프로젝트 설정"),
            "sb-settings",
            "",
            "settings",
            "icon-btn",
            locked,
        ),
        banner(&s.task_error),
        tr(a, "Conversation", "대화"),
        icon("message"),
        tr(a, "Conversation", "대화"),
        tr(a, "AI Sandbox project", "AI Sandbox 프로젝트"),
    );
    if !s.tasks_loaded && s.task_error.is_empty() {
        let _ = write!(
            out,
            "<p class=\"canvas-loading\">{}</p>",
            tr(a, "Loading…", "불러오는 중…")
        );
    }
    messages(a, out);
    out.push_str("</div>");
    composer(a, p, out);
    let _ = write!(
        out,
        "</section><aside class=\"canvas-activity\" aria-label=\"{}\"><header class=\"canvas-panel-header\"><div>{}<h3>{}</h3></div><span class=\"canvas-count\">{}</span></header><div id=\"canvas-task-panel\" class=\"canvas-activity-scroll\">",
        tr(a, "Project tasks", "프로젝트 작업"),
        icon("board"),
        tr(a, "Tasks", "작업"),
        if s.tasks_loaded {
            s.tasks.len().to_string()
        } else {
            "…".into()
        },
    );
    if s.tasks_loaded && s.tasks.is_empty() {
        let _ = write!(
            out,
            "<div class=\"canvas-empty-tasks\">{}<p>{}</p><span>{}</span></div>",
            icon("board"),
            tr(a, "No tasks yet", "아직 작업이 없다"),
            tr(
                a,
                "Messages and code tasks appear here.",
                "대화와 코드 작업이 여기에 표시된다."
            ),
        );
    }
    for task in s.tasks.iter().rev() {
        let _ = write!(
            out,
            "<button type=\"button\" id=\"sb-card-{}\" class=\"canvas-task\" data-action=\"sb-review\" data-id=\"{}\"><span class=\"canvas-task-top\"><span class=\"sb-task-state {}\">{}{}</span><span>{}</span></span><strong>{}</strong><span class=\"canvas-task-meta\">{} · {}</span></button>",
            escape(text(task, "id")),
            escape(text(task, "id")),
            escape(text(task, "state")),
            icon(if active(task) {
                "refresh"
            } else if matches!(text(task, "state"), "failed" | "interrupted") {
                "circle-x"
            } else {
                "circle-check"
            }),
            state_label(a, task),
            if task["request"]["kind"] == "code" {
                tr(a, "Code", "코드")
            } else {
                tr(a, "Chat", "대화")
            },
            escape(text(&task["request"], "input")),
            escape(text(&task["selection"], "model")),
            if task["request"]["kind"] == "code" {
                format!(
                    "{} {}",
                    task["changeCount"].as_u64().unwrap_or(0),
                    tr(a, "files", "개 파일")
                )
            } else {
                tr(a, "Conversation", "대화").into()
            },
        );
    }
    resources::content(a, out);
    out.push_str("</div>");
    canvas::content(a, out);
    out.push_str("</aside></div>");
}

fn workspace(a: &App, p: &Value, out: &mut String) {
    let s = &a.sandbox;
    let running = s.tasks.iter().find(|t| active(t));
    let locked = s.busy || running.is_some();
    let vm = if !text(&p["spec"], "environmentId").is_empty() {
        tr(a, "Managed environment", "관리형 환경")
    } else {
        a.vms
            .iter()
            .find(|v| v.id == text(&p["spec"], "vmId"))
            .map(|v| v.name.as_str())
            .unwrap_or(tr(a, "Conversation workspace", "대화 작업 공간"))
    };
    let _ = write!(
        out,
        "<header class=\"sb-workspace-header\"><div class=\"sb-project-heading\">{}<div><h2>{}</h2><span>{}</span></div></div><div class=\"sb-row\">{}{}</div></header><div class=\"sb-tabs\" role=\"tablist\" aria-label=\"{}\">{}{}</div>{}<div class=\"sb-content\" id=\"sb-panel\" role=\"tabpanel\" aria-labelledby=\"sb-{}-tab\">",
        icon("folder"),
        escape(text(&p["spec"], "name")),
        escape(vm),
        btn(
            tr(a, "Project settings", "프로젝트 설정"),
            "sb-settings",
            "",
            "settings",
            "icon-btn",
            locked
        ),
        btn(
            tr(a, "Delete project", "프로젝트 삭제"),
            "sb-delete",
            "",
            "trash",
            "icon-btn",
            locked
        ),
        tr(a, "Sandbox view", "Sandbox 보기"),
        tab(
            tr(a, "Conversation", "대화"),
            "sb-chat-tab",
            "sb-tab-chat",
            "sb-panel",
            s.tab != "tasks",
            "message"
        ),
        tab(
            &format!("{} {}", tr(a, "Tasks", "작업"), s.tasks.len()),
            "sb-tasks-tab",
            "sb-tab-tasks",
            "sb-panel",
            s.tab == "tasks",
            "board"
        ),
        banner(&s.task_error),
        if s.tab == "tasks" { "tasks" } else { "chat" }
    );
    if !s.tasks_loaded && s.task_error.is_empty() {
        out.push_str(&format!(
            "<div class=\"sb-welcome\">{}</div>",
            tr(a, "Loading…", "불러오는 중…")
        ));
    }
    resources::content(a, out);
    if s.tab == "tasks" {
        board(a, out);
    } else {
        messages(a, out);
    }
    out.push_str("</div>");
    composer(a, p, out);
}

fn composer(a: &App, p: &Value, out: &mut String) {
    let s = &a.sandbox;
    let running = s.tasks.iter().find(|t| active(t));
    let _ = write!(
        out,
        "<form id=\"sb-composer\" class=\"sb-composer\"><div class=\"sb-composer-box\"><label class=\"sr-only\" for=\"sb-input\">{}</label><textarea id=\"sb-input\" rows=\"3\" maxlength=\"32768\" placeholder=\"{}\">{}</textarea><div class=\"sb-composer-tools\"><div class=\"sb-row\"><select id=\"sb-kind\" class=\"sb-kind\" aria-label=\"{}\"><option value=\"chat\"{}>{}</option><option value=\"code\"{}>{}</option></select><select id=\"sb-selection\" class=\"sb-model-select\" aria-label=\"{}\"><option value=\"\"{}>{}</option>",
        tr(a, "Message or task", "대화 또는 작업 내용"),
        if s.kind == "code" {
            tr(
                a,
                "Describe a change and how to test it…",
                "수정할 내용과 테스트 방법을 설명한다…",
            )
        } else {
            tr(
                a,
                "Ask a question or plan your next step…",
                "질문이나 다음 작업을 입력한다…",
            )
        },
        escape(&s.input),
        tr(a, "Execution mode", "실행 모드"),
        selected(s.kind != "code"),
        tr(a, "Conversation", "대화"),
        selected(s.kind == "code"),
        tr(a, "Code task", "코드 작업"),
        tr(a, "Task model", "작업 모델"),
        selected(s.selection.is_null()),
        escape(p["spec"]["selection"]["model"].as_str().unwrap_or(tr(
            a,
            "Choose a default model",
            "기본 모델 선택"
        )))
    );
    for account in &s.accounts {
        let connection = text(account, "id");
        let _ = write!(
            out,
            "<optgroup label=\"{}\">",
            escape(text(account, "name"))
        );
        if let Some(c) = s.catalogs.get(connection) {
            for m in list(&c["models"]).iter().filter(|m| m["verified"] == true) {
                let selection = json!({"connectionId":connection,"model":m["id"]});
                let _ = write!(
                    out,
                    "<option value=\"{}\"{}>{}{}</option>",
                    escape(&selection.to_string()),
                    selected(s.selection == selection),
                    escape(text(m, "name")),
                    if m["toolsVerified"] == true {
                        " · tools"
                    } else {
                        ""
                    }
                );
            }
        }
        out.push_str("</optgroup>");
    }
    out.push_str("</select></div>");
    if let Some(t) = running {
        out.push_str(&btn(
            tr(a, "Stop task", "작업 중지"),
            "sb-stop",
            text(t, "id"),
            "square",
            "sb-send stopping",
            s.stopping == text(t, "id"),
        ));
    } else {
        let _ = write!(
            out,
            "<button id=\"sb-send\" type=\"submit\" class=\"sb-send\" aria-label=\"{}\"{}>{}</button>",
            if s.kind == "code" {
                tr(a, "Run code task", "코드 작업 실행")
            } else {
                tr(a, "Send message", "메시지 전송")
            },
            disabled(!can_send(a)),
            icon("send")
        );
    }
    let note = if s.kind == "code" {
        if verified(s, &effective(s), true).is_some()
            && p["spec"]["sourcePath"].is_string()
            && p["spec"]["vmId"].is_string()
        {
            tr(
                a,
                "Runs inside the selected MicroVM · source changes require review",
                "선택한 MicroVM에서 실행 · 원본 반영 전 검토",
            )
        } else {
            tr(
                a,
                "Set a project path, running MicroVM and tool-verified model in settings",
                "설정에서 프로젝트 경로, 실행 중인 MicroVM, 도구 검증 모델을 선택한다",
            )
        }
    } else if verified(s, &effective(s), false).is_some() {
        tr(a, "Uses your connected account", "연결한 계정으로 요청")
    } else {
        tr(
            a,
            "Verify and select a model in project settings",
            "프로젝트 설정에서 모델을 검증하고 선택한다",
        )
    };
    let _ = write!(out, "</div></div>",);
    resources::coordination(a, out);
    let _ = write!(
        out,
        "<div class=\"sb-composer-note\"><span>{}{note}</span><kbd>⌘ / Ctrl ↵</kbd></div></form>",
        icon(if s.kind == "code" {
            "terminal"
        } else {
            "shield"
        })
    );
}
fn messages(a: &App, out: &mut String) {
    let s = &a.sandbox;
    if !s.tasks_loaded {
        return;
    }
    out.push_str(&format!(
        "<div class=\"sb-messages\" aria-label=\"{}\">",
        tr(a, "Conversation messages", "대화 메시지")
    ));
    if s.tasks.is_empty() {
        let _ = write!(
            out,
            "<div class=\"sb-conversation-empty\">{}<h3>{}</h3><p>{}</p><div class=\"sb-suggestions\">{}{}</div></div>",
            icon("bot"),
            tr(
                a,
                "What would you like to work on?",
                "어떤 작업을 시작할까?"
            ),
            tr(
                a,
                "Ask a question, or switch to Code task to edit and test your project.",
                "질문을 보내거나 코드 작업으로 전환해 프로젝트를 수정하고 테스트한다."
            ),
            btn(
                tr(
                    a,
                    "Help me plan my next feature",
                    "다음 기능의 구현 계획을 세워줘"
                ),
                "sb-suggest-plan",
                "",
                "arrow",
                "btn quiet",
                false
            ),
            btn(
                tr(
                    a,
                    "Find a bug and add a regression test",
                    "버그를 찾고 회귀 테스트를 추가해줘"
                ),
                "sb-suggest-code",
                "",
                "arrow",
                "btn quiet",
                false
            )
        );
    }
    if s.tasks.len() >= 50 && !s.older_end {
        out.push_str(&btn(
            tr(a, "Load older messages", "이전 대화 불러오기"),
            "sb-older",
            "",
            "refresh",
            "btn quiet",
            s.busy,
        ));
    }
    for t in &s.tasks {
        let _ = write!(
            out,
            "<article class=\"sb-exchange\"><div class=\"sb-user-message\"><span>{}</span><p>{}</p></div><div class=\"sb-assistant-message\"><div class=\"sb-message-avatar\">{}</div><div class=\"sb-message-body\"><header><strong>{}</strong><span class=\"sb-task-state {}\">{}</span></header><div class=\"sb-message-text\">{}</div>",
            tr(a, "You", "사용자"),
            escape(text(&t["request"], "input")),
            icon("bot"),
            escape(text(&t["selection"], "model")),
            escape(text(t, "state")),
            state_label(a, t),
            escape(text(t, "result"))
        );
        if active(t) {
            let _ = write!(
                out,
                "<div class=\"sb-progress\" role=\"status\">{}</div>",
                escape(t["lastTool"].as_str().unwrap_or(tr(
                    a,
                    "Preparing response…",
                    "응답을 준비하는 중…"
                )))
            );
        }
        let _ = write!(
            out,
            "{}<div class=\"sb-message-actions\">",
            banner(text(t, "error"))
        );
        if t["request"]["kind"] == "code" {
            out.push_str(&btn(
                tr(a, "Review changes & activity", "변경·실행 기록 검토"),
                "sb-review",
                text(t, "id"),
                "diff",
                "btn quiet",
                false,
            ));
        }
        if matches!(text(t, "state"), "failed" | "cancelled") {
            out.push_str(&btn(
                tr(a, "Retry as a new attempt", "새 실행으로 다시 시도"),
                "sb-retry",
                text(t, "id"),
                "refresh",
                "btn quiet",
                s.busy || s.tasks.iter().any(active),
            ));
        }
        if t["state"] == "interrupted" {
            out.push_str(&btn(
                tr(a, "Reconcile attempt", "실행 상태 확인"),
                "sb-reconcile",
                text(t, "id"),
                "refresh",
                "btn",
                s.busy,
            ));
        }
        out.push_str("</div></div></div></article>");
    }
    out.push_str("</div>");
}
fn board(a: &App, out: &mut String) {
    let s = &a.sandbox;
    let _ = write!(
        out,
        "<div class=\"sb-board\" aria-label=\"{}\">",
        tr(a, "Task board", "작업 보드")
    );
    for (label, ko, states) in [
        ("Waiting", "대기", vec!["waiting", "accepted"]),
        ("In progress", "진행 중", vec!["in_progress"]),
        (
            "Results",
            "결과",
            vec!["result", "merged", "failed", "cancelled", "interrupted"],
        ),
    ] {
        let tasks = s
            .tasks
            .iter()
            .filter(|t| states.contains(&text(t, "state")))
            .collect::<Vec<_>>();
        let _ = write!(
            out,
            "<section class=\"sb-board-column\"><h3>{}<span>{}</span></h3>",
            tr(a, label, ko),
            tasks.len()
        );
        if tasks.is_empty() {
            let _ = write!(
                out,
                "<p class=\"sb-column-empty\">{}</p>",
                tr(a, "No tasks", "작업 없음")
            );
        }
        for t in tasks {
            if t["request"]["kind"] == "chat" {
                let _ = write!(
                    out,
                    "<button type=\"button\" id=\"sb-card-{}\" class=\"sb-task-card\" data-action=\"sb-review\" data-id=\"{}\"><span class=\"sb-task-state {}\">{}{}</span><strong>{}</strong><span class=\"sb-card-meta\">{}<span>{}</span></span><span class=\"sb-error-text\">{}</span></button>",
                    escape(text(t, "id")),
                    escape(text(t, "id")),
                    escape(text(t, "state")),
                    icon("message"),
                    state_label(a, t),
                    escape(text(&t["request"], "input")),
                    escape(text(&t["selection"], "model")),
                    tr(a, "Conversation", "대화"),
                    escape(text(t, "error"))
                );
                continue;
            }
            let tests = t["testCount"].as_u64().unwrap_or(0);
            let failed = t["failedTestCount"].as_u64();
            let agent = text(&t["request"]["coordination"], "assignedAgent");
            let test_label = if tests == 0 {
                tr(a, "No tests recorded", "기록된 테스트 없음").into()
            } else if failed.is_some_and(|failed| failed > 0) {
                format!(
                    "{} {}/{tests}",
                    tr(a, "Tests failed", "테스트 실패"),
                    failed.unwrap()
                )
            } else if failed == Some(0) {
                format!("{} {tests}", tr(a, "Tests passed", "테스트 통과"))
            } else {
                format!("{} {tests}", tr(a, "Tests recorded", "기록된 테스트"))
            };
            let _ = write!(
                out,
                "<button type=\"button\" id=\"sb-card-{}\" class=\"sb-task-card\" data-action=\"sb-review\" data-id=\"{}\"><span class=\"sb-task-state {}\">{}{}</span><strong>{}</strong><span class=\"sb-card-meta\">{}<span>{} {}</span></span><span class=\"sb-card-meta\"><span>{}: {}</span><span>{}: {}</span></span><span class=\"sb-card-tests {}\">{}</span><span class=\"sb-error-text\">{}</span></button>",
                escape(text(t, "id")),
                escape(text(t, "id")),
                escape(text(t, "state")),
                icon(if matches!(text(t, "state"), "failed" | "interrupted") {
                    "circle-x"
                } else if text(t, "state") == "result" {
                    "diff"
                } else if text(t, "state") == "merged" {
                    "circle-check"
                } else {
                    "refresh"
                }),
                state_label(a, t),
                escape(text(&t["request"], "input")),
                escape(text(&t["selection"], "model")),
                t["changeCount"].as_u64().unwrap_or(0),
                tr(a, "files", "개 파일"),
                tr(a, "Agent", "에이전트"),
                if agent.is_empty() {
                    tr(a, "Unassigned", "미지정").into()
                } else {
                    escape(agent)
                },
                tr(a, "Artifacts", "파일 결과"),
                t["artifactCount"].as_u64().unwrap_or(0),
                if failed.is_some_and(|failed| failed > 0) {
                    "failed"
                } else if tests > 0 && failed == Some(0) {
                    "passed"
                } else {
                    "none"
                },
                test_label,
                escape(text(t, "error"))
            );
        }
        out.push_str("</section>");
    }
    out.push_str("</div>");
}
pub(super) fn field(label: &str, id: &str, value: &str, extra: &str, locked: bool) -> String {
    format!(
        "<label for=\"{id}\">{}<input id=\"{id}\" value=\"{}\" {extra}{}></label>",
        escape(label),
        escape(value),
        disabled(locked)
    )
}
fn editor(a: &App, e: &Editor, out: &mut String) {
    let title = if e.existing {
        tr(a, "Project settings", "프로젝트 설정")
    } else {
        tr(a, "New project", "새 프로젝트")
    };
    let _ = write!(
        out,
        "<dialog id=\"sb-project-dialog\" class=\"sb-dialog\" aria-labelledby=\"sb-project-title\"><header class=\"sb-dialog-header\"><h2 id=\"sb-project-title\">{}{title}</h2>{}</header><form id=\"sb-project-form\" class=\"sb-project-form\">{}{}<fieldset class=\"sb-fieldset\"{}><legend>{}</legend>",
        icon("folder"),
        btn(
            tr(a, "Close", "닫기"),
            "sb-close",
            "",
            "close",
            "icon-btn",
            e.busy
        ),
        banner(&e.error),
        field(
            tr(a, "Project name", "프로젝트 이름"),
            "sb-name",
            text(&e.spec, "name"),
            "autofocus required maxlength=\"200\"",
            e.busy
        ),
        disabled(e.busy),
        tr(a, "Model", "모델")
    );
    if a.sandbox.accounts.is_empty() {
        let _ = write!(
            out,
            "<p class=\"muted\">{} <a href=\"#/pipe/ai\">Pipe AI ↗</a></p>",
            tr(
                a,
                "Connect an account in Pipe AI to use models.",
                "Pipe AI에서 계정을 연결한 뒤 모델을 사용한다."
            )
        );
    } else {
        let _ = write!(
            out,
            "<label for=\"sb-connection\">{}<select id=\"sb-connection\"{}>",
            tr(a, "Connected account", "연결한 계정"),
            disabled(e.busy || e.loading)
        );
        for c in &a.sandbox.accounts {
            let _ = write!(
                out,
                "<option value=\"{}\"{}>{} · {}</option>",
                escape(text(c, "id")),
                selected(c["id"] == e.connection),
                escape(text(c, "name")),
                if c["authentication"] == "chatgpt" {
                    "ChatGPT"
                } else if c["provider"] == "anthropic" {
                    "Anthropic"
                } else {
                    "OpenAI"
                }
            );
        }
        out.push_str("</select></label>");
        let _ = write!(
            out,
            "<div class=\"sb-model-entry\">{}{}</div><datalist id=\"sb-models\">",
            field(
                tr(a, "Model ID", "모델 ID"),
                "sb-model",
                &e.model,
                &format!(
                    "list=\"sb-models\" placeholder=\"{}\"",
                    tr(a, "Choose or enter a model ID", "모델을 고르거나 ID 입력")
                ),
                e.busy
            ),
            btn(
                tr(a, "Discover models", "모델 목록 조회"),
                "sb-discover",
                "",
                "refresh",
                "btn",
                e.busy || e.loading || e.connection.is_empty()
            )
        );
        for m in list(&e.catalog["models"]) {
            let _ = write!(
                out,
                "<option value=\"{}\">{}{}</option>",
                escape(text(m, "id")),
                escape(text(m, "name")),
                if m["verified"] == true { " ✓" } else { "" }
            );
        }
        let _ = write!(
            out,
            "</datalist><label class=\"sb-check\"><input id=\"sb-tools\" type=\"checkbox\"{}{}>{}</label><div class=\"sb-verify\">{}",
            checked(e.tools),
            disabled(e.busy),
            tr(
                a,
                "Verify tool support for code tasks",
                "코드 작업에 필요한 도구 호출도 검증"
            ),
            btn(
                tr(a, "Verify access", "접근 권한 검증"),
                "sb-verify",
                "",
                "shield",
                "btn",
                e.busy || e.loading || e.model.trim().is_empty() || e.connection.is_empty()
            )
        );
        if let Some(m) = list(&e.catalog["models"]).iter().find(|m| {
            text(m, "id") == e.model
                && m["verified"] == true
                && e.spec["selection"]["model"] == m["id"]
        }) {
            let _ = write!(
                out,
                "<span class=\"sb-verified\">{}{}</span>",
                icon("check"),
                if m["toolsVerified"] == true {
                    tr(a, "Conversation + code tasks", "대화 + 코드 작업")
                } else {
                    tr(a, "Conversation verified", "대화 검증 완료")
                }
            );
        }
        let _ = write!(
            out,
            "</div><p class=\"sb-hint\">{}</p>",
            tr(
                a,
                "Verification sends small provider requests. Model usage may be billed by your provider.",
                "검증 시 공급자에 작은 요청을 보낸다. 공급자 정책에 따라 사용 요금이 발생할 수 있다."
            )
        );
    }
    let _ = write!(
        out,
        "</fieldset><fieldset class=\"sb-fieldset\"{}><legend>{}</legend>{}<label for=\"sb-vm\">{}<select id=\"sb-vm\"><option value=\"\">{}</option>",
        disabled(e.busy),
        tr(a, "Code workspace", "코드 작업 공간"),
        field(
            tr(
                a,
                "Project directory on API host",
                "API 호스트의 프로젝트 경로"
            ),
            "sb-source",
            text(&e.spec, "sourcePath"),
            "placeholder=\"/home/me/my-project\"",
            e.busy
        ),
        tr(a, "Execution MicroVM", "실행 MicroVM"),
        tr(a, "Conversation only", "대화만 사용")
    );
    for vm in &a.vms {
        let _ = write!(
            out,
            "<option value=\"{}\"{}>{} · {}</option>",
            escape(&vm.id),
            selected(e.spec["vmId"] == vm.id),
            escape(&vm.name),
            escape(&vm.state)
        );
    }
    out.push_str("</select></label>");
    let _ = write!(
        out,
        "<label for=\"sb-environment\">{}<select id=\"sb-environment\"><option value=\"\">{}</option>",
        tr(a, "Managed environment", "관리형 환경"),
        tr(a, "Use selected MicroVM", "선택한 MicroVM 사용")
    );
    for environment in &a.sandbox.environments {
        if environment["state"] == "deleted" {
            continue;
        }
        let id = text(environment, "id");
        let _ = write!(
            out,
            "<option value=\"{}\"{}>{} · {}</option>",
            escape(id),
            selected(e.spec["environmentId"] == id),
            escape(&id.chars().take(8).collect::<String>()),
            escape(resources::status(a, text(environment, "state")))
        );
    }
    let _ = write!(
        out,
        "</select></label><p class=\"sb-hint\">{}</p></fieldset>",
        tr(
            a,
            "Files are edited in an isolated task branch. Commands run as a dedicated user inside this MicroVM. Your source changes only when you apply reviewed files.",
            "파일은 별도 작업 브랜치에서 수정한다. 명령은 이 MicroVM의 전용 사용자로 실행한다. 검토한 파일을 반영할 때 원본이 변경된다."
        ),
    );
    let context = &e.spec["context"];
    let _ = write!(
        out,
        "<details id=\"sb-project-context\" class=\"sb-fieldset\"><summary>{}</summary>{}",
        tr(a, "Project context", "프로젝트 문맥"),
        field(
            tr(a, "Git ref", "Git 참조"),
            "sb-git-ref",
            text(context, "gitRef"),
            "placeholder=\"HEAD\"",
            e.busy
        )
    );
    for (key, id, en, ko) in [
        (
            "selectedSkills",
            "sb-skills",
            "Selected skill paths, one per line",
            "선택한 스킬 경로, 한 줄에 하나",
        ),
        (
            "references",
            "sb-references",
            "Reference file paths, one per line",
            "참조 파일 경로, 한 줄에 하나",
        ),
    ] {
        let value = list(&context[key])
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        let _ = write!(
            out,
            "<label for=\"{id}\">{}<textarea id=\"{id}\" rows=\"2\"{}>{}</textarea></label>",
            tr(a, en, ko),
            disabled(e.busy),
            escape(&value)
        );
    }
    let _ = write!(
        out,
        "<p class=\"sb-hint\">{}</p></details><label for=\"sb-instructions\">{}<textarea id=\"sb-instructions\" rows=\"3\" maxlength=\"16384\"{}>{}</textarea></label><footer class=\"sb-dialog-footer\">{}<button id=\"sb-save\" type=\"submit\" class=\"btn primary\"{}>{}</button></footer></form></dialog>",
        tr(
            a,
            "Paths must be inside this project's source directory.",
            "경로는 이 프로젝트의 원본 디렉터리 안에 있어야 한다."
        ),
        tr(a, "Project instructions", "프로젝트 지침"),
        disabled(e.busy),
        escape(text(&e.spec, "instructions")),
        btn(
            tr(a, "Cancel", "취소"),
            "sb-close",
            "cancel",
            "close",
            "btn",
            e.busy
        ),
        disabled(e.busy || text(&e.spec, "name").trim().is_empty()),
        tr(a, "Save project", "프로젝트 저장")
    );
}
fn review(a: &App, r: &Review, out: &mut String) {
    let Some(task) = &r.task else {
        let _ = write!(
            out,
            "<div class=\"sb-banner\" role=\"{}\">{}{}</div>",
            if r.load_error.is_empty() {
                "status"
            } else {
                "alert"
            },
            if r.load_error.is_empty() {
                tr(a, "Loading task…", "작업을 불러오는 중…").into()
            } else {
                escape(&r.load_error)
            },
            btn(
                tr(a, "Close", "닫기"),
                "sb-close",
                "loading",
                "close",
                "btn",
                false
            )
        );
        return;
    };
    let code = task["request"]["kind"] == "code";
    let review_tab = if code && r.tab == "changes" {
        "changes"
    } else {
        "activity"
    };
    let changes = list(&task["changes"]);
    let events = list(&task["events"]);
    let _ = write!(
        out,
        "<dialog id=\"sb-review-dialog\" class=\"sb-dialog sb-review\" aria-labelledby=\"sb-review-title\"><header class=\"sb-dialog-header\"><h2 id=\"sb-review-title\">{}{}</h2><div class=\"sb-row\">{}{}</div></header><div class=\"sb-review-summary\"><p>{}</p><span>{} · {}</span><p class=\"sb-message-text\">{}</p></div><div class=\"sb-tabs\" role=\"tablist\" aria-label=\"{}\">{}{}</div><div class=\"sb-review-body\" id=\"sb-review-panel\" role=\"tabpanel\" aria-labelledby=\"sb-review-{}-tab\">{}{}{}",
        icon("diff"),
        tr(a, "Task result", "작업 결과"),
        btn(
            tr(a, "Export task result", "작업 결과 내보내기"),
            "sb-export",
            "",
            "download",
            "icon-btn",
            false
        ),
        btn(
            tr(a, "Close", "닫기"),
            "sb-close",
            "review",
            "close",
            "icon-btn",
            r.busy
        ),
        escape(text(&task["request"], "input")),
        escape(text(&task["selection"], "model")),
        escape(&text(task, "attemptId").chars().take(8).collect::<String>()),
        escape(text(task, "result")),
        tr(a, "Task details", "작업 상세"),
        if code {
            tab(
                &format!("{} {}", tr(a, "Changes", "변경 내용"), changes.len()),
                "sb-review-changes-tab",
                "sb-review-changes",
                "sb-review-panel",
                review_tab == "changes",
                "diff",
            )
        } else {
            String::new()
        },
        tab(
            &format!("{} {}", tr(a, "Activity", "실행 기록"), events.len()),
            "sb-review-activity-tab",
            "sb-review-activity",
            "sb-review-panel",
            review_tab == "activity",
            "terminal"
        ),
        review_tab,
        banner(&r.error),
        banner(&r.load_error),
        banner(text(task, "error"))
    );
    resources::result(a, task, out);
    if review_tab == "changes" {
        if changes.is_empty() {
            let _ = write!(
                out,
                "<p class=\"sb-empty-note\">{}</p>",
                tr(
                    a,
                    "This attempt did not change any project files.",
                    "이 실행에서 변경한 프로젝트 파일이 없다."
                )
            );
        }
        for (index, c) in changes.iter().enumerate() {
            let path = text(c, "path");
            let applied = c["applied"] == true;
            let _ = write!(
                out,
                "<details id=\"sb-change-{index}\" class=\"sb-change\"{}><summary><label><input type=\"checkbox\" data-sb-path=\"{}\" id=\"sb-path-{}\" aria-label=\"{}\"{}{}>{}</label><span>{}</span></summary><div class=\"sb-diff-grid\">",
                if changes.len() == 1 { " open" } else { "" },
                escape(path),
                escape(path),
                escape(&format!(
                    "{}{}",
                    if a.locale == "ko" { "" } else { "Apply " },
                    if a.locale == "ko" {
                        format!("{path} 반영")
                    } else {
                        path.into()
                    }
                )),
                checked(applied || r.paths.contains(path)),
                disabled(applied || r.busy || active(task)),
                escape(path),
                if applied {
                    tr(a, "Applied", "반영됨")
                } else if c["before"].is_null() {
                    tr(a, "Added", "추가")
                } else if c["after"].is_null() {
                    tr(a, "Deleted", "삭제")
                } else {
                    tr(a, "Modified", "수정")
                }
            );
            for (key, mode, en, ko, class) in [
                ("before", "beforeMode", "Before", "변경 전", "before"),
                ("after", "afterMode", "After", "변경 후", "after"),
            ] {
                let _ = write!(
                    out,
                    "<div><h4>{} {}</h4><pre class=\"sb-diff-{class}\">{}</pre></div>",
                    tr(a, en, ko),
                    c[mode]
                        .as_u64()
                        .map(|v| format!("{v:o}"))
                        .unwrap_or_default(),
                    escape(c[key].as_str().unwrap_or(if key == "before" {
                        tr(a, "New file", "새 파일")
                    } else {
                        tr(a, "File deleted", "파일 삭제")
                    }))
                );
            }
            out.push_str("</div></details>");
        }
    } else {
        if events.is_empty() {
            let _ = write!(
                out,
                "<p class=\"sb-empty-note\">{}</p>",
                tr(
                    a,
                    "No tool calls in this attempt.",
                    "이 실행에는 도구 호출 기록이 없다."
                )
            );
        }
        for (index, e) in events.iter().enumerate() {
            let _ = write!(
                out,
                "<details id=\"sb-event-{index}\" class=\"sb-event\"{}><summary>{}<strong>{}</strong>",
                if events.len() < 4 { " open" } else { "" },
                icon("terminal"),
                escape(text(e, "name"))
            );
            if !e["exitCode"].is_null() {
                let _ = write!(
                    out,
                    "<span class=\"sb-exit {}\">{} {}</span>",
                    if e["exitCode"] == 0 { "ok" } else { "error" },
                    tr(a, "exit", "종료 코드"),
                    e["exitCode"]
                );
            }
            let _ = write!(
                out,
                "</summary><pre class=\"sb-event-input\">{}</pre>",
                escape(text(e, "input"))
            );
            for (key, label) in [("output", "stdout"), ("stderr", "stderr")] {
                if !text(e, key).is_empty() {
                    let _ = write!(out, "<h4>{label}</h4><pre>{}</pre>", escape(text(e, key)));
                }
            }
            out.push_str("</details>");
        }
    }
    out.push_str("</div><footer class=\"sb-dialog-footer\">");
    if code {
        let _ = write!(
            out,
            "<p class=\"sb-hint\">{}</p>",
            tr(
                a,
                "Only selected files are applied. A changed source file is reported as a conflict.",
                "선택한 파일만 원본에 반영한다. 원본이 바뀌었으면 충돌을 알린다."
            )
        );
    }
    if active(task) {
        out.push_str(&btn(
            tr(a, "Stop task", "작업 중지"),
            "sb-stop",
            text(task, "id"),
            "square",
            "btn",
            r.busy || a.sandbox.busy,
        ));
    }
    if task["state"] == "interrupted" {
        out.push_str(&btn(
            tr(a, "Reconcile attempt", "실행 상태 확인"),
            "sb-reconcile",
            text(task, "id"),
            "refresh",
            "btn",
            r.busy || a.sandbox.busy,
        ));
    }
    if matches!(text(task, "state"), "failed" | "cancelled") {
        out.push_str(&btn(
            tr(a, "New attempt", "새 실행"),
            "sb-retry",
            text(task, "id"),
            "refresh",
            "btn",
            r.busy || a.sandbox.busy,
        ));
    }
    if code {
        out.push_str(&btn(
            tr(a, "Apply selected files", "선택한 파일 반영"),
            "sb-apply",
            "",
            "check",
            "btn primary",
            r.busy
                || !r.load_error.is_empty()
                || active(task)
                || task["state"] == "interrupted"
                || !changes
                    .iter()
                    .any(|c| c["applied"] != true && r.paths.contains(text(c, "path"))),
        ));
    }
    out.push_str("</footer></dialog>");
}
