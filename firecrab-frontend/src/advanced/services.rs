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
    /// The Size step shows its own CPU and memory boxes (the "Custom" card).
    size_custom: bool,
}
/// The five steps of a setup: (name, Korean name, title, Korean title).
pub const STEPS: [(&str, &str, &str, &str); 5] = [
    (
        "App",
        "앱 선택",
        "What would you like to run?",
        "어떤 앱을 실행할까요?",
    ),
    (
        "Access",
        "접속 방법",
        "How do people reach your app?",
        "앱에 어떻게 접속할까요?",
    ),
    (
        "Size",
        "실행 크기",
        "How large should it run?",
        "얼마나 크게 실행할까요?",
    ),
    (
        "Connections",
        "연결",
        "What should it connect to?",
        "무엇과 연결할까요?",
    ),
    ("Review", "확인", "Create it like this?", "이대로 만들까요?"),
];
const REVIEW: usize = 4;
fn tr<'a>(en: &'a str, ko: &'a str) -> &'a str {
    if crate::i18n::t("Name") == "이름" {
        ko
    } else {
        en
    }
}
fn setup(form: &Form) -> bool {
    form.is_wizard()
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
    let value = |key: &str| {
        form.fields
            .iter()
            .find(|f| f.key == key)
            .map_or("", |f| f.value.as_str())
    };
    let size_custom = size_preset(value("cpu"), value("ram")) == "custom";
    let epoch = s.advanced.services.epoch + 1;
    s.advanced.services = State {
        epoch,
        initial,
        size_custom,
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
    let step = step.min(REVIEW);
    if !setup(form) {
        let out = form.html(app.busy, &app.field_errors);
        return if form.title == "Deploy MicroService" {
            format!(
                "<div id=\"service-controls\" class=\"wasm-actions\"></div><div id=\"service-auth-check\"></div><div id=\"service-placement\"></div><div id=\"service-deploy-form\">{out}</div>"
            )
        } else {
            out
        };
    }
    // The steps are part of the form: the picker, the error line and the review come before its
    // footer, and Back and Continue are in the footer.
    let mut body = format!(
        "<section id=\"service-app-picker\" class=\"wasm-wide\"><p>{} <a href=\"#/pipe/auth\">{}</a></p>",
        tr(
            "Choose an app from an account connected in Pipe Auth.",
            "Pipe Auth에 연결된 계정에서 앱을 선택한다."
        ),
        tr("Manage AUTH", "AUTH 관리")
    );
    if s.accounts.is_null() {
        body.push_str(&button("Retry accounts", "service-accounts", ""));
    } else if list(&s.accounts).is_empty() {
        body.push_str(&format!(
            "<p>{}</p><a href=\"#/pipe/auth\">{}</a>{}",
            tr("Connect an account first", "먼저 계정을 연결한다"),
            tr("Connect in AUTH", "AUTH에서 계정 연결"),
            button("Check connections", "service-accounts", "")
        ));
    } else {
        body.push_str(&selector(
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
            body.push_str(&selector(
                "service-repository",
                tr("App repository", "앱 저장소"),
                &s.repository,
                s.repositories
                    .iter()
                    .map(|v| (v.clone(), v.clone()))
                    .collect(),
                app.busy || s.loading,
            ));
            body.push_str(&button("Retry repositories", "service-repositories", ""));
            if s.next_page.is_some() {
                body.push_str(&button(
                    "More repositories",
                    "service-more-repositories",
                    "",
                ));
            }
            if !s.repository.is_empty() {
                body.push_str(&selector(
                    "service-tag",
                    tr("App version", "앱 버전"),
                    &s.tag,
                    s.tags.iter().map(|v| (v.clone(), v.clone())).collect(),
                    app.busy || s.loading,
                ));
                body.push_str(&button("Retry versions", "service-tags", ""));
                if s.next_tag.is_some() {
                    body.push_str(&button("More versions", "service-more-tags", ""));
                }
            }
        }
    }
    body.push_str("</section><div id=\"service-wizard-error\" class=\"wasm-wide\" aria-live=\"polite\"></div><section class=\"service-review wasm-wide\" id=\"service-review\">");
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
    body.push_str(&properties(&json!({"Name":field("name"),"Image":field("image"),"App port":field("port"),"Health path":field("healthPath"),"Domain":field("domain"),"Security group":name("securityGroupId","/api/security-groups"),"Size":crate::forms::size_label(&field("cpu"),&field("ram")),"Replicas":field("replicas"),"S3 connection":name("storageConnectionId","/api/pipe/storage"),"Variables / secret references":format!("{} / {}",field("env").lines().count(),field("secrets").lines().count())})));
    body.push_str("</section>");
    body.push_str(&size_cards(
        s.size_custom || size_preset(&field("cpu"), &field("ram")) == "custom",
        &field("cpu"),
        &field("ram"),
    ));
    let ko = tr("en", "ko") == "ko";
    let nav = format!(
        "<nav id=\"service-steps\" class=\"wizard-steps\" data-step=\"{step}\" aria-label=\"{}\"><ol>{}</ol></nav>",
        tr("Setup steps", "설정 단계"),
        STEPS
            .iter()
            .enumerate()
            .map(|(i, (en, kr, _, _))| format!(
                "<li><button type=\"button\" class=\"wizard-step\" data-action=\"service-step\" data-id=\"{i}\"{}{}><span class=\"wizard-step-no\" aria-hidden=\"true\">{}</span>{}</button></li>",
                if i == step { " aria-current=\"step\"" } else { "" },
                if i > s.step { " disabled" } else { "" },
                i + 1,
                tr(en, kr)
            ))
            .collect::<String>()
    );
    let title = format!(
        "<h3 tabindex=\"-1\" id=\"service-step-title\">{}</h3>",
        tr(STEPS[step].2, STEPS[step].3)
    );
    let form_html = form.html_with(app.busy, &app.field_errors, &body);
    if app.route == "services/new" {
        let values: BTreeMap<String, String> = form
            .fields
            .iter()
            .map(|f| (f.key.to_string(), f.value.clone()))
            .collect();
        return format!(
            "<div class=\"wizard-layout\">{nav}<div class=\"wizard-main\">{title}{form_html}</div><aside class=\"wizard-summary\" aria-label=\"{label}\"><h3>{label}</h3><dl id=\"wizard-summary-list\">{}</dl></aside></div>",
            summary_list(&values, ko),
            label = tr("Summary", "요약")
        );
    }
    format!("{nav}{title}{form_html}")
}
/// "Small", "Medium", "Large" or "Custom" for a CPU and memory pair.
pub fn size_preset(cpu: &str, ram: &str) -> &'static str {
    match (cpu.trim(), ram.trim()) {
        ("1", "512") => "small",
        ("2", "1024") => "medium",
        ("4", "4096") => "large",
        _ => "custom",
    }
}
/// The size cards of the Size step. They are radios without a name, so the form does not send
/// them: the choice is written into the CPU and memory boxes.
fn size_cards(custom: bool, cpu: &str, ram: &str) -> String {
    let current = if custom {
        "custom"
    } else {
        size_preset(cpu, ram)
    };
    let cards = [
        ("small", tr("Small", "작게"), "1 vCPU · 512 MiB"),
        ("medium", tr("Medium", "보통"), "2 vCPU · 1 GiB"),
        ("large", tr("Large", "크게"), "4 vCPU · 4 GiB"),
        (
            "custom",
            tr("Custom", "직접 입력"),
            tr("Set your own values", "값을 직접 정한다"),
        ),
    ];
    let cards = cards
        .iter()
        .map(|(id, name, detail)| {
            format!(
                "<label class=\"size-card\"><input type=\"radio\" id=\"service-size-{id}\" value=\"{id}\"{}><span class=\"size-card-body\"><b>{name}</b>{}<em>{detail}</em></span></label>",
                if *id == current { " checked" } else { "" },
                if *id == "medium" {
                    format!(
                        "<span class=\"size-card-tag\">{}</span>",
                        tr("Recommended", "권장")
                    )
                } else {
                    String::new()
                }
            )
        })
        .collect::<String>();
    format!(
        "<div id=\"service-sizes\" class=\"service-sizes wasm-wide\" role=\"radiogroup\" aria-label=\"{}\">{cards}</div>",
        crate::i18n::t("Size")
    )
}
/// What the form is about to create, as label and value rows.
pub fn service_summary(values: &BTreeMap<String, String>, ko: bool) -> Vec<(&'static str, String)> {
    let get = |key: &str| values.get(key).map_or("", |v| v.trim());
    let or_none = |value: String| {
        if value.is_empty() {
            "—".to_owned()
        } else {
            value
        }
    };
    let count = |key: &str| get(key).lines().filter(|l| !l.trim().is_empty()).count();
    let port = match (get("port"), get("healthPath")) {
        ("", "") => String::new(),
        (port, "") => port.to_owned(),
        ("", path) => path.to_owned(),
        (port, path) => format!("{port} · {path}"),
    };
    let access = match get("domain") {
        "" => (if ko { "내부 전용" } else { "Internal only" }).to_owned(),
        domain => format!("{} {domain}", if ko { "도메인" } else { "Domain" }),
    };
    let size = if get("cpu").is_empty() && get("ram").is_empty() {
        String::new()
    } else {
        crate::forms::size_label(get("cpu"), get("ram"))
    };
    let connections = [
        ("env", "variables", "변수"),
        ("secrets", "secrets", "시크릿"),
        ("queues", "queues", "큐"),
    ]
    .iter()
    .filter_map(|(key, en, kr)| match count(key) {
        0 => None,
        n if ko => Some(format!("{kr} {n}개")),
        n => Some(format!("{n} {en}")),
    })
    .collect::<Vec<_>>()
    .join(" · ");
    vec![
        ("Image", or_none(get("image").to_owned())),
        ("Name", or_none(get("name").to_owned())),
        ("Port", or_none(port)),
        ("Access", access),
        ("Size", or_none(size)),
        ("Replicas", or_none(get("replicas").to_owned())),
        ("Connections", or_none(connections)),
    ]
}
/// The summary rows as the `<dt>`/`<dd>` pairs of `#wizard-summary-list`.
pub fn summary_list(values: &BTreeMap<String, String>, ko: bool) -> String {
    service_summary(values, ko)
        .iter()
        .map(|(label, value)| {
            format!(
                "<dt>{}</dt><dd>{}</dd>",
                escape(crate::i18n::t(label)),
                escape(value)
            )
        })
        .collect()
}
fn field_value(form: &Form, key: &str) -> String {
    form.fields
        .iter()
        .find(|f| f.key == key)
        .map(|f| f.value.clone())
        .unwrap_or_default()
}
fn show(id: &str, visible: bool) {
    if let Some(el) = element(id).and_then(|v| v.dyn_into::<HtmlElement>().ok()) {
        el.set_hidden(!visible);
    }
}
/// The step of the setup that holds a field.
fn field_step(key: &str) -> usize {
    match key {
        "name" | "image" | "runtime" => 0,
        "port" | "healthPath" | "domain" | "securityGroupId" => 1,
        "cpu" | "ram" | "replicas" | "startupTimeout" | "drainTimeout" => 2,
        "strategy" => REVIEW,
        _ => 3,
    }
}
pub fn owns_resources(deployment: &Value) -> bool {
    !list(&deployment["instances"]).is_empty()
        || !list(&deployment["update"]["previous"]["instances"]).is_empty()
        || deployment["domainRouteOwned"] == true
        || deployment["internalRouteOwned"] == true
        || deployment["update"]["previous"]["internalRouteOwned"] == true
        || crate::deploy::busy(deployment)
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
            // The image comes from the picker, so its own box never shows.
            let own_box = field.key != "image"
                && field_step(field.key) == state.step
                && (state.size_custom
                    || !matches!(field.key, "cpu" | "ram")
                    || size_preset(&field_value(form, "cpu"), &field_value(form, "ram"))
                        == "custom");
            if let Some(el) = element(&format!("field-{}", field.key))
                .and_then(|el| el.closest(".field").ok().flatten())
                .and_then(|el| el.dyn_into::<HtmlElement>().ok())
            {
                el.set_hidden(!own_box);
            }
        }
        show("service-app-picker", state.step == 0);
        show("service-sizes", state.step == 2);
        show("service-review", state.step == REVIEW);
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
            next.set_hidden(state.step == REVIEW);
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
                .set_hidden(state.step != REVIEW);
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
        crate::details::set_service_html("service-auth-check", &auth);
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
        crate::details::set_service_html("service-placement", &errors);
        let owns = s.service_deployment.as_ref().is_some_and(owns_resources);
        show("service-deploy-form", !owns);
        crate::details::set_service_html(
            "service-controls",
            &if owns {
                crate::render::request(
                    "Stop service",
                    "POST",
                    &form.path.replace("/deployment", "/stop"),
                    Value::Null,
                )
                .replace("type=\"button\"", "type=\"button\" id=\"service-stop\"")
            } else {
                String::new()
            },
        );
        if (s.busy
            || s.service_deployment
                .as_ref()
                .is_some_and(crate::deploy::busy))
            && let Ok(Some(stop)) = crate::document().query_selector("#service-controls button")
        {
            let _ = stop.set_attribute("disabled", "");
        }
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
        && matches!(app.route.as_str(), "services" | "services/new")
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
        "service-size-small"
        | "service-size-medium"
        | "service-size-large"
        | "service-size-custom" => {
            let preset = target.id().trim_start_matches("service-size-").to_string();
            let (cpu, ram) = match preset.as_str() {
                "small" => ("1", "512"),
                "medium" => ("2", "1024"),
                "large" => ("4", "4096"),
                _ => ("", ""),
            };
            super::capture_form(app);
            app.borrow_mut().advanced.services.size_custom = preset == "custom";
            if preset != "custom" {
                set_value(app, "cpu", cpu);
                set_value(app, "ram", ram);
            }
            app.borrow_mut().dirty = true;
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
        if p != 0 && (p < 1024 || p > max) {
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
    let owns = s.service_deployment.as_ref().is_some_and(owns_resources);
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
            let draft = json!({"serviceId":detail["id"],"serviceName":detail["name"],"image":detail["image"],"microNetworkId":body["microNetworkId"].as_str().unwrap_or(""),"hostPort":body["hostPort"].as_u64().unwrap_or(0)});
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
    fn the_setup_sections_are_inside_the_form_and_the_wizard_buttons_in_its_footer() {
        let app = App {
            route: "services".into(),
            ..Default::default()
        };
        for kind in ["services", "services-edit"] {
            let form = if kind == "services" {
                crate::forms::resource_form("services", &Value::Null, "")
            } else {
                crate::forms::resource_form("services", &json!({"name": "orders"}), "s1")
            }
            .unwrap();
            let html = html(&app, &form);
            let at = |needle: &str| {
                html.find(needle)
                    .unwrap_or_else(|| panic!("{needle}: {html}"))
            };
            // One form; the picker, the error line and the review come before its footer.
            assert_eq!(html.matches("<form ").count(), 1, "{html}");
            assert!(html.ends_with("</form>"), "{html}");
            let footer = at("form-footer wizard-footer");
            for section in [
                "id=\"service-app-picker\"",
                "id=\"service-wizard-error\"",
                "id=\"service-review\"",
            ] {
                assert!(
                    at("<form ") < at(section) && at(section) < footer,
                    "{section}: {html}"
                );
            }
            // Back and Continue are in the footer, once; nothing else follows the form.
            for button in ["service-back", "service-next"] {
                assert_eq!(html.matches(button).count(), 1, "{button}");
                assert!(at(button) > footer, "{button}");
            }
        }
    }
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

#[cfg(test)]
mod wizard_tests {
    use super::{STEPS, field_step, service_summary, size_preset, summary_list};
    use std::collections::BTreeMap;

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn maps_each_field_to_its_step() {
        assert_eq!(field_step("image"), 0);
        assert_eq!(field_step("domain"), 1);
        assert_eq!(field_step("replicas"), 2);
        assert_eq!(field_step("drainTimeout"), 2);
        assert_eq!(field_step("secrets"), 3);
        assert_eq!(field_step("storageConnectionId"), 3);
        assert_eq!(field_step("strategy"), 4);
        assert_eq!(STEPS.len(), 5);
    }

    #[test]
    fn recognises_size_presets() {
        assert_eq!(size_preset("1", "512"), "small");
        assert_eq!(size_preset("2", "1024"), "medium");
        assert_eq!(size_preset("4", "4096"), "large");
        assert_eq!(size_preset("2", "512"), "custom");
    }

    #[test]
    fn summarises_the_draft() {
        let rows = service_summary(
            &values(&[
                ("image", "nginx:1.27"),
                ("name", "nginx"),
                ("port", "80"),
                ("healthPath", "/health"),
                ("domain", ""),
                ("cpu", "2"),
                ("ram", "1024"),
                ("replicas", "2"),
                ("env", "A=1\nB=2"),
                ("secrets", ""),
                ("queues", ""),
            ]),
            false,
        );
        assert!(rows.contains(&("Size", "2 vCPU · 1 GiB".to_string())));
        assert!(rows.contains(&("Access", "Internal only".to_string())));
        assert!(rows.contains(&("Port", "80 · /health".to_string())));
        assert!(rows.contains(&("Connections", "2 variables".to_string())));
        assert!(service_summary(&values(&[]), false).contains(&("Image", "—".to_string())));
    }

    #[test]
    fn the_summary_reads_in_korean_and_escapes_values() {
        let rows = service_summary(
            &values(&[
                ("domain", "a.example.com"),
                ("env", "A=1"),
                ("secrets", "S=1"),
            ]),
            true,
        );
        assert!(rows.contains(&("Access", "도메인 a.example.com".to_string())));
        assert!(rows.contains(&("Connections", "변수 1개 · 시크릿 1개".to_string())));
        let html = summary_list(&values(&[("name", "<b>x</b>")]), false);
        assert!(html.contains("<dd>&lt;b&gt;x&lt;/b&gt;</dd>"), "{html}");
    }
}
