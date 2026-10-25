use crate::{
    App, Shared, api, element,
    forms::Form,
    model::{escape, list, text},
    render::{button, properties},
    render_editor, render_messages, set_html, window,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::spawn_local;
use web_sys::{Element, HtmlElement, HtmlInputElement, HtmlSelectElement};

#[derive(Default)]
pub struct State {
    epoch: u64,
    request: u64,
    pub step: usize,
    accounts: Value,
    account: Value,
    repositories: Vec<String>,
    tags: Vec<String>,
    repository: String,
    tag: String,
    initial: String,
    next_page: Option<u64>,
    cursor: Option<String>,
    next_tag: Option<String>,
    loading: bool,
    error: String,
    verified: bool,
    auth: String,
    placement: Value,
    checking: bool,
    submitting: bool,
}
fn tr<'a>(en: &'a str, ko: &'a str) -> &'a str {
    if crate::i18n::t("Name") == "이름" {
        ko
    } else {
        en
    }
}
fn setup(form: &Form) -> bool {
    matches!(
        form.title.as_str(),
        "New MicroService" | "Edit MicroService"
    )
}
fn set_value(app: &Shared, key: &str, value: &str) {
    if let Some(form) = app.borrow_mut().form.as_mut() {
        for field in &mut form.fields {
            if field.key == key {
                field.value = value.into();
            }
        }
    }
    if let Some(el) = element(&format!("field-{key}")) {
        if let Some(el) = el.dyn_ref::<HtmlInputElement>() {
            el.set_value(value);
        } else if let Some(el) = el.dyn_ref::<HtmlSelectElement>() {
            el.set_value(value);
        }
    }
}
fn redraw(app: &Shared) {
    let app = app.clone();
    spawn_local(async move {
        render_editor(&app);
        sync(&app);
        render_messages(&app);
    });
}
pub fn open(app: &Shared) {
    let mut s = app.borrow_mut();
    let Some(form) = s.form.as_ref() else {
        return;
    };
    if !setup(form) && form.title != "Deploy MicroService" {
        return;
    }
    let initial = form
        .fields
        .iter()
        .find(|f| f.key == "image")
        .map(|f| f.value.clone())
        .unwrap_or_default();
    let is_setup = setup(form);
    let epoch = s.advanced.services.epoch + 1;
    s.advanced.services = State {
        epoch,
        initial,
        ..State::default()
    };
    drop(s);
    if is_setup {
        set_value(app, "image", "");
        accounts(app);
    } else {
        check(app);
    }
}
fn selector(
    id: &str,
    label: &str,
    selected: &str,
    options: Vec<(String, String)>,
    disabled: bool,
) -> String {
    let mut out = format!(
        "<div class=\"field\"><label for=\"{id}\">{label}</label><select id=\"{id}\"{}><option value=\"\">{}</option>",
        if disabled { " disabled" } else { "" },
        tr("Select…", "선택한다…")
    );
    for (value, label) in options {
        out.push_str(&format!(
            "<option value=\"{}\"{}>{}</option>",
            escape(&value),
            if value == selected { " selected" } else { "" },
            escape(&label)
        ));
    }
    out.push_str("</select></div>");
    out
}
pub fn html(app: &App, form: &Form) -> String {
    let s = &app.advanced.services;
    let step = app
        .field_errors
        .keys()
        .next()
        .map(|key| field_step(key))
        .unwrap_or(s.step);
    let mut out = form.html(app.busy, &app.field_errors);
    if setup(form) {
        let titles = [
            tr("What would you like to run?", "어떤 앱을 실행할까요?"),
            tr("How does your app respond?", "앱에 어떻게 접속할까요?"),
            tr(
                "Start with the recommended settings",
                "권장 설정으로 시작한다",
            ),
            tr("Ready to save your app?", "마지막으로 설정을 확인한다"),
        ];
        out = format!(
            "<nav id=\"service-steps\" data-step=\"{step}\" class=\"wasm-actions\" aria-label=\"{}\">{}</nav><h3 tabindex=\"-1\" id=\"service-step-title\">{}</h3>{out}",
            tr("Setup steps", "설정 단계"),
            [
                tr("App", "앱 선택"),
                tr("Connection", "접속 설정"),
                tr("Resources", "실행 설정"),
                tr("Review", "확인")
            ]
            .iter()
            .enumerate()
            .map(
                |(i, label)| button(label, "service-step", &i.to_string()).replace(
                    "type=\"button\"",
                    if i > s.step {
                        "type=\"button\" disabled"
                    } else {
                        "type=\"button\""
                    }
                )
            )
            .collect::<String>(),
            titles[step.min(3)]
        );
        out.push_str(&format!(
            "<section id=\"service-app-picker\"><p>{} <a href=\"#/pipe/auth\">{}</a></p>",
            tr(
                "Choose an app from an account connected in Pipe Auth.",
                "Pipe Auth에 연결된 계정에서 앱을 선택한다."
            ),
            tr("Manage AUTH", "AUTH 관리")
        ));
        if s.accounts.is_null() {
            out.push_str(&button("Retry accounts", "service-accounts", ""));
        } else if list(&s.accounts).is_empty() {
            out.push_str(&format!(
                "<p>{}</p><a href=\"#/pipe/auth\">{}</a>{}",
                tr("Connect an account first", "먼저 계정을 연결한다"),
                tr("Connect in AUTH", "AUTH에서 계정 연결"),
                button("Check connections", "service-accounts", "")
            ));
        } else {
            out.push_str(&selector(
                "service-account",
                tr("AUTH account", "AUTH 계정"),
                text(&s.account, "provider"),
                list(&s.accounts)
                    .iter()
                    .map(|v| {
                        (
                            text(v, "provider").into(),
                            format!(
                                "{} · {} · {}",
                                text(v, "provider"),
                                text(v, "account"),
                                text(v, "region")
                            ),
                        )
                    })
                    .collect(),
                app.busy || s.loading,
            ));
            if !s.account.is_null() {
                out.push_str(&selector(
                    "service-repository",
                    tr("App repository", "앱 저장소"),
                    &s.repository,
                    s.repositories
                        .iter()
                        .map(|v| (v.clone(), v.clone()))
                        .collect(),
                    app.busy || s.loading,
                ));
                out.push_str(&button("Retry repositories", "service-repositories", ""));
                if s.next_page.is_some() {
                    out.push_str(&button(
                        "More repositories",
                        "service-more-repositories",
                        "",
                    ));
                }
                if !s.repository.is_empty() {
                    out.push_str(&selector(
                        "service-tag",
                        tr("App version", "앱 버전"),
                        &s.tag,
                        s.tags.iter().map(|v| (v.clone(), v.clone())).collect(),
                        app.busy || s.loading,
                    ));
                    out.push_str(&button("Retry versions", "service-tags", ""));
                    if s.next_tag.is_some() {
                        out.push_str(&button("More versions", "service-more-tags", ""));
                    }
                }
            }
        }
        out.push_str("</section><div id=\"service-wizard-error\" aria-live=\"polite\"></div><section class=\"service-review\" id=\"service-review\">");
        let field = |key: &str| {
            form.fields
                .iter()
                .find(|f| f.key == key)
                .map(|f| f.value.clone())
                .unwrap_or_default()
        };
        let name = |key: &str, path: &str| {
            app.cache
                .get(path)
                .and_then(|rows| list(rows).iter().find(|row| row["id"] == field(key)))
                .map(|row| text(row, "name").to_string())
                .unwrap_or_else(|| field(key))
        };
        out.push_str(&properties(&json!({"Name":field("name"),"Image":field("image"),"App port":field("port"),"Health path":field("healthPath"),"Domain":field("domain"),"Security group":name("securityGroupId","/api/security-groups"),"Memory (MiB)":field("ram"),"CPU":field("cpu"),"Replicas":field("replicas"),"S3 connection":name("storageConnectionId","/api/pipe/storage"),"Variables / secret references":format!("{} / {}",field("env").lines().count(),field("secrets").lines().count())})));
        out.push_str("</section><div class=\"wasm-actions\">");
        out.push_str(&button("Back", "service-back", ""));
        out.push_str(&button("Continue", "service-next", ""));
        out.push_str("</div>");
    } else if form.title == "Deploy MicroService" {
        out.push_str("<div id=\"service-auth-check\"></div><div id=\"service-placement\"></div><div id=\"service-controls\"></div>");
    }
    out
}
fn show(id: &str, visible: bool) {
    if let Some(el) = element(id).and_then(|v| v.dyn_into::<HtmlElement>().ok()) {
        el.set_hidden(!visible);
    }
}
fn field_step(key: &str) -> usize {
    match key {
        "name" | "image" | "runtime" => 0,
        "port" | "healthPath" | "domain" | "securityGroupId" => 1,
        _ => 2,
    }
}
pub fn sync(app: &Shared) {
    let invalid = app.borrow().field_errors.keys().next().cloned();
    if let Some(key) = invalid {
        app.borrow_mut().advanced.services.step = field_step(&key);
    }
    let s = app.borrow();
    let Some(form) = &s.form else {
        return;
    };
    let state = &s.advanced.services;
    if setup(form) {
        for field in &form.fields {
            let step = match field.key {
                "name" => 0,
                "image" => 4,
                "port" | "healthPath" | "domain" | "securityGroupId" => 1,
                _ => 2,
            };
            if let Some(el) = element(&format!("field-{}", field.key))
                .and_then(|el| el.closest(".field").ok().flatten())
                .and_then(|el| el.dyn_into::<HtmlElement>().ok())
            {
                el.set_hidden(step != state.step);
            }
        }
        show("service-app-picker", state.step == 0);
        show("service-review", state.step == 3);
        set_html(
            "service-wizard-error",
            &if state.error.is_empty() {
                String::new()
            } else {
                format!("<p role=\"alert\">{}</p>", escape(&state.error))
            },
        );
        if let Ok(Some(next)) = crate::document().query_selector("[data-action=service-next]") {
            let next = next.unchecked_into::<web_sys::HtmlButtonElement>();
            next.set_hidden(state.step == 3);
            next.set_disabled(
                s.busy
                    || state.loading
                    || (state.step == 0 && list(&state.accounts).is_empty())
                    || (state.step >= 1 && form.base["policyError"] == true),
            );
        }
        if let Ok(Some(back)) = crate::document().query_selector("[data-action=service-back]") {
            back.unchecked_into::<HtmlElement>()
                .set_hidden(state.step == 0);
        }
        if let Ok(Some(save)) = crate::document().query_selector("#editor button[type=submit]") {
            save.unchecked_into::<HtmlElement>()
                .set_hidden(state.step != 3);
        }
    } else if form.title == "Deploy MicroService" {
        let mut auth = String::new();
        if state.auth != "connected" {
            auth = format!(
                "<p>{}</p><a href=\"#/pipe/auth\" data-action=\"service-auth\">{}</a>{}",
                if state.auth == "missing" {
                    tr(
                        "Reconnect AUTH to deploy",
                        "배포하려면 AUTH를 다시 연결한다",
                    )
                } else {
                    tr("Unable to verify AUTH", "AUTH 연결을 확인할 수 없다")
                },
                tr("Reconnect AUTH", "AUTH 다시 연결"),
                button("Check again", "service-check", "")
            );
        }
        set_html("service-auth-check", &auth);
        let mut errors = String::new();
        for (key, msg) in state.placement.as_object().into_iter().flatten() {
            let id = if key == "microNetworkId" {
                "service-deploy-network-error"
            } else {
                "service-deploy-port-error"
            };
            errors.push_str(&format!(
                "<p role=\"alert\" id=\"{id}\">{}</p>",
                escape(msg.as_str().unwrap_or_default())
            ));
            if key == "microNetworkId" {
                errors.push_str(&button(
                    "Use automatic services network",
                    "service-auto-network",
                    "",
                ));
            }
        }
        if !state.error.is_empty() {
            errors.push_str(&format!("<p role=\"alert\">{}</p>", escape(&state.error)));
            errors.push_str(&button("Retry deployment check", "service-check", ""));
        }
        set_html("service-placement", &errors);
        let owns = s.service_deployment.as_ref().is_some_and(|d| {
            !list(&d["instances"]).is_empty()
                || matches!(
                    text(d, "phase"),
                    "queued"
                        | "importing"
                        | "starting"
                        | "checking_health"
                        | "connecting_domain"
                        | "stopping"
                )
        });
        set_html(
            "service-controls",
            &if owns {
                crate::render::request(
                    "Stop service",
                    "POST",
                    &form.path.replace("/deployment", "/stop"),
                    Value::Null,
                )
            } else {
                String::new()
            },
        );
        if let Ok(Some(save)) = crate::document().query_selector("#editor button[type=submit]") {
            save.unchecked_into::<web_sys::HtmlButtonElement>()
                .set_disabled(
                    s.busy
                        || state.checking
                        || form.base["policyError"] == true
                        || state.auth != "connected"
                        || state.placement.as_object().is_none_or(|o| !o.is_empty())
                        || !state.error.is_empty()
                        || owns,
                );
        }
        for key in ["hostPort", "microNetworkId"] {
            if let Some(input) = element(&format!("field-{key}")) {
                let _ = input.set_attribute(
                    "aria-invalid",
                    if !state.placement[key].is_null() {
                        "true"
                    } else {
                        "false"
                    },
                );
            }
        }
    }
}
fn alive(app: &App, generation: u64, epoch: u64) -> bool {
    app.generation == generation
        && app.advanced.services.epoch == epoch
        && app.route == "services"
        && app.form.is_some()
}
fn accounts(app: &Shared) {
    let mut s = app.borrow_mut();
    let generation = s.generation;
    let epoch = s.advanced.services.epoch;
    s.advanced.services.loading = true;
    s.advanced.services.error.clear();
    s.advanced.services.accounts = Value::Null;
    drop(s);
    redraw(app);
    let a = app.clone();
    spawn_local(async move {
        let result = api::request("GET", "/api/pipe/auth", None).await;
        let mut s = a.borrow_mut();
        if !alive(&s, generation, epoch) {
            return;
        }
        let st = &mut s.advanced.services;
        st.loading = false;
        match result {
            Err(e) => st.error = e,
            Ok(v) => {
                st.accounts = json!(
                    list(&v)
                        .iter()
                        .filter(|a| {
                            !text(a, "registry").is_empty()
                                && list(&a["services"]).iter().any(|s| s == "registry")
                        })
                        .collect::<Vec<_>>()
                );
                st.account = list(&st.accounts)
                    .iter()
                    .find(|row| {
                        st.initial
                            .starts_with(&format!("{}/", text(row, "registry")))
                    })
                    .or_else(|| {
                        if list(&st.accounts).len() == 1 {
                            list(&st.accounts).first()
                        } else {
                            None
                        }
                    })
                    .cloned()
                    .unwrap_or(Value::Null);
            }
        }
        let selected = !st.account.is_null();
        drop(s);
        redraw(&a);
        if selected {
            repositories(&a, false);
        }
    });
}
fn repositories(app: &Shared, more: bool) {
    super::capture_form(app);
    let mut s = app.borrow_mut();
    let generation = s.generation;
    let st = &mut s.advanced.services;
    st.request += 1;
    let request = st.request;
    let epoch = st.epoch;
    st.loading = true;
    st.error.clear();
    st.tags.clear();
    st.tag.clear();
    let registry = text(&st.account, "registry").to_string();
    let page = if more { st.next_page.unwrap_or(1) } else { 1 };
    let cursor = if more { st.cursor.clone() } else { None };
    if !more {
        st.repositories.clear();
        st.repository.clear();
    }
    let mut path = format!(
        "/api/pipe/registries/{}/repositories?page={page}",
        api::encode(&registry)
    );
    if let Some(cursor) = cursor {
        path.push_str(&format!("&cursor={}", api::encode(&cursor)));
    }
    drop(s);
    set_value(app, "image", "");
    redraw(app);
    let a = app.clone();
    spawn_local(async move {
        let result = api::request("GET", &path, None).await;
        let mut s = a.borrow_mut();
        if !alive(&s, generation, epoch) || s.advanced.services.request != request {
            return;
        }
        let st = &mut s.advanced.services;
        st.loading = false;
        match result {
            Err(e) => st.error = e,
            Ok(v) => {
                if v["listingSupported"] != true {
                    st.error="The connected account cannot list repositories. Update its credentials in AUTH.".into();
                } else {
                    for name in list(&v["repositories"]).iter().filter_map(Value::as_str) {
                        if !st.repositories.iter().any(|v| v == name) {
                            st.repositories.push(name.into());
                        }
                    }
                    st.next_page = v["nextPage"].as_u64();
                    st.cursor = v["nextCursor"].as_str().map(str::to_owned);
                    let initial = st
                        .initial
                        .strip_prefix(&format!("{registry}/"))
                        .unwrap_or("")
                        .split([':', '@'])
                        .next()
                        .unwrap_or("");
                    if st.repositories.iter().any(|v| v == initial) {
                        st.repository = initial.into();
                    }
                }
            }
        }
        let selected = !st.repository.is_empty();
        drop(s);
        redraw(&a);
        if selected {
            tags(&a, false);
        }
    });
}
fn tags(app: &Shared, more: bool) {
    super::capture_form(app);
    let mut s = app.borrow_mut();
    let generation = s.generation;
    let st = &mut s.advanced.services;
    st.request += 1;
    let request = st.request;
    let epoch = st.epoch;
    st.loading = true;
    st.error.clear();
    st.tag.clear();
    if !more {
        st.tags.clear();
    }
    let prefix = format!("{}/{}", text(&st.account, "registry"), st.repository);
    let mut path = format!(
        "/api/pipe/registry-tags?reference={}",
        api::encode(&format!("{prefix}:latest"))
    );
    if more && let Some(last) = &st.next_tag {
        path.push_str(&format!("&last={}", api::encode(last)));
    }
    drop(s);
    set_value(app, "image", "");
    redraw(app);
    let a = app.clone();
    spawn_local(async move {
        let result = api::request("GET", &path, None).await;
        let mut s = a.borrow_mut();
        if !alive(&s, generation, epoch) || s.advanced.services.request != request {
            return;
        }
        let st = &mut s.advanced.services;
        st.loading = false;
        match result {
            Err(e) => st.error = e,
            Ok(v) => {
                for tag in list(&v["tags"]).iter().filter_map(Value::as_str) {
                    if !st.tags.iter().any(|v| v == tag) {
                        st.tags.push(tag.into());
                    }
                }
                st.next_tag = v["nextLast"].as_str().map(str::to_owned);
                if let Some(tag) = st.initial.strip_prefix(&format!("{prefix}:"))
                    && st.tags.iter().any(|t| t == tag)
                {
                    st.tag = tag.into();
                }
            }
        }
        let image = if st.tag.is_empty() {
            String::new()
        } else {
            format!("{prefix}:{}", st.tag)
        };
        drop(s);
        set_value(&a, "image", &image);
        redraw(&a);
    });
}
pub fn change(app: &Shared, target: &Element) -> bool {
    match target.id().as_str() {
        "service-account" => {
            let provider = target.clone().unchecked_into::<HtmlSelectElement>().value();
            super::capture_form(app);
            let mut s = app.borrow_mut();
            let st = &mut s.advanced.services;
            st.account = list(&st.accounts)
                .iter()
                .find(|row| row["provider"] == provider)
                .cloned()
                .unwrap_or(Value::Null);
            st.initial.clear();
            drop(s);
            repositories(app, false);
            true
        }
        "service-repository" => {
            super::capture_form(app);
            let mut s = app.borrow_mut();
            s.advanced.services.repository =
                target.clone().unchecked_into::<HtmlSelectElement>().value();
            s.advanced.services.initial.clear();
            drop(s);
            tags(app, false);
            true
        }
        "service-tag" => {
            super::capture_form(app);
            let mut s = app.borrow_mut();
            let st = &mut s.advanced.services;
            st.tag = target.clone().unchecked_into::<HtmlSelectElement>().value();
            st.verified = false;
            let image = if st.tags.contains(&st.tag) {
                format!(
                    "{}/{}:{}",
                    text(&st.account, "registry"),
                    st.repository,
                    st.tag
                )
            } else {
                String::new()
            };
            drop(s);
            set_value(app, "image", &image);
            redraw(app);
            true
        }
        "field-hostPort" | "field-microNetworkId"
            if app
                .borrow()
                .form
                .as_ref()
                .is_some_and(|f| f.title == "Deploy MicroService") =>
        {
            let key = target.id().trim_start_matches("field-").to_string();
            let value = target
                .dyn_ref::<HtmlInputElement>()
                .map(|input| input.value())
                .or_else(|| {
                    target
                        .dyn_ref::<HtmlSelectElement>()
                        .map(|input| input.value())
                })
                .unwrap_or_default();
            if app
                .borrow()
                .form
                .as_ref()
                .and_then(|f| f.fields.iter().find(|f| f.key == key))
                .is_some_and(|f| f.value == value)
            {
                return true;
            }
            app.borrow_mut().dirty = true;
            super::capture_form(app);
            schedule_check(app);
            true
        }
        _ => false,
    }
}
fn validate_step(app: &Shared) -> Result<(), String> {
    super::capture_form(app);
    let s = app.borrow();
    let st = &s.advanced.services;
    let get = |key| {
        s.form
            .as_ref()
            .and_then(|f| f.fields.iter().find(|f| f.key == key))
            .map(|f| f.value.as_str())
            .unwrap_or("")
    };
    if st.step == 0
        && (get("name").is_empty() || get("image").is_empty() || !st.tags.contains(&st.tag))
    {
        return Err(tr(
            "Select an app and version from a connected AUTH account.",
            "AUTH에 연결된 계정에서 앱과 버전을 선택한다.",
        )
        .into());
    }
    if st.step == 1 {
        let path = get("healthPath");
        if !path.starts_with('/')
            || path.starts_with("//")
            || path
                .bytes()
                .any(|b| !(33..=126).contains(&b) || b"?#\\".contains(&b))
        {
            return Err("Start with one / and omit spaces, query strings and fragments.".into());
        }
    }
    let mut names = BTreeSet::new();
    let mut count = 0;
    for key in ["env", "secrets"] {
        for line in get(key).lines().filter(|l| !l.trim().is_empty()) {
            let Some((name, _)) = line.split_once('=') else {
                return Err("Use NAME=value, one per line.".into());
            };
            let name = name.trim();
            if name.is_empty()
                || !name.bytes().enumerate().all(|(i, b)| {
                    b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit())
                })
                || !names.insert(name)
            {
                return Err(
                    "Use unique variable names across environment variables and secret references."
                        .into(),
                );
            }
            count += 1;
        }
    }
    if count > 64 {
        return Err("Use at most 64 variables and secret references in total.".into());
    }
    Ok(())
}
pub fn has_image_auth(image: &str, accounts: &Value) -> bool {
    let mut parts = image.split('/');
    let registry = parts.next().unwrap_or("");
    let owner = parts.next().unwrap_or("");
    let repository = parts.next();
    list(accounts).iter().any(|a| {
        list(&a["services"]).iter().any(|s| s == "registry")
            && text(a, "registry") == registry
            && match text(a, "provider") {
                "github" => {
                    registry == "ghcr.io"
                        && repository.is_some()
                        && owner.eq_ignore_ascii_case(text(a, "account"))
                }
                "aws" => true,
                _ => false,
            }
    })
}
fn verify(app: &Shared, save: bool) {
    if let Err(e) = validate_step(app) {
        app.borrow_mut().advanced.services.error = e;
        redraw(app);
        return;
    }
    let mut s = app.borrow_mut();
    if s.busy {
        return;
    }
    let generation = s.generation;
    let epoch = s.advanced.services.epoch;
    let account = s.advanced.services.account.clone();
    let image = s
        .form
        .as_ref()
        .and_then(|f| f.fields.iter().find(|f| f.key == "image"))
        .map(|f| f.value.clone())
        .unwrap_or_default();
    s.busy = true;
    drop(s);
    redraw(app);
    let a = app.clone();
    spawn_local(async move {
        let result = api::request("GET", "/api/pipe/auth", None).await;
        let mut s = a.borrow_mut();
        if !alive(&s, generation, epoch) {
            return;
        }
        s.busy = false;
        let valid = match result {
            Err(e) => {
                s.advanced.services.error = e;
                false
            }
            Ok(rows) => {
                has_image_auth(&image, &rows)
                    && list(&rows).iter().any(|a| {
                        ["provider", "account", "registry", "region"]
                            .iter()
                            .all(|key| a[*key] == account[*key])
                    })
            }
        };
        if !valid {
            if s.advanced.services.error.is_empty() {
                s.advanced.services.error =
                    "This AUTH account is no longer connected. Select a connected account again."
                        .into();
            }
            s.advanced.services.step = 0;
        } else if save {
            s.advanced.services.verified = true;
        } else {
            s.advanced.services.step = (s.advanced.services.step + 1).min(3);
            s.advanced.services.error.clear();
        }
        drop(s);
        redraw(&a);
        if valid && save && element("editor").is_some() {
            render_editor(&a);
            sync(&a);
            if let Some(form) =
                element("editor").and_then(|f| f.dyn_into::<web_sys::HtmlFormElement>().ok())
            {
                crate::save_form(a, form);
            }
        }
    });
}
pub fn before_save(app: &Shared) -> bool {
    let mut s = app.borrow_mut();
    let Some(form) = s.form.as_ref() else {
        return false;
    };
    if setup(form) {
        if s.advanced.services.step < 3 {
            return true;
        }
        if s.advanced.services.verified {
            s.advanced.services.verified = false;
            return false;
        }
        drop(s);
        verify(app, true);
        true
    } else if form.title == "Deploy MicroService" {
        if s.advanced.services.submitting {
            s.advanced.services.submitting = false;
            return false;
        }
        drop(s);
        final_check(app);
        true
    } else {
        false
    }
}
fn request_body(form: &Form) -> Value {
    let values = form
        .fields
        .iter()
        .map(|f| (f.key.to_string(), f.value.clone()))
        .collect();
    form.payload(&values).unwrap_or(Value::Null)
}
fn schedule_check(app: &Shared) {
    let mut s = app.borrow_mut();
    s.advanced.services.request += 1;
    s.advanced.services.checking = true;
    let request = s.advanced.services.request;
    let epoch = s.advanced.services.epoch;
    let generation = s.generation;
    drop(s);
    sync(app);
    let a = app.clone();
    let callback = Closure::once_into_js(move || {
        if alive(&a.borrow(), generation, epoch) && a.borrow().advanced.services.request == request
        {
            check(&a);
        }
    });
    let _ = window()
        .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), 250);
}
fn port_error(form: &Form, body: &Value) -> Option<String> {
    if text(&form.base, "domain").is_empty() {
        let p = body["hostPort"].as_u64().unwrap_or(0);
        let max = 65536 - form.base["replicas"].as_u64().unwrap_or(1);
        if p < 1024 || p > max {
            return Some(format!(
                "Choose a first host port from 1024 to {max}. All replicas need consecutive ports."
            ));
        }
    }
    None
}
pub(crate) fn check(app: &Shared) {
    super::capture_form(app);
    let mut s = app.borrow_mut();
    let Some(form) = s.form.as_ref().filter(|f| f.title == "Deploy MicroService") else {
        return;
    };
    if form.base["policyError"] == true {
        s.advanced.services.checking = true;
        drop(s);
        sync(app);
        return;
    }
    let body = request_body(form);
    let path = format!("{}/check", form.path);
    let image = s
        .detail
        .as_ref()
        .map(|v| text(v, "image").to_string())
        .unwrap_or_default();
    let error = port_error(form, &body);
    let generation = s.generation;
    let st = &mut s.advanced.services;
    st.request += 1;
    let request = st.request;
    let epoch = st.epoch;
    st.checking = true;
    st.error.clear();
    st.placement = json!({});
    if let Some(error) = error {
        st.placement = json!({"hostPort":error});
        st.checking = false;
        drop(s);
        sync(app);
        return;
    }
    drop(s);
    sync(app);
    let a = app.clone();
    spawn_local(async move {
        let auth = api::request("GET", "/api/pipe/auth", None).await;
        let networks = api::request("GET", "/api/micro-networks", None).await;
        let result = api::request_detailed("POST", &path, Some(&body), None).await;
        let mut s = a.borrow_mut();
        if !alive(&s, generation, epoch) || s.advanced.services.request != request {
            return;
        }
        let old_options = s
            .form
            .as_ref()
            .and_then(|f| f.fields.iter().find(|f| f.key == "microNetworkId"))
            .map(|f| f.options.clone());
        if let Ok(networks) = &networks {
            s.cache
                .insert("/api/micro-networks".into(), networks.clone());
            if let Some(form) = s.form.as_mut() {
                form.choose("microNetworkId", networks, "id", "name");
                if let Some(field) = form.fields.iter_mut().find(|f| f.key == "microNetworkId")
                    && !field.value.is_empty()
                    && !field.options.iter().any(|(id, _)| id == &field.value)
                {
                    field.options.push((
                        field.value.clone(),
                        format!("Unavailable network · {}", field.value),
                    ));
                }
            }
        }
        let options_changed = old_options
            != s.form
                .as_ref()
                .and_then(|f| f.fields.iter().find(|f| f.key == "microNetworkId"))
                .map(|f| f.options.clone());
        let st = &mut s.advanced.services;
        st.checking = false;
        st.auth = match auth {
            Ok(rows) => {
                if has_image_auth(&image, &rows) {
                    "connected"
                } else {
                    "missing"
                }
            }
            Err(_) => "unavailable",
        }
        .into();
        if let Err(e) = networks {
            st.error = e;
        }
        if let Err(e) = result {
            st.error = if e.fields.is_empty() {
                e.message
            } else {
                String::new()
            };
            st.placement = json!(e.fields);
        }
        drop(s);
        if options_changed {
            render_editor(&a);
        }
        sync(&a);
    });
}
fn final_check(app: &Shared) {
    super::capture_form(app);
    let mut s = app.borrow_mut();
    let Some(form) = s.form.as_ref() else {
        return;
    };
    let body = request_body(form);
    if let Some(error) = port_error(form, &body) {
        s.advanced.services.placement = json!({"hostPort":error});
        drop(s);
        sync(app);
        return;
    }
    if s.advanced.services.checking
        || s.advanced.services.auth != "connected"
        || s.advanced
            .services
            .placement
            .as_object()
            .is_none_or(|v| !v.is_empty())
    {
        return;
    }
    let path = format!("{}/check", form.path);
    let image = s
        .detail
        .as_ref()
        .map(|v| text(v, "image").to_string())
        .unwrap_or_default();
    let generation = s.generation;
    let epoch = s.advanced.services.epoch;
    s.busy = true;
    drop(s);
    sync(app);
    let a = app.clone();
    spawn_local(async move {
        let auth = api::request("GET", "/api/pipe/auth", None).await;
        let result = if auth.as_ref().is_ok_and(|rows| has_image_auth(&image, rows)) {
            api::request_detailed("POST", &path, Some(&body), None).await
        } else {
            Err(api::Failure {
                status: None,
                code: None,
                message: "Reconnect AUTH to deploy".into(),
                fields: BTreeMap::new(),
            })
        };
        let mut s = a.borrow_mut();
        if !alive(&s, generation, epoch) {
            return;
        }
        s.busy = false;
        match result {
            Ok(_) => s.advanced.services.submitting = true,
            Err(e) => {
                if e.fields.is_empty() {
                    s.advanced.services.error = e.message;
                    s.advanced.services.auth = "missing".into();
                } else {
                    s.advanced.services.placement = json!(e.fields);
                }
            }
        }
        let valid = s.advanced.services.submitting;
        drop(s);
        sync(&a);
        if valid && element("editor").is_some() {
            render_editor(&a);
            sync(&a);
            if let Some(form) =
                element("editor").and_then(|f| f.dyn_into::<web_sys::HtmlFormElement>().ok())
            {
                crate::save_form(a, form);
            }
        }
    });
}
pub fn tick(app: &Shared) {
    let s = app.borrow();
    if s.busy
        || s.advanced.services.checking
        || !s
            .form
            .as_ref()
            .is_some_and(|f| f.title == "Deploy MicroService")
    {
        return;
    }
    let owns = s.service_deployment.as_ref().is_some_and(|d| {
        !list(&d["instances"]).is_empty()
            || d["domainRouteOwned"] == true
            || matches!(
                text(d, "phase"),
                "queued"
                    | "importing"
                    | "starting"
                    | "checking_health"
                    | "connecting_domain"
                    | "stopping"
            )
    });
    drop(s);
    if owns {
        let mut s = app.borrow_mut();
        s.advanced.services.error.clear();
        s.advanced.services.placement = json!({});
        drop(s);
        sync(app);
    } else {
        check(app);
    }
}
fn draft() -> Option<Value> {
    let state = window().history().ok()?.state().ok()?;
    let value =
        js_sys::Reflect::get(&state, &JsValue::from_str("firecrabServiceDeployment")).ok()?;
    let value: Value =
        serde_json::from_str(&js_sys::JSON::stringify(&value).ok()?.as_string()?).ok()?;
    if value["serviceId"].is_string() && value["hostPort"].is_number() {
        Some(value)
    } else {
        None
    }
}
fn save_draft(value: &Value) {
    if let Ok(history) = window().history() {
        let state = history
            .state()
            .ok()
            .filter(JsValue::is_object)
            .unwrap_or_else(|| js_sys::Object::new().into());
        let value = js_sys::JSON::parse(&value.to_string()).unwrap_or(JsValue::NULL);
        let _ = js_sys::Reflect::set(
            &state,
            &JsValue::from_str("firecrabServiceDeployment"),
            &value,
        );
        let _ = history.replace_state_with_url(&state, "", None);
    }
}
fn clear_draft() {
    save_draft(&Value::Null);
}
pub fn after_refresh(app: &Shared) {
    if app.borrow().route != "services" || app.borrow().form.is_some() {
        return;
    }
    let Some(draft) = draft() else {
        return;
    };
    clear_draft();
    let id = text(&draft, "serviceId");
    if !list(&app.borrow().resource).iter().any(|s| s["id"] == id) {
        app.borrow_mut().error =
            "The service to resume no longer exists. Select another service.".into();
        render_messages(app);
        return;
    }
    crate::show_detail(app.clone(), id.into());
    crate::open_form(app.clone(), "service-deploy", id);
    set_value(app, "microNetworkId", text(&draft, "microNetworkId"));
    set_value(app, "hostPort", &draft["hostPort"].to_string());
    redraw(app);
    check(app);
}
pub fn after_mutation(app: &Shared, path: &str) {
    if app.borrow().route == "services"
        && app.borrow().form.is_none()
        && (path == "/api/micro-services"
            || app.borrow().detail.as_ref().is_some_and(|d| {
                path == format!("/api/micro-services/{}", api::encode(text(d, "id")))
            }))
    {
        let id = app
            .borrow()
            .detail
            .as_ref()
            .map(|d| text(d, "id").to_string())
            .unwrap_or_default();
        if !id.is_empty() {
            crate::open_form(app.clone(), "service-deploy", &id);
        }
        return;
    }
    if !path.starts_with("/api/pipe/auth/") {
        return;
    }
    let Some(saved) = draft() else {
        return;
    };
    let a = app.clone();
    spawn_local(async move {
        let result = api::request("GET", "/api/pipe/auth", None).await;
        if a.borrow().route != "pipe/auth" {
            return;
        }
        if result
            .as_ref()
            .is_ok_and(|rows| has_image_auth(text(&saved, "image"), rows))
        {
            let _ = window().location().set_hash("/services");
            save_draft(&saved);
        } else {
            a.borrow_mut().error="This account does not match the deployment image. Connect its GitHub account or the matching AWS account and region.".into();
            render_messages(&a);
        }
    });
}
pub fn dispatch(app: &Shared, action: &str, id: &str) -> bool {
    match action {
        "service-accounts" => accounts(app),
        "service-repositories" => repositories(app, false),
        "service-more-repositories" => repositories(app, true),
        "service-tags" => tags(app, false),
        "service-more-tags" => tags(app, true),
        "service-step" => {
            super::capture_form(app);
            let mut s = app.borrow_mut();
            let step = id.parse::<usize>().unwrap_or(0);
            if step <= s.advanced.services.step {
                s.advanced.services.step = step;
            }
            drop(s);
            redraw(app);
        }
        "service-back" => {
            super::capture_form(app);
            let mut s = app.borrow_mut();
            s.advanced.services.step = s.advanced.services.step.saturating_sub(1);
            drop(s);
            redraw(app);
        }
        "service-next" => {
            if let Err(e) = validate_step(app) {
                app.borrow_mut().advanced.services.error = e;
                redraw(app);
            } else if app.borrow().advanced.services.step == 0 {
                verify(app, false);
            } else {
                app.borrow_mut().advanced.services.step += 1;
                redraw(app);
            }
        }
        "service-check" => check(app),
        "service-auto-network" => {
            set_value(app, "microNetworkId", "");
            redraw(app);
            check(app);
        }
        "service-auth" => {
            super::capture_form(app);
            let s = app.borrow();
            let form = s.form.as_ref().unwrap();
            let body = request_body(form);
            let detail = s.detail.as_ref().unwrap();
            let draft = json!({"serviceId":detail["id"],"serviceName":detail["name"],"image":detail["image"],"microNetworkId":body["microNetworkId"].as_str().unwrap_or(""),"hostPort":body["hostPort"].as_u64().unwrap_or(30100)});
            drop(s);
            app.borrow_mut().dirty = false;
            save_draft(&draft);
            let _ = window().location().set_hash("/pipe/auth");
            save_draft(&draft);
        }
        _ => return false,
    }
    true
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn image_auth_requires_the_connected_registry_owner_and_service() {
        let a = json!([{"provider":"github","account":"Team","registry":"ghcr.io","services":["registry"]},{"provider":"aws","registry":"123.dkr.ecr.region.amazonaws.com","services":["storage"]}]);
        assert!(has_image_auth("ghcr.io/team/app:v1", &a));
        assert!(!has_image_auth("ghcr.io/team-other/app:v1", &a));
        assert!(!has_image_auth("ghcr.io/team:v1", &a));
        assert!(!has_image_auth(
            "123.dkr.ecr.region.amazonaws.com/app:v1",
            &a
        ));
    }
}
