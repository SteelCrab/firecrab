//! Deploy actions share the backend executor and the existing service wizard.

use crate::{
    App, Shared, api,
    forms::Field,
    model::{escape, list, text},
};
use serde_json::{Value, json};
use std::fmt::Write;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{Element, HtmlSelectElement};

#[derive(Default)]
pub struct State {
    service: String,
    releases: Option<Value>,
    /// What the list was last asked for: `load_key` of the deployment at that time. A new active
    /// release, or the same one settling, asks again.
    loaded_for: String,
    loading: bool,
    /// The deployment log is shown in the Overview. It is the panel's, so a service of its own starts closed.
    log_open: bool,
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

#[cfg(test)]
impl State {
    /// The same state with the deployment log shown.
    pub fn with_log_open(mut self) -> Self {
        self.log_open = true;
        self
    }

    /// The state of a panel that already holds the release list of `service`, for other modules' tests.
    pub fn loaded(service: &str, releases: Value) -> Self {
        Self {
            service: service.into(),
            releases: Some(releases),
            ..Self::default()
        }
    }
}

pub fn busy(deployment: &Value) -> bool {
    busy_phase(text(deployment, "phase"))
}

/// The phases in which the deployment is changing something, so a person must not start another.
pub fn busy_phase(phase: &str) -> bool {
    matches!(
        phase,
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

/// Releases are kept for a service that runs behind its domain: updates and roll backs go
/// through the Nginx route.
pub fn has_releases(deployment: &Value) -> bool {
    deployment.is_object() && !text(&deployment["spec"], "domain").is_empty()
}

fn recovering(deployment: &Value) -> bool {
    deployment["update"].is_object() && deployment["update"]["completed"] != true
}

/// The host stores a release for a deployment that is Running (or Degraded) and whose update, if
/// there is one, has completed. Until then the deployment is on its way and is not in the list.
pub fn settled_deployment(deployment: &Value) -> bool {
    matches!(text(deployment, "phase"), "running" | "degraded") && !recovering(deployment)
}

/// The release that serves: the deployment itself once it settled; during an update the one the
/// update started from. A first deployment that is still going, a failed one and a stopped
/// service have none.
pub fn active_id(deployment: &Value) -> &str {
    if recovering(deployment) {
        text(&deployment["update"]["previous"], "id")
    } else if settled_deployment(deployment) {
        text(deployment, "id")
    } else {
        ""
    }
}

/// What a release list is asked for: the deployment and whether it settled. The list changes when
/// either does, not on every poll.
pub fn load_key(deployment: &Value) -> String {
    format!(
        "{}:{}",
        text(deployment, "id"),
        settled_deployment(deployment)
    )
}

/// Whether the release list must be asked for now: a service whose list is not this one's, or
/// the same service when the deployment is not the one (or not in the state) the list was
/// asked for. A request in flight is not doubled, and a failed one is not repeated each poll.
fn needs_load(state: &State, deployment: &Value) -> bool {
    has_releases(deployment)
        && (state.service != text(deployment, "serviceId")
            || (!state.loading && state.loaded_for != load_key(deployment)))
}

/// A running deployment can take an update: the phase allows it, none is going and the dashboard
/// is not in the middle of a request.
pub fn can_deploy_update(app: &App) -> bool {
    app.service_deployment
        .as_ref()
        .is_some_and(|deployment| has_releases(deployment) && !app.busy && updatable(deployment))
}

fn updatable(deployment: &Value) -> bool {
    !busy(deployment) && !recovering(deployment) && settled_deployment(deployment)
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

/// The release tab: update, roll back and the releases the host keeps. A service without a
/// domain has none of it.
pub fn html(app: &App) -> String {
    let Some(deployment) = &app.service_deployment else {
        return String::new();
    };
    if !has_releases(deployment) {
        // Never deployed: nothing was released yet. Deployed without a domain: it will have none.
        return format!(
            "<p class=\"detail-note\">{}</p>",
            crate::i18n::t(if deployment.is_null() {
                "No releases yet. Deploy the service first."
            } else {
                "Releases and updates are available for a service that has a domain."
            })
        );
    }
    let state = &app.deploy;
    let known = state.service == text(deployment, "serviceId");
    let disabled = app.busy || busy(deployment);
    let can_update = updatable(deployment);
    let mut out = format!(
        "<section aria-label=\"Deploy\"><p>{}</p><div class=\"wasm-actions\">",
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
    if known && !state.error.is_empty() {
        out.push_str(&format!("<p role=\"alert\">{}</p>", escape(&state.error)));
    }
    if !known || state.loading {
        out.push_str(&format!(
            "<p role=\"status\">{}</p>",
            tr("Loading releases…", "릴리스를 불러오는 중…")
        ));
    }
    if known && let Some(releases) = &state.releases {
        if list(releases).is_empty() {
            out.push_str(&format!(
                "<p>{}</p>",
                tr(
                    "No successful releases are retained yet.",
                    "보관 중인 성공한 릴리스가 아직 없습니다."
                )
            ));
        } else {
            out.push_str(&release_table(app, None));
            out.push_str(&format!("<div class=\"wasm-form\"><div class=\"field\"><label for=\"deploy-release\">{}</label>", tr("Rollback release", "롤백할 릴리스")));
            out.push_str(&format!(
                "<select id=\"deploy-release\"{}><option value=\"\">{}</option>",
                if disabled || !can_update {
                    " disabled"
                } else {
                    ""
                },
                tr("Select a previous release", "이전 릴리스를 선택합니다")
            ));
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
            out.push_str(&format!("</select></div><div class=\"field\"><label for=\"deploy-strategy\">{}</label><select id=\"deploy-strategy\"{}><option value=\"rolling\">{}</option><option value=\"blue_green\"{}>{}</option></select></div></div>", tr("Strategy", "전략"), if disabled || !can_update { " disabled" } else { "" }, tr("Rolling — one extra VM", "롤링 — VM 1개 추가"), if state.strategy == "blue_green" { " selected" } else { "" }, tr("Blue-green — a full extra generation", "블루그린 — 전체 세대 추가")));
            out.push_str(&action(
                tr("Roll back to selected release", "선택한 릴리스로 롤백"),
                "deploy-rollback",
                disabled
                    || !can_update
                    || state.selected.is_empty()
                    || !list(releases)
                        .iter()
                        .any(|row| row["id"] == state.selected && row["id"] != deployment["id"]),
            ));
            out.push_str(&format!("<p>{}</p>", tr("Rollback restores the image and service configuration. It uses current credentials and Secret values. External data is not restored.", "롤백은 이미지와 서비스 구성을 되돌립니다. 현재 자격 증명과 Secret 값을 사용합니다. 외부 데이터는 복원하지 않습니다.")));
        }
    }
    if known && !state.error.is_empty() {
        out.push_str(&action(
            tr("Reload releases", "릴리스 다시 불러오기"),
            "deploy-reload",
            state.loading,
        ));
    }
    out.push_str("</section>");
    out
}

/// The first characters of a release id: enough to tell releases apart in a narrow column. The
/// cell keeps the whole id as its tooltip.
pub fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

/// The releases as a table with its head. `limit` keeps the first few. Without releases: nothing.
pub fn release_table(app: &App, limit: Option<usize>) -> String {
    let rows = release_rows(app, limit);
    if rows.is_empty() {
        return String::new();
    }
    format!(
        "<div class=\"table-scroll\"><table class=\"wasm-table release-table\"><colgroup><col class=\"release-col-id\"><col><col class=\"release-col-actions\"></colgroup><thead><tr><th>{}</th><th>{}</th><th class=\"row-actions\"><span class=\"sr-only\">{}</span></th></tr></thead><tbody>{rows}</tbody></table></div>",
        crate::i18n::t("Release"),
        crate::i18n::t("Image"),
        crate::i18n::t("Actions")
    )
}

/// The rows of the release table, in the order the host lists them. `limit` keeps the first few:
/// the Overview shows three. A release other than the active one can be rolled back to.
pub fn release_rows(app: &App, limit: Option<usize>) -> String {
    let Some(deployment) = &app.service_deployment else {
        return String::new();
    };
    // A list that was asked for another service is not this one's.
    if app.deploy.service != text(deployment, "serviceId") {
        return String::new();
    }
    let releases = app.deploy.releases.as_ref().map_or(&[][..], list);
    let active_id = active_id(deployment);
    let mut out = String::new();
    for release in releases.iter().take(limit.unwrap_or(releases.len())) {
        let id = text(release, "id");
        let image = text(&release["spec"], "image");
        let digest = text(release, "digest");
        let active = !active_id.is_empty() && id == active_id;
        let _ = write!(
            out,
            "<tr data-release=\"{}\"><td><span class=\"mono\" title=\"{}\">{}</span>{}</td><td>{}</td><td class=\"row-actions\">{}</td></tr>",
            escape(id),
            escape(id),
            escape(&short(id)),
            if active {
                crate::render::status_dot("ok", crate::i18n::t("Active"))
            } else {
                format!(
                    "<span class=\"release-archived\">{}</span>",
                    crate::i18n::t("Archived")
                )
            },
            // The image, and its digest under it; the column cuts both and keeps them as tooltips.
            crate::render::name_cell_cut(image, digest, false),
            if active {
                String::new()
            } else {
                crate::render::icon_button(
                    "Roll back to this release",
                    "deploy-select",
                    id,
                    crate::render::Icon::Rollback,
                )
            }
        );
    }
    out
}

/// The Overview's short list of releases: the newest three, each with a word for where it stands
/// and, for an archived one, an icon that rolls back to it (shown at rest). A deployment that is
/// going is the first row ("In progress"): the host stores its release only when it settles.
pub fn recent_releases(app: &App) -> String {
    let Some(deployment) = &app.service_deployment else {
        return String::new();
    };
    if !has_releases(deployment) {
        return String::new();
    }
    let active = active_id(deployment);
    let going = text(deployment, "id");
    let mut rows = Vec::new();
    if busy(deployment) && !going.is_empty() && going != active {
        rows.push((going, "busy", "In progress"));
    }
    // A list that was asked for another service is not this one's.
    if app.deploy.service == text(deployment, "serviceId") {
        for release in app.deploy.releases.as_ref().map_or(&[][..], list) {
            let id = text(release, "id");
            if rows.iter().any(|(row, _, _)| *row == id) {
                continue;
            }
            if !active.is_empty() && id == active {
                rows.push((id, "ok", "Active"));
            } else {
                rows.push((id, "off", "Archived"));
            }
        }
    }
    if rows.is_empty() {
        return String::new();
    }
    let mut out = String::from("<ul class=\"detail-list release-recent\">");
    for (id, tone, word) in rows.into_iter().take(3) {
        let _ = write!(
            out,
            "<li data-release=\"{}\"><span class=\"mono detail-list-main\" title=\"{}\">{}</span>{}<span class=\"release-roll\">{}</span></li>",
            escape(id),
            escape(id),
            escape(&short(id)),
            crate::render::status_dot(tone, crate::i18n::t(word)),
            if tone == "off" {
                crate::render::icon_button(
                    "Roll back to this release",
                    "deploy-select",
                    id,
                    crate::render::Icon::Rollback,
                )
            } else {
                String::new()
            }
        );
    }
    out.push_str("</ul>");
    out
}

/// Whether the deployment log shows in the Overview. It belongs to the service the panel shows.
pub fn log_open(app: &App) -> bool {
    app.deploy.log_open
        && app
            .service_deployment
            .as_ref()
            .is_some_and(|deployment| app.deploy.service == text(deployment, "serviceId"))
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

/// Puts the state on the service the panel shows; a list of another service is dropped.
fn follow(state: &mut App, service: &str) {
    if state.deploy.service != service {
        state.deploy = State {
            service: service.into(),
            request: state.deploy.request,
            ..State::default()
        };
    }
}

/// Asks for the release list when the active release is not the one it was last asked for: on
/// the first look at a service and after each deploy or roll back. The Overview and the release
/// tab read the same list. Runs from the panel's own status poll, so it adds no timer.
pub fn refresh(app: &Shared) {
    let (service, key) = {
        let state = app.borrow();
        let Some(current) = state.service_deployment.as_ref() else {
            return;
        };
        if !needs_load(&state.deploy, current) {
            return;
        }
        (text(current, "serviceId").to_owned(), load_key(current))
    };
    {
        let mut state = app.borrow_mut();
        follow(&mut state, &service);
        // Set before the request: a failure shows an error and a button, it does not retry each poll.
        state.deploy.loaded_for = key;
    }
    load(app, service);
}

fn load(app: &Shared, service: String) {
    let mut state = app.borrow_mut();
    if state.deploy.loading {
        return;
    }
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
                    .map(active_id)
                    .unwrap_or("")
                    .to_owned();
                // A release the person already picked stays picked.
                let kept = list(&rows)
                    .iter()
                    .any(|row| row["id"] == state.deploy.selected && row["id"] != active);
                if !kept {
                    state.deploy.selected = list(&rows)
                        .iter()
                        .find(|row| text(row, "id") != active)
                        .map(|row| text(row, "id").to_owned())
                        .unwrap_or_default();
                }
                state.deploy.releases = Some(rows);
            }
            Err(error) => {
                state.deploy.error = error;
                state.deploy.releases = None;
            }
        }
        // A form in the panel keeps what its owner typed; the list shows when it closes.
        let editing = state.form.is_some();
        drop(state);
        draw(&app);
        if !editing {
            crate::render_detail(&app);
        }
    });
}

/// Picks `id` for the roll back and shows the release tab, where the strategy and the Roll back
/// button are. The active release, or one the list does not hold, is not picked.
fn choose(app: &mut App, id: &str) {
    let active = app
        .service_deployment
        .as_ref()
        .map_or("", |current| text(current, "id"));
    if id != active
        && app
            .deploy
            .releases
            .as_ref()
            .is_some_and(|rows| list(rows).iter().any(|row| row["id"] == id))
    {
        app.deploy.selected = id.into();
    }
    app.detail_tab = "releases".into();
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

pub fn dispatch(app: &Shared, action_name: &str, id: &str) -> bool {
    if !action_name.starts_with("deploy-") {
        return false;
    }
    match action_name {
        "deploy-log" => {
            let service = text(
                app.borrow()
                    .service_deployment
                    .as_ref()
                    .unwrap_or(&Value::Null),
                "serviceId",
            )
            .to_owned();
            if !service.is_empty() {
                let mut state = app.borrow_mut();
                follow(&mut state, &service);
                state.deploy.log_open = !state.deploy.log_open;
                drop(state);
                crate::render_detail(app);
            }
        }
        "deploy-select" => {
            choose(&mut app.borrow_mut(), id);
            crate::render_detail(app);
        }
        "deploy-reload" => {
            let service = text(
                app.borrow()
                    .service_deployment
                    .as_ref()
                    .unwrap_or(&Value::Null),
                "serviceId",
            )
            .to_owned();
            if !service.is_empty() {
                follow(&mut app.borrow_mut(), &service);
                load(app, service);
            }
        }
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
            // Cancel returns to the tab the update belongs to, whichever icon started it.
            app.borrow_mut().detail_tab = "releases".into();
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
            follow(&mut state, &service);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn release(id: &str, image: &str) -> Value {
        json!({"id": id, "spec": {"image": image}, "digest": format!("sha256:{}", "a".repeat(64))})
    }

    /// Four retained releases; `r3` is the active one.
    fn app() -> App {
        App {
            route: "services".into(),
            service_deployment: Some(json!({
                "id": "r3-aaaa-bbbb", "serviceId": "s1", "phase": "running", "instances": [],
                "spec": {"domain": "orders.example.com", "image": "orders:3"}
            })),
            deploy: State {
                service: "s1".into(),
                releases: Some(json!([
                    release("r3-aaaa-bbbb", "orders:3"),
                    release("r2-aaaa-bbbb", "orders:2"),
                    release("r1-aaaa-bbbb", "orders:1"),
                    release("r0-aaaa-bbbb", "orders:0")
                ])),
                ..State::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn only_a_service_behind_a_domain_has_releases() {
        assert!(has_releases(&app().service_deployment.unwrap()));
        assert!(!has_releases(&json!(null)));
        assert!(!has_releases(&json!({"spec": {"domain": ""}})));
        assert!(!has_releases(&json!({"spec": {}})));
    }

    #[test]
    fn rows_mark_the_active_release_and_offer_roll_back_on_the_others() {
        let rows = release_rows(&app(), None);
        assert_eq!(rows.matches("<tr ").count(), 4, "{rows}");
        let (active, rest) = rows.split_once("</tr>").unwrap();
        // The first column is a short mono id with the whole id as its tooltip; the active release says so.
        assert!(active.contains("<td><span class=\"mono\" title=\"r3-aaaa-bbbb\">r3-aaaa-</span><span class=\"status\" data-tone=\"ok\"><span class=\"status-dot\" aria-hidden=\"true\"></span>Active</span></td>"), "{active}");
        assert!(
            !active.contains("deploy-select"),
            "no roll back to itself: {active}"
        );
        assert!(rest.contains("aria-label=\"Roll back to this release\" title=\"Roll back to this release\" data-action=\"deploy-select\" data-id=\"r2-aaaa-bbbb\""), "{rest}");
        assert_eq!(rest.matches("deploy-select").count(), 3, "{rest}");
        // The others are archived, in words, next to their id.
        assert_eq!(
            rest.matches("<span class=\"release-archived\">Archived</span>")
                .count(),
            3,
            "{rest}"
        );
        assert!(!active.contains("Archived"), "{active}");
        // The action sits in a row-actions cell, as in every table.
        assert!(
            rest.contains("<td class=\"row-actions\"><button type=\"button\" class=\"icon-btn\""),
            "{rest}"
        );
    }

    #[test]
    fn the_limit_keeps_the_first_rows() {
        let state = app();
        assert_eq!(release_rows(&state, Some(3)).matches("<tr ").count(), 3);
        assert_eq!(release_rows(&state, Some(9)).matches("<tr ").count(), 4);
        assert_eq!(release_rows(&App::default(), None), "");
        // The list of another service is not shown.
        let mut other = app();
        other.deploy.service = "s2".into();
        assert_eq!(release_rows(&other, None), "");
        assert_eq!(release_table(&other, Some(3)), "");
    }

    #[test]
    fn long_values_stay_on_one_line_and_keep_the_whole_text_as_the_tooltip() {
        let mut state = app();
        let long =
            "ghcr.io/team/a-very-long-repository-name/with/many/path/segments:2026.10.28-rc1";
        state.deploy.releases = Some(json!([release("r3-aaaa-bbbb", long)]));
        let rows = release_rows(&state, None);
        let digest = format!("sha256:{}", "a".repeat(64));
        // The image is the name; its digest is the line under it. Both keep their whole text as the tooltip.
        assert!(rows.contains(&format!("<div class=\"cell-name\"><b class=\"\" title=\"{long}\">{long}</b><span class=\"cell-sub\" data-tone=\"\" title=\"{digest}\">{digest}</span></div>")), "{rows}");
    }

    #[test]
    fn the_release_tab_lists_the_releases_without_a_history_toggle() {
        let html = html(&app());
        assert!(
            !html.contains("Release history") && !html.contains("deploy-history"),
            "{html}"
        );
        assert!(!html.contains("<h3"), "{html}");
        assert!(
            html.contains("<table class=\"wasm-table release-table\">"),
            "{html}"
        );
        assert!(
            html.contains("<th>Release</th><th>Image</th><th class=\"row-actions\">"),
            "{html}"
        );
        assert!(
            html.contains("id=\"deploy-release\"") && html.contains("id=\"deploy-rollback\""),
            "{html}"
        );
        assert!(html.contains("id=\"deploy-update\""), "{html}");
        // The release the person picked is the one the select shows.
        let mut chosen = app();
        chosen.deploy.selected = "r1-aaaa-bbbb".into();
        assert!(super::html(&chosen).contains("<option value=\"r1-aaaa-bbbb\" selected>"));
    }

    #[test]
    fn the_release_tab_waits_for_the_list_and_shows_why_it_failed() {
        let mut state = app();
        state.deploy.releases = None;
        state.deploy.loading = true;
        let loading = html(&state);
        assert!(
            loading.contains("<p role=\"status\">Loading releases…</p>"),
            "{loading}"
        );
        assert!(!loading.contains("release-table"), "{loading}");
        // A list that belongs to another service is not shown.
        state.deploy.loading = false;
        state.deploy.service = "other".into();
        assert!(html(&state).contains("Loading releases…"));
        state.deploy.service = "s1".into();
        state.deploy.error = "HTTP 503: Release storage unavailable".into();
        let failed = html(&state);
        assert!(
            failed.contains("<p role=\"alert\">HTTP 503: Release storage unavailable</p>"),
            "{failed}"
        );
        assert!(failed.contains("id=\"deploy-reload\""), "{failed}");
        assert!(
            !failed.contains("Loading releases…") && !failed.contains("No successful releases"),
            "{failed}"
        );
    }

    #[test]
    fn a_service_without_a_domain_says_what_the_tab_is_for() {
        let mut state = app();
        state.service_deployment.as_mut().unwrap()["spec"]["domain"] = json!("");
        assert_eq!(
            html(&state),
            "<p class=\"detail-note\">Releases and updates are available for a service that has a domain.</p>"
        );
        // Never deployed: nothing was released yet, whatever the domain.
        state.service_deployment = Some(json!(null));
        assert_eq!(
            html(&state),
            "<p class=\"detail-note\">No releases yet. Deploy the service first.</p>"
        );
        // Nothing yet from the host: nothing to say.
        state.service_deployment = None;
        assert_eq!(html(&state), "");
    }

    #[test]
    fn choosing_a_release_selects_it_and_opens_the_release_tab() {
        let mut state = app();
        choose(&mut state, "r1-aaaa-bbbb");
        assert_eq!(state.deploy.selected, "r1-aaaa-bbbb");
        assert_eq!(state.detail_tab, "releases");
        // The active release and one that is not in the list cannot be picked; the tab still opens.
        state.deploy.selected.clear();
        choose(&mut state, "r3-aaaa-bbbb");
        assert!(state.deploy.selected.is_empty());
        choose(&mut state, "nope");
        assert!(state.deploy.selected.is_empty());
    }

    #[test]
    fn the_release_tab_and_its_rows_read_in_korean() {
        let mut state = app();
        state.deploy.selected = "r2-aaaa-bbbb".into();
        crate::i18n::set("ko");
        let html = html(&state);
        crate::i18n::set("en");
        for text in [
            "릴리스</th>",
            "이미지</th>",
            "활성</span>",
            "보관됨</span>",
            "롤백할 릴리스</label>",
            "이전 릴리스를 선택합니다",
            "전략</label>",
            "롤링 — VM 1개 추가",
            "블루그린 — 전체 세대 추가",
            ">선택한 릴리스로 롤백</button>",
            "롤백은 이미지와 서비스 구성을 되돌립니다.",
        ] {
            assert!(html.contains(text), "{text}: {html}");
        }
        state.deploy.error = "x".into();
        crate::i18n::set("ko");
        let failed = super::html(&state);
        crate::i18n::set("en");
        assert!(
            failed.contains(">릴리스 다시 불러오기</button>"),
            "{failed}"
        );
    }

    fn deployment(id: &str, phase: &str, update: Value) -> Value {
        json!({"id": id, "serviceId": "s1", "phase": phase, "update": update, "instances": [],
            "spec": {"domain": "orders.example.com"}})
    }

    #[test]
    fn a_deployment_is_settled_when_the_backend_keeps_it_as_a_release() {
        let done = json!({"completed": true, "previous": {"id": "r2"}});
        let going = json!({"completed": false, "previous": {"id": "r2"}});
        // Running or Degraded, and no update or a completed one: the host stores the release.
        for (phase, update, settled) in [
            ("running", json!(null), true),
            ("degraded", json!(null), true),
            ("running", done.clone(), true),
            ("running", going.clone(), false),
            ("degraded", going.clone(), false),
            ("queued", json!(null), false),
            ("switching_traffic", going.clone(), false),
            ("failed", json!(null), false),
            ("stopped", done.clone(), false),
        ] {
            assert_eq!(
                settled_deployment(&deployment("r3", phase, update.clone())),
                settled,
                "{phase} {update}"
            );
        }
    }

    #[test]
    fn the_active_release_is_the_settled_one_or_the_one_before_an_update() {
        let going = json!({"completed": false, "previous": {"id": "r2"}});
        assert_eq!(active_id(&deployment("r3", "running", json!(null))), "r3");
        assert_eq!(
            active_id(&deployment(
                "r3",
                "running",
                json!({"completed": true, "previous": {"id": "r2"}})
            )),
            "r3"
        );
        // During an update the release that serves is the previous one, not the one being made.
        assert_eq!(
            active_id(&deployment("r3", "switching_traffic", going.clone())),
            "r2"
        );
        assert_eq!(active_id(&deployment("r3", "failed", going)), "r2");
        // Nothing serves a first deployment that is still going, a failed one, or a stopped service.
        assert_eq!(active_id(&deployment("r1", "queued", json!(null))), "");
        assert_eq!(
            active_id(&deployment(
                "r1",
                "stopped",
                json!({"completed": true, "previous": {"id": "r0"}})
            )),
            ""
        );
        assert_eq!(active_id(&json!(null)), "");
    }

    #[test]
    fn the_release_list_is_asked_again_when_the_deployment_settles() {
        let state = |service: &str, key: &str, loading: bool| State {
            service: service.into(),
            loaded_for: key.into(),
            loading,
            ..State::default()
        };
        let going = deployment(
            "r3",
            "switching_traffic",
            json!({"completed": false, "previous": {"id": "r2"}}),
        );
        let done = deployment(
            "r3",
            "running",
            json!({"completed": true, "previous": {"id": "r2"}}),
        );
        assert_eq!(load_key(&going), "r3:false");
        assert_eq!(load_key(&done), "r3:true");
        // Asked while the update was going: asked again once it is done, and not before.
        assert!(!needs_load(&state("s1", "r3:false", false), &going));
        assert!(needs_load(&state("s1", "r3:false", false), &done));
        assert!(!needs_load(&state("s1", "r3:true", false), &done));
        // A new active release asks again; a request in flight is not doubled.
        assert!(needs_load(&state("s1", "r2:true", false), &done));
        assert!(!needs_load(&state("s1", "r2:true", true), &done));
        // A list that belongs to another service is never enough, even while it is loading.
        assert!(needs_load(&state("s2", "r3:true", false), &done));
        assert!(needs_load(&state("s2", "r3:true", true), &done));
        assert!(needs_load(&State::default(), &done));
        // Nothing to ask for before a deployment, or without a domain.
        assert!(!needs_load(&State::default(), &json!(null)));
        let mut plain = done;
        plain["spec"]["domain"] = json!("");
        assert!(!needs_load(&State::default(), &plain));
    }

    #[test]
    fn a_failed_ask_is_not_repeated_while_nothing_changed() {
        // `refresh` marks the key before it asks, so a failure leaves the key equal to the deployment's.
        let done = deployment("r3", "running", json!(null));
        let state = State {
            service: "s1".into(),
            loaded_for: load_key(&done),
            error: "HTTP 503".into(),
            ..State::default()
        };
        assert!(!needs_load(&state, &done));
    }

    #[test]
    fn during_an_update_the_serving_release_is_active_in_the_table_too() {
        let mut state = app();
        state.service_deployment = Some(deployment(
            "r3-aaaa-bbbb",
            "switching_traffic",
            json!({"completed": false, "previous": {"id": "r2-aaaa-bbbb"}}),
        ));
        state.service_deployment.as_mut().unwrap()["serviceId"] = json!("s1");
        let rows = release_rows(&state, None);
        let (before, after) = rows.split_once("r2-aaaa-bbbb").unwrap();
        // The release that serves is marked, and has no roll back to itself; the others are archived.
        assert!(
            after
                .split("</tr>")
                .next()
                .unwrap()
                .contains(">Active</span>"),
            "{rows}"
        );
        assert!(
            !after
                .split("</tr>")
                .next()
                .unwrap()
                .contains("deploy-select"),
            "{rows}"
        );
        assert!(!before.contains(">Active</span>"), "{rows}");
    }

    fn recent(deployment: Value, releases: Value) -> String {
        let mut state = app();
        state.service_deployment = Some(deployment);
        state.deploy.releases = Some(releases);
        recent_releases(&state)
    }

    #[test]
    fn recent_releases_list_three_with_a_word_for_each_state() {
        let releases = json!([
            release("r3-aaaa-bbbb", "orders:3"),
            release("r2-aaaa-bbbb", "orders:2"),
            release("r1-aaaa-bbbb", "orders:1"),
            release("r0-aaaa-bbbb", "orders:0")
        ]);
        let html = recent(deployment("r3-aaaa-bbbb", "running", json!(null)), releases);
        assert!(
            html.starts_with("<ul class=\"detail-list release-recent\">"),
            "{html}"
        );
        assert_eq!(html.matches("<li ").count(), 3, "{html}");
        let first = html.split("</li>").next().unwrap();
        assert!(
            first.contains(
                "<span class=\"mono detail-list-main\" title=\"r3-aaaa-bbbb\">r3-aaaa-</span>"
            ),
            "{first}"
        );
        assert!(first.contains("<span class=\"status\" data-tone=\"ok\"><span class=\"status-dot\" aria-hidden=\"true\"></span>Active</span>"), "{first}");
        assert!(!first.contains("deploy-select"), "{first}");
        // The others are archived (a grey dot and the word) and roll back with an icon that is always visible:
        // not in a hidden row-actions cell.
        assert_eq!(html.matches("data-tone=\"off\"><span class=\"status-dot\" aria-hidden=\"true\"></span>Archived</span>").count(), 2, "{html}");
        assert_eq!(
            html.matches("aria-label=\"Roll back to this release\"")
                .count(),
            2,
            "{html}"
        );
        assert!(
            !html.contains("row-actions") && !html.contains("<table"),
            "{html}"
        );
    }

    #[test]
    fn during_an_update_the_new_release_is_in_progress_and_the_old_one_is_active() {
        let going = deployment(
            "r3-aaaa-bbbb",
            "switching_traffic",
            json!({"completed": false, "previous": {"id": "r2-aaaa-bbbb"}}),
        );
        // The host has not stored r3 yet: its row comes from the deployment.
        let html = recent(
            going.clone(),
            json!([
                release("r2-aaaa-bbbb", "orders:2"),
                release("r1-aaaa-bbbb", "orders:1")
            ]),
        );
        let rows: Vec<_> = html
            .split("</li>")
            .filter(|row| row.contains("<li "))
            .collect();
        assert_eq!(rows.len(), 3, "{html}");
        assert!(rows[0].contains("title=\"r3-aaaa-bbbb\"") && rows[0].contains("data-tone=\"busy\"><span class=\"status-dot\" aria-hidden=\"true\"></span>In progress</span>"), "{}", rows[0]);
        assert!(!rows[0].contains("deploy-select"), "{}", rows[0]);
        assert!(
            rows[1].contains("title=\"r2-aaaa-bbbb\"") && rows[1].contains(">Active</span>"),
            "{}",
            rows[1]
        );
        assert!(
            rows[2].contains(">Archived</span>") && rows[2].contains("deploy-select"),
            "{}",
            rows[2]
        );
        // Once it is stored the row is not doubled.
        let again = recent(
            going,
            json!([
                release("r3-aaaa-bbbb", "orders:3"),
                release("r2-aaaa-bbbb", "orders:2")
            ]),
        );
        assert_eq!(again.matches("<li ").count(), 2, "{again}");
        // A first deployment has the one row, in progress, before any release is stored.
        let first = recent(
            deployment("r1-aaaa-bbbb", "starting", json!(null)),
            json!([]),
        );
        assert_eq!(first.matches("<li ").count(), 1, "{first}");
        assert!(first.contains(">In progress</span>"), "{first}");
    }

    #[test]
    fn recent_releases_wait_for_their_list_and_skip_a_service_without_a_domain() {
        let mut state = app();
        state.deploy.service = "other".into();
        // Another service's list is not shown, but a deployment that is going still is.
        assert_eq!(recent_releases(&state), "");
        state.service_deployment.as_mut().unwrap()["phase"] = json!("queued");
        assert_eq!(recent_releases(&state).matches("<li ").count(), 1);
        state.service_deployment.as_mut().unwrap()["spec"]["domain"] = json!("");
        assert_eq!(recent_releases(&state), "");
        assert_eq!(recent_releases(&App::default()), "");
    }

    #[test]
    fn the_roll_back_button_of_the_tab_has_its_own_name() {
        let mut state = app();
        state.deploy.selected = "r2-aaaa-bbbb".into();
        let html = html(&state);
        assert!(html.contains("id=\"deploy-rollback\" class=\"btn\" data-action=\"deploy-rollback\" data-id=\"\">Roll back to selected release</button>"), "{html}");
        crate::i18n::set("ko");
        let korean = super::html(&state);
        crate::i18n::set("en");
        assert!(
            korean.contains(">선택한 릴리스로 롤백</button>"),
            "{korean}"
        );
    }

    #[test]
    fn the_head_icon_deploys_an_update_when_it_can() {
        let mut state = app();
        assert!(can_deploy_update(&state));
        state.busy = true;
        assert!(!can_deploy_update(&state));
        state.busy = false;
        state.service_deployment.as_mut().unwrap()["phase"] = json!("draining");
        assert!(!can_deploy_update(&state));
        state.service_deployment.as_mut().unwrap()["phase"] = json!("running");
        state.service_deployment.as_mut().unwrap()["update"] = json!({"completed": false});
        assert!(!can_deploy_update(&state));
        state.service_deployment.as_mut().unwrap()["update"] = json!(null);
        state.service_deployment.as_mut().unwrap()["spec"]["domain"] = json!("");
        assert!(!can_deploy_update(&state));
    }

    #[test]
    fn busy_phases_are_the_ones_that_hold_the_stop_button() {
        for phase in [
            "queued",
            "importing",
            "starting",
            "checking_health",
            "connecting_domain",
            "switching_traffic",
            "draining",
            "rolling_back",
            "stopping",
        ] {
            assert!(busy_phase(phase), "{phase}");
            assert!(busy(&json!({"phase": phase})), "{phase}");
        }
        for phase in ["running", "failed", "stopped", "degraded", ""] {
            assert!(!busy_phase(phase), "{phase}");
        }
    }
}
