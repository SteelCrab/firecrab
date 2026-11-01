use super::*;
use crate::model::escape;
use std::fmt::Write;

fn button(app: &App, action: &str, en: &str, ko: &str, disabled: bool) -> String {
    format!(
        "<button class=\"btn{}\" type=\"button\" data-action=\"{action}\" {}>{}</button>",
        if action == "code-binding-save" {
            " primary"
        } else {
            ""
        },
        if disabled { "disabled" } else { "" },
        tr(app, en, ko)
    )
}
fn field(app: &App, key: &str, en: &str, ko: &str, options: &str) -> String {
    format!(
        "<div class=\"code-setup-field\"><label for=\"code-binding-{key}\">{}</label><select id=\"code-binding-{key}\">{options}</select></div>",
        tr(app, en, ko)
    )
}
pub fn content(app: &App, w: &Workspace) -> String {
    let mut out = String::from("<div class=\"code-account-state\">");
    if let Some(b) = &w.spec.agent {
        let _ = write!(
            out,
            "<strong>{} · AI AUTH</strong><p role=\"status\">{}</p><code>{}</code>",
            super::super::view::agent_name(app, &b.agent),
            super::super::view::connection(app, &w.connections.ai),
            escape(&b.connection_id)
        );
    } else {
        let _ = write!(
            out,
            "<p role=\"status\">{}</p>",
            tr(app, "No agent selected", "선택된 에이전트 없음")
        );
    }
    let _ = write!(
        out,
        "<p class=\"code-hint\">{}</p><div class=\"code-account-actions\">",
        tr(
            app,
            "An account reference does not confirm agent sign-in. Agent authentication is not verified here.",
            "계정을 지정해도 에이전트 로그인이 확인되지는 않습니다. 이 화면에서는 에이전트 인증을 검증하지 않습니다."
        )
    );
    out.push_str(&button(
        app,
        "code-auth-open",
        "Manage AI AUTH",
        "AI AUTH 관리",
        locked(app),
    ));
    if let Some(s) = &app.code.binding {
        out.push_str("</div>");
        out.push_str(&form(app, w, s));
    } else {
        out.push_str(&button(
            app,
            "code-binding-edit",
            "Choose AI AUTH",
            "AI AUTH 선택",
            false,
        ));
        out.push_str("</div>");
    }
    out.push_str("</div>");
    out
}
fn form(app: &App, w: &Workspace, s: &Draft) -> String {
    let locked = s.busy || s.pending.is_some();
    let mut out = format!(
        "<section class=\"code-binding\" aria-label=\"{}\"><h4>{}</h4><fieldset {}>",
        tr(
            app,
            "Workspace AI AUTH assignment",
            "워크스페이스 AI AUTH 할당"
        ),
        tr(app, "Workspace AI AUTH", "워크스페이스 AI AUTH"),
        if locked { "disabled" } else { "" }
    );
    let mut options = accounts::option(
        "",
        tr(
            app,
            "No agent (remove assignment)",
            "에이전트 없음 (할당 제거)",
        ),
        s.agent.as_ref().map_or("", |b| b.agent.as_str()),
    );
    for (id, label) in [
        ("codex", "Codex"),
        ("claude_code", "Claude Code"),
        ("opencode", "OpenCode"),
    ] {
        options.push_str(&accounts::option(
            id,
            label,
            s.agent.as_ref().map_or("", |b| b.agent.as_str()),
        ));
    }
    out.push_str(&field(app, "agent", "Agent", "에이전트", &options));
    if let Some(b) = &s.agent {
        let mut options = accounts::choices(app, &s.accounts, b);
        if !b.connection_id.is_empty()
            && !s
                .accounts
                .iter()
                .any(|a| a.id == b.connection_id && compatible(&b.agent, a))
        {
            options.push_str(&accounts::option(
                &b.connection_id,
                tr(
                    app,
                    "Assigned account unavailable — choose another",
                    "할당 계정 사용 불가 — 다른 계정 선택",
                ),
                &b.connection_id,
            ));
        }
        out.push_str(&field(
            app,
            "account",
            "AI AUTH account",
            "AI AUTH 계정",
            &options,
        ));
        if !s.loading && !s.accounts.iter().any(|a| compatible(&b.agent, a)) {
            let _ = write!(
                out,
                "<p class=\"code-hint\">{}</p>",
                tr(
                    app,
                    "No compatible account. Connect one in AI AUTH, then reload connections.",
                    "호환되는 계정이 없습니다. AI AUTH에서 계정을 연결한 뒤 연결 정보를 새로 불러와 주세요."
                )
            );
        }
    }
    let _ = write!(
        out,
        "<p class=\"code-hint\">{}</p></fieldset>",
        tr(
            app,
            "Reuse an existing account without signing in again. Removing this assignment keeps the shared account connected.",
            "다시 로그인하지 않고 기존 계정을 사용합니다. 이 할당을 제거해도 공유 계정은 연결된 상태로 유지됩니다."
        )
    );
    if s.loading {
        let _ = write!(
            out,
            "<p role=\"status\">{}</p>",
            tr(app, "Loading accounts…", "계정 불러오는 중…")
        );
    }
    let error = if !unchanged(s, w) && !s.busy && s.pending.is_none() {
        "conflict"
    } else {
        s.error
    };
    if !error.is_empty() {
        let message = match error {
            "accounts_unavailable" => tr(
                app,
                "AI AUTH accounts could not be loaded. Reload connections to retry.",
                "AI AUTH 계정을 불러오지 못했습니다. 연결 정보를 다시 불러와 주세요.",
            ),
            "conflict" => tr(
                app,
                "Workspace settings changed. Cancel and review the saved workspace before choosing again.",
                "워크스페이스 설정이 바뀌었습니다. 취소한 뒤 저장된 설정을 확인하고 다시 선택해 주세요.",
            ),
            "runtime_bound" => tr(
                app,
                "Wait for the current MicroVM operation to finish, then retry.",
                "현재 MicroVM 작업을 마친 뒤 다시 시도해 주세요.",
            ),
            "rejected" => tr(
                app,
                "Assignment was rejected. Check operator access and reload account connections.",
                "할당이 거부됐습니다. 운영자 권한을 확인하고 계정 연결을 다시 불러와 주세요.",
            ),
            _ => tr(
                app,
                "The save result is unknown. Check the saved assignment before continuing. The original request is retained.",
                "저장 결과를 확인할 수 없습니다. 계속하기 전에 저장된 할당을 확인해 주세요. 기존 요청은 유지됩니다.",
            ),
        };
        let _ = write!(
            out,
            "<p class=\"code-message\" role=\"alert\">{message}</p>"
        );
    }
    out.push_str("<div class=\"code-account-actions\">");
    out.push_str(&button(
        app,
        "code-binding-reload",
        "Reload connections",
        "연결 새로 불러오기",
        locked || s.loading,
    ));
    out.push_str(&button(
        app,
        "code-binding-cancel",
        "Cancel assignment",
        "할당 취소",
        locked,
    ));
    out.push_str(&button(
        app,
        "code-binding-save",
        if s.pending.is_some() {
            "Check saved assignment"
        } else {
            "Save assignment"
        },
        if s.pending.is_some() {
            "저장된 할당 확인"
        } else {
            "할당 저장"
        },
        s.busy
            || (s.pending.is_none()
                && (!unchanged(s, w) || !eligible(s) || s.original.spec.agent == s.agent)),
    ));
    out.push_str("</div></section>");
    out
}
