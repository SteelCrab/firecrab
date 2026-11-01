use super::*;
use crate::model::escape;
fn phase<'a>(app: &App, value: &'a str) -> &'a str {
    match value {
        "queued" => tr(app, "Queued", "대기 중"),
        "starting" => tr(app, "Preparing agent…", "에이전트 준비 중…"),
        "running" => tr(app, "Running", "실행 중"),
        "cancelling" => tr(app, "Cancelling…", "취소 중…"),
        "succeeded" => tr(app, "Completed", "완료"),
        "failed" => tr(app, "Failed", "실패"),
        "cancelled" => tr(app, "Cancelled", "취소됨"),
        "interrupted" => tr(app, "Interrupted", "중단됨"),
        _ => value,
    }
}
pub(in crate::code) fn content(app: &App, w: &Workspace) -> String {
    let empty = State::default();
    let s = if app.code.agent_run.workspace == w.id
        && app.code.agent_run.generation == app.generation
    {
        &app.code.agent_run
    } else {
        &empty
    };
    let locked = s.busy || s.pending.is_some() || s.rows.iter().any(Run::active);
    let assignment = w.spec.agent.as_ref();
    let name = assignment
        .map(|a| match a.agent.as_str() {
            "codex" => "Codex",
            "claude_code" => "Claude Code",
            "opencode" => "OpenCode",
            _ => "AI",
        })
        .unwrap_or("AI");
    let account = assignment
        .and_then(|a| agent_auth::account_label(app, &a.connection_id))
        .unwrap_or(tr(app, "Choose AI AUTH", "AI AUTH 선택"));
    let mut out = format!(
        "<section id=\"code-agent-execution\" class=\"code-section\" aria-label=\"{}\"><div class=\"code-task-head\"><h3>{}</h3><span>{name} · {}</span><button class=\"code-text-link\" data-action=\"code-edit-binding\">{}</button></div><label for=\"code-agent-prompt\">{}</label><textarea id=\"code-agent-prompt\" rows=\"5\" maxlength=\"16384\" placeholder=\"{}\" {}>{}</textarea><details id=\"code-agent-options\" class=\"code-options\"><summary>{}</summary><label for=\"code-agent-model\">{}</label><input id=\"code-agent-model\" maxlength=\"200\" value=\"{}\" placeholder=\"{}\" {}></details><div class=\"code-account-actions\"><button id=\"code-agent-run-start\" class=\"btn primary\" data-action=\"code-agent-run-start\" {}>{}</button>",
        tr(app, "Agent execution", "에이전트 실행"),
        tr(app, "Work with AI", "AI와 작업"),
        escape(account),
        tr(app, "Change", "변경"),
        tr(app, "Task", "작업 지시"),
        tr(app, "Describe what to change…", "수정할 내용을 입력하세요…"),
        if locked { "disabled" } else { "" },
        escape(&s.prompt),
        tr(app, "Options", "옵션"),
        tr(app, "Model ID (optional)", "모델 ID (선택)"),
        escape(&s.model),
        tr(app, "Agent default", "에이전트 기본 모델"),
        if locked { "disabled" } else { "" },
        if can_start(app, w) { "" } else { "disabled" },
        tr(app, "Run agent", "에이전트 실행")
    );
    if s.pending.is_some() {
        out.push_str(&format!(
            "<button class=\"btn\" data-action=\"code-agent-run-retry\" {}>{}</button>",
            if s.busy { "disabled" } else { "" },
            tr(app, "Retry same request", "같은 요청 재시도")
        ));
    }
    if locked {
        out.push_str(&format!(
            "<button class=\"btn\" data-action=\"code-agent-run-cancel\" {}>{}</button>",
            if s.busy || s.rows.iter().any(|r| r.phase == "cancelling") {
                "disabled"
            } else {
                ""
            },
            tr(app, "Cancel run", "실행 취소")
        ));
    }
    out.push_str(&format!(
        "</div><p class=\"code-hint\">{}</p>",
        tr(
            app,
            "AI can edit files and run commands. Usage is billed to this AI account.",
            "AI가 파일을 수정하고 명령을 실행합니다. 사용료는 이 AI 계정에 청구됩니다."
        )
    ));
    if s.pending.is_some() && !s.busy {
        out.push_str(&format!(
            "<p class=\"code-message\" role=\"status\">{}</p>",
            tr(
                app,
                "Start was not confirmed. Retry the same request or cancel it.",
                "시작을 확인하지 못했습니다. 같은 요청을 재시도하거나 취소해 주세요."
            )
        ));
    }
    if s.error {
        out.push_str(&format!("<p class=\"code-message\" role=\"alert\">{}</p>",tr(app,"Could not confirm execution. Check the account, workspace and browser session storage.","실행을 확인하지 못했습니다. 계정, 워크스페이스와 브라우저 세션 저장소를 확인해 주세요.")));
    }
    if !agent_auth::execution_supported(app, w) {
        out.push_str(&format!(
            "<p class=\"code-hint\">{}</p>",
            tr(
                app,
                "Select a supported agent and AI AUTH connection to run a task.",
                "지원되는 에이전트와 AI AUTH 연결을 선택해야 작업을 실행할 수 있습니다."
            )
        ));
    }
    if let Some(run) = s.rows.first() {
        out.push_str(&observation(app, run));
    }
    if s.rows.len() > 1 {
        out.push_str(&format!(
            "<details class=\"code-agent-history\"><summary>{}</summary>",
            tr(app, "Earlier runs", "이전 실행")
        ));
        for run in s.rows.iter().skip(1) {
            out.push_str(&observation(app, run));
        }
        out.push_str("</details>");
    }
    out.push_str("</section>");
    out
}
fn observation(app: &App, r: &Run) -> String {
    let observed = r.models_used.join(", ");
    let mut out = format!(
        "<article class=\"code-agent-observation\" data-run-id=\"{}\" data-phase=\"{}\"><p role=\"status\"><strong>{}</strong></p>",
        escape(&r.id),
        escape(&r.phase),
        phase(app, &r.phase)
    );
    if r.error.is_some() {
        out.push_str(&format!(
            "<p class=\"code-message\">{}</p>",
            tr(
                app,
                "Review the workspace before trying again.",
                "다시 실행하기 전에 워크스페이스를 확인해 주세요."
            )
        ));
    }
    if !r.output.is_empty() {
        out.push_str(&format!(
            "<pre class=\"code-agent-output\" tabindex=\"0\" aria-label=\"{}\">{}</pre>",
            tr(app, "Agent output", "에이전트 출력"),
            escape(&r.output)
        ));
    }
    if r.truncated {
        out.push_str(&format!(
            "<p class=\"code-hint\">{}</p>",
            tr(
                app,
                "Output capture limit reached.",
                "출력 저장 한도에 도달했습니다."
            )
        ));
    }
    if r.phase == "interrupted" {
        out.push_str(&format!("<p class=\"code-hint\">{}</p>",tr(app,"Files may already have changed. Review the workspace before starting a new task.","파일이 이미 변경됐을 수 있습니다. 새 작업을 시작하기 전에 워크스페이스를 확인해 주세요.")));
    }
    out.push_str(&format!("<details id=\"code-run-{}\" class=\"code-options\"><summary>{}</summary><p>{} {} · {} · {} {}</p><p>AI AUTH {}</p>",escape(&r.id),tr(app,"Run details","실행 상세"),escape(&r.agent),escape(&r.cli_version),escape(if observed.is_empty(){&r.model}else{&observed}),r.provider_requests,tr(app,"model requests","모델 요청"),escape(agent_auth::account_label(app,&r.connection_id).unwrap_or(tr(app,"Disconnected account","연결 해제된 계정")))));
    if let Some(reason) = &r.error {
        out.push_str(&format!("<code>{}</code>", escape(reason)));
    }
    out.push_str("</details>");
    out.push_str("</article>");
    out
}
