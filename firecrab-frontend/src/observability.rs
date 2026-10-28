//! Service observations are scoped to the selected service and its panel.
use crate::{
    App, Shared, api,
    model::{escape, list, text},
    render::{Icon, icon_button},
};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{Element, HtmlInputElement};

#[derive(Clone, Copy, PartialEq)]
enum View {
    Logs,
    Events,
    Alerts,
    Otlp,
}

#[derive(Default)]
pub struct State {
    service: String,
    generation: u64,
    request: u64,
    view: Option<View>,
    pending: bool,
    saving: bool,
    snapshot: Option<Value>,
    endpoint: String,
    dirty: bool,
    error: String,
}

fn tr<'a>(app: &App, en: &'a str, ko: &'a str) -> &'a str {
    if app.locale == "ko" { ko } else { en }
}

fn matches_selection(app: &App) -> bool {
    app.route == "services"
        && app.panel_open
        && app.detail.as_ref().is_some_and(|detail| {
            text(detail, "id") == app.observability.service
                && app.detail_generation == app.observability.generation
        })
}

pub fn clear(app: &mut App) {
    if !app.observability.service.is_empty() && !matches_selection(app) {
        let request = app.observability.request.wrapping_add(1);
        app.observability = State {
            request,
            ..State::default()
        };
    }
}

fn view_button(
    label: &str,
    action: &str,
    icon: Icon,
    selected: Option<bool>,
    disabled: bool,
) -> String {
    icon_button(label, action, "", icon).replace(
        "type=\"button\"",
        &format!(
            "type=\"button\" id=\"{action}\"{}{}",
            selected
                .map(|pressed| format!(" aria-pressed=\"{pressed}\""))
                .unwrap_or_default(),
            if disabled { " disabled" } else { "" }
        ),
    )
}

fn records(app: &App, view: View, snapshot: &Value) -> String {
    let (key, empty) = match view {
        View::Logs => ("logs", tr(app, "No logs.", "로그가 없다.")),
        View::Events => ("events", tr(app, "No events.", "이벤트가 없다.")),
        View::Alerts => ("alerts", tr(app, "No alerts.", "알림이 없다.")),
        View::Otlp => return String::new(),
    };
    let rows = list(&snapshot[key]);
    if rows.is_empty() {
        return format!("<p role=\"status\">{}</p>", escape(empty));
    }
    let mut out = "<ul class=\"service-observability-list\">".to_owned();
    for row in rows {
        let mut meta = vec![text(row, "timestamp")];
        match view {
            View::Logs => {
                meta.push(text(row, "stream"));
                meta.push(text(row, "instanceId"));
            }
            View::Events => {
                meta.push(text(row, "severity"));
                meta.push(text(row, "kind"));
            }
            View::Alerts => {
                meta.push(if row["active"] == true {
                    tr(app, "Active", "활성")
                } else {
                    tr(app, "Resolved", "해결")
                });
                meta.push(text(row, "kind"));
            }
            View::Otlp => unreachable!(),
        }
        let meta = meta
            .into_iter()
            .filter(|value| !value.is_empty())
            .map(escape)
            .collect::<Vec<_>>()
            .join(" · ");
        out.push_str(&format!(
            "<li><div class=\"meta\">{meta}</div><pre>{}</pre></li>",
            escape(text(row, "message"))
        ));
    }
    out.push_str("</ul>");
    out
}

