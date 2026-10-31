use super::*;
use std::fmt::Write;

pub(super) fn position(n: &Node) -> String {
    format!(
        "left:{}px;top:{}px;width:{}px;height:{}px",
        n.x, n.y, n.width, n.height
    )
}
pub(super) fn transform(l: &Layout) -> String {
    format!(
        "transform:translate({}px,{}px) scale({});",
        l.x, l.y, l.zoom
    )
}
fn button(action: &str, id: &str, label: &str) -> String {
    format!(
        "<button type=\"button\" data-action=\"{action}\" data-id=\"{}\">{}</button>",
        escape(id),
        escape(label)
    )
}
pub(super) fn page(a: &App) -> String {
    let s = &a.canvas;
    let current = tab(s);
    let project = current.map(|t| t.identity.project.as_str()).unwrap_or("");
    let record = s.projects.iter().find(|p| text(p, "id") == project);
    let name = record
        .map(|p| text(&p["spec"], "name"))
        .filter(|n| !n.is_empty())
        .unwrap_or(tr(a, "Empty canvas", "빈 캔버스"));
    let mut tabs = String::new();
    for t in &s.tabs {
        let title = s
            .projects
            .iter()
            .find(|p| text(p, "id") == t.identity.project)
            .map(|p| text(&p["spec"], "name"))
            .filter(|n| !n.is_empty())
            .unwrap_or(tr(a, "New canvas", "새 캔버스"));
        let _ = write!(
            tabs,
            "<div class=\"cw-tab {}\"><button role=\"tab\" aria-selected=\"{}\" data-action=\"cw-tab\" data-id=\"{}\">▦ {}</button><button data-action=\"cw-close\" data-id=\"{}\" aria-label=\"{}\">×</button></div>",
            if t.identity.id == s.active {
                "active"
            } else {
                ""
            },
            t.identity.id == s.active,
            escape(&t.identity.id),
            escape(title),
            escape(&t.identity.id),
            tr(a, "Close tab", "탭 닫기")
        );
    }
    let sidebar = if current.is_some_and(|t| t.loaded) {
        let t = current.unwrap();
        let mut files = String::new();
        if !t.layout.directory.is_empty() {
            let parent = t
                .layout
                .directory
                .rsplit_once('/')
                .map(|v| v.0)
                .unwrap_or("");
            files.push_str(&button("cw-directory", parent, "← .."));
        }
        for file in &t.files {
            let path = if t.layout.directory.is_empty() {
                text(file, "name").to_string()
            } else {
                format!("{}/{}", t.layout.directory, text(file, "name"))
            };
            files.push_str(&button(
                if file["directory"] == true {
                    "cw-directory"
                } else {
                    "cw-editor"
                },
                &path,
                &format!(
                    "{} {}",
                    if file["directory"] == true {
                        "▱"
                    } else {
                        "▤"
                    },
                    text(file, "name")
                ),
            ));
        }
        let options = t
            .terminals
            .iter()
            .map(|r| {
                format!(
                    "<option value=\"{}\">{} · {}</option>",
                    escape(text(r, "id")),
                    escape(text(r, "label")),
                    escape(text(r, "status"))
                )
            })
            .collect::<String>();
        let view_options = t
            .views
            .iter()
            .enumerate()
            .map(|(index, saved)| {
                format!(
                    "<option value=\"{}\">{} {}</option>",
                    escape(text(saved, "id")),
                    tr(a, "Saved canvas", "저장된 캔버스"),
                    index + 1
                )
            })
            .collect::<String>();
        let private_views = format!(
            "<div class=\"cw-terminal-picker\"><select id=\"cw-saved-view\" aria-label=\"{}\"><option value=\"\">{}</option>{view_options}</select>{}{}</div>",
            tr(a, "Saved canvases", "저장된 캔버스"),
            tr(a, "Choose a private canvas", "개인 캔버스 선택"),
            button("cw-reopen", "", tr(a, "Reopen", "다시 열기")),
            button(
                "cw-views",
                "",
                tr(a, "Refresh saved canvases", "저장 목록 새로고침")
            )
        );
        let terminal_picker = format!(
            "<div class=\"cw-terminal-picker\"><select id=\"cw-existing-terminal\" aria-label=\"{}\"><option value=\"\">{}</option>{options}</select>{}{}</div>",
            tr(a, "Existing terminals", "기존 터미널"),
            tr(a, "Choose a retained terminal", "저장된 터미널 선택"),
            button("cw-attach", "", tr(a, "Attach", "연결")),
            button(
                "cw-terminals",
                "",
                tr(a, "Refresh sessions", "세션 새로고침")
            )
        );
        format!(
            "<section class=\"cw-project-nav\"><h2>{}</h2><small>/workspace/shared</small>{}<div class=\"cw-tree\">{files}</div><h3>{}</h3>{}{}{}<h3>{}</h3>{}{}{}</section>",
            escape(name),
            format_args!(
                "{}{}",
                button("cw-editor", "", tr(a, "Open folder", "폴더 열기")),
                button(
                    "cw-editor",
                    "",
                    tr(
                        a,
                        "Personal Page · Web VS Code",
                        "개인 페이지 · Web VS Code"
                    )
                )
            ),
            tr(a, "Project resources", "프로젝트 자료"),
            button("cw-add", "folder", tr(a, "Shared folder", "공용 작업 폴더")),
            button("cw-add", "history", tr(a, "Version history", "버전 기록")),
            button("cw-editor", "AGENTS.md", "AGENTS.md"),
            tr(a, "Workspace", "작업 공간"),
            format_args!(
                "{}{}",
                button("cw-add", "terminal", tr(a, "Add terminal", "터미널 추가")),
                terminal_picker
            ),
            button("cw-add", "note", tr(a, "Private note", "개인 메모")),
            format_args!(
                "{}{}",
                button("cw-refresh", "", tr(a, "Refresh files", "파일 새로고침")),
                private_views
            )
        )
    } else {
        String::new()
    };
    let body = match current {
        Some(t) if t.identity.project.is_empty() => {
            let options = s
                .projects
                .iter()
                .map(|p| {
                    format!(
                        "<option value=\"{}\">{}</option>",
                        escape(text(p, "id")),
                        escape(text(&p["spec"], "name"))
                    )
                })
                .collect::<String>();
            format!(
                "<div class=\"cw-empty\"><div class=\"cw-empty-icon\">▦</div><h1>{}</h1><p>{}</p><label for=\"cw-project-select\">{}</label><select id=\"cw-project-select\"><option value=\"\">{}</option>{options}</select>{}<a href=\"#/ai-sandbox\">{}</a></div>",
                tr(a, "A new workspace", "새 작업 공간"),
                tr(
                    a,
                    "Open a project to use its retained files. This tab starts empty.",
                    "프로젝트를 열면 저장된 작업 폴더에 연결한다. 새 탭은 빈 화면에서 시작한다."
                ),
                tr(a, "Project", "프로젝트"),
                tr(a, "Choose a project", "프로젝트 선택"),
                button("cw-open", "", tr(a, "Open project", "프로젝트 열기")),
                tr(a, "Manage projects", "프로젝트 관리")
            )
        }
        Some(t) if !t.loaded => format!(
            "<div class=\"cw-empty\"><h1>{}</h1><p>{}</p>{}<a href=\"#/ai-sandbox\">{}</a></div>",
            tr(a, "Opening project", "프로젝트 여는 중"),
            escape(&t.error),
            button("cw-retry", "", tr(a, "Retry connection", "다시 연결")),
            tr(a, "Project settings", "프로젝트 설정")
        ),
        Some(t) => {
            let mut cards = String::new();
            for node in &t.layout.nodes {
                cards.push_str(&card(a, t, node));
            }
            let status = if text(&t.workspace, "status") == "ready" {
                String::new()
            } else {
                format!(
                    "<div class=\"cw-runtime-notice\">{} · {} {}</div>",
                    escape(text(&t.workspace, "status")),
                    tr(
                        a,
                        "Files and commands require a running, verified project VM. Retained records remain available.",
                        "파일과 명령은 실행 중인 프로젝트 VM에 연결한다. 저장된 기록은 계속 조회할 수 있다."
                    ),
                    button(
                        "cw-reconcile",
                        "",
                        tr(a, "Verify workspace", "작업 폴더 확인")
                    )
                )
            };
            format!(
                "{status}<section id=\"cw-board\" aria-label=\"{}\"><div class=\"cw-board-label\">CANVAS / {}</div><div class=\"cw-board-help\">{}</div><div id=\"cw-world\" style=\"{}\">{cards}</div><div class=\"cw-tools\">{}{}{}<span>{:.0}%</span>{}</div></section><section id=\"cw-editor-pane\" {}><div class=\"cw-editor-bar\">{}<strong>{}</strong></div><div id=\"cw-editor-slot\"></div></section>",
                tr(a, "AI Canvas workspace", "AI Canvas 작업 공간"),
                escape(name),
                tr(
                    a,
                    "Drag empty space to pan · Ctrl/⌘ scroll to zoom",
                    "빈 공간 드래그로 이동 · Ctrl/⌘ 스크롤로 확대"
                ),
                transform(&t.layout),
                button("cw-add", "folder", tr(a, "Folder", "폴더")),
                button("cw-add", "terminal", tr(a, "Terminal", "터미널")),
                button("cw-add", "note", tr(a, "Note", "메모")),
                t.layout.zoom * 100.,
                button("cw-reset", "", tr(a, "Reset view", "화면 초기화")),
                if t.editor.is_some() { "" } else { "hidden" },
                button("cw-back", "", tr(a, "Back to canvas", "캔버스로 돌아가기")),
                tr(
                    a,
                    "Personal Page · Web VS Code",
                    "개인 페이지 · Web VS Code"
                )
            )
        }
        None => String::new(),
    };
    let error = current
        .map(|t| t.error.as_str())
        .filter(|e| !e.is_empty())
        .unwrap_or(&s.error);
    format!(
        "<div class=\"cw-app\"><aside class=\"cw-sidebar\"><a class=\"cw-brand\" href=\"#/ai-canvas\"><img src=\"/firecrab.png\" alt=\"\" width=\"24\" height=\"24\"><div><strong>firecrab</strong><small>private microvm cloud</small></div></a><p class=\"cw-nav-label\">WORKSPACE</p><button class=\"cw-nav-current\" data-action=\"cw-back\">▦ AI Canvas</button>{sidebar}<footer><a href=\"#/vms\">{}</a><label for=\"language\">KO / EN</label><select id=\"language\"><option value=\"ko\" {}>한국어</option><option value=\"en\" {}>English</option></select></footer></aside><main class=\"cw-main\"><header class=\"cw-header\"><span>AI / <strong>AI Canvas</strong></span><span id=\"cw-save-state\">{}</span>{}</header><div class=\"cw-tabs\"><div role=\"tablist\" aria-label=\"{}\">{tabs}</div><button data-action=\"cw-new\" aria-label=\"{}\">+</button></div><div id=\"cw-error\" role=\"status\">{}</div><div class=\"cw-body\">{body}</div></main></div><div id=\"menu-root\"></div><div id=\"modal-root\"></div><div id=\"palette-root\"></div><div id=\"tooltip-root\"></div>",
        tr(a, "Dashboard", "대시보드"),
        if a.locale == "ko" { "selected" } else { "" },
        if a.locale == "en" { "selected" } else { "" },
        if a.dirty {
            tr(a, "Unsaved changes", "미저장 변경")
        } else {
            tr(a, "Private workspace", "개인 작업 화면")
        },
        format_args!(
            "{}{}",
            button("cw-save", "", tr(a, "Save layout", "화면 저장")),
            button(
                "cw-fork",
                "",
                tr(
                    a,
                    "Preserve layout in a new tab",
                    "화면 배치를 새 탭으로 보존"
                )
            )
        ),
        tr(a, "Independent canvas tabs", "독립 캔버스 탭"),
        tr(a, "New empty canvas tab", "새 빈 캔버스 탭"),
        escape(error)
    )
}
fn card(a: &App, t: &Tab, n: &Node) -> String {
    let title = match n.kind.as_str() {
        "folder" => tr(a, "Shared workspace folder", "공용 작업 폴더"),
        "note" => tr(a, "Private note", "개인 메모"),
        "history" => tr(a, "Version history", "버전 기록"),
        _ => tr(a, "Terminal", "터미널"),
    };
    let body = match n.kind.as_str() {
        "note" => format!(
            "<textarea id=\"cw-note-{}\" data-cw-note=\"{}\" aria-label=\"{}\" maxlength=\"2048\" placeholder=\"{}\">{}</textarea>",
            escape(&n.id),
            escape(&n.id),
            title,
            tr(a, "Write a private note…", "개인 메모를 작성한다…"),
            escape(&n.content)
        ),
        "folder" => {
            let mut files = String::new();
            for file in &t.files {
                let path = if t.layout.directory.is_empty() {
                    text(file, "name").to_string()
                } else {
                    format!("{}/{}", t.layout.directory, text(file, "name"))
                };
                files.push_str(&button(
                    if file["directory"] == true {
                        "cw-directory"
                    } else {
                        "cw-editor"
                    },
                    &path,
                    &format!(
                        "{} {}",
                        if file["directory"] == true {
                            "▱"
                        } else {
                            "▤"
                        },
                        text(file, "name")
                    ),
                ));
            }
            format!(
                "<div class=\"cw-folder-path\">/workspace/shared/{}{}</div><div class=\"cw-tree\">{files}</div><footer>{}{}</footer>",
                escape(&t.layout.directory),
                button("cw-editor", "", tr(a, "Open folder", "폴더 열기")),
                button("cw-refresh", "", tr(a, "Refresh", "새로고침")),
                button("cw-history", "", tr(a, "History", "기록"))
            )
        }
        "history" => {
            let rows = t
                .history
                .iter()
                .map(|r| {
                    format!(
                        "<li><span>v{}</span><strong>{}</strong><small>{} B</small></li>",
                        r["id"],
                        escape(text(r, "path")),
                        r["byteLength"]
                    )
                })
                .collect::<String>();
            format!(
                "{}<ul class=\"cw-history\">{rows}</ul>",
                button(
                    "cw-history",
                    "",
                    tr(a, "Load retained history", "저장된 기록 불러오기")
                )
            )
        }
        "terminal" => pty::card(a, t, n),
        _ => String::new(),
    };
    format!(
        "<article class=\"cw-node cw-{}\" id=\"cw-node-{}\" style=\"{}\"><header><div data-cw-drag data-id=\"{}\"><span class=\"cw-node-icon\">{}</span><strong>{}</strong></div><button data-action=\"cw-remove\" data-id=\"{}\" aria-label=\"{}\">×</button></header><div class=\"cw-node-body\">{body}</div><button class=\"cw-resize\" data-cw-resize data-id=\"{}\" aria-label=\"{}\">⌟</button></article>",
        escape(&n.kind),
        escape(&n.id),
        position(n),
        escape(&n.id),
        match n.kind.as_str() {
            "folder" => "▱",
            "terminal" => ">_",
            "history" => "◷",
            _ => "▤",
        },
        title,
        escape(&n.id),
        tr(a, "Close panel", "패널 닫기"),
        escape(&n.id),
        tr(a, "Resize panel", "패널 크기 조절")
    )
}
