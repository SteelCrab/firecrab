//! Runtime-bound CLI configuration checks never enable agent execution by themselves.
use super::*;
use crate::model::escape;
use serde_json::json;

#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Adapter {
    id: String,
    agent: String,
    provider: String,
    authentication: String,
    cli_version: String,
    supported: bool,
}
#[derive(Default)]
pub(super) struct State {
    metadata: Option<(Vec<Adapter>, Vec<accounts::Account>)>,
    loading: bool,
    unavailable: bool,
    checking: bool,
    epoch: u64,
    report: Option<Value>,
    error: bool,
}
pub(super) fn invalidate(app: &mut App) {
    let s = &mut app.code.agent_auth;
    s.epoch += 1;
    s.checking = false;
    s.report = None;
    s.error = false;
    s.loading = false;
}
pub(super) fn load(app: Shared) {
    let mut a = app.borrow_mut();
    if a.code.agent_auth.loading || auth::is_open(&a) {
        return;
    }
    a.code.agent_auth.loading = true;
    let (generation, epoch) = (a.generation, a.code.agent_auth.epoch);
    drop(a);
    spawn_local(async move {
        let catalog = api::request("GET", "/api/code/agent-auth/adapters", None)
            .await
            .ok()
            .and_then(|v| serde_json::from_value::<Vec<Adapter>>(v).ok());
        let accounts = if catalog.is_some() {
            api::request("GET", "/api/pipe/ai/connections", None)
                .await
                .ok()
                .and_then(accounts::parse)
        } else {
            None
        };
        let mut a = app.borrow_mut();
        if a.generation != generation || a.code.agent_auth.epoch != epoch || auth::is_open(&a) {
            return;
        }
        let metadata = catalog.zip(accounts);
        let changed = a.code.agent_auth.metadata != metadata
            || a.code.agent_auth.unavailable != metadata.is_none();
        if changed {
            invalidate(&mut a);
        }
        let s = &mut a.code.agent_auth;
        s.loading = false;
        s.unavailable = metadata.is_none();
        s.metadata = metadata;
        drop(a);
        if changed {
            render_content(&app);
        }
    });
}
fn profile<'a>(app: &'a App, w: &Workspace) -> Option<&'a Adapter> {
    let b = w.spec.agent.as_ref()?;
    let (profiles, accounts) = app.code.agent_auth.metadata.as_ref()?;
    let account = accounts.iter().find(|a| a.id == b.connection_id)?;
    profiles.iter().find(|p| {
        p.agent == b.agent
            && p.provider == account.provider
            && p.authentication == account.authentication
    })
}
fn runtime(app: &App) -> Option<(&str, &str)> {
    match &app.code.runtime {
        RuntimeState::Current(r)
            if r.phase == "ready"
                && r.error.is_none()
                && r.vm_state.as_deref() == Some("running") =>
        {
            r.runtime_id.as_deref().map(|id| (r.vm_id.as_str(), id))
        }
        _ => None,
    }
}
fn can_check(app: &App, w: &Workspace) -> bool {
    !auth::is_open(app)
        && app.code.binding.is_none()
        && w.connections.ai == "present_unverified"
        && runtime(app).is_some()
        && profile(app, w).is_some_and(|p| p.supported)
}
pub(super) fn content(app: &App, w: &Workspace) -> String {
    if w.spec.agent.is_none() {
        return String::new();
    }
    let s = &app.code.agent_auth;
    let description = if s.unavailable {
        tr(
            app,
            "Agent adapter status unavailable.",
            "에이전트 어댑터 상태 확인 불가.",
        )
        .into()
    } else if let Some(p) = profile(app, w) {
        if p.supported {
            format!("{} · CLI {}", escape(&p.id), escape(&p.cli_version))
        } else {
            tr(app,"This account method has no verified CLI handoff. This combination stays disabled.","이 계정 방식의 CLI 전달 계약이 검증되지 않았습니다. 이 조합은 비활성 상태를 유지합니다.").into()
        }
    } else {
        tr(
            app,
            "Agent authentication adapter is not verified.",
            "에이전트 인증 어댑터가 검증되지 않았습니다.",
        )
        .into()
    };
    let mut out = format!(
        "<section class=\"code-agent-auth\" aria-label=\"{}\"><h4>{}</h4><p class=\"code-hint\">{description}</p><p class=\"code-hint\">{}</p><button class=\"btn\" type=\"button\" data-action=\"code-agent-auth-verify\" {}>{}</button>",
        tr(app, "Agent authentication adapter", "에이전트 인증 어댑터"),
        tr(app, "Agent AUTH verification", "에이전트 AUTH 검증"),
        tr(
            app,
            "Requires a ready MicroVM and the exact CLI version. The check uses disposable credentials and runs no agent or inference.",
            "준비된 MicroVM과 정확한 CLI 버전이 필요합니다. 임시 자격 증명으로 확인하며 에이전트나 추론을 실행하지 않습니다."
        ),
        if s.checking || !can_check(app, w) {
            "disabled"
        } else {
            ""
        },
        if s.checking {
            tr(app, "Checking agent AUTH…", "에이전트 AUTH 확인 중…")
        } else {
            tr(app, "Verify agent AUTH", "에이전트 AUTH 검증")
        }
    );
    if s.error {
        out.push_str(&format!("<p role=\"alert\" class=\"code-message\">{}</p>",tr(app,"Agent AUTH could not be checked or its context changed. Reload and retry.","에이전트 AUTH를 확인할 수 없거나 연결 상태가 바뀌었습니다. 새로 불러온 뒤 다시 시도해 주세요.")));
    }
    if let Some(report) = &s.report {
        let result = &report["result"];
        let label = if result["status"] == "configuration_verified" {
            tr(
                app,
                "Last check: provider credential and CLI configuration verified. Temporary credentials were removed; agent execution remains disabled.",
                "최근 확인: 서비스 자격 증명과 CLI 설정을 검증했습니다. 임시 자격 증명을 제거했으며 에이전트 실행은 비활성 상태입니다.",
            )
        } else {
            match result["reason"].as_str().unwrap_or("") {
                "adapter_unavailable" => tr(
                    app,
                    "Blocked: this account method has no verified adapter.",
                    "차단됨: 이 계정 방식의 어댑터가 검증되지 않았습니다.",
                ),
                "cli_unavailable" | "version_mismatch" | "cli_untrusted" => tr(
                    app,
                    "Blocked: install the exact trusted CLI version in the MicroVM.",
                    "차단됨: MicroVM에 정확한 버전의 신뢰할 수 있는 CLI를 설치해 주세요.",
                ),
                "authentication_rejected" | "missing" | "consent_required" => tr(
                    app,
                    "Blocked: reconnect or approve the assigned account in AI AUTH.",
                    "차단됨: AI AUTH에서 할당 계정을 다시 연결하거나 승인해 주세요.",
                ),
                "runtime_unavailable" => tr(
                    app,
                    "Blocked: the MicroVM is not ready or has changed.",
                    "차단됨: MicroVM이 준비되지 않았거나 바뀌었습니다.",
                ),
                _ => tr(
                    app,
                    "Agent AUTH configuration was not verified.",
                    "에이전트 AUTH 설정을 검증하지 못했습니다.",
                ),
            }
        };
        out.push_str(&format!(
            "<p class=\"code-agent-auth-result\" role=\"status\">{label}</p>"
        ));
    }
    out.push_str("</section>");
    out
}
pub(super) fn action(app: &Shared, action: &str) -> bool {
    if action != "code-agent-auth-verify" || !is_route(&app.borrow().route) {
        return false;
    }
    let mut a = app.borrow_mut();
    let Some(w) = binding::selected(&a) else {
        return true;
    };
    if a.code.agent_auth.checking || !can_check(&a, w) {
        return true;
    }
    let (id, revision, spec) = (w.id.clone(), w.revision, w.spec.clone());
    let Some((vm, instance)) = runtime(&a).map(|(v, i)| (v.to_owned(), i.to_owned())) else {
        return true;
    };
    let Some(p) = profile(&a, w) else { return true };
    let (adapter, version) = (p.id.clone(), p.cli_version.clone());
    let (generation, epoch) = (a.generation, a.code.agent_auth.epoch);
    a.code.agent_auth.checking = true;
    a.code.agent_auth.report = None;
    a.code.agent_auth.error = false;
    drop(a);
    render_content(app);
    let app = app.clone();
    spawn_local(async move {
        let result = api::request_detailed(
            "POST",
            &format!("{WORKSPACES}/{id}/agent-auth/verify"),
            Some(&json!({"expectedRevision":revision})),
            None,
        )
        .await;
        let mut a = app.borrow_mut();
        if a.generation != generation || a.code.agent_auth.epoch != epoch {
            return;
        }
        let current = binding::selected(&a).is_some_and(|w| {
            w.id == id && w.revision == revision && w.spec == spec && can_check(&a, w)
        }) && runtime(&a) == Some((vm.as_str(), instance.as_str()));
        a.code.agent_auth.checking = false;
        match result {
            Ok(r)
                if current
                    && r["id"] == id
                    && r["revision"] == revision
                    && (r["result"]["status"] == "blocked"
                        || (r["result"]["status"] == "configuration_verified"
                            && r["result"]["adapterId"] == adapter
                            && r["result"]["cliVersion"] == version
                            && r["result"]["vmId"] == vm
                            && r["result"]["runtimeId"] == instance)) =>
            {
                a.code.agent_auth.report = Some(r)
            }
            _ => a.code.agent_auth.error = true,
        }
        drop(a);
        render_content(&app);
    });
    true
}