pub fn html(app: &App) -> String {
    let state = &app.observability;
    let selected = if matches_selection(app) {
        state.view
    } else {
        None
    };
    let mut out = "<section id=\"service-observability\" class=\"service-observability\"><div class=\"wasm-actions\">".to_owned();
    for (view, label, action, icon) in [
        (
            View::Logs,
            tr(app, "Logs", "로그"),
            "service-observe-logs",
            Icon::Logs,
        ),
        (
            View::Events,
            tr(app, "Events", "이벤트"),
            "service-observe-events",
            Icon::Events,
        ),
        (
            View::Alerts,
            tr(app, "Alerts", "알림"),
            "service-observe-alerts",
            Icon::Alerts,
        ),
        (View::Otlp, "OTLP", "service-observe-otlp", Icon::Export),
    ] {
        out.push_str(&view_button(
            label,
            action,
            icon,
            Some(selected == Some(view)),
            state.saving,
        ));
    }
    out.push_str("</div>");
    let Some(view) = selected else {
        out.push_str("</section>");
        return out;
    };
    let label = match view {
        View::Logs => tr(app, "Logs", "로그"),
        View::Events => tr(app, "Events", "이벤트"),
        View::Alerts => tr(app, "Alerts", "알림"),
        View::Otlp => "OTLP",
    };
    out.push_str(&format!("<h3>{}</h3>", escape(label)));
    out.push_str(&view_button(
        tr(app, "Refresh", "새로고침"),
        "service-observe-refresh",
        Icon::Refresh,
        None,
        state.pending,
    ));
    if state.pending && state.snapshot.is_none() {
        out.push_str(&format!(
            "<p role=\"status\">{}</p>",
            tr(app, "Loading…", "불러오는 중…")
        ));
    }
    if !state.error.is_empty() {
        out.push_str(&format!("<p role=\"alert\">{}</p>", escape(&state.error)));
    }
    if let Some(snapshot) = &state.snapshot {
        if view == View::Otlp {
            out.push_str(&format!(
                "<p role=\"status\">{}: {}</p><div class=\"field\"><label for=\"service-observe-endpoint\">{}</label><input id=\"service-observe-endpoint\" type=\"url\" maxlength=\"2048\" autocomplete=\"off\" value=\"{}\"{}></div><div class=\"wasm-actions\">{}</div><p>{}</p><p>{}</p><p>{}</p>",
                tr(app, "Status", "상태"),
                escape(text(&snapshot["otlp"], "status")),
                tr(app, "Collector base URL", "수집기 기본 URL"),
                escape(&state.endpoint),
                if state.pending { " disabled" } else { "" },
                view_button(tr(app, "Save", "저장"), "service-observe-save", Icon::Save, None, state.pending),
                tr(app, "Signal paths are added automatically. Clear the URL to disable export.", "신호별 경로는 자동으로 추가한다. URL을 비우면 내보내기를 끈다."),
                tr(app, "Platform export applies immediately. App settings apply after Deploy or VM restart.", "플랫폼 내보내기는 즉시 적용한다. 앱 설정은 Deploy 또는 VM 재시작 후 적용한다."),
                tr(app, "Saving sends platform logs and events to this collector. App logs can contain sensitive values.", "저장하면 플랫폼 로그와 이벤트를 이 수집기로 보낸다. 앱 로그에는 민감한 값이 있을 수 있다.")
            ));
            let otlp = &snapshot["otlp"];
            out.push_str("<dl class=\"service-otlp-metrics\">");
            for (key, en, ko) in [
                ("queued", "Queued", "대기"),
                ("inFlight", "In flight", "전송 중"),
                ("delivered", "Delivered", "전송 완료"),
                ("dropped", "Dropped", "손실"),
                ("rejected", "Rejected", "거부"),
                ("failedBatches", "Failed batches", "전송 실패"),
            ] {
                out.push_str(&format!(
                    "<dt>{}</dt><dd>{}</dd>",
                    tr(app, en, ko),
                    otlp[key].as_u64().unwrap_or(0)
                ));
            }
            out.push_str("</dl>");
            if otlp["queuePersistent"] == false {
                out.push_str(&format!(
                    "<p>{}</p>",
                    tr(
                        app,
                        "Queued data is lost on API restart or when the endpoint is changed or disabled.",
                        "API를 재시작하거나 수집기 설정을 변경 또는 해제하면 대기 데이터가 사라진다."
                    )
                ));
            }
        } else {
            if view == View::Logs {
                let status = match text(snapshot, "logsStatus") {
                    "active" => tr(app, "Collecting logs.", "로그를 수집한다."),
                    "waiting" => tr(app, "Waiting for logs.", "로그를 기다린다."),
                    "unavailable" => tr(
                        app,
                        "Log collection is unavailable. Start a new VM or redeploy.",
                        "로그를 수집할 수 없다. 새 VM을 시작하거나 다시 배포한다.",
                    ),
                    "truncated" => tr(app, "Some logs were lost.", "일부 로그가 손실됐다."),
                    "error" => tr(app, "Log collection failed.", "로그 수집에 실패했다."),
                    _ => tr(
                        app,
                        "Log status is unavailable.",
                        "로그 상태를 확인할 수 없다.",
                    ),
                };
                out.push_str(&format!("<p role=\"status\">{}</p>", escape(status)));
            }
            out.push_str(&records(app, view, snapshot));
        }
    }
    out.push_str("</section>");
    out
}

