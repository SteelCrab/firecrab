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
    let mut out = format!(
        "<section id=\"code-workspaces\" class=\"code-setup\"><p class=\"code-setup-intro\">{}</p><div class=\"code-setup-connections\">{} {} <button type=\"button\" class=\"btn\" data-action=\"code-reload-accounts\" {}>{}</button></div><form id=\"code-setup\"><fieldset {disabled}>",
        tr(
            app,
            "Choose a repository, development environment and optional agent. Save the workspace to keep these settings.",
            "저장소, 개발 환경과 에이전트를 선택합니다. 워크스페이스를 저장하면 이 설정이 유지됩니다."
        ),
        link(app, "pipe/auth", "Manage GitHub", "GitHub 관리"),
        link(app, "pipe/ai", "Manage AI AUTH", "AI AUTH 관리"),
        if s.loading || s.busy { "disabled" } else { "" },
        tr(app, "Reload connections", "연결 새로 불러오기")
    );
    out.push_str(&format!("<section class=\"code-setup-section\"><h2><span>1</span> {}</h2><div class=\"code-setup-fields\">",tr(app,"Repository","저장소")));
    out.push_str(&input(app,"name","Workspace name","워크스페이스 이름",&s.spec.name,"type=\"text\" maxlength=\"64\" required autocomplete=\"off\" aria-describedby=\"code-name-hint\""));
    out.push_str(&format!("<p id=\"code-name-hint\" class=\"code-hint\">{}</p><p class=\"code-connected\">GitHub · {}</p>",
        tr(app,"Use up to 64 letters, digits, dots, underscores or hyphens.","영문, 숫자, 점, 밑줄, 하이픈을 사용합니다. 최대 64자까지 입력할 수 있습니다."),
        if s.github.is_empty() {tr(app,"Connect a GitHub account to choose a repository.","저장소를 선택하려면 GitHub 계정을 연결해 주세요.").into()} else {escape(&s.github)}));
    if s.loading || s.repo_loading {
        out.push_str(&format!(
            "<p role=\"status\" class=\"code-hint\">{}</p>",
            tr(
                app,
                "Loading repository choices…",
                "저장소 목록을 불러오는 중…"
            )
        ));
    }
    let chosen = format!("{}/{}", s.spec.github.owner, s.spec.github.repository);
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
    let mut options = option(
        "",
        tr(app, "Select a branch", "브랜치 선택"),
        &s.spec.github.branch,
    );
    for name in &s.branches {
        options.push_str(&option(name, name, &s.spec.github.branch));
    }
    out.push_str(&select(app, "branch", "Branch", "브랜치", &options));
    if s.branch_loading {
        out.push_str(&format!(
            "<p class=\"code-hint\" role=\"status\">{}</p>",
            tr(app, "Loading branches…", "브랜치를 불러오는 중…")
        ));
    }
    if !s.spec.github.repository.is_empty()
        && !s.branch_loading
        && s.branches.is_empty()
        && s.repo_error.is_empty()
    {
        out.push_str(&format!(
            "<p class=\"code-hint\">{}</p>",
            tr(
                app,
                "No branches found. An empty repository needs a first commit.",
                "브랜치가 없습니다. 빈 저장소에는 첫 커밋이 필요합니다."
            )
        ));
    }
    out.push_str(&more(
        app,
        "code-more-branches",
        "Load more branches",
        "브랜치 더 불러오기",
        s.branch_next,
        s.branch_loading,
    ));
    if !s.repo_error.is_empty() {
        out.push_str(&format!(
            "<p class=\"code-message\" role=\"alert\">{}</p>",
            message(app, &s.repo_error)
        ));
    }
    out.push_str("</div></section>");
    out.push_str(&format!("<section class=\"code-setup-section\"><h2><span>2</span> {}</h2><div class=\"code-setup-fields\">",tr(app,"Development environment","개발 환경")));
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
        "type=\"number\" min=\"1\" max=\"32\" step=\"1\" required",
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
        "type=\"number\" min=\"1\" max=\"500\" step=\"1\" required",
    ));
    out.push_str("</div></div></section>");
    out.push_str(&format!("<section class=\"code-setup-section\"><h2><span>3</span> {}</h2><div class=\"code-setup-fields\">",tr(app,"Agent & AI AUTH","에이전트 및 AI AUTH")));
    let selected = s.spec.agent.as_ref().map_or("", |a| a.agent.as_str());
    let mut options = option(
        "",
        tr(
            app,
            "No agent (workspace only)",
            "에이전트 없음 (워크스페이스만 생성)",
        ),
        selected,
    );
    for (id, label) in [
        ("codex", "Codex"),
        ("claude_code", "Claude Code"),
        ("opencode", "OpenCode"),
    ] {
        options.push_str(&option(id, label, selected));
    }
    out.push_str(&select(app, "agent", "Agent", "에이전트", &options));
    if let Some(binding) = &s.spec.agent {
        let mut options = option(
            "",
            tr(app, "Select a compatible account", "호환 계정 선택"),
            &binding.connection_id,
        );
        let accounts = s
            .accounts
            .iter()
            .filter(|a| compatible(&binding.agent, a))
            .collect::<Vec<_>>();
        for a in &accounts {
            let method = if a.authentication == "chatgpt" {
                "ChatGPT"
            } else if a.provider == "anthropic" {
                "Anthropic API key"
            } else {
                "OpenAI API key"
            };
            options.push_str(&option(
                &a.id,
                &format!("{} · {method}", a.name),
                &binding.connection_id,
            ));
        }
        out.push_str(&select(
            app,
            "account",
            "AI AUTH account",
            "AI AUTH 계정",
            &options,
        ));
        if accounts.is_empty() {
            out.push_str(&format!("<p class=\"code-hint\">{}</p>",tr(app,"No compatible account. Connect one in AI AUTH, then reload connections.","호환되는 계정이 없습니다. AI AUTH에서 계정을 연결한 뒤 연결 정보를 새로 불러와 주세요.")));
        }
    }
    if s.ai_error {
        out.push_str(&format!("<p class=\"code-message\" role=\"alert\">{}</p>",tr(app,"AI AUTH accounts could not be loaded. Reload connections to select an agent account.","AI AUTH 계정을 불러오지 못했습니다. 에이전트 계정을 선택하려면 연결 정보를 새로 불러와 주세요.")));
    }
    let _ = write!(
        out,
        "<p class=\"code-hint\">{}</p></div></section></fieldset>",
        tr(
            app,
            "Account compatibility is checked when saving. Agent sign-in is verified separately.",
            "저장할 때 계정 호환성을 확인합니다. 에이전트 로그인은 별도로 검증합니다."
        )
    );
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
    out.push_str(&format!("<footer class=\"code-setup-footer\"><p>{}</p><div><a class=\"btn\" href=\"#/code\">{}</a><button id=\"code-save\" class=\"btn primary\" type=\"submit\" {}>{}</button></div></footer></form></section>",
        tr(app,"Saves configuration only. No VM or agent starts yet.","설정만 저장합니다. VM이나 에이전트는 아직 실행하지 않습니다."),tr(app,"Cancel","취소"),
        if s.busy || (s.pending.is_none() && !can_save(s)) {"disabled"} else {""},
        if s.busy {tr(app,"Saving…","저장 중…")} else if s.pending.is_some() {tr(app,"Check saved workspace","저장된 워크스페이스 확인")} else {tr(app,"Create workspace","워크스페이스 생성")}));
    out
}
