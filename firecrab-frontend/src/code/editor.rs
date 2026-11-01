//! The editor shell observes real workspace AUTH; runtime adapters enable tools later.
use super::*;
use crate::model::escape;

pub(super) fn content(app: &App, w: &Workspace) -> String {
    let tools = tools(app, w);
    let microvm = view::runtime_section(app, w);
    let checks = format!(
        "{}{}",
        auth::verification(app, w),
        agent_auth::content(app, w)
    );
    format!(
        "<div id=\"code-editor\"><header class=\"code-detail-head\"><a class=\"code-text-link\" href=\"#/code/{}\">{}</a><h2>{}</h2><p>{}/{}</p></header><div class=\"code-editor-layout\"><div>{microvm}{tools}{}</div><section id=\"code-editor-auth\" class=\"code-section\" aria-label=\"AI AUTH\"><h3>AI AUTH</h3>{}{}</section></div></div>",
        w.id,
        tr(app, "Back to workspace", "워크스페이스로 돌아가기"),
        escape(&w.spec.name),
        escape(&w.spec.github.owner),
        escape(&w.spec.github.repository),
        agent_run::content(app, w),
        binding::content(app, w),
        checks
    )
}

#[derive(Default)]
pub(super) struct State {
    current: Option<Status>,
    loading: bool,
    pub(super) busy: bool,
    error: bool,
    epoch: u64,
}
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Status {
    workspace_id: String,
    revision: u64,
    vm_id: String,
    runtime_id: String,
    operation_id: String,
    phase: String,
    version: String,
}
fn runtime<'a>(app: &'a App, w: &Workspace) -> Option<&'a Runtime> {
    match &app.code.runtime {
        RuntimeState::Current(r)
            if r.phase == "ready"
                && r.action == "start"
                && uuid(&w.id)
                && !lifecycle::unresolved(app) =>
        {
            Some(r)
        }
        _ => None,
    }
}
fn matches(w: &Workspace, r: &Runtime, s: &Status) -> bool {
    s.workspace_id == w.id
        && s.revision == w.revision
        && s.vm_id == r.vm_id
        && Some(s.runtime_id.as_str()) == r.runtime_id.as_deref()
        && s.operation_id == r.operation_id
        && ["preparing", "ready", "failed"].contains(&s.phase.as_str())
        && !s.version.is_empty()
}
pub(super) fn invalidate(app: &mut App) {
    app.code.editor.epoch += 1;
    app.code.editor.current = None;
    app.code.editor.loading = false;
    app.code.editor.error = false;
}
pub(super) fn load(app: Shared) {
    let mut a = app.borrow_mut();
    if !is_editor(&a.route) || a.code.editor.loading || a.code.editor.busy {
        return;
    }
    let Some(w) = binding::selected(&a).cloned() else {
        invalidate(&mut a);
        return;
    };
    let Some(r) = runtime(&a, &w).cloned() else {
        invalidate(&mut a);
        return;
    };
    a.code.editor.loading = true;
    let (generation, epoch) = (a.generation, a.code.editor.epoch);
    drop(a);
    spawn_local(async move {
        let result = api::request("GET", &format!("{WORKSPACES}/{}/editor", w.id), None)
            .await
            .and_then(|v| {
                serde_json::from_value::<Option<Status>>(v).map_err(|_| "invalid".into())
            });
        let mut a = app.borrow_mut();
        if a.generation != generation || a.code.editor.epoch != epoch {
            return;
        }
        a.code.editor.loading = false;
        let valid = binding::selected(&a) == Some(&w) && runtime(&a, &w) == Some(&r);
        let value = result
            .ok()
            .filter(|value| value.as_ref().is_none_or(|s| matches(&w, &r, s)));
        let changed = a.code.editor.current != value.clone().flatten()
            || a.code.editor.error != value.is_none();
        a.code.editor.current = if valid { value.clone().flatten() } else { None };
        a.code.editor.error = valid && value.is_none();
        drop(a);
        if changed {
            render_content(&app);
        }
    });
}
pub(super) fn action(app: &Shared, action: &str) -> bool {
    if action != "code-editor-prepare" {
        return false;
    }
    let mut a = app.borrow_mut();
    if a.code.editor.busy || auth::is_open(&a) || a.code.binding.is_some() {
        return true;
    }
    let Some(w) = binding::selected(&a).cloned() else {
        return true;
    };
    let Some(r) = runtime(&a, &w).cloned() else {
        return true;
    };
    a.code.editor.busy = true;
    invalidate(&mut a);
    let (generation, epoch) = (a.generation, a.code.editor.epoch);
    drop(a);
    render_content(app);
    let app = app.clone();
    spawn_local(async move {
        let body = serde_json::json!({"expectedRevision":w.revision,"vmId":r.vm_id,"runtimeId":r.runtime_id,"operationId":r.operation_id});
        let result = api::request(
            "POST",
            &format!("{WORKSPACES}/{}/editor", w.id),
            Some(&body),
        )
        .await
        .ok()
        .and_then(|v| serde_json::from_value::<Status>(v).ok())
        .filter(|s| matches(&w, &r, s));
        let mut a = app.borrow_mut();
        if a.generation != generation || a.code.editor.epoch != epoch {
            return;
        }
        a.code.editor.busy = false;
        a.code.editor.error = result.is_none();
        a.code.editor.current = result;
        drop(a);
        render_content(&app);
        load(app);
    });
    true
}
fn tools(app: &App, w: &Workspace) -> String {
    let s = &app.code.editor;
    let current = runtime(app, w).and_then(|r| s.current.as_ref().filter(|s| matches(w, r, s)));
    let ready = current.is_some_and(|s| s.phase == "ready");
    let preparing = s.busy || current.is_some_and(|s| s.phase == "preparing");
    let status = if ready {
        tr(
            app,
            "Web VS Code and terminal are ready for this workspace.",
            "이 워크스페이스의 Web VS Code와 터미널이 준비됐습니다.",
        )
    } else if preparing {
        tr(
            app,
            "Preparing Web VS Code and terminal…",
            "Web VS Code와 터미널 준비 중…",
        )
    } else {
        tr(
            app,
            "Web VS Code is not connected yet. Account changes do not start an editor or terminal.",
            "Web VS Code가 아직 연결되지 않았습니다. 계정을 변경해도 편집기나 터미널을 실행하지 않습니다.",
        )
    };
    let mut out = format!(
        "<section class=\"code-section code-editor-tools\" aria-label=\"Web VS Code\"><h3>Web VS Code</h3><p class=\"code-hint\" role=\"status\">{status}</p><div class=\"code-account-actions\">"
    );
    for (terminal, en, ko) in [
        (false, "Open Web VS Code", "Web VS Code 열기"),
        (true, "Open terminal", "터미널 열기"),
    ] {
        let label = tr(app, en, ko);
        if ready {
            out.push_str(&format!("<a class=\"btn\" href=\"{WORKSPACES}/{}/editor/open{}\" target=\"_blank\" rel=\"noopener noreferrer\">{label}</a>",w.id,if terminal{"?terminal=true"}else{""}));
        } else {
            out.push_str(&format!("<button class=\"btn\" disabled>{label}</button>"));
        }
    }
    if !ready {
        out.push_str(&format!(
            "<button class=\"btn primary\" data-action=\"code-editor-prepare\" {}>{}</button>",
            if preparing
                || runtime(app, w).is_none()
                || app.code.binding.is_some()
                || auth::is_open(app)
            {
                "disabled"
            } else {
                ""
            },
            tr(app, "Prepare editor and terminal", "편집기와 터미널 준비")
        ));
    }
    out.push_str("</div>");
    if s.error || current.is_some_and(|s| s.phase == "failed") {
        out.push_str(&format!("<p class=\"code-message\" role=\"alert\">{}</p>",tr(app,"Could not confirm editor readiness. Check its status or retry preparation. A supported Ubuntu/glibc image is required.","편집기 준비 상태를 확인하지 못했습니다. 상태를 확인하거나 준비를 재시도해 주세요. 지원되는 Ubuntu/glibc 이미지가 필요합니다.")));
    }
    out.push_str(&format!("<p class=\"code-hint\">{}</p></section>",tr(app,"Opens in a separate authenticated site. Terminal commands run as the workspace user in /home/fc-code/project. Stop or restart ends this browser session.","인증된 별도 사이트에서 엽니다. 터미널 명령은 /home/fc-code/project에서 워크스페이스 사용자로 실행됩니다. 중지하거나 다시 시작하면 브라우저 세션이 종료됩니다.")));
    out
}