pub fn change(app: &Shared, target: &Element) -> bool {
    if target.id() != "service-observe-endpoint" {
        return false;
    }
    if let Some(input) = target.dyn_ref::<HtmlInputElement>() {
        let mut state = app.borrow_mut();
        state.observability.endpoint = input.value();
        state.observability.dirty = true;
    }
    true
}

fn endpoint_allowed(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    let Some((scheme, rest)) = value.split_once("://") else {
        return false;
    };
    if !["http", "https"]
        .iter()
        .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
        || value.contains(['?', '#'])
        || value.chars().any(|c| c.is_whitespace() || c.is_control())
        || rest
            .split('/')
            .next()
            .is_none_or(|authority| authority.is_empty() || authority.contains('@'))
        || value.len() > 2048
    {
        return false;
    }
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::Url::new(value).is_ok_and(|url| {
            !url.hostname().is_empty() && url.username().is_empty() && url.password().is_empty()
        })
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        true
    }
}

fn otlp_valid(value: &Value) -> bool {
    value.is_object()
        && (value["endpoint"].is_null() || value["endpoint"].is_string())
        && value["status"].is_string()
        && value["queuePersistent"].is_boolean()
        && [
            "queued",
            "inFlight",
            "delivered",
            "dropped",
            "rejected",
            "failedBatches",
        ]
        .iter()
        .all(|key| value[*key].as_u64().is_some())
}

fn snapshot_valid(value: &Value) -> bool {
    ["logs", "events", "alerts"]
        .iter()
        .all(|key| value[*key].is_array())
        && ["active", "waiting", "unavailable", "truncated", "error"]
            .contains(&text(value, "logsStatus"))
        && otlp_valid(&value["otlp"])
}

pub fn dispatch(app: &Shared, action: &str) -> bool {
    if !action.starts_with("service-observe-") {
        return false;
    }
    let mut app_state = app.borrow_mut();
    if app_state.route != "services" || !app_state.panel_open || app_state.busy {
        return true;
    }
    let Some(service) = app_state
        .detail
        .as_ref()
        .map(|detail| text(detail, "id").to_owned())
        .filter(|id| !id.is_empty())
    else {
        return true;
    };
    let generation = app_state.detail_generation;
    let invalid_endpoint = tr(
        &app_state,
        "Enter an HTTP(S) endpoint without credentials, query or fragment.",
        "인증 정보, 쿼리와 fragment가 없는 HTTP(S) 엔드포인트를 입력한다.",
    );
    let state = &mut app_state.observability;
    if state.service != service || state.generation != generation {
        *state = State {
            service,
            generation,
            request: state.request.wrapping_add(1),
            ..State::default()
        };
    }
    if state.saving {
        return true;
    }
    let view = match action {
        "service-observe-logs" => Some(View::Logs),
        "service-observe-events" => Some(View::Events),
        "service-observe-alerts" => Some(View::Alerts),
        "service-observe-otlp" => Some(View::Otlp),
        _ => None,
    };
    if let Some(view) = view {
        if state.view == Some(view) {
            let request = state.request.wrapping_add(1);
            *state = State {
                request,
                ..State::default()
            };
            drop(app_state);
            crate::render_detail(app);
            return true;
        }
        state.view = Some(view);
        state.error.clear();
        let read = state.snapshot.is_none() && !state.pending;
        drop(app_state);
        crate::render_detail(app);
        if read {
            load(app, true);
        }
        return true;
    }
    if state.view.is_none() {
        return true;
    }
    if action == "service-observe-refresh" {
        if state.pending {
            return true;
        }
        drop(app_state);
        load(app, true);
        return true;
    }
    if action != "service-observe-save" || state.view != Some(View::Otlp) {
        return true;
    }
    // A background read must not discard a save initiated while it is in flight.
    if state.pending {
        state.request = state.request.wrapping_add(1);
        state.pending = false;
    }
    let endpoint = state.endpoint.trim().to_owned();
    if !endpoint_allowed(&endpoint) {
        state.error = invalid_endpoint.to_owned();
        drop(app_state);
        crate::render_detail(app);
        return true;
    }
    state.pending = true;
    state.saving = true;
    state.error.clear();
    state.request = state.request.wrapping_add(1);
    let request = state.request;
    let service = state.service.clone();
    let generation = state.generation;
    drop(app_state);
    crate::render_detail(app);
    let app = app.clone();
    spawn_local(async move {
        let body = json!({"endpoint": if endpoint.is_empty() { None } else { Some(endpoint) }});
        let result = api::request(
            "PUT",
            &format!(
                "/api/micro-services/{}/observability/otlp",
                api::encode(&service)
            ),
            Some(&body),
        )
        .await;
        let mut app_state = app.borrow_mut();
        if !matches_selection(&app_state)
            || app_state.observability.request != request
            || app_state.detail_generation != generation
        {
            return;
        }
        let failure = tr(
            &app_state,
            "Could not save OTLP settings.",
            "OTLP 설정을 저장하지 못했다.",
        )
        .to_owned();
        let state = &mut app_state.observability;
        state.pending = false;
        state.saving = false;
        match result {
            Ok(value) if otlp_valid(&value) => {
                state.endpoint = text(&value, "endpoint").to_owned();
                state.dirty = false;
                if let Some(snapshot) = &mut state.snapshot {
                    snapshot["otlp"] = value;
                }
            }
            _ => state.error = failure,
        }
        drop(app_state);
        crate::render_detail(&app);
    });
    true
}

