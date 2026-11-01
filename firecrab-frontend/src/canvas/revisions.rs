//! Version comparison and explicit, idempotent file restoration.
use super::*;
mod view;
pub(super) use view::card;
#[derive(Default)]
pub(super) struct State {
    pub(super) comparison: Option<Value>,
    busy: bool,
    pending: Option<Pending>,
    operations: Vec<Value>,
    message: Option<Notice>,
    catalog_generation: u64,
    catalog_ready: bool,
}
enum Notice {
    Restored,
    Recovered,
}
#[derive(Clone)]
struct Pending {
    path: String,
    body: Value,
}
impl Pending {
    fn from_comparison(diff: &Value, operation: &str) -> Option<Self> {
        let path = text(&diff["before"]["version"], "path");
        let selected = diff["before"]["version"]["id"].as_u64()?;
        let current = diff["after"]["version"]["id"].as_u64()?;
        if path.is_empty()
            || path != text(&diff["after"]["version"], "path")
            || selected == 0
            || current == 0
            || operation.is_empty()
        {
            return None;
        }
        Some(Self {
            path: path.into(),
            body: json!({"operationId":operation,"expectedVersion":current,"version":selected}),
        })
    }
}
fn selected<'a>(s: &'a mut crate::canvas::State, identity: &Identity) -> Option<&'a mut Tab> {
    s.tabs
        .iter_mut()
        .find(|t| t.identity.id == identity.id && t.identity.project == identity.project)
}
fn begin(app: &Shared) -> Option<Identity> {
    let mut a = app.borrow_mut();
    let t = tab_mut(&mut a.canvas)?;
    if t.revisions.busy {
        return None;
    }
    t.revisions.busy = true;
    t.error.clear();
    Some(t.identity.clone())
}
pub(super) fn compare(app: Shared, version: &str) {
    let Some(identity) = begin(&app) else {
        return;
    };
    let item = {
        let a = app.borrow();
        tab(&a.canvas).and_then(|t| {
            t.history
                .iter()
                .find(|r| r["id"].as_u64().map(|v| v.to_string()).as_deref() == Some(version))
                .cloned()
        })
    };
    let Some(item) = item else {
        if let Some(t) = selected(&mut app.borrow_mut().canvas, &identity) {
            t.revisions.busy = false;
        }
        return;
    };
    render(&app);
    spawn_local(async move {
        let path = api::encode(text(&item, "path"));
        let result = async {
            let current = workspace_request(
                "GET",
                &format!("{}/files?path={path}", base(&identity.project)),
                None,
            )
            .await?;
            let current = current["version"]["id"]
                .as_u64()
                .ok_or("Invalid current file version")?;
            workspace_request(
                "GET",
                &format!(
                    "{}/files/diff?path={path}&from={}&to={current}",
                    base(&identity.project),
                    item["id"]
                ),
                None,
            )
            .await
        }
        .await;
        if let Some(t) = selected(&mut app.borrow_mut().canvas, &identity) {
            t.revisions.busy = false;
            match result {
                Ok(diff) => {
                    t.revisions.comparison = Some(diff);
                    t.revisions.message = None;
                }
                Err(e) => t.error = e,
            }
        }
        render(&app);
    });
}
pub(super) fn restore(app: Shared) {
    {
        let mut a = app.borrow_mut();
        let Some(t) = tab_mut(&mut a.canvas) else {
            return;
        };
        if t.revisions.busy
            || !t.revisions.catalog_ready
            || t.revisions.pending.is_some()
            || t.revisions
                .operations
                .iter()
                .any(|op| text(op, "status") == "pending")
        {
            return;
        }
        let Some(pending) = t
            .revisions
            .comparison
            .as_ref()
            .and_then(|d| Pending::from_comparison(d, &uuid()))
        else {
            return;
        };
        t.revisions.pending = Some(pending);
    }
    retry(app);
}
pub(super) fn retry(app: Shared) {
    let Some(identity) = begin(&app) else {
        return;
    };
    let pending = tab(&app.borrow().canvas).and_then(|t| t.revisions.pending.clone());
    let Some(pending) = pending else {
        if let Some(t) = selected(&mut app.borrow_mut().canvas, &identity) {
            t.revisions.busy = false;
        }
        return;
    };
    render(&app);
    spawn_local(async move {
        // An uncertain result retains the exact operation ID and saved base; never rebase or issue a replacement write.
        let result = workspace_request(
            "POST",
            &format!(
                "{}/files/restore?path={}",
                base(&identity.project),
                api::encode(&pending.path)
            ),
            Some(&pending.body),
        )
        .await;
        if let Some(t) = selected(&mut app.borrow_mut().canvas, &identity) {
            t.revisions.busy = false;
            match result {
                Ok(_) => {
                    t.revisions.pending = None;
                    t.revisions.comparison = None;
                    t.revisions.message = Some(Notice::Restored);
                }
                Err(e) => t.error = e,
            }
        }
        history_for(app.clone(), identity, None);
        render(&app);
    });
}
pub(super) fn reconcile(app: Shared, operation: &str) {
    let Some(identity) = begin(&app) else {
        return;
    };
    let operation = operation.to_string();
    render(&app);
    spawn_local(async move {
        let result = workspace_request(
            "POST",
            &format!(
                "{}/file-operations/{}/reconcile",
                base(&identity.project),
                api::encode(&operation)
            ),
            Some(&json!({})),
        )
        .await;
        if let Some(t) = selected(&mut app.borrow_mut().canvas, &identity) {
            t.revisions.busy = false;
            match result {
                Ok(_) => {
                    if t.revisions
                        .pending
                        .as_ref()
                        .is_some_and(|p| text(&p.body, "operationId") == operation)
                    {
                        t.revisions.pending = None;
                    }
                    t.revisions.comparison = None;
                    t.revisions.message = Some(Notice::Recovered);
                }
                Err(e) => t.error = e,
            }
        }
        history_for(app.clone(), identity, None);
        render(&app);
    });
}
pub(super) fn catalog(app: Shared, identity: Identity) {
    let generation = {
        let mut a = app.borrow_mut();
        let Some(t) = selected(&mut a.canvas, &identity) else {
            return;
        };
        t.revisions.catalog_generation += 1;
        t.revisions.catalog_ready = false;
        t.revisions.catalog_generation
    };
    spawn_local(async move {
        let result = workspace_request(
            "GET",
            &format!("{}/file-operations", base(&identity.project)),
            None,
        )
        .await;
        if let Some(t) = selected(&mut app.borrow_mut().canvas, &identity) {
            if t.revisions.catalog_generation != generation {
                return;
            }
            match result {
                Ok(rows) => {
                    t.revisions.catalog_ready = true;
                    t.revisions.operations = list(&rows).to_vec();
                    if let Some(pending) = &t.revisions.pending {
                        let known = t
                            .revisions
                            .operations
                            .iter()
                            .find(|op| text(op, "id") == text(&pending.body, "operationId"));
                        if known
                            .is_some_and(|op| matches!(text(op, "status"), "applied" | "failed"))
                        {
                            t.revisions.pending = None;
                            t.revisions.comparison = None;
                        }
                    }
                }
                Err(e) => t.error = e,
            }
        }
        render(&app);
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restore_keeps_selected_version_and_observed_base_in_one_immutable_request() {
        let diff = json!({"before":{"version":{"id":1,"path":"api/auth.rs"}},"after":{"version":{"id":7,"path":"api/auth.rs"}}});
        let original = Pending::from_comparison(&diff, "restore-identity").unwrap();
        let retry = original.clone();
        assert_eq!(
            retry.body,
            json!({"operationId":"restore-identity","expectedVersion":7,"version":1})
        );
        assert_eq!(retry.path, "api/auth.rs");
        let mut mixed = diff.clone();
        mixed["after"]["version"]["path"] = json!("other.rs");
        assert!(Pending::from_comparison(&mixed, "restore-identity").is_none());
    }
}
