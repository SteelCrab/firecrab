use super::*;
use crate::model::{escape, lowercase};
use std::fmt::Write;

pub fn content(app: &App) -> String {
    if auth::is_open(app) {
        return auth::content(app);
    }
    if app.route == "code/new" {
        return setup::content(app);
    }
    let mut out = format!(
        "<section id=\"code-workspaces\" aria-label=\"{}\">",
        if is_editor(&app.route) {
            tr(app, "Workspace editor", "워크스페이스 편집기")
        } else {
            tr(app, "CODE workspaces", "CODE 워크스페이스")
        }
    );
    if !is_editor(&app.route) {
        let label = tr(app, "Search workspaces", "워크스페이스 검색");
        let _ = write!(
            out,
            "<div class=\"code-toolbar\"><p>{}</p><div class=\"code-tools\"><input id=\"code-search\" type=\"search\" aria-label=\"{label}\" placeholder=\"{label}\" value=\"{}\"><button id=\"code-refresh\" class=\"btn\" type=\"button\" data-action=\"code-refresh\">{}</button></div></div>",
            tr(
                app,
                "Your repositories, development environments and AI AUTH connections.",
                "저장소, 개발 환경과 AI AUTH 연결을 확인합니다."
            ),
            escape(&app.code.query),
            tr(app, "Refresh", "새로고침")
        );
    }
    if !app.loaded {
        out.push_str(&empty(tr(
            app,
            "Loading workspaces…",
            "워크스페이스를 불러오는 중…",
        )));
    } else if let Some(error) = app.code.error {
        out.push_str(&problem(app, error));
    } else if app.code.workspaces.is_empty() {
        out.push_str(&empty(tr(
            app,
            "No workspaces yet.",
            "아직 워크스페이스가 없습니다.",
        )));
    } else {
        let selected = binding::selected(app);
        if is_editor(&app.route) {
            match selected {
                Some(w) => out.push_str(&editor::content(app, w)),
                None => out.push_str(&empty(tr(
                    app,
                    "This workspace no longer exists.",
                    "이 워크스페이스는 더 이상 존재하지 않습니다.",
                ))),
            }
            out.push_str("</section>");
            return out;
        }
        out.push_str("<div class=\"code-layout\">");
        out.push_str(&workspace_list(app, selected));
        out.push_str("<div id=\"code-details\" class=\"code-details\">");
        match selected {
            Some(workspace) => out.push_str(&detail(app, workspace)),
            None => out.push_str(&empty(tr(
                app,
                "This workspace no longer exists.",
                "이 워크스페이스는 더 이상 존재하지 않습니다.",
            ))),
        }
        out.push_str("</div></div>");
    }
    out.push_str("</section>");
    out
}
fn empty(message: &str) -> String {
    format!("<p class=\"code-empty\" role=\"status\">{message}</p>")
}
fn problem(app: &App, error: Problem) -> String {
    let message = match error {
        Problem::Access => tr(
            app,
            "Operator access is required to view CODE workspaces.",
            "CODE 워크스페이스를 보려면 운영자 권한이 필요합니다.",
        ),
        Problem::Unavailable => tr(
            app,
            "Workspace status is unavailable. Retrying automatically.",
            "워크스페이스 상태를 확인할 수 없습니다. 자동으로 다시 시도합니다.",
        ),
        Problem::Invalid => tr(
            app,
            "Workspace data could not be read. Refresh or check the API version.",
            "워크스페이스 정보를 읽을 수 없습니다. 새로고침하거나 API 버전을 확인해 주세요.",
        ),
    };
    format!("<p class=\"code-message\" role=\"alert\">{message}</p>")
}
fn workspace_list(app: &App, selected: Option<&Workspace>) -> String {
    let query = lowercase(app.code.query.trim());
    let workspaces = app
        .code
        .workspaces
        .iter()
        .filter(|w| {
            [
                &w.spec.name,
                &w.spec.github.owner,
                &w.spec.github.repository,
                &w.spec.github.branch,
            ]
            .iter()
            .any(|value| lowercase(value).contains(&query))
        })
        .collect::<Vec<_>>();
    let mut out = format!(
        "<nav class=\"code-list\" aria-label=\"{}\"><div class=\"code-list-head\">{} <span>{} / {}</span></div><ul>",
        tr(app, "Workspace list", "워크스페이스 목록"),
        tr(app, "Workspaces", "워크스페이스"),
        workspaces.len(),
        app.code.workspaces.len()
    );
    for workspace in &workspaces {
        let spec = &workspace.spec;
        let _ = write!(
            out,
            "<li><a id=\"code-link-{}\" href=\"#/code/{}\" {}><strong>{}</strong><span>{}/{}</span><small>{} · {}</small></a></li>",
            workspace.id,
            workspace.id,
            if selected.is_some_and(|s| s.id == workspace.id) {
                "aria-current=\"page\""
            } else {
                ""
            },
            escape(&spec.name),
            escape(&spec.github.owner),
            escape(&spec.github.repository),
            escape(&spec.github.branch),
            runtime_name(app, &spec.runtime)
        );
    }
    out.push_str("</ul>");
    if workspaces.is_empty() {
        out.push_str(&empty(tr(
            app,
            "No matching workspaces.",
            "검색 결과가 없습니다.",
        )));
    }
    out.push_str("</nav>");
    out
}
fn property(app: &App, en: &str, ko: &str, value: &str) -> String {
    format!(
        "<div><dt>{}</dt><dd>{}</dd></div>",
        tr(app, en, ko),
        escape(value)
    )
}
fn section(title: &str, body: &str) -> String {
    format!("<section class=\"code-section\"><h3>{title}</h3>{body}</section>")
}
fn detail(app: &App, workspace: &Workspace) -> String {
    let spec = &workspace.spec;
    let repo = &spec.github;
    let mut out = format!(
        "<header class=\"code-detail-head\"><span class=\"code-eyebrow\">{}</span><h2>{}</h2><p>{}/{} <span>· {}</span></p></header>",
        tr(app, "Workspace", "워크스페이스"),
        escape(&spec.name),
        escape(&repo.owner),
        escape(&repo.repository),
        escape(&repo.branch)
    );
    if let RuntimeState::Current(runtime) = &app.code.runtime {
        let (label, _) = phase(app, &runtime.phase);
        let _ = write!(
            out,
            "<p class=\"code-workspace-state\" role=\"status\">{label}</p>"
        );
    }
    let _ = write!(
        out,
        "<p><a class=\"btn primary\" href=\"#/code/{}/editor\">{}</a></p><details id=\"code-overview-settings\" class=\"code-options\" {}><summary>{}</summary>",
        workspace.id,
        tr(app, "Open workspace", "워크스페이스 열기"),
        if app.code.binding.is_some() || (lifecycle::unresolved(app) && !lifecycle::busy(app)) {
            "open"
        } else {
            ""
        },
        tr(app, "Workspace settings", "워크스페이스 설정")
    );
    let mut config = String::from("<dl class=\"code-properties\">");
    config.push_str(&property(
        app,
        "Environment",
        "개발 환경",
        runtime_name(app, &spec.runtime),
    ));
    config.push_str(&property(
        app,
        "Resources",
        "사양",
        &format!(
            "{} vCPU · {} MiB RAM · {} GiB",
            spec.resources.cpu, spec.resources.ram, spec.resources.disk_gb
        ),
    ));
    config.push_str(&property(
        app,
        "GitHub account",
        "GitHub 계정",
        &repo.account,
    ));
    config.push_str(&property(
        app,
        "Repository",
        "저장소",
        &format!("{}/{}", repo.owner, repo.repository),
    ));
    config.push_str(&property(app, "Branch", "브랜치", &repo.branch));
    config.push_str(&property(
        app,
        "Configuration revision",
        "설정 버전",
        &workspace.revision.to_string(),
    ));
    config.push_str("</dl>");
    let mut auth = format!(
        "<div class=\"code-auth-row\"><div><strong>GitHub</strong><p>{}</p></div><a class=\"code-text-link\" href=\"#/pipe/auth\">{}</a></div>",
        connection(app, &workspace.connections.github),
        tr(app, "Manage GitHub", "GitHub 관리")
    );
    auth.push_str(&binding::content(app, workspace));
    auth.push_str(&crate::code::auth::verification(app, workspace));
    auth.push_str(&agent_auth::content(app, workspace));
    out.push_str(&runtime_section(app, workspace));
    out.push_str(&section(tr(app, "Account connections", "계정 연결"), &auth));
    out.push_str(&section(tr(app, "Configuration", "설정"), &config));
    out.push_str("</details>");
    out
}
pub(super) fn runtime_section(app: &App, workspace: &Workspace) -> String {
    let mut body = match &app.code.runtime {
        RuntimeState::Loading => {
            empty(tr(app, "Checking MicroVM status…", "MicroVM 상태 확인 중…"))
        }
        RuntimeState::Absent => empty(tr(app, "MicroVM not provisioned", "MicroVM 생성 전")),
        RuntimeState::Failed(error) => problem(app, *error),
        RuntimeState::Current(runtime) => {
            let (label, tone) = phase(app, &runtime.phase);
            let mut body = format!(
                "<p class=\"code-runtime-status {tone}\" role=\"status\">{label}</p><dl class=\"code-properties\">"
            );
            body.push_str(&property(app, "MicroVM", "MicroVM", &runtime.vm_id));
            body.push_str(&property(
                app,
                "VM state",
                "VM 상태",
                vm_state(app, runtime.vm_state.as_deref()),
            ));
            body.push_str(&property(
                app,
                "Image",
                "이미지",
                &format!(
                    "{} · {}",
                    runtime.placement.template.name, runtime.placement.template.version
                ),
            ));
            body.push_str(&property(
                app,
                "Project folder",
                "프로젝트 폴더",
                &runtime.project_path,
            ));
            body.push_str(&property(
                app,
                "Initial commit",
                "최초 커밋",
                runtime.initial_commit.as_deref().unwrap_or("—"),
            ));
            body.push_str(&property(
                app,
                "Network",
                "네트워크",
                &runtime.placement.micro_network_id,
            ));
            body.push_str(&property(
                app,
                "Storage",
                "스토리지",
                &runtime.placement.storage_root,
            ));
            body.push_str(&property(
                app,
                "Runtime configuration revision",
                "실행 환경 설정 버전",
                &runtime.configuration_revision.to_string(),
            ));
            body.push_str("</dl>");
            if let Some(error) = &runtime.error {
                let _ = write!(
                    body,
                    "<p class=\"code-message\" role=\"alert\">{}</p>",
                    failure(app, error)
                );
            }
            body.push_str(&format!("<p class=\"code-hint\">{}</p>", tr(app, "Stopping the MicroVM retains project files. MicroVM readiness does not confirm editor or agent readiness.", "MicroVM을 중지해도 프로젝트 파일은 유지됩니다. MicroVM 준비 상태는 편집기나 에이전트 준비 상태를 뜻하지 않습니다.")));
            body
        }
    };
    body.push_str(&lifecycle::content(app, workspace));
    section(tr(app, "MicroVM environment", "MicroVM 환경"), &body)
}
pub(super) fn connection<'a>(app: &App, state: &str) -> &'a str {
    match state {
        "present_unverified" => tr(
            app,
            "Account present · access not verified",
            "계정 있음 · 접근 검증 전",
        ),
        "missing" => tr(
            app,
            "Account missing or disconnected",
            "계정 없음 또는 연결 해제됨",
        ),
        "identity_changed" => tr(
            app,
            "Connected GitHub identity changed",
            "연결된 GitHub 계정이 바뀜",
        ),
        "incompatible" => tr(
            app,
            "Account is incompatible with the selected agent",
            "선택한 에이전트와 호환되지 않는 계정",
        ),
        "not_required" => tr(app, "Not required", "필요 없음"),
        _ => tr(app, "Account status unknown", "계정 상태 알 수 없음"),
    }
}
pub(super) fn runtime_name<'a>(app: &App, name: &str) -> &'a str {
    match name {
        "node_js" => "Node.js",
        "python" => "Python",
        "rust" => "Rust",
        _ => tr(app, "Unknown environment", "알 수 없는 개발 환경"),
    }
}
pub(super) fn agent_name<'a>(app: &App, name: &str) -> &'a str {
    match name {
        "codex" => "Codex",
        "claude_code" => "Claude Code",
        "opencode" => "OpenCode",
        _ => tr(app, "Unknown agent", "알 수 없는 에이전트"),
    }
}
fn phase<'a>(app: &App, value: &str) -> (&'a str, &'static str) {
    match value {
        "queued" => (tr(app, "Queued", "대기 중"), "busy"),
        "provisioning" => (tr(app, "Creating MicroVM", "MicroVM 생성 중"), "busy"),
        "preparing" => (tr(app, "Preparing project", "프로젝트 준비 중"), "busy"),
        "ready" => (tr(app, "MicroVM ready", "MicroVM 준비 완료"), "ok"),
        "stopping" => (tr(app, "Stopping", "중지 중"), "busy"),
        "stopped" => (
            tr(app, "Stopped · files retained", "중지됨 · 파일 유지"),
            "idle",
        ),
        "failed" => (tr(app, "Operation failed", "작업 실패"), "error"),
        "reconcile_required" => (
            tr(
                app,
                "Runtime needs reconciliation",
                "실행 환경 상태 재확인 필요",
            ),
            "error",
        ),
        _ => (
            tr(app, "Runtime status unknown", "실행 환경 상태 알 수 없음"),
            "error",
        ),
    }
}
fn vm_state<'a>(app: &App, state: Option<&str>) -> &'a str {
    match state {
        Some("running") => tr(app, "Running", "실행 중"),
        Some("created") => tr(app, "Created", "생성됨"),
        Some("starting") => tr(app, "Starting", "시작 중"),
        Some("stopping") => tr(app, "Stopping", "중지 중"),
        Some("stopped") => tr(app, "Stopped", "중지됨"),
        Some("paused") => tr(app, "Paused", "일시 중지됨"),
        Some("error") => tr(app, "Error", "오류"),
        None => tr(app, "VM absent", "VM 없음"),
        _ => tr(app, "Unknown", "알 수 없음"),
    }
}
fn failure<'a>(app: &App, error: &str) -> &'a str {
    match error {
        "ownership_changed" => tr(
            app,
            "The VM no longer matches the workspace binding.",
            "VM과 워크스페이스 연결 정보가 일치하지 않습니다.",
        ),
        "creation_failed" => tr(
            app,
            "MicroVM creation failed.",
            "MicroVM 생성에 실패했습니다.",
        ),
        "startup_failed" => tr(
            app,
            "MicroVM startup failed.",
            "MicroVM 시작에 실패했습니다.",
        ),
        "repository_denied" => tr(
            app,
            "GitHub repository or branch access was denied.",
            "GitHub 저장소 또는 브랜치에 접근할 수 없습니다.",
        ),
        "account_changed" => tr(
            app,
            "The selected GitHub account changed during checkout.",
            "저장소를 준비하는 동안 선택한 GitHub 계정이 바뀌었습니다.",
        ),
        "guest_unavailable" => tr(
            app,
            "The guest connection could not be confirmed.",
            "게스트 연결을 확인할 수 없습니다.",
        ),
        "preparation_failed" => tr(
            app,
            "Project preparation failed.",
            "프로젝트 준비에 실패했습니다.",
        ),
        "sync_failed" => tr(
            app,
            "File synchronization failed. The VM was kept running.",
            "파일 동기화에 실패했습니다. VM 실행을 유지했습니다.",
        ),
        "stop_failed" => tr(
            app,
            "MicroVM stop could not be confirmed.",
            "MicroVM 중지를 확인할 수 없습니다.",
        ),
        _ => tr(
            app,
            "The runtime reported an unknown failure.",
            "실행 환경에서 알 수 없는 오류가 발생했습니다.",
        ),
    }
}
