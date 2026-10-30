//! Read-only Git source status and recovery guidance for registered stacks.
use crate::{
    App, Shared, api,
    model::{escape, list, text},
    render::{Icon, icon_button},
};
use serde_json::{Value, json};
use wasm_bindgen_futures::spawn_local;

fn tr<'a>(app: &App, en: &'a str, ko: &'a str) -> &'a str {
    if app.locale == "ko" { ko } else { en }
}

pub fn refresh(app: Shared) {
    let mut state = app.borrow_mut();
    if state.refreshing || state.route != "stacks" {
        return;
    }
    state.refreshing = true;
    let generation = state.generation;
    drop(state);
    spawn_local(async move {
        let result = api::request("GET", "/api/stacks", None).await;
        let result = match result {
            Ok(stacks) if stacks.is_array() => {
                let mut rows = Vec::new();
                for stack in list(&stacks) {
                    let name = text(stack, "name");
                    let path = format!("/api/stacks/{}/source", api::encode(name));
                    let source = api::request_detailed("GET", &path, None, None).await;
                    rows.push(match source {
                        Ok(value) => json!({"stack":stack,"source":value}),
                        Err(error) if error.status == Some(404) => {
                            json!({"stack":stack,"source":null})
                        }
                        Err(error) => json!({"stack":stack,"error":error.to_string()}),
                    });
                }
                let ownership = api::request_detailed("GET", "/api/iac/owners", None, None).await;
                let (owners, error) = match ownership {
                    Ok(owners) if owners.is_array() => (owners, Value::Null),
                    Err(error) if error.status == Some(404) => (Value::Array(vec![]), Value::Null),
                    Err(error) => (Value::Null, Value::String(error.to_string())),
                    _ => (
                        Value::Null,
                        Value::String("Invalid ownership response".into()),
                    ),
                };
                Ok(json!({"stacks":rows,"owners":owners,"ownershipError":error}))
            }
            Ok(_) => Err("Invalid stack response".to_owned()),
            Err(error) => Err(error),
        };
        let mut state = app.borrow_mut();
        if state.generation != generation || state.route != "stacks" {
            return;
        }
        state.refreshing = false;
        let was_loaded = state.loaded;
        let changed = match result {
            Ok(value) => {
                let changed = state.resource != value || !state.connection_error.is_empty();
                state.resource = value;
                state.connection_error.clear();
                changed
            }
            Err(error) => {
                let changed = state.connection_error != error;
                if !was_loaded {
                    state.resource = Value::Array(Vec::new());
                }
                state.connection_error = error;
                changed
            }
        } || !was_loaded;
        state.loaded = true;
        drop(state);
        if changed {
            crate::render_content(&app);
        } else {
            crate::render_messages(&app);
        }
    });
}

fn value<'a>(value: &'a Value, key: &str) -> &'a str {
    let item = text(value, key);
    if item.is_empty() { "—" } else { item }
}

fn label<'a>(app: &App, state: &'a str) -> &'a str {
    match state {
        "checking" => tr(app, "Checking", "확인 중"),
        "applying" => tr(app, "Applying", "적용 중"),
        "in_sync" => tr(app, "In sync", "동기화됨"),
        "drifted" => tr(app, "Drift detected", "설정 차이"),
        "applied" => tr(app, "Applied", "적용됨"),
        "blocked" => tr(app, "Blocked", "차단됨"),
        "failed" => tr(app, "Failed", "실패"),
        "interrupted" => tr(app, "Interrupted", "중단됨"),
        "recovery_required" => tr(app, "Recovery required", "복구 필요"),
        _ => tr(app, "No observation", "확인 기록 없음"),
    }
}

fn error_guidance<'a>(app: &App, error: &str) -> &'a str {
    match error {
        "credential_unavailable" | "source_access_denied_or_rate_limited" => tr(
            app,
            "Check the GitHub connection, repository access and rate limit, then check again.",
            "GitHub 연결, 저장소 권한과 요청 제한을 확인한 뒤 다시 확인한다.",
        ),
        "source_not_found" | "manifest_path_not_found" | "manifest_path_not_regular" => tr(
            app,
            "Check the repository, ref and manifest path, then check again.",
            "저장소, 참조와 매니페스트 경로를 확인한 뒤 다시 확인한다.",
        ),
        "invalid_manifest" | "manifest_stack_name_mismatch" => tr(
            app,
            "Validate the manifest and stack name in Git, then check again.",
            "Git의 매니페스트와 Stack 이름을 검증한 뒤 다시 확인한다.",
        ),
        _ => tr(
            app,
            "Inspect the source status and resolve the error before retrying.",
            "Git 상태를 확인하고 오류를 해결한 뒤 다시 시도한다.",
        ),
    }
}

