use super::*;

enum Inspection {
    Accepted(Runtime),
    Unchanged,
    Conflict,
}
fn matches(p: &Pending, r: &Runtime) -> bool {
    r.configuration_revision == p.revision
        && r.action == p.action.expected()
        && p.before
            .as_ref()
            .is_none_or(|b| b.vm == r.vm_id && b.operation != r.operation_id)
        && p.placement.as_ref().is_none_or(|v| {
            v.template == r.placement.template.name
                && v.micro_network_id == r.placement.micro_network_id
                && v.storage_root == r.placement.storage_root
        })
}
async fn inspect(p: &Pending) -> Result<Inspection, &'static str> {
    let w = api::request_detailed("GET", &format!("{WORKSPACES}/{}", p.id), None, None)
        .await
        .ok()
        .and_then(|v| serde_json::from_value::<Workspace>(v).ok())
        .ok_or("unknown")?;
    if w.id != p.id || w.revision != p.revision || w.spec != p.spec {
        return Ok(Inspection::Conflict);
    }
    match super::super::runtime(&p.id).await {
        RuntimeState::Absent if p.before.is_none() => Ok(Inspection::Unchanged),
        RuntimeState::Absent => Ok(Inspection::Conflict),
        RuntimeState::Current(r) => {
            if p.before
                .as_ref()
                .is_some_and(|b| b.vm == r.vm_id && b.operation == r.operation_id)
            {
                Ok(Inspection::Unchanged)
            } else if matches(p, &r) {
                Ok(Inspection::Accepted(r))
            } else {
                Ok(Inspection::Conflict)
            }
        }
        _ => Err("unknown"),
    }
}
async fn execute(p: &Pending, send: bool) -> Result<Option<Runtime>, &'static str> {
    match inspect(p).await? {
        Inspection::Accepted(r) => return Ok(Some(r)),
        Inspection::Conflict => return Err("conflict"),
        Inspection::Unchanged if !send => return Ok(None),
        Inspection::Unchanged => {}
    }
    let body = if let Some(v) = &p.placement {
        json!({"expectedRevision":p.revision,"template":v.template,"microNetworkId":v.micro_network_id,"storageRoot":v.storage_root})
    } else {
        json!({"expectedRevision":p.revision})
    };
    let result = api::request_detailed(
        "POST",
        &format!("{WORKSPACES}/{}/runtime{}", p.id, p.action.suffix()),
        Some(&body),
        None,
    )
    .await;
    match result {
        Ok(value) => decode_runtime(&p.id, value)
            .ok()
            .filter(|r| matches(p, r))
            .map(Some)
            .ok_or("unknown"),
        Err(e) if e.code.as_deref() == Some("code_workspace_revision_conflict") => Err("conflict"),
        Err(e) if e.code.as_deref() == Some("code_runtime_busy") => Err("busy"),
        Err(e) if e.code.as_deref() == Some("code_placement_bound") => Err("placement_bound"),
        Err(e) if matches!(e.status, Some(400 | 401 | 403 | 404 | 422)) => Err("rejected"),
        Err(_) => match inspect(p).await {
            Ok(Inspection::Accepted(r)) => Ok(Some(r)),
            Ok(Inspection::Conflict) => Err("conflict"),
            _ => Err("unknown"),
        },
    }
}
pub(super) fn begin(app: &Shared, action: Action) {
    let mut a = app.borrow_mut();
    let Some(w) = binding::selected(&a) else {
        return;
    };
    if !available(&a, w, &action) {
        return;
    }
    let p = Pending {
        id: w.id.clone(),
        revision: w.revision,
        spec: w.spec.clone(),
        placement: if action == Action::Create {
            placement(&a, w)
        } else {
            None
        },
        action,
        before: match &a.code.runtime {
            RuntimeState::Current(r) => Some(Before {
                vm: r.vm_id.clone(),
                operation: r.operation_id.clone(),
            }),
            _ => None,
        },
    };
    a.code.selection = Some(p.id.clone());
    a.code.lifecycle.request = Some(p.clone());
    a.code.lifecycle.notice = false;
    a.code.lifecycle.error = "";
    a.code.lifecycle.retryable = false;
    remember(&p.id, Some(&p));
    drop(a);
    run(app, p, true);
}
pub(super) fn recover(app: &Shared, send: bool) {
    let a = app.borrow();
    let Some(p) = a.code.lifecycle.request.clone() else {
        return;
    };
    if busy(&a) || send && !a.code.lifecycle.retryable {
        return;
    }
    drop(a);
    run(app, p, send);
}
fn run(app: &Shared, p: Pending, send: bool) {
    let mut a = app.borrow_mut();
    a.code.lifecycle.busy = true;
    a.code.lifecycle.error = "";
    a.code.lifecycle.retryable = false;
    invalidate_observations(&mut a);
    auth::context_changed(&mut a);
    let (generation, epoch) = (a.generation, a.code.lifecycle.epoch);
    drop(a);
    render_content(app);
    let app = app.clone();
    spawn_local(async move {
        let result = execute(&p, send).await;
        let mut a = app.borrow_mut();
        if a.generation != generation || a.code.lifecycle.epoch != epoch {
            return;
        }
        a.code.lifecycle.busy = false;
        match result {
            Ok(Some(r)) => {
                a.code.runtime = RuntimeState::Current(r);
                a.code.lifecycle.request = None;
                a.code.lifecycle.notice = true;
                a.code.lifecycle.error = "";
                remember(&p.id, None);
            }
            Ok(None) => {
                a.code.lifecycle.error = "unchanged";
                a.code.lifecycle.retryable = true;
            }
            Err(reason) => {
                a.code.lifecycle.error = reason;
                if matches!(reason, "rejected" | "busy" | "placement_bound") {
                    a.code.lifecycle.request = None;
                    remember(&p.id, None);
                }
            }
        }
        invalidate_observations(&mut a);
        drop(a);
        super::super::load(app.clone());
        render_content(&app);
    });
}
