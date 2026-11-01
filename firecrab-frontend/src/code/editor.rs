//! The editor shell observes real workspace AUTH; runtime adapters enable tools later.
use super::*;
use crate::model::escape;

pub(super) fn content(app: &App, w: &Workspace) -> String {
    let checks = format!(
        "{}{}",
        auth::verification(app, w),
        agent_auth::content(app, w)
    );
    format!(
        "<div id=\"code-editor\"><header class=\"code-detail-head\"><a class=\"code-text-link\" href=\"#/code/{}\">{}</a><h2>{}</h2><p>{}/{}</p></header><div class=\"code-editor-layout\"><section class=\"code-section\" aria-label=\"Web VS Code\"><h3>Web VS Code</h3><p class=\"code-hint\">{}</p><div class=\"code-account-actions\"><button class=\"btn\" disabled>{}</button><button class=\"btn\" disabled>{}</button></div></section><section id=\"code-editor-auth\" class=\"code-section\" aria-label=\"AI AUTH\"><h3>AI AUTH</h3>{}{}<p class=\"code-hint\">{}</p><button class=\"btn\" disabled>{}</button></section></div></div>",
        w.id,
        tr(app, "Back to workspace", "워크스페이스로 돌아가기"),
        escape(&w.spec.name),
        escape(&w.spec.github.owner),
        escape(&w.spec.github.repository),
        tr(
            app,
            "Web VS Code is not connected yet. Account changes do not start an editor or terminal.",
            "Web VS Code가 아직 연결되지 않았습니다. 계정을 변경해도 편집기나 터미널을 실행하지 않습니다."
        ),
        tr(app, "Open Web VS Code", "Web VS Code 열기"),
        tr(app, "Open terminal", "터미널 열기"),
        binding::content(app, w),
        checks,
        tr(
            app,
            "Agent authentication must be verified before execution is available.",
            "에이전트 인증을 검증해야 실행할 수 있습니다."
        ),
        tr(app, "Run agent", "에이전트 실행")
    )
}
