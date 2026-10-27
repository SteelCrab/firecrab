//! Service credentials stay in this panel's memory only until close, refresh or navigation.
use super::*;
use crate::render::{Icon, icon_button};
use web_sys::HtmlInputElement;

#[derive(Default)]
pub(super) struct State {
    open: bool,
    loading: bool,
    request: u64,
    generation: u64,
    tokens: Vec<Value>,
    issued: Option<String>,
    reveal: bool,
    name: String,
    days: String,
}

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

fn expiry(value: &Value) -> String {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(
            value.as_i64().unwrap_or(0) as f64 * 1000.,
        ))
        .to_iso_string()
        .as_string()
        .unwrap_or_default()
        .replace('T', " ")
        .replace(".000Z", " UTC")
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        value.to_string()
    }
}

pub(super) fn html(app: &App) -> String {
    let ci = &app.deploy.ci;
    let current = app.service_deployment.as_ref().unwrap();
    let mut out = button(tr("CI tokens", "CI 토큰"), "deploy-ci", Icon::Key, app.busy);
    if !ci.open
        || ci.generation != app.detail_generation
        || app.deploy.service != text(current, "serviceId")
    {
        return out;
    }
    let disabled = ci.loading || app.busy;
    out.push_str(&format!("<div class=\"wasm-form\"><div class=\"field\"><label for=\"deploy-ci-name\">{}</label><input id=\"deploy-ci-name\" maxlength=\"64\" value=\"{}\"{}></div><div class=\"field\"><label for=\"deploy-ci-days\">{}</label><select id=\"deploy-ci-days\"{}>",tr("Name","이름"),escape(&ci.name),if disabled{" disabled"}else{""},tr("Lifetime","유효 기간"),if disabled{" disabled"}else{""}));
    for days in ["7", "30", "90"] {
        out.push_str(&format!(
            "<option value=\"{days}\"{}>{days} {}</option>",
            if ci.days == days { " selected" } else { "" },
            tr("days", "일")
        ));
    }
    out.push_str("</select></div></div><div class=\"wasm-actions\">");
    out.push_str(&button(
        tr("Token", "토큰"),
        "deploy-ci-create",
        Icon::Add,
        disabled || ci.issued.is_some() || busy(current) || recovering(current),
    ));
    out.push_str(&button(
        tr("Refresh", "새로고침"),
        "deploy-ci-refresh",
        Icon::Refresh,
        disabled,
    ));
    out.push_str("</div>");
    let endpoint = format!(
        "https://{}/__firecrab/deploy",
        text(&current["spec"], "domain")
    );
    out.push_str(&format!(
        "<p class=\"prop-value wrap\">{}: <code>{}</code>{}</p>",
        tr("Endpoint", "엔드포인트"),
        escape(&endpoint),
        button(
            tr("Endpoint copy", "엔드포인트 복사"),
            "deploy-ci-url",
            Icon::Copy,
            false
        )
    ));
    if let Some(token) = &ci.issued {
        out.push_str(&format!("<p role=\"status\">{}</p><div class=\"field\"><label for=\"deploy-ci-issued\">{}</label><input id=\"deploy-ci-issued\" type=\"{}\" readonly autocomplete=\"off\" value=\"{}\"></div><div class=\"wasm-actions\">{}{}{}</div>",tr("Copy this token now. Closing or refreshing removes this value.","지금 토큰을 복사한다. 닫거나 새로고침하면 값을 지운다."),tr("Token","토큰"),if ci.reveal{"text"}else{"password"},escape(token),button(if ci.reveal{tr("Hide","숨김")}else{tr("Reveal","표시")},"deploy-ci-reveal",if ci.reveal{Icon::Hide}else{Icon::Reveal},false),button(tr("Copy","복사"),"deploy-ci-copy",Icon::Copy,false),button(tr("Dismiss","닫기"),"deploy-ci-forget",Icon::Close,false)));
    }
    if ci.loading {
        out.push_str(&format!(
            "<p role=\"status\">{}</p>",
            tr("Loading tokens…", "토큰을 불러온다…")
        ));
    }
    for token in &ci.tokens {
        out.push_str(&format!("<div><div class=\"wasm-actions\"><strong>{}</strong>{}</div><p class=\"prop-value wrap\"><code>{}</code><br>{}: {}</p></div>",escape(text(token,"name")),button(&format!("{}: {}",tr("Revocation","폐기"),text(token,"name")),&format!("deploy-ci-revoke:{}",text(token,"id")),Icon::Delete,disabled),escape(text(token,"repository")),tr("Expiry","만료"),escape(&expiry(&token["expiresAt"]))));
    }
    out
}

