//! Deploy actions share the backend executor and the existing service wizard.

use crate::{
    App, Shared, api,
    forms::Field,
    model::{escape, list, text},
};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{Element, HtmlSelectElement};

#[derive(Default)]
pub struct State {
    service: String,
    open: bool,
    releases: Option<Value>,
    loading: bool,
    error: String,
    selected: String,
    strategy: String,
    pending: Option<(String, String)>,
    update_request: Option<(String, String)>,
    request: u64,
}

fn tr<'a>(en: &'a str, ko: &'a str) -> &'a str {
    if crate::i18n::t("Name") == "이름" {
        ko
    } else {
        en
    }
}

pub fn busy(deployment: &Value) -> bool {
    matches!(
        text(deployment, "phase"),
        "queued"
            | "importing"
            | "starting"
            | "checking_health"
            | "connecting_domain"
            | "switching_traffic"
            | "draining"
            | "rolling_back"
            | "stopping"
    )
}

fn recovering(deployment: &Value) -> bool {
    deployment["update"].is_object() && deployment["update"]["completed"] != true
}

fn action(label: &str, action: &str, disabled: bool) -> String {
    crate::render::button(label, action, "").replace(
        "type=\"button\"",
        &format!(
            "type=\"button\" id=\"{}\"{}",
            escape(action),
            if disabled { " disabled" } else { "" }
        ),
    )
}

pub fn html(app: &App) -> String {
    let Some(deployment) = &app.service_deployment else {
        return String::new();
    };
    if text(&deployment["spec"], "domain").is_empty() {
        return String::new();
    }
    let state = &app.deploy;
    let visible = state.service == text(deployment, "serviceId") && state.open;
    let disabled = app.busy || busy(deployment);
    let can_update =
        !recovering(deployment) && matches!(text(deployment, "phase"), "running" | "degraded");
    let mut out = format!(
        "<section aria-label=\"Deploy\"><h3>Deploy</h3><p>{}</p><div class=\"wasm-actions\">",
        tr(
            "Update this app through the existing Nginx route. The service address stays the same.",
            "기존 Nginx 경로로 앱을 업데이트한다. 서비스 주소는 유지한다."
        )
    );
    if can_update {
        out.push_str(&action(
            tr("Deploy update", "업데이트 배포"),
            "deploy-update",
            disabled,
        ));
    }
    if recovering(deployment) && text(deployment, "phase") == "failed" {
        out.push_str(&action(
            tr("Retry Deploy", "Deploy 재시도"),
            "deploy-retry",
            disabled,
        ));
    }
    out.push_str(
        &action(
            tr("Release history", "릴리스 이력"),
            "deploy-history",
            false,
        )
        .replace(
            "type=\"button\"",
            &format!("type=\"button\" aria-expanded=\"{visible}\""),
        ),
    );
    out.push_str("</div>");
    if recovering(deployment) {
        let strategy = if deployment["update"]["strategy"] == "blue_green" {
            tr("Blue-green", "블루그린")
        } else {
            tr("Rolling", "롤링")
        };
        out.push_str(&format!(
            "<p role=\"status\">Deploy {} · {strategy} · {}: {}</p>",
            escape(text(deployment, "id")),
            tr("Previous instances retained", "보존한 이전 인스턴스"),
            list(&deployment["update"]["previous"]["instances"]).len()
        ));
    }
    if state.service == text(deployment, "serviceId") && !state.error.is_empty() {
        out.push_str(&format!("<p role=\"alert\">{}</p>", escape(&state.error)));
    }
    if visible {
        if state.loading {
            out.push_str("<p role=\"status\">Loading releases…</p>");
        }
        if let Some(releases) = &state.releases {
            if list(releases).is_empty() {
                out.push_str("<p>No successful releases are retained yet.</p>");
            } else {
                out.push_str("<div class=\"table-scroll\"><table class=\"wasm-table\"><thead><tr><th>Release</th><th>Image</th><th>Digest</th></tr></thead><tbody>");
                for release in list(releases) {
                    out.push_str(&format!(
                        "<tr><td>{}{}</td><td>{}</td><td><code>{}</code></td></tr>",
                        escape(text(release, "id")),
                        if release["id"] == deployment["id"] {
                            " · active"
                        } else {
                            ""
                        },
                        escape(text(&release["spec"], "image")),
                        escape(text(release, "digest"))
                    ));
                }
                out.push_str("</tbody></table></div><div class=\"wasm-form\"><div class=\"field\"><label for=\"deploy-release\">Rollback release</label>");
                out.push_str(&format!("<select id=\"deploy-release\"{}><option value=\"\">Select a previous release</option>", if disabled || !can_update { " disabled" } else { "" }));
                for release in list(releases)
                    .iter()
                    .filter(|release| release["id"] != deployment["id"])
                {
                    let id = text(release, "id");
                    out.push_str(&format!(
                        "<option value=\"{}\"{}>{} · {}</option>",
                        escape(id),
                        if state.selected == id {
                            " selected"
                        } else {
                            ""
                        },
                        escape(id),
                        escape(text(&release["spec"], "image"))
                    ));
                }
                out.push_str(&format!("</select></div><div class=\"field\"><label for=\"deploy-strategy\">Strategy</label><select id=\"deploy-strategy\"{}><option value=\"rolling\">Rolling — one extra VM</option><option value=\"blue_green\"{}>Blue-green — a full extra generation</option></select></div></div>", if disabled || !can_update { " disabled" } else { "" }, if state.strategy == "blue_green" { " selected" } else { "" }));
                out.push_str(&action(
                    tr("Roll back", "롤백"),
                    "deploy-rollback",
                    disabled
                        || !can_update
                        || state.selected.is_empty()
                        || !list(releases).iter().any(|row| {
                            row["id"] == state.selected && row["id"] != deployment["id"]
                        }),
                ));
                out.push_str("<p>Rollback restores the image and service configuration. It uses current credentials and Secret values. External data is not restored.</p>");
            }
        }
        if !state.error.is_empty() {
            out.push_str(&action("Reload releases", "deploy-reload", state.loading));
        }
    }
    out.push_str("</section>");
    out
}

