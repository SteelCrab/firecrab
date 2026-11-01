use super::*;
pub(super) fn action(app: &Shared, action: &str) -> bool {
    if ![
        "code-agent-run-start",
        "code-agent-run-retry",
        "code-agent-run-cancel",
    ]
    .contains(&action)
    {
        return false;
    }
    let mut a = app.borrow_mut();
    let Some(w) = binding::selected(&a).cloned() else {
        return true;
    };
    if a.code.agent_run.busy
        || a.code.agent_run.workspace != w.id
        || a.code.agent_run.generation != a.generation
    {
        return true;
    }
    let cancel = action == "code-agent-run-cancel";
    let pending = if cancel {
        a.code.agent_run.pending.clone().or_else(|| {
            a.code
                .agent_run
                .rows
                .iter()
                .find(|r| r.active())
                .map(|r| Pending {
                    id: r.id.clone(),
                    workspace: w.id.clone(),
                    request: Value::Null,
                })
        })
    } else if action == "code-agent-run-retry" {
        a.code.agent_run.pending.clone()
    } else {
        if !can_start(&a, &w) {
            return true;
        }
        let Some(r) = runtime(&a) else { return true };
        let Ok(crypto) = crate::window().crypto() else {
            return true;
        };
        Some(Pending {
            id: {
                let mut bytes = [0u8; 16];
                if crypto.get_random_values_with_u8_array(&mut bytes).is_err() {
                    return true;
                }
                bytes[6] = (bytes[6] & 15) | 64;
                bytes[8] = (bytes[8] & 63) | 128;
                let hex = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
                format!(
                    "{}-{}-{}-{}-{}",
                    &hex[..8],
                    &hex[8..12],
                    &hex[12..16],
                    &hex[16..20],
                    &hex[20..]
                )
            },
            workspace: w.id.clone(),
            request: json!({"expectedRevision":w.revision,"vmId":r.vm_id,"runtimeId":r.runtime_id,"operationId":r.operation_id,"model":a.code.agent_run.model,"prompt":a.code.agent_run.prompt}),
        })
    };
    let Some(p) = pending else { return true };
    if !cancel && !remember(&w.id, Some(&p)) {
        a.code.agent_run.error = true;
        drop(a);
        render_content(app);
        return true;
    }
    let s = &mut a.code.agent_run;
    s.epoch += 1;
    s.loading = false;
    s.busy = true;
    s.error = false;
    if !cancel {
        s.pending = Some(p.clone());
    }
    let (generation, epoch) = (a.generation, a.code.agent_run.epoch);
    drop(a);
    render_content(app);
    let app = app.clone();
    spawn_local(async move {
        let suffix = if cancel { "/cancel" } else { "" };
        let result = api::request(
            "POST",
            &format!("{WORKSPACES}/{}/agent-runs/{}{suffix}", w.id, p.id),
            if cancel { None } else { Some(&p.request) },
        )
        .await
        .ok();
        let mut a = app.borrow_mut();
        if a.generation != generation
            || a.code.agent_run.epoch != epoch
            || a.code.agent_run.workspace != w.id
            || a.code.agent_run.generation != a.generation
        {
            return;
        }
        let confirmed_cancel = cancel
            && result.as_ref().is_some_and(|v| {
                v["id"] == p.id && v["workspaceId"] == w.id && v.get("run").is_some()
            });
        let row = result
            .and_then(|v| {
                if cancel {
                    v.get("run").cloned()
                } else {
                    Some(v)
                }
            })
            .and_then(|v| serde_json::from_value::<Run>(v).ok())
            .filter(|r| r.id == p.id && r.valid(&w.id));
        let s = &mut a.code.agent_run;
        s.busy = false;
        if row.is_some() || confirmed_cancel {
            remember(&w.id, None);
            s.pending = None;
            s.error = false;
            if let Some(row) = row {
                s.rows.retain(|r| r.id != row.id);
                s.rows.insert(0, row);
                s.rows.truncate(20);
            }
        } else {
            s.error = true;
        }
        s.loaded = false;
        drop(a);
        render_content(&app);
        load(app);
    });
    true
}