pub(super) fn clear(app: &mut App) {
    let ci = &mut app.deploy.ci;
    if (ci.open || ci.issued.is_some())
        && (!app.panel_open
            || app.route != "services"
            || ci.generation != app.detail_generation
            || app
                .detail
                .as_ref()
                .is_none_or(|detail| text(detail, "id") != app.deploy.service))
    {
        let request = ci.request.wrapping_add(1);
        *ci = State {
            request,
            ..State::default()
        };
    }
}

pub(super) fn change(app: &Shared, target: &Element) -> bool {
    match target.id().as_str() {
        "deploy-ci-name" => {
            if let Some(input) = target.dyn_ref::<HtmlInputElement>() {
                app.borrow_mut().deploy.ci.name = input.value();
            }
            true
        }
        "deploy-ci-days" => {
            if let Some(input) = target.dyn_ref::<HtmlSelectElement>() {
                app.borrow_mut().deploy.ci.days = input.value();
            }
            true
        }
        _ => false,
    }
}

pub(super) fn dispatch(app: &Shared, action: &str) -> bool {
    if !action.starts_with("deploy-ci") {
        return false;
    }
    let mut state = app.borrow_mut();
    let Some(current) = &state.service_deployment else {
        return true;
    };
    let service = text(current, "serviceId").to_string();
    if state.route != "services"
        || state
            .detail
            .as_ref()
            .is_none_or(|d| text(d, "id") != service)
        || state.busy
    {
        return true;
    }
    if state.deploy.service != service {
        state.deploy = super::State {
            service: service.clone(),
            ..super::State::default()
        };
    }
    let generation = state.detail_generation;
    let ci = &mut state.deploy.ci;
    if action == "deploy-ci" && ci.open {
        ci.request += 1;
        ci.open = false;
        ci.loading = false;
        ci.issued = None;
        ci.reveal = false;
        drop(state);
        draw(app);
        return true;
    }
    if action == "deploy-ci-forget" || action == "deploy-ci-reveal" {
        if action == "deploy-ci-forget" {
            ci.issued = None;
            ci.reveal = false;
        } else {
            ci.reveal = !ci.reveal;
        }
        drop(state);
        draw(app);
        return true;
    }
    if action == "deploy-ci-copy" || action == "deploy-ci-url" {
        let value = if action == "deploy-ci-copy" {
            ci.issued.clone().unwrap_or_default()
        } else {
            format!(
                "https://{}/__firecrab/deploy",
                text(
                    &state.service_deployment.as_ref().unwrap()["spec"],
                    "domain"
                )
            )
        };
        drop(state);
        if !value.is_empty() {
            spawn_local(async move {
                let _ = crate::workflows::copy(&value).await;
            });
        }
        return true;
    }
    if ci.loading {
        return true;
    }
    let (method, suffix, body) = if action == "deploy-ci-create" {
        let days = ci.days.parse::<u32>().unwrap_or(30);
        if ci.name.trim().is_empty()
            || ci.name.len() > 64
            || ![7, 30, 90].contains(&days)
            || ci.issued.is_some()
        {
            state.deploy.error = tr(
                "Enter a name and choose a valid lifetime.",
                "이름과 유효 기간을 입력한다.",
            )
            .into();
            drop(state);
            draw(app);
            return true;
        }
        (
            "POST",
            String::new(),
            Some(json!({"name":ci.name.trim(),"expiresInSeconds":days*86400})),
        )
    } else if let Some(id) = action.strip_prefix("deploy-ci-revoke:") {
        ("DELETE", format!("/{}", api::encode(id)), None)
    } else if action == "deploy-ci" || action == "deploy-ci-refresh" {
        ("GET", String::new(), None)
    } else {
        return true;
    };
    ci.open = true;
    ci.loading = true;
    ci.issued = None;
    ci.reveal = false;
    ci.generation = generation;
    ci.request += 1;
    if ci.days.is_empty() {
        ci.days = "30".into();
    }
    let request = ci.request;
    state.deploy.error.clear();
    drop(state);
    draw(app);
    let app = app.clone();
    spawn_local(async move {
        let path = format!(
            "/api/micro-services/{}/deploy-tokens",
            api::encode(&service)
        );
        let result = api::request(method, &format!("{path}{suffix}"), body.as_ref()).await;
        let result = if method == "DELETE" && result.is_ok() {
            api::request("GET", &path, None).await
        } else {
            result
        };
        let mut state = app.borrow_mut();
        if state.deploy.service != service
            || state.detail_generation != generation
            || state.deploy.ci.request != request
            || !state.deploy.ci.open
        {
            return;
        }
        state.deploy.ci.loading = false;
        match result {
            Ok(value) if method == "POST" => {
                state.deploy.ci.issued = value["token"].as_str().map(str::to_string);
                state
                    .deploy
                    .ci
                    .tokens
                    .insert(0, value["credential"].clone());
            }
            Ok(value) => state.deploy.ci.tokens = list(&value).to_vec(),
            Err(error) => state.deploy.error = error,
        }
        drop(state);
        draw(&app);
    });
    true
}
