//! Opt-in registry policy and monitoring controls use the existing Deploy panel.

use super::*;
use crate::render::{Icon, icon_button};
use web_sys::HtmlInputElement;

fn button(label: &str, action: &str, icon: Icon, disabled: bool) -> String {
    icon_button(label, action, "", icon).replace(
        "type=\"button\"",
        if disabled {
            "type=\"button\" disabled"
        } else {
            "type=\"button\""
        },
    )
}

pub(super) fn html(app: &App) -> String {
    let state = &app.deploy;
    let current = app.service_deployment.as_ref().unwrap();
    let visible = state.service == text(current, "serviceId") && state.auto_open;
    let mut out = button(
        tr("Automation", "자동 배포"),
        "deploy-auto",
        Icon::Edit,
        app.busy,
    );
    if !visible {
        return out;
    }
    let disabled = state.auto_loading || app.busy;
    let status = state.auto_status.as_ref().unwrap_or(&Value::Null);
    let pause = text(status, "pause");
    out.push_str(&format!("<div class=\"wasm-form\"><div class=\"field\"><label for=\"deploy-auto-reference\">{}</label><input id=\"deploy-auto-reference\" value=\"{}\" placeholder=\"ghcr.io/team/app:v1.*\" maxlength=\"2048\"{}></div><div class=\"field\"><label for=\"deploy-auto-interval\">{}</label><input id=\"deploy-auto-interval\" type=\"number\" min=\"60\" max=\"3600\" value=\"{}\"{}></div></div>", tr("Registry source", "레지스트리 소스"), escape(&state.auto_reference), if disabled { " disabled" } else { "" }, tr("Interval (seconds)", "확인 간격(초)"), escape(&state.auto_interval), if disabled { " disabled" } else { "" }));
    out.push_str(&format!("<p>{}</p><div class=\"wasm-actions\">", tr("Exact tags and stable version rules (*, v*, v1.*, v1.2.*) are supported. Rollback pauses the observed digest. A new digest or Resume permits another Deploy.", "고정 태그와 안정 버전 규칙(*, v*, v1.*, v1.2.*)을 지원한다. 롤백한 digest는 자동 배포를 중지한다. 새 digest 또는 재개로 다음 Deploy를 허용한다.")));
    out.push_str(&button(
        tr("Save", "저장"),
        "deploy-auto-save",
        Icon::Save,
        disabled || busy(current) || recovering(current),
    ));
    out.push_str(&button(
        tr("Disable", "해제"),
        "deploy-auto-disable",
        Icon::Close,
        disabled || busy(current) || recovering(current) || status["policy"].is_null(),
    ));
    out.push_str(&button(
        if pause.is_empty() {
            tr("Pause", "일시 중지")
        } else {
            tr("Resume", "재개")
        },
        if pause.is_empty() {
            "deploy-auto-pause"
        } else {
            "deploy-auto-resume"
        },
        if pause.is_empty() {
            Icon::Pause
        } else {
            Icon::Play
        },
        disabled || status["policy"].is_null(),
    ));
    out.push_str(&button(
        tr("Refresh", "새로 고침"),
        "deploy-auto-refresh",
        Icon::Refresh,
        disabled,
    ));
    out.push_str("</div>");
    let mode = if status["policy"].is_null() {
        tr("Disabled", "해제")
    } else if !pause.is_empty() {
        match pause {
            "manual" => tr("Paused", "일시 중지"),
            "rollback" => tr("Rollback pause", "롤백 후 중지"),
            _ => tr("Failure pause", "실패 후 중지"),
        }
    } else {
        tr("Monitoring", "확인 중")
    };
    out.push_str(&format!(
        "<p role=\"status\">{} · {} <code>{}</code></p>",
        mode,
        escape(text(status, "observedTag")),
        escape(text(status, "observedDigest"))
    ));
    if !text(status, "error").is_empty() {
        out.push_str(&format!(
            "<p role=\"alert\">{}</p>",
            escape(text(status, "error"))
        ));
    }
    out
}

