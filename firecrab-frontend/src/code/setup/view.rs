use super::*;
use crate::model::escape;
use std::fmt::Write;

fn option(value: &str, label: &str, selected: &str) -> String {
    format!(
        "<option value=\"{}\" {}>{}</option>",
        escape(value),
        if value == selected { "selected" } else { "" },
        escape(label)
    )
}
fn select(app: &App, key: &str, en: &str, ko: &str, options: &str) -> String {
    format!(
        "<div class=\"code-setup-field\"><label for=\"code-field-{key}\">{}</label><select id=\"code-field-{key}\">{options}</select></div>",
        tr(app, en, ko)
    )
}
fn input(app: &App, key: &str, en: &str, ko: &str, value: &str, attrs: &str) -> String {
    format!(
        "<div class=\"code-setup-field\"><label for=\"code-field-{key}\">{}</label><input id=\"code-field-{key}\" value=\"{}\" {attrs}></div>",
        tr(app, en, ko),
        escape(value)
    )
}
fn link(app: &App, path: &str, en: &str, ko: &str) -> String {
    if path == "pipe/ai" {
        return format!(
            "<button type=\"button\" class=\"btn\" data-action=\"code-auth-open\">{}</button>",
            tr(app, en, ko)
        );
    }
    format!(
        "<a class=\"code-text-link\" href=\"#/{path}\" target=\"_blank\" rel=\"noopener\">{} ↗</a>",
        tr(app, en, ko)
    )
}
fn more(app: &App, action: &str, en: &str, ko: &str, next: Option<u32>, loading: bool) -> String {
    if next.is_none() {
        return String::new();
    }
    format!(
        "<button type=\"button\" class=\"btn\" data-action=\"{action}\" {}>{}</button>",
        if loading { "disabled" } else { "" },
        tr(app, en, ko)
    )
}
fn message(app: &App, code: &str) -> &'static str {
    match code {
        "save_conflict" => tr(
            app,
            "The saved workspace changed after the request. Review its current settings before continuing.",
            "요청 이후 저장된 워크스페이스가 바뀌었습니다. 현재 설정을 확인한 뒤 계속해 주세요.",
        ),
        "save_unknown" => tr(
            app,
            "The save result is unknown. Check the saved workspace before retrying. Your original request is retained.",
            "저장 결과를 확인할 수 없습니다. 재시도 전에 저장된 워크스페이스를 확인합니다. 기존 요청은 유지됩니다.",
        ),
        "code_workspace_name_taken" => tr(
            app,
            "This workspace name is already in use. Choose another name.",
            "이미 사용 중인 워크스페이스 이름입니다. 다른 이름을 입력해 주세요.",
        ),
        "code_operator_required" | "forbidden" => tr(
            app,
            "Operator access is required to create a workspace.",
            "워크스페이스를 생성하려면 운영자 권한이 필요합니다.",
        ),
        "validation_failed" | "accounts_changed" => tr(
            app,
            "Check the workspace fields and reload account connections. An account may have changed or been disconnected.",
            "입력 내용을 확인하고 계정 연결을 새로 불러와 주세요. 계정이 바뀌거나 연결이 해제되었을 수 있습니다.",
        ),
        "code_github_missing"
        | "code_github_identity_changed"
        | "code_github_account_changed"
        | "code_github_authentication_rejected" => tr(
            app,
            "Reconnect GitHub or reload connections, then choose the repository again.",
            "GitHub를 다시 연결하거나 연결 정보를 새로 불러온 뒤 저장소를 다시 선택해 주세요.",
        ),
        "code_github_rate_limited" => tr(
            app,
            "GitHub rate limit reached. Wait before reloading choices.",
            "GitHub 요청 한도에 도달했습니다. 잠시 후 다시 불러와 주세요.",
        ),
        "code_github_access_denied" | "code_github_not_found" => tr(
            app,
            "Repository access is unavailable. Check GitHub permissions and organization approval.",
            "저장소에 접근할 수 없습니다. GitHub 권한과 조직 승인을 확인해 주세요.",
        ),
        _ => tr(
            app,
            "Could not load or save workspace data. Check the connection and retry.",
            "워크스페이스 정보를 불러오거나 저장하지 못했습니다. 연결을 확인한 뒤 다시 시도해 주세요.",
        ),
    }
}
pub fn content(app: &App) -> String {
    let Some(s) = &app.code.setup else {
        return format!(
            "<p role=\"status\">{}</p>",
            tr(app, "Loading setup…", "설정 불러오는 중…")
        );
    };
    let disabled = if s.busy || s.pending.is_some() {
        "disabled"
    } else {
        ""
    };
    let chosen = format!("{}/{}", s.spec.github.owner, s.spec.github.repository);
    let mut out = format!(
        "<section id=\"code-workspaces\" class=\"code-setup code-quick-setup\"><p class=\"code-setup-intro\">{}</p><form id=\"code-setup\"><fieldset {disabled}><section class=\"code-setup-section\"><div class=\"code-setup-fields\">",
        tr(
            app,
            "Choose your repository. Start with the defaults.",
            "저장소를 선택하면 기본 설정을 준비합니다."
        )
    );
    if s.github.is_empty() {
        out.push_str(&link(app, "pipe/auth", "Connect GitHub", "GitHub 연결"));
    }
    let mut options = option("", tr(app, "Select a repository", "저장소 선택"), &chosen);
    for r in &s.repos {
        options.push_str(&option(&r.key(), &r.key(), &chosen));
    }
    out.push_str(&select(
        app,
        "repository",
        "GitHub repository",
        "GitHub 저장소",
        &options,
    ));
    out.push_str(&more(
        app,
        "code-more-repos",
        "Load more repositories",
        "저장소 더 불러오기",
        s.repo_next,
        s.repo_loading,
    ));
    if s.loading || s.repo_loading || s.branch_loading {
        out.push_str(&format!(
            "<p role=\"status\" class=\"code-hint\">{}</p>",
            tr(
                app,
                "Loading repository choices…",
                "저장소 목록을 불러오는 중…"
            )
        ));
    }
    if !s.repo_error.is_empty() {
        out.push_str(&format!(
            "<p class=\"code-message\" role=\"alert\">{}</p>",
            message(app, &s.repo_error)
        ));
    }
    if !s.github.is_empty()
        && !s.loading
        && !s.repo_loading
        && s.repos.is_empty()
        && s.repo_error.is_empty()
    {
        out.push_str(&format!(
            "<p class=\"code-hint\">{}</p>",
            tr(
                app,
                "No repositories found. Check the connected account's repository permissions.",
                "저장소가 없습니다. 연결된 계정의 저장소 접근 권한을 확인해 주세요."
            )
        ));
    }
    if !s.spec.github.repository.is_empty() {
        out.push_str(&format!(
            "<p class=\"code-default-summary\">{} · {} · {}</p>",
            escape(&s.spec.name),
            escape(&s.spec.github.branch),
            super::super::view::runtime_name(app, &s.spec.runtime)
        ));
        if !s.branch_loading && s.spec.github.branch.is_empty() {
            out.push_str(&format!("<p class=\"code-message\">{}</p>",tr(app,"Choose an available branch in Settings. An empty repository needs a first commit.","설정에서 사용 가능한 브랜치를 선택해 주세요. 빈 저장소에는 첫 커밋이 필요합니다.")));
        }
    }
    out.push_str("</div></section><section class=\"code-setup-section\"><h2>AI AUTH</h2><div class=\"code-setup-fields\">");
    let eligible = s
        .accounts
        .iter()
        .filter_map(|a| defaults::preferred(a).map(|agent| (agent, a)))
        .collect::<Vec<_>>();
    if let Some(binding) = &s.spec.agent
        && let Some(account) = s.accounts.iter().find(|a| a.id == binding.connection_id)
    {
        out.push_str(&format!(
            "<p class=\"code-default-summary\">{} · {}</p>",
            agent_name(&binding.agent),
            escape(&account.name)
        ));
    }
    if !eligible.is_empty()
        && (eligible.len() > 1
            || s.spec
                .agent
                .as_ref()
                .is_some_and(|a| a.connection_id.is_empty()))
        && !(s.agent_changed && s.spec.agent.is_none())
    {
        let selected = s
            .spec
            .agent
            .as_ref()
            .map(|a| a.connection_id.clone())
            .unwrap_or_default();
        let mut options = option(
            "",
            tr(app, "Choose an AI account", "AI 계정 선택"),
            &selected,
        );
        for (_, account) in &eligible {
            options.push_str(&option(
                &account.id,
                &format!(
                    "{} · {}",
                    account.name,
                    if account.provider == "anthropic" {
                        "Anthropic"
                    } else {
                        "OpenAI"
                    }
                ),
                &selected,
            ));
        }
        out.push_str(&select(
            app,
            "assignment",
            "AI account",
            "AI 계정",
            &options,
        ));
    } else if s.agent_changed && s.spec.agent.is_none() {
        out.push_str(&format!(
            "<p>{}</p>",
            tr(app, "Workspace only", "워크스페이스만 사용")
        ));
    } else if eligible.is_empty() {
        out.push_str(&link(app, "pipe/ai", "Connect AI AUTH", "AI AUTH 연결"));
    }
    if s.spec.agent.as_ref().is_some_and(|a| {
        a.connection_id.is_empty() && !s.accounts.iter().any(|c| compatible(&a.agent, c))
    }) {
        out.push_str(&format!("<p class=\"code-hint\">{}</p>",tr(app,"No compatible account. Connect one in AI AUTH, then reload connections.","호환되는 계정이 없습니다. AI AUTH에서 계정을 연결한 뒤 연결 정보를 새로 불러와 주세요.")));
    }
    if s.ai_error {
        out.push_str(&format!("<p class=\"code-message\" role=\"alert\">{}</p>",tr(app,"AI AUTH accounts could not be loaded. Reload connections to select an agent account.","AI AUTH 계정을 불러오지 못했습니다. 연결 정보를 새로 불러와 주세요.")));
    }
    out.push_str("</div></section>");
    out.push_str(&format!("<details id=\"code-setup-options\" class=\"code-options\" {}><summary>{}</summary><div class=\"code-setup-fields\">",if s.error=="code_workspace_name_taken"{"open"}else{""},tr(app,"Settings","설정")));
    out.push_str(&input(
        app,
        "name",
        "Workspace name",
        "워크스페이스 이름",
        &s.spec.name,
        "maxlength=\"64\" required autocomplete=\"off\"",
    ));
    let mut options = option(
        "",
        tr(app, "Select a branch", "브랜치 선택"),
        &s.spec.github.branch,
    );
    for name in &s.branches {
        options.push_str(&option(name, name, &s.spec.github.branch));
    }
    out.push_str(&select(app, "branch", "Branch", "브랜치", &options));
    out.push_str(&more(
        app,
        "code-more-branches",
        "Load more branches",
        "브랜치 더 불러오기",
        s.branch_next,
        s.branch_loading,
    ));
    let options = [
        ("node_js", "Node.js"),
        ("python", "Python"),
        ("rust", "Rust"),
    ]
    .iter()
    .map(|(id, label)| option(id, label, &s.spec.runtime))
    .collect::<String>();
    out.push_str(&select(app, "runtime", "Runtime", "개발 언어", &options));
    out.push_str("<div class=\"code-capacity\">");
    out.push_str(&input(
        app,
        "cpu",
        "vCPU",
        "vCPU",
        &s.spec.resources.cpu.to_string(),
        "type=\"number\" min=\"1\" max=\"32\" required",
    ));
    let options = [128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768]
        .iter()
        .map(|ram| {
            option(
                &ram.to_string(),
                &format!("{ram} MiB"),
                &s.spec.resources.ram.to_string(),
            )
        })
        .collect::<String>();
    out.push_str(&select(app, "ram", "Memory", "메모리", &options));
    out.push_str(&input(
        app,
        "disk",
        "Disk (GiB)",
        "디스크 (GiB)",
        &s.spec.resources.disk_gb.to_string(),
        "type=\"number\" min=\"1\" max=\"500\" required",
    ));
    out.push_str("</div>");
    let selected = s
        .spec
        .agent
        .as_ref()
        .map_or(if s.agent_changed { "none" } else { "" }, |a| {
            a.agent.as_str()
        });
    let mut options = option("", tr(app, "Choose an agent", "에이전트 선택"), selected);
    options.push_str(&option(
        "none",
        tr(
            app,
            "No agent (workspace only)",
            "에이전트 없음 (워크스페이스만 생성)",
        ),
        selected,
    ));
    for agent in ["codex", "claude_code", "opencode"] {
        options.push_str(&option(agent, agent_name(agent), selected));
    }
    out.push_str(&select(app, "agent", "Agent", "에이전트", &options));
    if let Some(binding) = &s.spec.agent {
        out.push_str(&select(
            app,
            "account",
            "AI AUTH account",
            "AI AUTH 계정",
            &accounts::choices(app, &s.accounts, binding),
        ));
    }
    out.push_str(&format!("<div class=\"code-account-actions\">{} {} <button class=\"btn\" type=\"button\" data-action=\"code-reload-accounts\">{}</button></div></div></details></fieldset>",link(app,"pipe/auth","Manage GitHub","GitHub 관리"),link(app,"pipe/ai","Manage AI AUTH","AI AUTH 관리"),tr(app,"Reload connections","연결 정보 새로 불러오기")));
    if s.pending.is_some() && !s.busy {
        out.push_str(&format!(
            "<p class=\"code-message\" role=\"status\">{}</p>",
            message(app, "save_unknown")
        ));
    }
    if !s.error.is_empty() {
        out.push_str(&format!(
            "<p id=\"code-setup-error\" class=\"code-message\" role=\"alert\">{}</p>",
            message(app, &s.error)
        ));
    }
    if s.error == "save_conflict" {
        out.push_str(&format!("<button class=\"btn\" type=\"button\" data-action=\"code-review-existing\">{}</button>",tr(app,"Review saved workspace","저장된 워크스페이스 검토")));
    }
    let label = if s.busy {
        tr(app, "Saving…", "저장 중…")
    } else if s.pending.is_some() {
        tr(app, "Check saved workspace", "저장된 워크스페이스 확인")
    } else {
        tr(app, "Create workspace", "워크스페이스 생성")
    };
    let _ = write!(
        out,
        "<footer class=\"code-setup-footer\"><p>{}</p><div><a class=\"btn\" href=\"#/code\">{}</a><button id=\"code-save\" class=\"btn primary\" type=\"submit\" {}>{label}</button></div></footer></form></section>",
        tr(
            app,
            "Start the workspace when you are ready to work.",
            "작업할 준비가 되면 워크스페이스를 시작합니다."
        ),
        tr(app, "Cancel", "취소"),
        if s.busy || (s.pending.is_none() && !can_save(s)) {
            "disabled"
        } else {
            ""
        }
    );
    out
}
fn agent_name(agent: &str) -> &str {
    match agent {
        "codex" => "Codex",
        "claude_code" => "Claude Code",
        "opencode" => "OpenCode",
        _ => agent,
    }
}
