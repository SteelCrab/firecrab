//! One configuration flow. Private credentials remain in the existing account services.
mod view;
use super::accounts::{Account, compatible};
use super::*;
use crate::{model::text, window};
use serde_json::json;
pub use view::content;
use web_sys::{HtmlButtonElement, HtmlSelectElement};

const PENDING: &str = "firecrab.code.create.v1";
const CATALOG: &str = "/api/code/github";
#[derive(Clone, Serialize, Deserialize)]
struct Pending {
    id: String,
    spec: Spec,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Choice {
    owner: String,
    repository: String,
    default_branch: String,
}
impl Choice {
    fn key(&self) -> String {
        format!("{}/{}", self.owner, self.repository)
    }
}
pub(super) struct Setup {
    id: String,
    spec: Spec,
    github: String,
    accounts: Vec<Account>,
    repos: Vec<Choice>,
    branches: Vec<String>,
    repo_next: Option<u32>,
    branch_next: Option<u32>,
    metadata_epoch: u64,
    branch_epoch: u64,
    loading: bool,
    repo_loading: bool,
    branch_loading: bool,
    busy: bool,
    pending: Option<Pending>,
    error: String,
    repo_error: String,
    ai_error: bool,
}
fn stored() -> Option<Pending> {
    let storage = window().session_storage().ok().flatten()?;
    let raw = storage.get_item(PENDING).ok().flatten()?;
    let pending: Pending = serde_json::from_str(&raw).ok()?;
    (uuid(&pending.id) && valid_spec(&pending.spec)).then_some(pending)
}
fn remember(pending: Option<&Pending>) {
    if let Ok(Some(storage)) = window().session_storage() {
        if let Some(pending) = pending {
            if let Ok(raw) = serde_json::to_string(pending) {
                let _ = storage.set_item(PENDING, &raw);
            }
        } else {
            let _ = storage.remove_item(PENDING);
        }
    }
}
fn valid_spec(spec: &Spec) -> bool {
    let name = &spec.name;
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && name.bytes().any(|b| b.is_ascii_alphanumeric())
        && !spec.github.account.is_empty()
        && !spec.github.owner.is_empty()
        && !spec.github.repository.is_empty()
        && !spec.github.branch.is_empty()
        && ["node_js", "python", "rust"].contains(&spec.runtime.as_str())
        && (1..=32).contains(&spec.resources.cpu)
        && (128..=32768).contains(&spec.resources.ram)
        && spec.resources.ram.is_power_of_two()
        && (1..=500).contains(&spec.resources.disk_gb)
        && spec.agent.as_ref().is_none_or(|a| {
            uuid(&a.connection_id)
                && ["codex", "claude_code", "opencode"].contains(&a.agent.as_str())
        })
}
fn can_save(s: &Setup) -> bool {
    valid_spec(&s.spec)
        && s.github == s.spec.github.account
        && !s.loading
        && !s.repo_loading
        && !s.branch_loading
        && s.repos
            .iter()
            .any(|r| r.owner == s.spec.github.owner && r.repository == s.spec.github.repository)
        && s.branches.contains(&s.spec.github.branch)
        && s.spec.agent.as_ref().is_none_or(|a| {
            s.accounts
                .iter()
                .any(|c| c.id == a.connection_id && compatible(&a.agent, c))
        })
}
fn live(app: &App, generation: u64) -> bool {
    app.route == "code/new" && app.generation == generation && app.code.setup.is_some()
}
pub fn saving(app: &App) -> bool {
    super::auth::blocking(app)
        || (app.route == "code/new" && app.code.setup.as_ref().is_some_and(|s| s.busy))
}
pub fn api_status(app: &App) -> (&'static str, &'static str) {
    match app.code.setup.as_ref() {
        Some(s) if s.loading || s.repo_loading || s.branch_loading => {
            ("checking", tr(app, "Checking…", "확인 중…"))
        }
        Some(s) if !s.repo_error.is_empty() || s.ai_error => {
            ("down", tr(app, "Status unavailable", "상태 확인 불가"))
        }
        Some(_) => ("live", tr(app, "Live", "실시간")),
        None => ("checking", tr(app, "Checking…", "확인 중…")),
    }
}
pub fn load(app: Shared) {
    if app.borrow().code.setup.is_some() {
        return;
    }
    let pending = stored();
    let id = pending
        .as_ref()
        .map(|p| p.id.clone())
        .unwrap_or_else(|| window().crypto().expect("browser crypto").random_uuid());
    let spec = pending
        .as_ref()
        .map(|p| p.spec.clone())
        .unwrap_or_else(|| Spec {
            name: String::new(),
            github: Repository {
                account: String::new(),
                owner: String::new(),
                repository: String::new(),
                branch: String::new(),
            },
            runtime: "node_js".into(),
            resources: Resources {
                cpu: 2,
                ram: 4096,
                disk_gb: 16,
            },
            agent: None,
        });
    {
        let mut a = app.borrow_mut();
        a.dirty = pending.is_some();
        a.loaded = true;
        a.code.setup = Some(Setup {
            id,
            spec,
            github: String::new(),
            accounts: vec![],
            repos: vec![],
            branches: vec![],
            repo_next: None,
            branch_next: None,
            metadata_epoch: 0,
            branch_epoch: 0,
            loading: false,
            repo_loading: false,
            branch_loading: false,
            busy: false,
            pending,
            error: String::new(),
            repo_error: String::new(),
            ai_error: false,
        });
    }
    metadata(&app);
}
fn metadata(app: &Shared) {
    let mut a = app.borrow_mut();
    let generation = a.generation;
    let Some(s) = a.code.setup.as_mut() else {
        return;
    };
    if s.loading || s.busy {
        return;
    }
    s.loading = true;
    s.metadata_epoch += 1;
    s.branch_epoch += 1;
    s.repo_loading = false;
    s.branch_loading = false;
    let epoch = s.metadata_epoch;
    drop(a);
    render_content(app);
    let app = app.clone();
    spawn_local(async move {
        let github = api::request_detailed("GET", "/api/pipe/auth", None, None).await;
        let accounts = api::request_detailed("GET", "/api/pipe/ai/connections", None, None).await;
        let mut a = app.borrow_mut();
        if !live(&a, generation) {
            return;
        }
        let s = a.code.setup.as_mut().unwrap();
        if s.metadata_epoch != epoch {
            return;
        }
        s.loading = false;
        s.accounts.clear();
        s.ai_error = false;
        match accounts.and_then(|v| {
            super::accounts::parse(v).ok_or_else(|| api::Failure {
                status: None,
                message: String::new(),
                fields: Default::default(),
                code: None,
            })
        }) {
            Ok(accounts) => s.accounts = accounts,
            Err(_) => s.ai_error = true,
        }
        if !github.as_ref().is_ok_and(|v| v.is_array()) {
            s.repo_error = "catalog_unavailable".into();
        } else {
            s.repo_error.clear();
        }
        let login = github
            .ok()
            .and_then(|v| {
                v.as_array()
                    .and_then(|rows| rows.iter().find(|v| v["provider"] == "github"))
                    .map(|v| text(v, "account").to_owned())
            })
            .unwrap_or_default();
        if s.github != login {
            s.repos.clear();
            s.branches.clear();
            s.repo_next = None;
            s.branch_next = None;
            if s.pending.is_none() {
                s.spec.github = Repository {
                    account: login.clone(),
                    owner: String::new(),
                    repository: String::new(),
                    branch: String::new(),
                };
            }
        }
        s.github = login;
        if s.spec.agent.as_ref().is_some_and(|b| {
            !s.accounts
                .iter()
                .any(|c| c.id == b.connection_id && compatible(&b.agent, c))
        }) && s.pending.is_none()
        {
            s.spec.agent.as_mut().unwrap().connection_id.clear();
        }
        drop(a);
        repositories(&app, 1);
        render_content(&app);
    });
}
pub(super) fn reload_accounts(app: &Shared) {
    metadata(app);
}
fn repositories(app: &Shared, page: u32) {
    let mut a = app.borrow_mut();
    let generation = a.generation;
    let Some(s) = a.code.setup.as_mut() else {
        return;
    };
    if s.github.is_empty() || s.repo_loading || s.busy {
        return;
    }
    s.repo_loading = true;
    s.repo_error.clear();
    let (login, epoch) = (s.github.clone(), s.metadata_epoch);
    drop(a);
    render_content(app);
    let app = app.clone();
    spawn_local(async move {
        let path = format!(
            "{CATALOG}/repositories?account={}&page={page}",
            api::encode(&login)
        );
        let result = api::request_detailed("GET", &path, None, None).await;
        let mut a = app.borrow_mut();
        if !live(&a, generation) {
            return;
        }
        let s = a.code.setup.as_mut().unwrap();
        if s.metadata_epoch != epoch {
            return;
        }
        s.repo_loading = false;
        match result {
            Ok(v) if v["account"] == login => {
                if let Ok(repos) = serde_json::from_value::<Vec<Choice>>(v["repositories"].clone())
                {
                    if page == 1 {
                        s.repos.clear();
                    }
                    for repo in repos {
                        if !s.repos.iter().any(|r| r.key() == repo.key()) {
                            s.repos.push(repo);
                        }
                    }
                    s.repo_next = next_page(&v, page);
                } else {
                    s.repo_error = "catalog_invalid".into();
                }
            }
            Err(error) => {
                s.repo_error = error.code.unwrap_or_else(|| "catalog_unavailable".into());
                if page == 1 {
                    s.repos.clear();
                    s.branches.clear();
                }
            }
            _ => s.repo_error = "catalog_invalid".into(),
        }
        let selected = page == 1 && !s.spec.github.repository.is_empty();
        drop(a);
        if selected {
            branches(&app, 1);
        }
        render_content(&app);
    });
}
fn next_page(v: &Value, page: u32) -> Option<u32> {
    v["nextPage"]
        .as_u64()
        .and_then(|p| u32::try_from(p).ok())
        .filter(|p| *p == page + 1 && *p <= 1000)
}
fn branches(app: &Shared, page: u32) {
    let mut a = app.borrow_mut();
    let generation = a.generation;
    let Some(s) = a.code.setup.as_mut() else {
        return;
    };
    if s.spec.github.repository.is_empty() || s.busy {
        return;
    }
    s.branch_epoch += 1;
    s.branch_loading = true;
    let (epoch, metadata, repo) = (
        s.branch_epoch,
        s.metadata_epoch,
        Repository {
            account: s.github.clone(),
            owner: s.spec.github.owner.clone(),
            repository: s.spec.github.repository.clone(),
            branch: String::new(),
        },
    );
    drop(a);
    render_content(app);
    let app = app.clone();
    spawn_local(async move {
        let path = format!(
            "{CATALOG}/branches?account={}&owner={}&repository={}&page={page}",
            api::encode(&repo.account),
            api::encode(&repo.owner),
            api::encode(&repo.repository)
        );
        let result = api::request_detailed("GET", &path, None, None).await;
        let mut a = app.borrow_mut();
        if !live(&a, generation) {
            return;
        }
        let s = a.code.setup.as_mut().unwrap();
        if s.branch_epoch != epoch || s.metadata_epoch != metadata {
            return;
        }
        s.branch_loading = false;
        match result {
            Ok(v)
                if v["account"] == repo.account
                    && v["owner"] == repo.owner
                    && v["repository"] == repo.repository =>
            {
                if let Ok(names) = serde_json::from_value::<Vec<String>>(v["branches"].clone()) {
                    if page == 1 {
                        s.branches.clear();
                    }
                    for name in names {
                        if !s.branches.contains(&name) {
                            s.branches.push(name);
                        }
                    }
                    s.branch_next = next_page(&v, page);
                    if s.pending.is_none() && !s.branches.contains(&s.spec.github.branch) {
                        let default = s
                            .repos
                            .iter()
                            .find(|r| r.owner == repo.owner && r.repository == repo.repository)
                            .map(|r| r.default_branch.as_str());
                        s.spec.github.branch = default
                            .filter(|b| s.branches.iter().any(|v| v == b))
                            .map(str::to_owned)
                            .unwrap_or_default();
                    }
                } else {
                    s.repo_error = "catalog_invalid".into();
                }
            }
            Err(error) => {
                s.repo_error = error.code.unwrap_or_else(|| "catalog_unavailable".into());
                if page == 1 {
                    s.branches.clear();
                }
            }
            _ => s.repo_error = "catalog_invalid".into(),
        }
        drop(a);
        render_content(&app);
    });
}
fn controls(app: &Shared) {
    let a = app.borrow();
    if let Some(s) = &a.code.setup
        && let Some(button) =
            element("code-save").and_then(|n| n.dyn_into::<HtmlButtonElement>().ok())
    {
        button.set_disabled(s.busy || (s.pending.is_none() && !can_save(s)));
    }
}
pub fn input(app: &Shared, node: &Element) -> bool {
    let Some(key) = node.id().strip_prefix("code-field-").map(str::to_owned) else {
        return false;
    };
    let Some(input) = node.dyn_ref::<HtmlInputElement>() else {
        return false;
    };
    let mut a = app.borrow_mut();
    let Some(s) = a.code.setup.as_mut() else {
        return false;
    };
    if s.busy || s.pending.is_some() {
        return true;
    }
    let value = input.value();
    match key.as_str() {
        "name" => s.spec.name = value,
        "cpu" => s.spec.resources.cpu = value.parse().unwrap_or(0),
        "disk" => s.spec.resources.disk_gb = value.parse().unwrap_or(0),
        _ => return false,
    }
    a.dirty = true;
    drop(a);
    controls(app);
    true
}
pub fn change(app: &Shared, node: &Element) -> bool {
    let Some(key) = node.id().strip_prefix("code-field-").map(str::to_owned) else {
        return false;
    };
    let Some(select) = node.dyn_ref::<HtmlSelectElement>() else {
        return false;
    };
    let value = select.value();
    let mut a = app.borrow_mut();
    let Some(s) = a.code.setup.as_mut() else {
        return false;
    };
    if s.busy || s.pending.is_some() {
        return true;
    }
    match key.as_str() {
        "repository" => {
            s.branch_epoch += 1;
            s.branch_loading = false;
            s.branches.clear();
            s.branch_next = None;
            s.repo_error.clear();
            let repo = s.repos.iter().find(|r| r.key() == value);
            s.spec.github = repo
                .map(|r| Repository {
                    account: s.github.clone(),
                    owner: r.owner.clone(),
                    repository: r.repository.clone(),
                    branch: String::new(),
                })
                .unwrap_or_else(|| Repository {
                    account: s.github.clone(),
                    owner: String::new(),
                    repository: String::new(),
                    branch: String::new(),
                });
            if s.spec.name.is_empty() {
                s.spec.name = s.spec.github.repository.chars().take(64).collect();
            }
        }
        "branch" => s.spec.github.branch = value,
        "runtime" => s.spec.runtime = value,
        "ram" => s.spec.resources.ram = value.parse().unwrap_or(0),
        "agent" => {
            s.spec.agent = if value.is_empty() {
                None
            } else {
                Some(Agent {
                    connection_id: s
                        .accounts
                        .iter()
                        .find(|c| compatible(&value, c))
                        .map(|c| c.id.clone())
                        .unwrap_or_default(),
                    agent: value,
                })
            }
        }
        "account" => {
            if let Some(binding) = &mut s.spec.agent {
                binding.connection_id = value;
            }
        }
        _ => return false,
    }
    a.dirty = true;
    drop(a);
    // Removing a focused control can emit change again. Let this event return first.
    let generation = app.borrow().generation;
    let app = app.clone();
    spawn_local(async move {
        if !live(&app.borrow(), generation) {
            return;
        }
        if key == "repository" {
            branches(&app, 1);
        }
        render_content(&app);
    });
    true
}
pub fn action(app: &Shared, action: &str) -> bool {
    if app.borrow().route != "code/new" {
        return false;
    }
    match action {
        "code-review-existing" => {
            let id = app
                .borrow()
                .code
                .setup
                .as_ref()
                .filter(|s| !s.busy && s.error == "save_conflict")
                .and_then(|s| s.pending.as_ref())
                .map(|p| p.id.clone());
            if let Some(id) = id {
                complete(app, &id);
            }
        }
        "code-reload-accounts" => metadata(app),
        "code-more-repos" => {
            let page = app.borrow().code.setup.as_ref().and_then(|s| s.repo_next);
            if let Some(page) = page {
                repositories(app, page);
            }
        }
        "code-more-branches" => {
            let page = app.borrow().code.setup.as_ref().and_then(|s| s.branch_next);
            if let Some(page) = page {
                branches(app, page);
            }
        }
        _ => return false,
    }
    true
}
fn complete(app: &Shared, id: &str) {
    remember(None);
    {
        let mut a = app.borrow_mut();
        a.dirty = false;
        a.busy = false;
        if let Some(s) = a.code.setup.as_mut() {
            s.busy = false;
            s.pending = None;
        }
    }
    let _ = window().location().set_hash(&format!("/code/{id}"));
}
pub fn submit(app: &Shared, form: &str) -> bool {
    if form != "code-setup" {
        return false;
    }
    let mut a = app.borrow_mut();
    let generation = a.generation;
    let Some(s) = a.code.setup.as_mut() else {
        return true;
    };
    if s.busy || (s.pending.is_none() && !can_save(s)) {
        return true;
    }
    let retry = s.pending.is_some();
    let pending = s.pending.clone().unwrap_or_else(|| Pending {
        id: s.id.clone(),
        spec: s.spec.clone(),
    });
    s.pending = Some(pending.clone());
    s.busy = true;
    s.error.clear();
    a.busy = true;
    drop(a);
    remember(Some(&pending));
    render_content(app);
    let app = app.clone();
    spawn_local(async move {
        let path = format!("{WORKSPACES}/{}", pending.id);
        let spec = json!(pending.spec);
        if retry {
            match api::request_detailed("GET", &path, None, None).await {
                Ok(v)
                    if v["id"] == pending.id
                        && v["spec"] == spec
                        && v["revision"].as_u64().is_some_and(|r| r > 0) =>
                {
                    if live(&app.borrow(), generation) {
                        complete(&app, &pending.id);
                    }
                    return;
                }
                Ok(v) if v["id"] == pending.id && v["revision"].as_u64().is_some_and(|r| r > 0) => {
                    finish(&app, generation, "save_conflict", true);
                    return;
                }
                Err(e) if e.status == Some(404) && e.code.as_deref() == Some("not_found") => {}
                _ => {
                    finish(&app, generation, "save_unknown", true);
                    return;
                }
            }
            if !live(&app.borrow(), generation) {
                return;
            }
            if !app.borrow().code.setup.as_ref().is_some_and(can_save) {
                finish(&app, generation, "accounts_changed", false);
                return;
            }
        }
        let body = json!({"expectedRevision":0,"spec":spec});
        let result = api::request_detailed("PUT", &path, Some(&body), None).await;
        if !live(&app.borrow(), generation) {
            return;
        }
        match result {
            Ok(v)
                if v["id"] == pending.id
                    && v["spec"] == body["spec"]
                    && v["revision"].as_u64().is_some_and(|r| r > 0) =>
            {
                complete(&app, &pending.id)
            }
            Ok(_) => finish(&app, generation, "save_unknown", true),
            Err(e)
                if e.status.is_none_or(|s| s >= 500)
                    || e.code.as_deref() == Some("code_workspace_revision_conflict") =>
            {
                finish(&app, generation, "save_unknown", true)
            }
            Err(e) => {
                let code = e.code.unwrap_or_else(|| "save_failed".into());
                finish(&app, generation, &code, false);
            }
        }
    });
    true
}
fn finish(app: &Shared, generation: u64, error: &str, uncertain: bool) {
    let mut a = app.borrow_mut();
    if !live(&a, generation) {
        return;
    }
    a.busy = false;
    let s = a.code.setup.as_mut().unwrap();
    s.busy = false;
    s.error = error.into();
    if !uncertain {
        s.pending = None;
        remember(None);
    }
    drop(a);
    render_content(app);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn setup_account_filter_matches_the_public_agent_contract() {
        use firecrab_api_types::{AiConnection, CodeAgent};
        for provider in ["openai", "anthropic"] {
            for authentication in ["api_key", "chatgpt"] {
                let value = json!({"id":"11111111-1111-4111-8111-111111111111","name":"Account","provider":provider,"authentication":authentication,"endpoint":"https://provider.invalid","models":[],"streaming":false,"toolCalls":false});
                let public: AiConnection = serde_json::from_value(value.clone()).unwrap();
                let browser: Account = serde_json::from_value(value).unwrap();
                for agent in ["codex", "claude_code", "opencode"] {
                    let public_agent: CodeAgent = serde_json::from_value(json!(agent)).unwrap();
                    assert_eq!(
                        compatible(agent, &browser),
                        public_agent.provider_compatible(&public)
                    );
                }
            }
        }
        assert!(!compatible(
            "opencode",
            &Account {
                id: "invalid".into(),
                name: "x".into(),
                provider: "openai".into(),
                authentication: "api_key".into()
            }
        ));
    }
}