pub fn content(app: &App) -> String {
    let mut out = format!(
        "<div class=\"wasm-actions\">{}</div>",
        icon_button(
            tr(app, "Refresh", "새로고침"),
            "stack-refresh",
            "",
            Icon::Refresh
        )
    );
    let rows = list(if app.resource.is_array() {
        &app.resource
    } else {
        &app.resource["stacks"]
    });
    let owners: Vec<_> = list(&app.resource["owners"])
        .iter()
        .filter(|record| text(&record["owner"], "manager") == "terraform")
        .collect();
    if rows.is_empty() {
        if app.connection_error.is_empty() {
            out.push_str(&format!(
                "<p class=\"{}\">{}</p>",
                if owners.is_empty() {
                    "empty-state"
                } else {
                    "stack-guidance"
                },
                tr(app, "No registered stacks.", "등록된 Stack이 없다.")
            ));
        }
        if list(&app.resource["owners"]).is_empty() && app.resource["ownershipError"].is_null() {
            return out;
        }
    }
    out.push_str("<div class=\"stack-list\">");
    for row in rows {
        let name = text(&row["stack"], "name");
        let source = &row["source"];
        out.push_str(&format!(
            "<section class=\"wasm-card stack-card\"><h2>{}</h2>",
            escape(name)
        ));
        if let Some(error) = row["error"].as_str() {
            out.push_str(&format!(
                "<p role=\"alert\">{}: {}</p>",
                tr(app, "Source unavailable", "Git 상태를 불러오지 못했다"),
                escape(error)
            ));
        } else if source.is_null() {
            out.push_str(&format!(
                "<p>{}</p>",
                tr(
                    app,
                    "Git source is not configured.",
                    "Git 소스가 설정되지 않았다."
                )
            ));
        } else {
            let config = &source["source"];
            let latest = &source["latest"];
            let state = text(latest, "state");
            let mode = if config["paused"] == true {
                tr(app, "Paused", "일시 중지")
            } else if text(config, "syncMode") == "automatic" {
                tr(app, "Automatic", "자동")
            } else {
                tr(app, "Detect only", "감지만")
            };
            out.push_str(&format!("<dl class=\"stack-status\"><dt>{}</dt><dd>{}</dd><dt>{}</dt><dd>{}</dd><dt>{}</dt><dd class=\"mono\">{}</dd><dt>{}</dt><dd class=\"mono\">{}</dd><dt>{}</dt><dd class=\"mono\">{}</dd><dt>{}</dt><dd class=\"mono\">{}</dd><dt>{}</dt><dd class=\"mono\">{}</dd></dl>",
                tr(app,"Status","상태"), escape(label(app,state)), tr(app,"Mode","방식"), escape(mode),
                tr(app,"Repository","저장소"), escape(value(config,"repository")), tr(app,"Ref","참조"),escape(value(config,"ref")),
                tr(app,"Desired commit","요청 커밋"),escape(value(&source["desiredRevision"],"commit")),
                tr(app,"Attempted commit","시도 커밋"),escape(value(&source["attemptedRevision"],"commit")),
                tr(app,"Applied commit","적용 커밋"),escape(value(&source["successfulRevision"],"commit"))));
            if let Some(error) = latest["error"].as_str() {
                out.push_str(&format!(
                    "<p role=\"alert\">{}: <code>{}</code></p><p>{}</p>",
                    tr(app, "Error", "오류"),
                    escape(error),
                    escape(error_guidance(app, error))
                ));
            }
            let operation = text(source, "recoveryOperationId");
            if !operation.is_empty() {
                let generation = source["generation"].as_u64().unwrap_or(0);
                out.push_str(&format!("<section class=\"stack-guidance\"><h3>{}</h3><p>{}</p><p>{}: <code>{}</code></p><code>firecrab stack source sync {} --generation {} --recover-operation-id {}</code></section>",
                    tr(app,"Recovery","복구"),tr(app,"Inspect the operation and repair its resources before acknowledging it. Recovery makes a new plan; it does not replay the interrupted operation.","작업과 리소스를 확인하고 수정한 뒤 복구한다. 복구는 새 계획을 만들며 중단된 작업을 다시 실행하지 않는다."),
                    tr(app,"Operation","작업"),escape(operation),escape(name),generation,escape(operation)));
                let apply = &source["lastApply"];
                if !apply.is_null() {
                    out.push_str(&format!(
                        "<p>{}: {}</p><ul>",
                        tr(app, "Operation status", "작업 상태"),
                        escape(value(apply, "state"))
                    ));
                    for step in list(&apply["changes"]) {
                        out.push_str(&format!(
                            "<li>{} · {} · {} · {}</li>",
                            escape(value(step, "kind")),
                            escape(value(step, "key")),
                            escape(value(step, "resourceId")),
                            escape(value(step, "state"))
                        ));
                    }
                    out.push_str("</ul>");
                }
            }
            out.push_str(&format!("<section class=\"stack-guidance\"><h3>{}</h3><p>{}</p><p>{}</p></section>",tr(app,"Revert","되돌리기"),tr(app,"Revert the manifest in Git and review the resulting commit. Guest disks and application data are not restored.","Git에서 매니페스트를 되돌리고 새 커밋을 확인한다. 게스트 디스크와 앱 데이터는 복원되지 않는다."),tr(app,"Automatic mode applies the commit on the next poll when active. In detect-only or paused mode, run a manual sync.","자동 방식이 활성 상태면 다음 확인 때 커밋을 적용한다. 감지만 사용하거나 일시 중지한 경우 수동 동기화가 필요하다.")));
        }
        out.push_str("</section>");
    }
    out.push_str("</div>");
    if let Some(error) = app.resource["ownershipError"].as_str() {
        out.push_str(&format!(
            "<p role=\"alert\">{}: {}</p>",
            tr(
                app,
                "Ownership unavailable",
                "소유권 정보를 불러오지 못했다"
            ),
            escape(error)
        ));
    }
    if !owners.is_empty() {
        out.push_str(&format!(
            "<section class=\"wasm-card stack-card\"><h2>{}</h2>",
            tr(app, "Terraform ownership", "Terraform 소유권")
        ));
        for record in owners {
            out.push_str(&format!("<dl class=\"stack-status\"><dt>{}</dt><dd>{}</dd><dt>{}</dt><dd class=\"mono\">{}</dd><dt>{}</dt><dd class=\"mono\">{}</dd><dt>{}</dt><dd class=\"mono\">{}</dd><dt>{}</dt><dd>{}</dd></dl>",
                tr(app,"Resource","리소스"),escape(text(&record["resource"],"kind")),
                tr(app,"Resource ID","리소스 ID"),escape(text(&record["resource"],"id")),
                tr(app,"Terraform address","Terraform 주소"),escape(text(&record["owner"],"key")),
                tr(app,"State owner ID","상태 소유자 ID"),escape(text(&record["owner"],"id")),
                tr(app,"Ownership generation","소유권 세대"),record["generation"].as_u64().unwrap_or(0)));
        }
        out.push_str("</section>");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renders_status_and_recovery_without_unescaped_values() {
        let app = App {
            locale: "en".into(),
            resource: json!([{"stack":{"name":"demo"},"source":{"generation":3,"source":{"repository":"https://github.com/a/<repo>","ref":"heads/main","syncMode":"automatic","paused":false},"latest":{"state":"recovery_required","error":"bad <input>"},"desiredRevision":{"commit":"desired"},"successfulRevision":{"commit":"applied"},"recoveryOperationId":"op-id"}}]),
            ..App::default()
        };
        let html = content(&app);
        assert!(html.contains("Recovery required"));
        assert!(html.contains("--generation 3 --recover-operation-id op-id"));
        assert!(html.contains("&lt;repo&gt;"));
        assert!(html.contains("bad &lt;input&gt;"));
        assert!(html.contains("Guest disks and application data are not restored"));
    }
    #[test]
    fn renders_terraform_ownership_without_registered_native_stacks() {
        let app = App {
            locale: "en".into(),
            resource: json!({"stacks":[],"owners":[{
            "resource":{"kind":"network","id":"owned-id"},"generation":7,
            "owner":{"manager":"terraform","id":"owner-id","key":"firecrab_network.<app>"}
        }],"ownershipError":null}),
            ..App::default()
        };
        let html = content(&app);
        assert!(html.contains("Terraform ownership"));
        assert!(html.contains("firecrab_network.&lt;app&gt;"));
        assert!(html.contains("owned-id"));
        assert!(html.contains("Ownership generation"));
    }
    #[test]
    fn reports_an_ownership_failure_without_claiming_no_managed_resources() {
        let app = App {
            locale: "en".into(),
            resource: json!({"stacks":[],"owners":null,"ownershipError":"HTTP 500: <unavailable>"}),
            ..App::default()
        };
        let html = content(&app);
        assert!(html.contains("Ownership unavailable: HTTP 500: &lt;unavailable&gt;"));
        assert!(!html.contains("Terraform ownership"));
    }
    #[test]
    fn distinguishes_missing_source_from_unavailable_source() {
        let app = App {
            locale: "en".into(),
            resource: json!([{"stack":{"name":"one"},"source":null},{"stack":{"name":"two"},"error":"HTTP 500"}]),
            ..App::default()
        };
        let html = content(&app);
        assert!(html.contains("Git source is not configured."));
        assert!(html.contains("Source unavailable: HTTP 500"));
    }
}
