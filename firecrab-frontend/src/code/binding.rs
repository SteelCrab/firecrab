//! Assign shared account references with the existing optimistic configuration write.
mod view;
use super::*;
use accounts::{Account, compatible};
use serde_json::json;
pub(super) use view::content;
use web_sys::HtmlSelectElement;

pub(super) struct Draft {
    original: Workspace,
    agent: Option<Agent>,
    accounts: Vec<Account>,
    loading: bool,
    busy: bool,
    pending: Option<Spec>,
    error: &'static str,
    epoch: u64,
}
pub(super) fn selected(app: &App) -> Option<&Workspace> {
    if app.route == "code"
        && let Some(id) = &app.code.selection
    {
        return app.code.workspaces.iter().find(|w| w.id == *id);
    }
    super::selected(&app.route, &app.code.workspaces)
}
pub(super) fn observation_route(app: &App) -> String {
    if app.route == "code"
        && let Some(id) = &app.code.selection
    {
        return format!("code/{id}");
    }
    app.route.clone()
}
pub(super) fn busy(app: &App) -> bool {
    app.code.binding.as_ref().is_some_and(|s| s.busy)
}
pub(super) fn locked(app: &App) -> bool {
    app.code
        .binding
        .as_ref()
        .is_some_and(|s| s.busy || s.pending.is_some())
}
fn unchanged(s: &Draft, w: &Workspace) -> bool {
    w.id == s.original.id && w.revision == s.original.revision && w.spec == s.original.spec
}
fn eligible(s: &Draft) -> bool {
    s.agent.as_ref().is_none_or(|b| {
        !s.loading
            && s.error != "accounts_unavailable"
            && s.accounts
                .iter()
                .any(|a| a.id == b.connection_id && compatible(&b.agent, a))
    })
}
pub(super) fn reload_accounts(app: &Shared) {
    let mut a = app.borrow_mut();
    let generation = a.generation;
    if locked(&a) || a.code.binding.is_none() {
        return;
    }
    a.code.binding_epoch += 1;
    let epoch = a.code.binding_epoch;
    let Some(s) = a.code.binding.as_mut() else {
        return;
    };
    s.loading = true;
    s.epoch = epoch;
    drop(a);
    render_content(app);
    let app = app.clone();
    spawn_local(async move {
        let result = api::request("GET", "/api/pipe/ai/connections", None)
            .await
            .ok()
            .and_then(accounts::parse);
        let mut a = app.borrow_mut();
        if a.generation != generation {
            return;
        }
        let Some(s) = a.code.binding.as_mut() else {
            return;
        };
        if s.epoch != epoch {
            return;
        }
        s.loading = false;
        s.accounts.clear();
        match result {
            Some(accounts) => {
                s.accounts = accounts;
                if s.error == "accounts_unavailable" {
                    s.error = "";
                }
            }
            None => s.error = "accounts_unavailable",
        }
        drop(a);
        render_content(&app);
    });
}
pub(super) fn action(app: &Shared, action: &str) -> bool {
    if !action.starts_with("code-binding-") || !is_route(&app.borrow().route) {
        return false;
    }
    if auth::is_open(&app.borrow()) || locked(&app.borrow()) && action != "code-binding-save" {
        return true;
    }
    match action {
        "code-binding-edit" => {
            let mut a = app.borrow_mut();
            if a.code.binding.is_some() {
                return true;
            }
            let Some(w) = selected(&a).cloned() else {
                return true;
            };
            a.code.selection = Some(w.id.clone());
            a.code.binding = Some(Draft {
                agent: w.spec.agent.clone(),
                original: w,
                accounts: vec![],
                loading: true,
                busy: false,
                pending: None,
                error: "",
                epoch: 0,
            });
            a.dirty = true;
            auth::context_changed(&mut a);
            drop(a);
            reload_accounts(app);
        }
        "code-binding-cancel" => {
            let mut a = app.borrow_mut();
            a.code.binding = None;
            a.dirty = false;
            drop(a);
            render_content(app);
        }
        "code-binding-reload" => reload_accounts(app),
        "code-binding-save" => save(app),
        _ => return false,
    }
    true
}
pub(super) fn change(app: &Shared, node: &Element) -> bool {
    let node_id = node.id();
    let Some(key) = node_id.strip_prefix("code-binding-") else {
        return false;
    };
    let Some(select) = node.dyn_ref::<HtmlSelectElement>() else {
        return false;
    };
    let value = select.value();
    let mut a = app.borrow_mut();
    let Some(s) = a.code.binding.as_mut() else {
        return false;
    };
    if s.busy || s.pending.is_some() {
        return true;
    }
    match key {
        "agent" if value.is_empty() => s.agent = None,
        "agent" if ["codex", "claude_code", "opencode"].contains(&value.as_str()) => {
            let id = s
                .agent
                .as_ref()
                .filter(|b| {
                    s.accounts
                        .iter()
                        .any(|c| c.id == b.connection_id && compatible(&value, c))
                })
                .map(|b| b.connection_id.clone())
                .unwrap_or_default();
            s.agent = Some(Agent {
                agent: value,
                connection_id: id,
            });
        }
        "account" => {
            if let Some(b) = &mut s.agent {
                b.connection_id = value;
            }
        }
        _ => return false,
    }
    if s.error != "accounts_unavailable" {
        s.error = "";
    }
    a.dirty = true;
    let generation = a.generation;
    drop(a);
    // Finish the native select event before replacing its focused element.
    let app = app.clone();
    spawn_local(async move {
        if app.borrow().generation == generation {
            render_content(&app);
        }
    });
    true
}
fn decode(value: Value, id: &str) -> Option<Workspace> {
    let w: Workspace = serde_json::from_value(value).ok()?;
    (w.id == id && w.revision > 0).then_some(w)
}
fn applied(w: &Workspace, original: &Workspace, desired: &Spec) -> bool {
    w.id == original.id
        && original.revision.checked_add(1) == Some(w.revision)
        && w.spec == *desired
}
async fn persist(
    original: &Workspace,
    desired: &Spec,
    recover: bool,
) -> Result<Workspace, &'static str> {
    let path = format!("{WORKSPACES}/{}", original.id);
    if recover {
        let current = api::request("GET", &path, None)
            .await
            .ok()
            .and_then(|v| decode(v, &original.id))
            .ok_or("unknown")?;
        if applied(&current, original, desired) {
            return Ok(current);
        }
        if current.revision != original.revision || current.spec != original.spec {
            return Err("conflict");
        }
    }
    let result = api::request_detailed(
        "PUT",
        &path,
        Some(&json!({"expectedRevision":original.revision,"spec":desired})),
        None,
    )
    .await;
    match result {
        Ok(value) => decode(value, &original.id)
            .filter(|w| applied(w, original, desired))
            .ok_or("unknown"),
        Err(e) if e.code.as_deref() == Some("code_runtime_bound") => Err("runtime_bound"),
        Err(e) if matches!(e.status, Some(400 | 401 | 403)) => Err("rejected"),
        Err(_) => {
            let current = api::request("GET", &path, None)
                .await
                .ok()
                .and_then(|v| decode(v, &original.id))
                .ok_or("unknown")?;
            if applied(&current, original, desired) {
                Ok(current)
            } else if current.revision != original.revision || current.spec != original.spec {
                Err("conflict")
            } else {
                Err("unknown")
            }
        }
    }
}
fn save(app: &Shared) {
    let mut a = app.borrow_mut();
    let generation = a.generation;
    let Some(w) = selected(&a).cloned() else {
        return;
    };
    let Some(s) = a.code.binding.as_mut() else {
        return;
    };
    if s.busy {
        return;
    }
    let recover = s.pending.is_some();
    if !recover && (!unchanged(s, &w) || !eligible(s) || s.original.spec.agent == s.agent) {
        return;
    }
    let desired = s.pending.clone().unwrap_or_else(|| Spec {
        agent: s.agent.clone(),
        ..s.original.spec.clone()
    });
    s.pending = Some(desired.clone());
    s.busy = true;
    s.error = "";
    let original = s.original.clone();
    let epoch = s.epoch;
    drop(a);
    render_content(app);
    let app = app.clone();
    spawn_local(async move {
        let result = persist(&original, &desired, recover).await;
        let mut a = app.borrow_mut();
        if a.generation != generation {
            return;
        }
        let Some(s) = a.code.binding.as_mut() else {
            return;
        };
        if s.epoch != epoch || s.original.id != original.id {
            return;
        }
        match result {
            Ok(workspace) => {
                if let Some(w) = a.code.workspaces.iter_mut().find(|w| w.id == workspace.id) {
                    *w = workspace;
                }
                a.code.binding = None;
                a.dirty = false;
                invalidate_observations(&mut a);
                auth::context_changed(&mut a);
            }
            Err(error) => {
                s.busy = false;
                s.error = error;
                if error != "unknown" {
                    s.pending = None;
                }
            }
        }
        drop(a);
        super::load(app.clone());
        render_content(&app);
    });
}