fn uuid() -> Result<String, String> {
    let mut bytes = [0; 16];
    crate::window()
        .crypto()
        .map_err(api::error)?
        .get_random_values_with_u8_array(&mut bytes)
        .map_err(api::error)?;
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let value: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &value[..8],
        &value[8..12],
        &value[12..16],
        &value[16..20],
        &value[20..]
    ))
}

pub fn prepare_update(app: &Shared, payload: &mut Value) -> Result<(), String> {
    let key = json!([
        payload["spec"],
        payload["strategy"],
        payload["expectedReleaseId"]
    ])
    .to_string();
    let mut state = app.borrow_mut();
    let id = match &state.deploy.update_request {
        Some((saved, id)) if *saved == key => id.clone(),
        Some(_) => uuid()?,
        None => text(payload, "operationId").to_owned(),
    };
    state.deploy.update_request = Some((key, id.clone()));
    payload["operationId"] = json!(id);
    Ok(())
}

pub fn failure(message: String) -> String {
    if message.starts_with("JsValue(") {
        tr("Unable to connect to the API. Retry the same Deploy request to recover its recorded result.", "API에 연결할 수 없다. 같은 Deploy 요청을 재시도해 기록된 결과를 확인한다.").into()
    } else {
        message
    }
}

fn draw(app: &Shared) {
    crate::set_html("deploy-panel", &html(&app.borrow()));
}

fn history(app: &Shared) {
    let mut state = app.borrow_mut();
    let Some(current) = &state.service_deployment else {
        return;
    };
    let service = text(current, "serviceId").to_owned();
    if state.deploy.service != service {
        state.deploy = State {
            service: service.clone(),
            ..State::default()
        };
    }
    if state.deploy.loading {
        return;
    }
    state.deploy.open = true;
    state.deploy.loading = true;
    state.deploy.error.clear();
    state.deploy.request += 1;
    let request = state.deploy.request;
    drop(state);
    draw(app);
    let app = app.clone();
    spawn_local(async move {
        let response = api::request(
            "GET",
            &format!("/api/micro-services/{}/releases", api::encode(&service)),
            None,
        )
        .await;
        let mut state = app.borrow_mut();
        if state.deploy.service != service || state.deploy.request != request {
            return;
        }
        state.deploy.loading = false;
        match response {
            Ok(rows) => {
                let active = state
                    .service_deployment
                    .as_ref()
                    .map(|current| text(current, "id"))
                    .unwrap_or("");
                state.deploy.selected = list(&rows)
                    .iter()
                    .find(|row| text(row, "id") != active)
                    .map(|row| text(row, "id").to_owned())
                    .unwrap_or_default();
                state.deploy.releases = Some(rows);
            }
            Err(error) => {
                state.deploy.error = error;
                state.deploy.releases = None;
            }
        }
        drop(state);
        draw(&app);
    });
}

