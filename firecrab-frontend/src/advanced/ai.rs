use crate::{
    App, Shared, api,
    model::{escape, list, text},
    render::button,
    render_content, window,
};
use serde_json::{Value, json};
use wasm_bindgen::{JsCast, closure::Closure};
use wasm_bindgen_futures::spawn_local;
use web_sys::{Element, HtmlInputElement, HtmlSelectElement};

#[derive(Default)]
pub struct State {
    id: String,
    name: String,
    key: String,
    provider: String,
    authentication: String,
    busy: bool,
    error: String,
    notice: String,
    login: Option<Value>,
    deadline: f64,
}
fn tr<'a>(en: &'a str, ko: &'a str) -> &'a str {
    if crate::i18n::t("Name") == "이름" {
        ko
    } else {
        en
    }
}
fn action(en: &str, ko: &str, action: &str, id: &str, disabled: bool) -> String {
    button(tr(en, ko), action, id).replace(
        "type=\"button\"",
        if disabled {
            "type=\"button\" disabled"
        } else {
            "type=\"button\""
        },
    )
}
pub fn content(app: &App) -> String {
    let s = &app.advanced.ai;
    let editing = !s.id.is_empty();
    let locked = s.busy || s.login.is_some();
    let rows = list(&app.resource);
    let mut out = format!(
        "<p><a href=\"#/pipe\">← Pipe</a></p><section class=\"pipe-ai\" aria-label=\"Pipe AI\"><header class=\"pipe-ai-heading\"><h2>Pipe AI</h2><p>{}</p>{}</header>",
        tr(
            "Connect and manage your AI accounts.",
            "AI 서비스 계정을 연결하고 관리한다."
        ),
        if !rows.is_empty() {
            action(
                "Add connection",
                "연결 추가",
                "ai-edit-chatgpt",
                "",
                locked || editing || !app.loaded,
            )
        } else {
            String::new()
        }
    );
    let error = if s.error.is_empty() {
        &app.connection_error
    } else {
        &s.error
    };
    if !error.is_empty() {
        out.push_str(&format!("<p role=\"alert\">{}</p>", escape(error)));
    }
    if !s.notice.is_empty() {
        out.push_str(&format!("<p role=\"status\">{}</p>", escape(&s.notice)));
    }
    if !app.loaded {
        out.push_str("<p role=\"status\">Loading connections…</p>");
    } else if !app.connection_error.is_empty() {
        out.push_str(&action("Retry", "다시 시도", "ai-retry", "", locked));
    } else if rows.is_empty() && !editing {
        out.push_str(&format!("<div class=\"pipe-ai-welcome\"><h3>{}</h3><p>{}</p><div class=\"wasm-actions\">{}{}</div><p>{}</p></div>", tr("Connect your AI account", "AI 계정을 연결한다"), tr("Sign in with ChatGPT, or connect an OpenAI or Anthropic API key.", "ChatGPT 로그인 또는 OpenAI·Anthropic API 키로 연결한다."),action("Connect ChatGPT", "ChatGPT 연결", "ai-edit-chatgpt", "", locked),action("Use API key", "API 키로 연결", "ai-edit-key", "", locked),tr("Credentials are stored only on the API host.", "인증 정보는 API 호스트에서만 저장한다.")));
    }
    if editing {
        let disabled = if locked { " disabled" } else { "" };
        let chatgpt = s.authentication == "chatgpt";
        out.push_str(&format!("<form id=\"ai-editor\" class=\"panel pipe-ai-editor\" aria-label=\"{}\"><div class=\"pipe-ai-section-head\"><h3>{}</h3>{}</div><fieldset class=\"pipe-ai-authentication\"{disabled}><legend>{}</legend><div class=\"pipe-ai-auth-options\">",tr("Connect AI", "AI 연결"),tr("Connect an account", "계정 연결"),action("Cancel", "취소", "ai-cancel", "", locked),tr("Authentication", "인증 방식")));
        for (value, label) in [("chatgpt", "ChatGPT"), ("api_key", tr("API key", "API 키"))] {
            out.push_str(&format!("<label><input type=\"radio\" name=\"ai-authentication\" value=\"{value}\"{}><span>{label}</span></label>",if s.authentication == value {" checked"}else{""}));
        }
        out.push_str("</div></fieldset>");
        if !chatgpt {
            out.push_str(&format!("<div class=\"field\"><label for=\"ai-provider\">{}</label><select id=\"ai-provider\"{disabled}><option value=\"openai\"{}>OpenAI</option><option value=\"anthropic\"{}>Anthropic (Claude)</option></select></div>",tr("Provider","서비스"),if s.provider=="openai"{" selected"}else{""},if s.provider=="anthropic"{" selected"}else{""}));
        }
        out.push_str(&format!("<div class=\"field\"><label for=\"ai-name\">{}</label><input id=\"ai-name\" required maxlength=\"100\" value=\"{}\"{disabled}></div>",tr("Connection name","연결 이름"),escape(&s.name)));
        if !chatgpt {
            out.push_str(&format!("<div class=\"field\"><label for=\"ai-key\">{} {}</label><input id=\"ai-key\" type=\"password\" autocomplete=\"off\" spellcheck=\"false\" required maxlength=\"4096\" value=\"{}\"{disabled}></div>",if s.provider=="anthropic"{"Anthropic"}else{"OpenAI"},tr("API key","API 키"),escape(&s.key)));
        }
        if let Some(login) = &s.login {
            out.push_str(&format!("<p role=\"status\">{} <a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">{}</a></p>",tr("Waiting for sign-in…","로그인을 기다리는 중…"),escape(text(login,"authorizationUrl")),tr("Open ChatGPT sign-in","ChatGPT 로그인 열기")));
        }
        out.push_str(&format!(
            "<button type=\"submit\"{disabled}>{}</button>",
            if chatgpt {
                "Continue with ChatGPT"
            } else {
                tr("Verify and save", "검증 후 저장")
            }
        ));
        if chatgpt {
            out.push_str(&format!("<details><summary>{}</summary><p>{}</p></details>",tr("Sign-in help","로그인 안내"),tr("The browser must reach the local API callback at 127.0.0.1. For a remote API host, forward its port before signing in.","브라우저에서 127.0.0.1의 로컬 API 콜백에 접근할 수 있어야 한다. 원격 API 호스트는 로그인 전에 포트를 전달한다.")));
        }
        out.push_str("</form>");
    }
    if !rows.is_empty() {
        out.push_str(&format!("<div class=\"panel pipe-ai-accounts\" aria-label=\"{}\"><h3>{}</h3><ul class=\"pipe-ai-connections\">",tr("AI connections","AI 연결 목록"),tr("Connected accounts","연결한 계정")));
        for row in rows {
            let chatgpt = row["authentication"] == "chatgpt";
            let method = if chatgpt {
                tr("ChatGPT account", "ChatGPT 계정").into()
            } else {
                format!(
                    "{} {}",
                    if row["provider"] == "anthropic" {
                        "Anthropic"
                    } else {
                        "OpenAI"
                    },
                    tr("API key", "API 키")
                )
            };
            let id = text(row, "id");
            out.push_str(&format!("<li><div class=\"pipe-ai-account-info\"><strong>{}</strong><p class=\"pipe-ai-hint\">{}</p></div><span>{}</span><div class=\"pipe-ai-manage\">{}{}</div></li>",escape(text(row,"name")),method,tr("Connected","연결됨"),if chatgpt{action("Sign in again","다시 로그인","ai-edit-chatgpt",id,locked||editing)}else{action("Replace API key","API 키 교체","ai-edit-key",id,locked||editing)},action("Disconnect","연결 해제","ai-disconnect",id,locked||editing)));
        }
        out.push_str("</ul></div>");
        if !editing {
            out.push_str(&format!(
                "<p>{}</p>",
                tr(
                    "Credentials stay on the server.",
                    "인증 정보는 서버에서 관리한다."
                )
            ));
            if rows.iter().any(|row| row["authentication"] == "chatgpt") {
                out.push_str(&format!("<a href=\"https://chatgpt.com/#settings/Usage\" target=\"_blank\" rel=\"noreferrer\">{}</a>",tr("Manage ChatGPT usage","ChatGPT 사용량 관리")));
            }
        }
    }
    out.push_str("</section>");
    out
}
pub fn input(app: &Shared, target: &Element) -> bool {
    if !["ai-name", "ai-key"].contains(&target.id().as_str()) {
        return false;
    }
    let value = target.clone().unchecked_into::<HtmlInputElement>().value();
    let mut state = app.borrow_mut();
    if target.id() == "ai-name" {
        state.advanced.ai.name = value;
    } else {
        state.advanced.ai.key = value;
    }
    true
}
pub fn change(app: &Shared, target: &Element) -> bool {
    let authentication = target.get_attribute("name").as_deref() == Some("ai-authentication");
    if target.id() != "ai-provider" && !authentication {
        return input(app, target);
    }
    let mut state = app.borrow_mut();
    let s = &mut state.advanced.ai;
    if s.busy || s.login.is_some() {
        return true;
    }
    if authentication {
        s.authentication = target.clone().unchecked_into::<HtmlInputElement>().value();
        s.provider = "openai".into();
    } else {
        s.provider = target.clone().unchecked_into::<HtmlSelectElement>().value();
    }
    s.key.clear();
    if ["ChatGPT", "OpenAI", "Anthropic"].contains(&s.name.as_str()) {
        s.name = if s.authentication == "chatgpt" {
            "ChatGPT"
        } else if s.provider == "anthropic" {
            "Anthropic"
        } else {
            "OpenAI"
        }
        .into();
    }
    drop(state);
    let app = app.clone();
    spawn_local(async move {
        render_content(&app);
    });
    true
}
fn uuid() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    window()
        .crypto()
        .map_err(api::error)?
        .get_random_values_with_u8_array(&mut bytes)
        .map_err(api::error)?;
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}
pub fn dispatch(app: &Shared, action: &str, id: &str) -> bool {
    if !action.starts_with("ai-") {
        return false;
    }
    if app.borrow().advanced.ai.busy || app.borrow().advanced.ai.login.is_some() {
        return true;
    }
    match action {
        "ai-retry" => crate::refresh(app.clone()),
        "ai-cancel" => {
            let mut state = app.borrow_mut();
            state.advanced.ai = State::default();
            state.dirty = false;
            drop(state);
            render_content(app);
        }
        "ai-edit-key" | "ai-edit-chatgpt" => {
            let mut state = app.borrow_mut();
            let row = list(&state.resource)
                .iter()
                .find(|row| row["id"] == id)
                .cloned()
                .unwrap_or(Value::Null);
            let method = if action == "ai-edit-key" {
                "api_key"
            } else {
                "chatgpt"
            };
            let id = if id.is_empty() { uuid() } else { Ok(id.into()) };
            match id {
                Err(error) => state.advanced.ai.error = error,
                Ok(id) => {
                    state.advanced.ai = State {
                        id,
                        name: if row.is_null() {
                            if method == "chatgpt" {
                                "ChatGPT"
                            } else {
                                "OpenAI"
                            }
                            .into()
                        } else {
                            text(&row, "name").into()
                        },
                        provider: if row.is_null() {
                            "openai".into()
                        } else {
                            text(&row, "provider").into()
                        },
                        authentication: method.into(),
                        ..State::default()
                    };
                    state.dirty = true;
                }
            }
            drop(state);
            render_content(app);
        }
        "ai-disconnect" => write(app, "DELETE", id, None),
        _ => {}
    }
    true
}
pub fn submit(app: &Shared) {
    let state = app.borrow();
    let s = &state.advanced.ai;
    if s.id.is_empty() || s.busy || s.login.is_some() {
        return;
    }
    let method = if s.authentication == "chatgpt" {
        "POST"
    } else {
        "PUT"
    };
    let body = if method == "POST" {
        json!({"name":s.name.trim()})
    } else {
        json!({"name":s.name.trim(),"provider":s.provider,"apiKey":s.key})
    };
    let id = s.id.clone();
    drop(state);
    write(app, method, &id, Some(body));
}
fn write(app: &Shared, method: &'static str, id: &str, body: Option<Value>) {
    let generation = app.borrow().generation;
    {
        let mut state = app.borrow_mut();
        state.advanced.ai.busy = true;
        state.advanced.ai.error.clear();
        state.advanced.ai.notice.clear();
    }
    render_content(app);
    let path = format!(
        "/api/pipe/ai/connections/{}{}",
        api::encode(id),
        if method == "POST" { "/login" } else { "" }
    );
    let app = app.clone();
    let id = id.to_string();
    spawn_local(async move {
        let result = api::request(method, &path, body.as_ref()).await;
        let mut state = app.borrow_mut();
        if state.generation != generation || state.route != "pipe/ai" {
            return;
        }
        state.advanced.ai.busy = false;
        let mut poll = false;
        match result {
            Err(error) => state.advanced.ai.error = error,
            Ok(value) if method == "POST" => {
                let url = text(&value, "authorizationUrl");
                if !url.starts_with("https://auth.openai.com/") {
                    state.advanced.ai.error = "Invalid ChatGPT authorization URL.".into();
                } else {
                    let _ = window().open_with_url_and_target_and_features(
                        url,
                        "_blank",
                        "noopener,noreferrer",
                    );
                    state.advanced.ai.deadline =
                        js_sys::Date::now() + value["expiresIn"].as_f64().unwrap_or(600.) * 1000.;
                    state.advanced.ai.login = Some(value);
                    poll = true;
                }
            }
            Ok(value) => {
                let mut rows = list(&state.resource).to_vec();
                rows.retain(|row| row["id"] != id);
                if method != "DELETE" {
                    rows.push(value.clone());
                    state.advanced.ai = State::default();
                    state.dirty = false;
                }
                state.resource = json!(rows);
                state.advanced.ai.notice=if method!="DELETE"{tr("Account verified and saved.","계정 연결을 확인하고 저장했다.")}else if value["remoteRevocationConfirmed"]==false{tr("Disconnected locally. Remote revocation was not confirmed. Disconnect Firecrab in ChatGPT settings.","로컬 연결을 해제했다. 원격 토큰 폐기는 확인하지 못했다. ChatGPT 설정에서 Firecrab 연결을 해제한다.")}else{tr("Account disconnected.","계정 연결을 해제했다.")}.into();
            }
        }
        drop(state);
        render_content(&app);
        if poll {
            poll_login(app, generation);
        }
    });
}
fn poll_login(app: Shared, generation: u64) {
    let Some(login) = app.borrow().advanced.ai.login.clone() else {
        return;
    };
    let id = text(&login, "loginId").to_string();
    spawn_local(async move {
        let result = api::request(
            "GET",
            &format!("/api/pipe/ai/logins/{}", api::encode(&id)),
            None,
        )
        .await;
        let mut state = app.borrow_mut();
        if state.generation != generation
            || state.route != "pipe/ai"
            || state
                .advanced
                .ai
                .login
                .as_ref()
                .is_none_or(|login| login["loginId"] != id)
        {
            return;
        }
        let mut again = false;
        let mut connected = false;
        match result {
            Err(error) => {
                state.advanced.ai.error = error;
                state.advanced.ai.login = None;
            }
            Ok(value) if value["status"] == "connected" => {
                state.advanced.ai = State::default();
                state.dirty = false;
                connected = true;
                state.advanced.ai.notice =
                    tr("ChatGPT account connected.", "ChatGPT 계정을 연결했다.").into();
            }
            Ok(value) if value["status"] == "failed" => {
                state.advanced.ai.error = value["message"]
                    .as_str()
                    .unwrap_or(tr("ChatGPT login failed.", "ChatGPT 로그인에 실패했다."))
                    .into();
                state.advanced.ai.login = None;
            }
            Ok(_) if js_sys::Date::now() >= state.advanced.ai.deadline => {
                state.advanced.ai.error = tr(
                    "ChatGPT login expired. Start again.",
                    "ChatGPT 로그인이 만료됐다. 다시 시작한다.",
                )
                .into();
                state.advanced.ai.login = None;
            }
            Ok(_) => again = true,
        }
        drop(state);
        render_content(&app);
        if connected {
            crate::refresh(app.clone());
        }
        if again {
            let callback = Closure::once_into_js(move || {
                if app.borrow().generation == generation {
                    poll_login(app, generation);
                }
            });
            let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.unchecked_ref(),
                1500,
            );
        }
    });
}
