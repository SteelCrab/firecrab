//! CODE hosts the existing Pipe AI manager; it owns no credential form or credential store.
use super::*;
use crate::{
    advanced::ai,
    model::{escape, list, text},
};
use serde_json::json;

#[derive(Default)]
pub(super) struct State {
    open: bool,
    return_dirty: bool,
    rows: Option<Value>,
    loading: bool,
    error: String,
    checking: bool,
    report: Option<Value>,
    check_error: String,
    epoch: u64,
}
pub fn is_open(app: &App) -> bool {
    app.code.auth.open && is_route(&app.route)
}
pub fn parent_dirty(app: &App) -> bool {
    is_open(app) && app.code.auth.return_dirty
}
pub fn blocking(app: &App) -> bool {
    is_open(app) && ai::blocking(app)
}
pub fn rows(app: &App) -> &[Value] {
    list(app.code.auth.rows.as_ref().unwrap_or(&Value::Null))
}
pub fn loaded(app: &App) -> bool {
    app.code.auth.rows.is_some() || !app.code.auth.error.is_empty()
}
pub fn error(app: &App) -> &str {
    &app.code.auth.error
}
pub fn publish(app: &mut App, value: Value) {
    app.code.auth.rows = Some(value);
    app.code.auth.error.clear();
    app.code.auth.report = None;
}
pub(super) fn invalidate(app: &mut App) {
    app.code.auth.report = None;
}
pub(super) fn context_changed(app: &mut App) {
    agent_auth::invalidate(app);
    app.code.auth.epoch += 1;
    app.code.auth.checking = false;
    app.code.auth.report = None;
    app.code.auth.check_error.clear();
}
pub fn api_status(app: &App) -> (&'static str, &'static str) {
    if !error(app).is_empty() {
        ("down", tr(app, "Status unavailable", "상태 확인 불가"))
    } else if !loaded(app) {
        ("checking", tr(app, "Checking…", "확인 중…"))
    } else {
        ("live", tr(app, "Live", "실시간"))
    }
}
pub fn content(app: &App) -> String {
    format!(
        "<section id=\"code-workspaces\" class=\"code-auth-manager\"><header class=\"code-auth-heading\"><h2>AI AUTH</h2><button class=\"btn\" type=\"button\" data-action=\"code-auth-close\" {}>{}</button></header><p class=\"code-hint\">{}</p>{}</section>",
        if ai::engaged(app) { "disabled" } else { "" },
        if is_editor(&app.route) {
            tr(app, "Back to editor", "편집기로 돌아가기")
        } else {
            tr(app, "Back to workspace", "워크스페이스로 돌아가기")
        },
        tr(
            app,
            "These accounts are shared with Pipe AI. Replacing or disconnecting one affects every workspace that uses it. Account connection does not confirm agent sign-in.",
            "Pipe AI와 같은 계정을 사용합니다. 계정을 교체하거나 연결을 해제하면 이 계정을 사용하는 모든 워크스페이스에 영향을 줍니다. 계정 연결은 에이전트 로그인 확인을 뜻하지 않습니다."
        ),
        ai::content(app)
    )
}
pub fn load(app: Shared) {
    let mut a = app.borrow_mut();
    if !is_open(&a) || a.code.auth.loading || ai::engaged(&a) {
        return;
    }
    let generation = a.generation;
    let epoch = a.code.auth.epoch;
    a.code.auth.loading = true;
    drop(a);
    spawn_local(async move {
        let result = api::request("GET", "/api/pipe/ai/connections", None).await;
        let mut a = app.borrow_mut();
        if a.generation != generation || !is_open(&a) || a.code.auth.epoch != epoch {
            return;
        }
        a.code.auth.loading = false;
        let before = a.code.auth.rows.clone();
        let failed = !a.code.auth.error.is_empty();
        match result {
            Ok(value) if value.is_array() => {
                a.code.auth.rows = Some(value);
                a.code.auth.error.clear();
            }
            _ => {
                a.code.auth.rows = None;
                a.code.auth.error = tr(
                    &a,
                    "Could not load AI AUTH accounts. Retry the connection.",
                    "AI AUTH 계정을 불러오지 못했습니다. 다시 시도해 주세요.",
                )
                .into();
            }
        }
        let changed = before != a.code.auth.rows || failed != !a.code.auth.error.is_empty();
        drop(a);
        if changed {
            render_content(&app);
        }
    });
}
pub fn action(app: &Shared, action: &str) -> bool {
    if !is_route(&app.borrow().route) {
        return false;
    }
    match action {
        "code-auth-open" => {
            let mut a = app.borrow_mut();
            if a.code.auth.open || a.busy || binding::locked(&a) {
                return true;
            }
            a.code.auth.open = true;
            agent_auth::invalidate(&mut a);
            invalidate_observations(&mut a);
            a.code.auth.epoch += 1;
            a.code.auth.loading = false;
            a.code.auth.rows = None;
            a.code.auth.error.clear();
            a.code.auth.checking = false;
            a.code.auth.return_dirty = a.dirty;
            a.code.auth.report = None;
            a.advanced.ai = ai::State::default();
            drop(a);
            load(app.clone());
            render_content(app);
        }
        "code-auth-close" => {
            let mut a = app.borrow_mut();
            if ai::engaged(&a) {
                return true;
            }
            a.dirty = a.code.auth.return_dirty;
            a.code.auth.open = false;
            a.code.auth.epoch += 1;
            a.code.auth.loading = false;
            a.code.auth.checking = false;
            a.advanced.ai = ai::State::default();
            let setup = a.route == "code/new";
            drop(a);
            if setup {
                super::setup::reload_accounts(app);
            } else {
                binding::reload_accounts(app);
                super::load(app.clone());
            }
            render_content(app);
        }
        "code-auth-verify" => check(app),
        _ => return false,
    }
    true
}
fn check(app: &Shared) {
    let mut a = app.borrow_mut();
    if a.code.auth.checking || a.code.auth.open || a.code.binding.is_some() {
        return;
    }
    let Some(w) = binding::selected(&a) else {
        return;
    };
    let (id, revision, spec, generation, epoch) = (
        w.id.clone(),
        w.revision,
        json!(w.spec),
        a.generation,
        a.code.auth.epoch,
    );
    let observations = (w.connections.github.clone(), w.connections.ai.clone());
    a.code.auth.checking = true;
    a.code.auth.report = None;
    a.code.auth.check_error.clear();
    drop(a);
    render_content(app);
    let app = app.clone();
    spawn_local(async move {
        let result = api::request_detailed(
            "POST",
            &format!("{WORKSPACES}/{id}/verify"),
            Some(&json!({"expectedRevision":revision})),
            None,
        )
        .await;
        let mut a = app.borrow_mut();
        if a.generation != generation || a.code.auth.epoch != epoch {
            return;
        }
        a.code.auth.checking = false;
        let current = binding::selected(&a).is_some_and(|w| {
            w.id == id
                && w.revision == revision
                && json!(w.spec) == spec
                && w.connections.github == observations.0
                && w.connections.ai == observations.1
        });
        match result {
            Ok(report) if current && report["id"] == id && report["revision"] == revision => a.code.auth.report = Some(report),
            _ => a.code.auth.check_error = tr(&a,"Connections changed or could not be checked. Reload the workspace and retry.","연결이 바뀌었거나 확인할 수 없습니다. 워크스페이스를 새로 불러온 뒤 다시 시도해 주세요.").into(),
        }
        drop(a);
        render_content(&app);
    });
}
pub(super) fn verification(app: &App, workspace: &Workspace) -> String {
    let s = &app.code.auth;
    let mut out = format!(
        "<div class=\"code-auth-check\"><button class=\"btn\" type=\"button\" data-action=\"code-auth-verify\" {}>{}</button>",
        if s.checking || app.code.binding.is_some() {
            "disabled"
        } else {
            ""
        },
        if s.checking {
            tr(app, "Checking connections…", "연결 확인 중…")
        } else {
            tr(app, "Check saved connections", "저장된 연결 확인")
        }
    );
    if !s.check_error.is_empty() {
        out.push_str(&format!(
            "<p role=\"alert\" class=\"code-message\">{}</p>",
            escape(&s.check_error)
        ));
    }
    if let Some(report) = &s.report
        && report["id"] == workspace.id
        && report["revision"] == workspace.revision
    {
        let ai = &report["ai"];
        let label = match (text(ai, "status"), text(ai, "method")) {
            ("not_required", _) => tr(
                app,
                "No agent account required",
                "에이전트 계정이 필요하지 않음",
            ),
            ("credentials_verified", "openai_api_key") => tr(
                app,
                "Provider credentials verified · OpenAI API key",
                "서비스 자격 증명 확인됨 · OpenAI API 키",
            ),
            ("credentials_verified", "chatgpt_plan") => tr(
                app,
                "Provider credentials verified · ChatGPT plan",
                "서비스 자격 증명 확인됨 · ChatGPT 플랜",
            ),
            ("credentials_verified", "anthropic_api_key") => tr(
                app,
                "Provider credentials verified · Anthropic API key",
                "서비스 자격 증명 확인됨 · Anthropic API 키",
            ),
            ("failed", _) if ai["reason"] == "consent_required" => tr(
                app,
                "Approval required. Sign in again from AI AUTH to grant access.",
                "승인이 필요합니다. AI AUTH에서 다시 로그인하여 접근을 승인해 주세요.",
            ),
            ("failed", _) if ai["reason"] == "authentication_rejected" => tr(
                app,
                "Authentication rejected. Reconnect the assigned account in AI AUTH.",
                "인증이 거부됐습니다. AI AUTH에서 할당된 계정을 다시 연결해 주세요.",
            ),
            _ => tr(
                app,
                "Provider credentials were not verified. Check the assigned account in AI AUTH.",
                "서비스 자격 증명을 확인하지 못했습니다. AI AUTH에서 지정된 계정을 확인해 주세요.",
            ),
        };
        out.push_str(&format!("<p class=\"code-credential-result\" role=\"status\">{}: {label}</p><p class=\"code-hint\">{}</p>",tr(app,"Last check","최근 확인"),tr(app,"This is a point-in-time provider check. Agent authentication remains unverified.","확인 시점의 서비스 자격 증명 결과입니다. 에이전트 인증은 아직 검증되지 않았습니다.")));
        out.push_str(&format!(
            "<p class=\"code-hint\">{}</p>",
            if report["github"]["status"] == "verified" {
                tr(
                    app,
                    "Repository read verified at the last check.",
                    "최근 확인에서 저장소 읽기를 검증했습니다.",
                )
            } else {
                tr(
                    app,
                    "Repository read was not verified at the last check.",
                    "최근 확인에서 저장소 읽기를 검증하지 못했습니다.",
                )
            }
        ));
    }
    out.push_str("</div>");
    out
}