pub fn change(app: &Shared, target: &Element) -> bool {
    if !["deploy-release", "deploy-strategy"].contains(&target.id().as_str()) {
        return false;
    }
    if let Some(select) = target.dyn_ref::<HtmlSelectElement>() {
        let mut state = app.borrow_mut();
        if target.id() == "deploy-release" {
            state.deploy.selected = select.value();
        } else {
            state.deploy.strategy = select.value();
        }
        drop(state);
        draw(app);
    }
    true
}

pub fn dispatch(app: &Shared, action_name: &str) -> bool {
    if !action_name.starts_with("deploy-") {
        return false;
    }
    match action_name {
        "deploy-history" => {
            if app.borrow().deploy.open {
                app.borrow_mut().deploy.open = false;
                draw(app);
            } else {
                history(app);
            }
        }
        "deploy-reload" => history(app),
        "deploy-update" => {
            let Some(current) = app.borrow().service_deployment.clone() else {
                return true;
            };
            if app.borrow().busy || busy(&current) || recovering(&current) {
                return true;
            }
            let operation = match uuid() {
                Ok(id) => id,
                Err(error) => {
                    app.borrow_mut().error = error;
                    crate::render_messages(app);
                    return true;
                }
            };
            let service = text(&current, "serviceId");
            app.borrow_mut().deploy.update_request = None;
            crate::open_form(app.clone(), "services", service);
            if let Some(form) = &mut app.borrow_mut().form {
                form.title = "Deploy update".into();
                form.method = "POST";
                form.path = format!("/api/micro-services/{}/updates", api::encode(service));
                form.base["operationId"] = json!(operation);
                form.base["expectedReleaseId"] = current["id"].clone();
                form.fields.push(
                    Field::new("strategy", "Deployment strategy", "select", "rolling").choices(&[
                        ("rolling", "Rolling — one extra VM"),
                        ("blue_green", "Blue-green — a full extra generation"),
                    ]),
                );
            }
            crate::advanced::prepare(app);
            crate::advanced::services::open(app);
            crate::render_editor(app);
        }
        "deploy-rollback" | "deploy-retry" => {
            let mut state = app.borrow_mut();
            let Some(current) = state.service_deployment.clone() else {
                return true;
            };
            if state.busy || busy(&current) {
                return true;
            }
            let service = text(&current, "serviceId").to_owned();
            if state.deploy.service != service {
                state.deploy = State {
                    service: service.clone(),
                    ..State::default()
                };
            }
            let retry = action_name == "deploy-retry";
            if !retry
                && (recovering(&current)
                    || state.deploy.selected.is_empty()
                    || !state.deploy.releases.as_ref().is_some_and(|rows| {
                        list(rows).iter().any(|row| {
                            row["id"] == state.deploy.selected && row["id"] != current["id"]
                        })
                    }))
            {
                return true;
            }
            let strategy = if state.deploy.strategy == "blue_green" {
                "blue_green"
            } else {
                "rolling"
            };
            let body = if retry {
                json!({"operationId":current["id"]})
            } else {
                let key = format!("{}:{}:{strategy}", current["id"], state.deploy.selected);
                if state
                    .deploy
                    .pending
                    .as_ref()
                    .is_none_or(|(saved, _)| *saved != key)
                {
                    match uuid() {
                        Ok(id) => state.deploy.pending = Some((key, id)),
                        Err(error) => {
                            state.deploy.error = error;
                            drop(state);
                            draw(app);
                            return true;
                        }
                    }
                }
                json!({"operationId":state.deploy.pending.as_ref().unwrap().1,"expectedReleaseId":current["id"],"releaseId":state.deploy.selected,"strategy":strategy})
            };
            let suffix = if retry {
                "deployment/retry"
            } else {
                "rollback"
            };
            drop(state);
            crate::mutate(
                app.clone(),
                "POST".into(),
                format!("/api/micro-services/{}/{suffix}", api::encode(&service)),
                Some(body),
                None,
            );
        }
        _ => {}
    }
    true
}