fn load(app: &Shared, show_loading: bool) {
    let mut app_state = app.borrow_mut();
    if !matches_selection(&app_state)
        || app_state.observability.view.is_none()
        || app_state.observability.pending
    {
        return;
    }
    let state = &mut app_state.observability;
    state.pending = true;
    state.error.clear();
    state.request = state.request.wrapping_add(1);
    let request = state.request;
    let service = state.service.clone();
    let generation = state.generation;
    drop(app_state);
    if show_loading {
        crate::render_detail(app);
    }
    let app = app.clone();
    spawn_local(async move {
        let result = api::request(
            "GET",
            &format!(
                "/api/micro-services/{}/observability",
                api::encode(&service)
            ),
            None,
        )
        .await;
        let mut app_state = app.borrow_mut();
        if !matches_selection(&app_state)
            || app_state.observability.request != request
            || app_state.detail_generation != generation
        {
            return;
        }
        let failure = tr(
            &app_state,
            "Could not load observations.",
            "관측 정보를 불러오지 못했다.",
        )
        .to_owned();
        let state = &mut app_state.observability;
        state.pending = false;
        match result {
            Ok(value) if snapshot_valid(&value) => {
                if !state.dirty {
                    state.endpoint = text(&value["otlp"], "endpoint").to_owned();
                }
                state.snapshot = Some(value);
                state.error.clear();
            }
            _ => state.error = failure,
        }
        let editing = crate::document()
            .active_element()
            .is_some_and(|element| element.id() == "service-observe-endpoint");
        drop(app_state);
        if !editing {
            crate::render_detail(&app);
        }
    });
}

pub fn tick(app: &Shared) {
    let state = app.borrow();
    let editing = crate::document()
        .active_element()
        .is_some_and(|element| element.id() == "service-observe-endpoint");
    if state.busy
        || state.refreshing
        || !matches_selection(&state)
        || state.observability.view.is_none()
        || state.observability.pending
        || (state.observability.dirty && state.observability.view == Some(View::Otlp))
        || editing
    {
        return;
    }
    drop(state);
    load(app, false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_rejects_credentials_query_and_non_http_schemes() {
        for value in [
            "",
            "https://collector.example.test/v1/logs",
            "http://127.0.0.1:4318",
        ] {
            assert!(endpoint_allowed(value), "{value}");
        }
        for value in [
            "ftp://collector",
            "https://user:secret@collector",
            "https://collector/?token=secret",
            "https://collector/#fragment",
            "https://",
            "https://a b",
        ] {
            assert!(!endpoint_allowed(value), "{value}");
        }
    }
}