pub(super) fn change(app: &Shared, target: &Element) -> bool {
    if !["deploy-auto-reference", "deploy-auto-interval"].contains(&target.id().as_str()) {
        return false;
    }
    if let Some(input) = target.dyn_ref::<HtmlInputElement>() {
        if target.id() == "deploy-auto-reference" {
            app.borrow_mut().deploy.auto_reference = input.value();
        } else {
            app.borrow_mut().deploy.auto_interval = input.value();
        }
    }
    true
}

pub(super) fn dispatch(app: &Shared, action: &str) -> bool {
    if !action.starts_with("deploy-auto") {
        return false;
    }
    let mut state = app.borrow_mut();
    let Some(current) = &state.service_deployment else {
        return true;
    };
    let service = text(current, "serviceId").to_owned();
    if text(&state.detail.clone().unwrap_or(Value::Null), "id") != service
        || state.route != "services"
    {
        return true;
    }
    if state.busy || state.deploy.auto_loading {
        return true;
    }
    if state.deploy.service != service {
        state.deploy = State {
            service: service.clone(),
            ..State::default()
        };
    }
    if action == "deploy-auto" && state.deploy.auto_open {
        state.deploy.auto_open = false;
        drop(state);
        draw(app);
        return true;
    }
    let initial = action == "deploy-auto";
    let (method, suffix, body) = match action {
        "deploy-auto" | "deploy-auto-refresh" => ("GET", "", None),
        "deploy-auto-disable" => ("PUT", "", Some(Value::Null)),
        "deploy-auto-save" => {
            let interval = state.deploy.auto_interval.parse::<u32>();
            if state.deploy.auto_reference.trim().is_empty()
                || !interval
                    .as_ref()
                    .is_ok_and(|value| (60..=3600).contains(value))
            {
                state.deploy.error = tr(
                    "Enter a tagged registry source and a 60–3600 second interval.",
                    "태그를 포함한 레지스트리 소스와 60–3600초 간격을 입력한다.",
                )
                .into();
                drop(state);
                draw(app);
                return true;
            }
            (
                "PUT",
                "",
                Some(
                    json!({"reference":state.deploy.auto_reference.trim(), "intervalSeconds":interval.unwrap()}),
                ),
            )
        }
        "deploy-auto-pause" => ("POST", "/pause", None),
        "deploy-auto-resume" => ("POST", "/resume", None),
        _ => return true,
    };
    if method == "PUT"
        && state
            .service_deployment
            .as_ref()
            .is_some_and(|d| busy(d) || recovering(d))
    {
        return true;
    }
    state.deploy.auto_open = true;
    state.deploy.auto_loading = true;
    state.deploy.error.clear();
    state.deploy.auto_request += 1;
    let request = state.deploy.auto_request;
    let generation = state.detail_generation;
    drop(state);
    draw(app);
    let app = app.clone();
    spawn_local(async move {
        let result = api::request(
            method,
            &format!(
                "/api/micro-services/{}/auto-deploy{suffix}",
                api::encode(&service)
            ),
            body.as_ref(),
        )
        .await;
        let mut state = app.borrow_mut();
        if state.deploy.service != service || state.deploy.auto_request != request {
            return;
        }
        state.deploy.auto_loading = false;
        if state.detail_generation != generation {
            state.deploy.auto_open = false;
            return;
        }
        match result {
            Ok(status) => {
                if initial || method == "PUT" {
                    state.deploy.auto_reference = text(&status["policy"], "reference").into();
                    state.deploy.auto_interval = status["policy"]["intervalSeconds"]
                        .as_u64()
                        .unwrap_or(300)
                        .to_string();
                }
                if method == "PUT" {
                    if let Some(current) = &mut state.service_deployment {
                        current["spec"]["autoDeploy"] = status["policy"].clone();
                    }
                    if let Some(detail) = &mut state.detail {
                        detail["autoDeploy"] = status["policy"].clone();
                    }
                }
                state.deploy.auto_status = Some(status);
            }
            Err(error) => state.deploy.error = error,
        }
        drop(state);
        draw(&app);
    });
    true
}
