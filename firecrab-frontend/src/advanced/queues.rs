//! Queue lifecycle and short-lived credential reveals. Secrets never enter the cache.
use crate::{
    App, Shared, api, element,
    forms::Field,
    model::{escape, list, text},
    render::{Icon, button, chips, empty_state, icon_button, status_dot},
    render_content, render_editor, render_messages, set_html,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{Element, FormData, HtmlFormElement, HtmlInputElement, HtmlSelectElement};

#[derive(Default)]
pub struct State {
    selected: String,
    metrics: Value,
    reveal: Option<(String, f64)>,
    reveal_generation: u64,
    setup: Setup,
    errors: BTreeMap<String, String>,
}
struct Setup {
    network: String,
    cpu: String,
    ram: String,
    disk: String,
}
impl Default for Setup {
    fn default() -> Self {
        Self {
            network: String::new(),
            cpu: "4".into(),
            ram: "4096".into(),
            disk: "8".into(),
        }
    }
}
fn title(value: &str) -> String {
    let mut chars = value.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}
fn tr(ko: bool, en: &'static str, local: &'static str) -> &'static str {
    if ko { local } else { en }
}
fn input(name: &str, label: &str, value: &str, min: u32, max: u32) -> String {
    format!(
        "<label class=\"field\"><span class=\"field-label\">{label}</span><input class=\"input\" name=\"{name}\" type=\"number\" value=\"{}\" min=\"{min}\" max=\"{max}\" required></label>",
        escape(value)
    )
}
fn controls_disabled(app: &App) -> &'static str {
    if app.busy || !app.connection_error.is_empty() {
        "disabled"
    } else {
        ""
    }
}
fn phase_tone(phase: &str) -> &'static str {
    match phase {
        "running" | "ready" => "ok",
        "stopped" => "off",
        "failed" => "error",
        _ => "busy",
    }
}
fn phase_label(phase: &str, ko: bool) -> String {
    if !ko {
        return title(phase);
    }
    match phase {
        "running" => "실행 중",
        "ready" => "사용 가능",
        "stopped" => "중지됨",
        "failed" => "실패",
        "installing" => "설치 중",
        "provisioning" => "준비 중",
        "starting" => "시작 중",
        "stopping" => "중지 중",
        _ => phase,
    }
    .to_owned()
}
fn setup(app: &App, disabled: &str) -> String {
    let ko = app.locale == "ko";
    let draft = &app.advanced.queues.setup;
    let mut options = format!(
        "<option value=\"\">{}</option>",
        tr(ko, "Select a network", "네트워크 선택")
    );
    for network in list(app.cache.get("/api/micro-networks").unwrap_or(&Value::Null)) {
        options.push_str(&format!(
            "<option value=\"{}\"{}>{}</option>",
            escape(text(network, "id")),
            if text(network, "id") == draft.network {
                " selected"
            } else {
                ""
            },
            escape(text(network, "name"))
        ));
    }
    let errors = setup_errors(&app.advanced.queues.errors, ko);
    format!(
        "<section class=\"queue-setup\" aria-labelledby=\"queue-setup-title\"><div class=\"queue-setup-copy\"><span class=\"queue-engine-icon\" aria-hidden=\"true\">{icon}</span><span class=\"queue-eyebrow\">RabbitMQ</span><h2 id=\"queue-setup-title\">{heading}</h2><p>{description}</p><p class=\"queue-setup-note\">{note}</p></div><form id=\"queue-engine-form\" class=\"queue-form\"><fieldset {disabled}><div class=\"queue-section-head\"><h3>{resources}</h3><p>{resource_note}</p></div>{errors}<label class=\"field\"><span class=\"field-label\">MicroNetwork</span><select class=\"input\" name=\"microNetworkId\" required>{options}</select></label><div class=\"queue-engine-resources\">{cpu}{ram}{disk}</div><div class=\"queue-form-footer\"><button type=\"submit\" class=\"btn primary\">{install}</button></div></fieldset></form></section>",
        icon = crate::render::navigation_icon("queues"),
        heading = tr(ko, "Set up your queue engine", "큐 엔진 설치"),
        description = tr(
            ko,
            "One RabbitMQ engine handles the queues your services use to send and receive messages.",
            "RabbitMQ 엔진 하나로 서비스 간 메시지를 보내고 받는 큐를 관리합니다."
        ),
        note = tr(
            ko,
            "The engine runs in a dedicated MicroVM. Install it once, then create your queues.",
            "엔진은 전용 MicroVM에서 실행됩니다. 한 번 설치한 뒤 큐를 생성합니다."
        ),
        resources = tr(ko, "Engine resources", "엔진 리소스"),
        resource_note = tr(
            ko,
            "Choose the network and resources for the engine.",
            "엔진이 사용할 네트워크와 리소스를 선택합니다."
        ),
        cpu = input("cpu", "CPU", &draft.cpu, 1, 32),
        ram = input(
            "ram",
            tr(ko, "Memory (MiB)", "메모리 (MiB)"),
            &draft.ram,
            128,
            32768
        ),
        disk = input(
            "diskGb",
            tr(ko, "Disk (GiB)", "디스크 (GiB)"),
            &draft.disk,
            4,
            1024
        ),
        install = tr(ko, "Install RabbitMQ", "RabbitMQ 설치"),
    )
}

fn setup_errors(errors: &BTreeMap<String, String>, ko: bool) -> String {
    if errors.is_empty() {
        return String::new();
    }
    let mut out = String::from("<div class=\"queue-engine-error\" role=\"alert\"><ul>");
    for (key, message) in errors {
        let label = match key.as_str() {
            "microNetworkId" => "MicroNetwork",
            "cpu" => "CPU",
            "ram" => tr(ko, "Memory", "메모리"),
            "diskGb" => tr(ko, "Disk", "디스크"),
            "storageRoot" => tr(ko, "VM disk storage", "VM 디스크 저장소"),
            _ => key,
        };
        let detail = if ko && key == "storageRoot" && message.starts_with("not enough free space") {
            "저장 공간이 부족하다. 디스크 크기를 줄이거나 저장 공간을 확보한다."
        } else if ko
            && key == "ram"
            && message == "must be a power of two between 128 and 32768 MiB"
        {
            "128~32768 MiB 사이의 2의 거듭제곱을 입력한다. 예: 1024, 2048, 4096."
        } else if ko && key == "microNetworkId" && message == "no MicroNetwork with this id exists"
        {
            "선택한 네트워크가 없다. 목록에서 다시 선택한다."
        } else {
            message
        };
        out.push_str(&format!(
            "<li><strong>{}</strong>: {}</li>",
            escape(label),
            escape(detail)
        ));
    }
    out.push_str("</ul></div>");
    out
}
fn engine_card(app: &App, engine: &Value, disabled: &str) -> String {
    let ko = app.locale == "ko";
    let phase = text(engine, "phase");
    let mut actions = format!("<fieldset class=\"queue-actions\" {disabled}>");
    if ["stopped", "failed"].contains(&phase) {
        actions.push_str(&button(
            tr(ko, "Start / recover", "시작 / 복구"),
            "queue-start",
            "",
        ));
    }
    if phase == "running" {
        actions.push_str(&icon_button("Stop", "queue-stop", "", Icon::Stop));
    }
    actions.push_str("</fieldset>");
    let mut out = format!(
        "<section class=\"queue-engine\" aria-label=\"{}\"><div class=\"queue-engine-head\"><span class=\"queue-engine-icon\" aria-hidden=\"true\">{}</span><div class=\"queue-engine-copy\"><h2>RabbitMQ</h2><p>{}</p></div>{}{actions}</div>",
        tr(ko, "Queue engine", "큐 엔진"),
        crate::render::navigation_icon("queues"),
        tr(
            ko,
            "Shared engine for your application queues",
            "애플리케이션 큐가 함께 사용하는 엔진"
        ),
        status_dot(phase_tone(phase), &phase_label(phase, ko)),
    );
    if !text(engine, "error").is_empty() {
        out.push_str(&format!(
            "<div class=\"queue-engine-error\" role=\"status\"><strong>{}</strong><p>{}</p></div>",
            tr(
                ko,
                "The engine needs attention",
                "엔진 상태를 확인해야 합니다"
            ),
            escape(text(engine, "error"))
        ));
    } else if phase != "running" {
        out.push_str(&format!(
            "<p class=\"queue-engine-note\">{}</p>",
            tr(
                ko,
                "Start the engine to create queues and exchange messages.",
                "엔진이 실행되면 큐를 생성하고 메시지를 주고받을 수 있습니다."
            )
        ));
    }
    if !text(engine, "log").is_empty() {
        out.push_str(&format!("<details class=\"queue-engine-log\" {}><summary>{}</summary><pre class=\"queue-log\">{}</pre></details>",if phase == "failed" { "open" } else { "" },tr(ko,"Installation log","설치 로그"),escape(text(engine,"log"))));
    }
    out.push_str("</section>");
    out
}
fn create_form(ready: bool, ko: bool) -> String {
    format!(
        "<section class=\"queue-create\" aria-labelledby=\"queue-create-title\"><div class=\"queue-section-head\"><h2 id=\"queue-create-title\">{heading}</h2><p>{description}</p></div><form id=\"queue-create-form\" class=\"queue-form\"><fieldset {disabled}><div class=\"queue-create-fields\"><div class=\"field\"><label for=\"queue-name\">{name}</label><input class=\"input\" id=\"queue-name\" name=\"name\" required pattern=\"[a-z0-9-]{{1,40}}\" maxlength=\"40\" placeholder=\"notifications\" autocomplete=\"off\" aria-describedby=\"queue-name-hint\"><p class=\"field-hint\" id=\"queue-name-hint\">{name_hint}</p></div>{delivery}{capacity}</div><div class=\"queue-form-footer\"><span class=\"queue-create-note\">{note}</span><button class=\"btn primary\" type=\"submit\">{submit}</button></div></fieldset></form></section>",
        heading = tr(ko, "Create a queue", "큐 생성"),
        description = tr(
            ko,
            "Name the queue and set its message limits.",
            "큐 이름과 메시지 한도를 설정합니다."
        ),
        disabled = if ready { "" } else { "disabled" },
        name = tr(ko, "Queue name", "큐 이름"),
        name_hint = tr(
            ko,
            "Lowercase letters, numbers and hyphens. Up to 40 characters.",
            "영문 소문자, 숫자, 하이픈을 사용합니다. 최대 40자입니다."
        ),
        delivery = input(
            "deliveryLimit",
            tr(ko, "Delivery limit", "전달 한도"),
            "5",
            1,
            100
        ),
        capacity = input(
            "maxMib",
            tr(ko, "Capacity per queue (MiB)", "큐 용량 (MiB)"),
            "64",
            1,
            1024
        ),
        note = if ready {
            tr(
                ko,
                "Connect a MicroService after creating the queue.",
                "큐를 생성한 뒤 MicroService에 연결합니다.",
            )
        } else {
            tr(
                ko,
                "The engine must be running to create a queue.",
                "엔진이 실행 중이어야 큐를 생성할 수 있습니다.",
            )
        },
        submit = tr(ko, "Create Queue", "큐 생성"),
    )
}
pub fn content(app: &App) -> String {
    let ko = app.locale == "ko";
    let engine = app.cache.get("/api/queue/engine").unwrap_or(&Value::Null);
    let disabled = controls_disabled(app);
    if engine.is_null() {
        return format!("<div class=\"queue-page\">{}</div>", setup(app, disabled));
    }
    let queues = list(&app.resource);
    let ready = text(engine, "phase") == "running" && disabled.is_empty();
    let mut out = format!(
        "<div class=\"queue-page\">{}{}<section class=\"queue-list\" aria-labelledby=\"queue-list-title\"><div class=\"queue-section-head queue-list-head\"><h2 id=\"queue-list-title\">{} <span class=\"queue-count\">{}</span></h2><p>{}</p></div>",
        engine_card(app, engine, disabled),
        create_form(ready, ko),
        tr(ko, "Queues", "큐"),
        queues.len(),
        tr(
            ko,
            "Select a queue to view its activity and connection details.",
            "큐를 선택하면 메시지 현황과 연결 정보를 확인할 수 있습니다."
        )
    );
    if queues.is_empty() {
        out.push_str(&empty_state(
            "queues",
            tr(ko, "No queues yet", "아직 큐가 없습니다"),
            tr(
                ko,
                "Your queues will appear here after you create one.",
                "큐를 생성하면 이곳에 표시됩니다.",
            ),
            "",
        ));
    } else {
        out.push_str(&format!("<div class=\"table-scroll\"><table class=\"wasm-table queue-table\" aria-label=\"{}\"><colgroup><col><col class=\"queue-col-status\"><col class=\"queue-col-services col-secondary\"><col class=\"queue-col-actions\"></colgroup><thead><tr><th>{}</th><th>{}</th><th class=\"col-secondary\">{}</th><th class=\"row-actions\"><span class=\"sr-only\">{}</span></th></tr></thead><tbody>",tr(ko,"Queues","큐"),tr(ko,"Name","이름"),tr(ko,"Status","상태"),tr(ko,"Used by","사용 서비스"),crate::i18n::t("Actions")));
        for queue in queues {
            let id = text(queue, "id");
            let phase = text(queue, "phase");
            let services = list(&queue["services"]);
            let used = services
                .iter()
                .map(|service| {
                    [text(service, "name"), text(service, "id")]
                        .into_iter()
                        .find(|v| !v.is_empty())
                        .unwrap_or("MicroService")
                        .to_string()
                })
                .collect::<Vec<_>>();
            let error = text(queue, "error");
            let sub = if error.is_empty() {
                format!(
                    "{} {} · {} {} MiB",
                    tr(ko, "Delivery limit", "전달 한도"),
                    queue["deliveryLimit"],
                    tr(ko, "Max", "최대"),
                    queue["maxBytes"]
                        .as_u64()
                        .map_or(0, |bytes| bytes / 1_048_576)
                )
            } else {
                error.to_owned()
            };
            let mut actions = format!("<fieldset class=\"queue-actions\" {disabled}>");
            if phase == "failed" {
                actions.push_str(&icon_button("Retry", "queue-retry", id, Icon::Retry));
            }
            actions.push_str(&format!("<button type=\"button\" class=\"icon-btn danger\" aria-label=\"{}\" title=\"{}\" data-action=\"queue-delete\" data-id=\"{}\" {}>{}</button></fieldset>",tr(ko,"Delete","삭제"),if services.is_empty(){tr(ko,"Delete","삭제")}else{tr(ko,"Used by a MicroService. Remove its connection before deletion.","MicroService 연결을 해제한 뒤 삭제할 수 있습니다.")},escape(id),if services.is_empty(){""}else{"disabled"},Icon::Delete.svg()));
            out.push_str(&format!("<tr data-action=\"queue-select\" data-id=\"{}\" aria-selected=\"{}\"><td><div class=\"cell-name\"><b><button type=\"button\" class=\"cell-open\" data-action=\"queue-select\" data-id=\"{}\" title=\"{}\">{}</button></b><span class=\"cell-sub\" data-tone=\"{}\">{}</span></div></td><td>{}</td><td class=\"col-secondary\">{}</td><td class=\"row-actions\">{actions}</td></tr>",escape(id),app.advanced.queues.selected==id,escape(id),escape(text(queue,"name")),escape(text(queue,"name")),if error.is_empty(){""}else{"error"},escape(&sub),status_dot(phase_tone(phase),&phase_label(phase,ko)),chips(&used)));
        }
        out.push_str("</tbody></table></div>");
    }
    out.push_str("</section>");
    if let Some(queue) = queues
        .iter()
        .find(|queue| text(queue, "id") == app.advanced.queues.selected)
    {
        out.push_str(&format!("<section class=\"queue-detail\" aria-label=\"{}\"><div class=\"queue-section-head queue-detail-head\"><div><span class=\"queue-eyebrow\">{}</span><h2>{}</h2></div>{}</div><div id=\"queue-metrics\">{}</div><div class=\"queue-connections\"><div class=\"queue-section-head\"><h3>{}</h3><p>{}</p></div><fieldset class=\"queue-actions\" {disabled}>{}{}{}</fieldset>",tr(ko,"Queue detail","큐 상세"),tr(ko,"Queue detail","큐 상세"),escape(text(queue,"name")),status_dot(phase_tone(text(queue,"phase")),&phase_label(text(queue,"phase"),ko)),metrics(&app.advanced.queues.metrics,ko),tr(ko,"Connection details","연결 정보"),tr(ko,"Show the credentials for the role your application needs.","애플리케이션에 필요한 역할의 연결 정보를 확인합니다."),button(tr(ko,"Show sender connection","전송 연결 정보 보기"),"queue-reveal-publish",""),button(tr(ko,"Show receiver connection","수신 연결 정보 보기"),"queue-reveal-consume",""),button(tr(ko,"Hide","숨기기"),"queue-hide","")));
        if let Some((url, _)) = &app.advanced.queues.reveal {
            out.push_str(&format!(
                "<div class=\"queue-connection\"><code>{}</code></div>",
                escape(url)
            ));
        }
        out.push_str("</div></section>");
    }
    out.push_str("</div>");
    out
}
fn metrics(value: &Value, ko: bool) -> String {
    if value.is_null() {
        return String::new();
    }
    let mut out = "<dl class=\"queue-metrics\">".to_string();
    for (key, en, local) in [
        ("ready", "Waiting", "대기 메시지"),
        ("unacknowledged", "Unacknowledged", "처리 중 메시지"),
        ("consumers", "Consumers", "수신자"),
        ("failed", "Failed", "실패 메시지"),
    ] {
        out.push_str(&format!(
            "<div><dt>{}</dt><dd>{}</dd></div>",
            tr(ko, en, local),
            value[key].as_u64().unwrap_or(0)
        ));
    }
    out + "</dl>"
}
pub fn refresh(app: Shared) {
    let mut state = app.borrow_mut();
    if state.refreshing {
        return;
    }
    state.refreshing = true;
    let generation = state.generation;
    let selected = state.advanced.queues.selected.clone();
    drop(state);
    spawn_local(async move {
        let result = async {
            let engine = api::request("GET", "/api/queue/engine", None).await?;
            let rows = api::request("GET", "/api/queues", None).await?;
            let networks = api::request("GET", "/api/micro-networks", None).await?;
            Ok::<_, String>((engine, rows, networks))
        }
        .await;
        let measured = if result.is_ok() && !selected.is_empty() {
            api::request(
                "GET",
                &format!("/api/queues/{}/metrics", api::encode(&selected)),
                None,
            )
            .await
            .ok()
        } else {
            None
        };
        let mut state = app.borrow_mut();
        if state.generation != generation {
            return;
        }
        state.refreshing = false;
        let mut changed = !state.loaded || !state.connection_error.is_empty();
        match result {
            Ok((engine, rows, networks)) => {
                changed |= state.resource != rows
                    || state.cache.get("/api/queue/engine") != Some(&engine)
                    || state.cache.get("/api/micro-networks") != Some(&networks);
                state.resource = rows;
                state.cache.insert("/api/queue/engine".into(), engine);
                state.cache.insert("/api/micro-networks".into(), networks);
                state.connection_error.clear();
                if !state.advanced.queues.selected.is_empty()
                    && !list(&state.resource)
                        .iter()
                        .any(|q| text(q, "id") == state.advanced.queues.selected)
                {
                    state.advanced.queues.selected.clear();
                    state.advanced.queues.reveal = None;
                    state.advanced.queues.reveal_generation += 1;
                }
            }
            Err(error) => {
                changed |= state.connection_error != error;
                state.connection_error = error;
                state.advanced.queues.reveal = None;
                state.advanced.queues.reveal_generation += 1;
            }
        }
        state.loaded = true;
        if state.advanced.queues.selected == selected {
            state.advanced.queues.metrics = measured.unwrap_or(Value::Null);
        }
        drop(state);
        if changed {
            render_content(&app);
        } else {
            set_html(
                "queue-metrics",
                &metrics(
                    &app.borrow().advanced.queues.metrics,
                    app.borrow().locale == "ko",
                ),
            );
        }
        render_messages(&app);
    });
}
fn mutate(app: &Shared, method: &'static str, path: String, body: Option<Value>) {
    if app.borrow().busy {
        return;
    }
    app.borrow_mut().busy = true;
    app.borrow_mut().advanced.queues.errors.clear();
    let generation = app.borrow().generation;
    render_content(app);
    let app = app.clone();
    spawn_local(async move {
        let result = api::request_detailed(method, &path, body.as_ref(), None).await;
        let mut state = app.borrow_mut();
        if state.generation != generation {
            return;
        }
        state.busy = false;
        match result {
            Ok(_) => state.error.clear(),
            Err(error) if path == "/api/queue/engine" && !error.fields.is_empty() => {
                state.error.clear();
                state.advanced.queues.errors = error.fields;
            }
            Err(error) => {
                state.error = if error.fields.is_empty() {
                    error.message
                } else {
                    format!(
                        "{}: {}",
                        error.message,
                        error
                            .fields
                            .iter()
                            .map(|(key, value)| format!("{key}: {value}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
            }
        }
        drop(state);
        render_content(&app);
        refresh(app);
    });
}

/// Preserve setup inputs when polling or an installation error redraws the page.
pub fn capture_setup_input(app: &Shared, target: &Element) -> bool {
    if target
        .closest("#queue-engine-form")
        .ok()
        .flatten()
        .is_none()
    {
        return false;
    }
    let name = target.get_attribute("name").unwrap_or_default();
    let value = if let Some(input) = target.dyn_ref::<HtmlInputElement>() {
        input.value()
    } else if let Some(select) = target.dyn_ref::<HtmlSelectElement>() {
        select.value()
    } else {
        return false;
    };
    let mut state = app.borrow_mut();
    let setup = &mut state.advanced.queues.setup;
    match name.as_str() {
        "microNetworkId" => setup.network = value,
        "cpu" => setup.cpu = value,
        "ram" => setup.ram = value,
        "diskGb" => setup.disk = value,
        _ => return false,
    }
    true
}
pub fn submit(app: &Shared, form: &HtmlFormElement) -> bool {
    if !["queue-engine-form", "queue-create-form"].contains(&form.id().as_str()) {
        return false;
    }
    if app.borrow().busy || !app.borrow().connection_error.is_empty() {
        return true;
    }
    let Ok(data) = FormData::new_with_form(form) else {
        return true;
    };
    let value = |key| data.get(key).as_string().unwrap_or_default();
    let number = |key| value(key).parse::<u32>().unwrap_or(0);
    let (path, body) = if form.id() == "queue-engine-form" {
        (
            "/api/queue/engine",
            json!({"microNetworkId":value("microNetworkId"),"cpu":number("cpu"),"ram":number("ram"),"diskGb":number("diskGb")}),
        )
    } else {
        (
            "/api/queues",
            json!({"name":value("name"),"deliveryLimit":number("deliveryLimit"),"maxBytes":u64::from(number("maxMib"))*1024*1024}),
        )
    };
    mutate(app, "POST", path.into(), Some(body));
    true
}
pub fn hide(app: &Shared) {
    let mut state = app.borrow_mut();
    state.advanced.queues.reveal_generation += 1;
    let visible = state.advanced.queues.reveal.take().is_some();
    drop(state);
    if visible && app.borrow().route == "queues" {
        render_content(app);
    }
}
pub fn tick(app: &Shared) {
    let expired = app
        .borrow()
        .advanced
        .queues
        .reveal
        .as_ref()
        .is_some_and(|(_, until)| *until < js_sys::Date::now());
    if expired {
        hide(app);
    }
}
pub fn dispatch(app: &Shared, action: &str, id: &str) -> bool {
    if action == "queue-binding-add" || action == "queue-binding-remove" {
        binding_edit(app, action, id);
        return true;
    }
    if !action.starts_with("queue-") {
        return false;
    }
    match action {
        "queue-select" => {
            hide(app);
            let mut state = app.borrow_mut();
            state.advanced.queues.selected = id.into();
            state.advanced.queues.metrics = Value::Null;
            drop(state);
            render_content(app);
            refresh(app.clone());
        }
        "queue-hide" => hide(app),
        "queue-start" => mutate(app, "POST", "/api/queue/engine/start".into(), None),
        "queue-stop" => mutate(app, "POST", "/api/queue/engine/stop".into(), None),
        "queue-delete" => mutate(
            app,
            "DELETE",
            format!("/api/queues/{}", api::encode(id)),
            None,
        ),
        "queue-retry" => mutate(
            app,
            "POST",
            format!("/api/queues/{}/retry", api::encode(id)),
            None,
        ),
        "queue-reveal-publish" | "queue-reveal-consume" => {
            hide(app);
            let state = app.borrow();
            if !state.connection_error.is_empty() || state.advanced.queues.selected.is_empty() {
                return true;
            }
            let selected = state.advanced.queues.selected.clone();
            let generation = state.generation;
            let reveal_generation = state.advanced.queues.reveal_generation;
            drop(state);
            let app = app.clone();
            let access = if action.ends_with("publish") {
                "publish"
            } else {
                "consume"
            };
            spawn_local(async move {
                let result = api::request(
                    "POST",
                    &format!("/api/queues/{}/connection/{access}", api::encode(&selected)),
                    None,
                )
                .await;
                let mut state = app.borrow_mut();
                if state.generation != generation
                    || state.advanced.queues.reveal_generation != reveal_generation
                    || state.advanced.queues.selected != selected
                {
                    return;
                }
                match result {
                    Ok(value) => {
                        state.advanced.queues.reveal =
                            Some((text(&value, "url").into(), js_sys::Date::now() + 30000.0))
                    }
                    Err(error) => state.error = error,
                }
                drop(state);
                render_content(&app);
            });
        }
        _ => {}
    }
    true
}

pub fn bindings(field: &Field) -> String {
    let rows: Value = serde_json::from_str(&field.value).unwrap_or(json!([]));
    let mut out = format!(
        "<input type=\"hidden\" id=\"field-queues\" name=\"queues\" value=\"{}\"><div class=\"queue-bindings\">",
        escape(&field.value)
    );
    for (index, binding) in list(&rows).iter().enumerate() {
        let alias = text(binding, "alias");
        out.push_str(&format!("<div class=\"queue-binding-row\"><label>Alias<input id=\"queue-alias-{index}\" data-queue-binding=\"alias\" value=\"{}\" pattern=\"[A-Z][A-Z0-9_]{{0,23}}\" required maxlength=\"24\"></label><label>Queue<select id=\"queue-id-{index}\" data-queue-binding=\"id\" required>",escape(alias)));
        if !field
            .options
            .iter()
            .any(|(id, _)| id == text(binding, "queueId"))
        {
            out.push_str(&format!(
                "<option value=\"{}\" selected>{}</option>",
                escape(text(binding, "queueId")),
                escape(text(binding, "queueId"))
            ));
        }
        for (id, name) in &field.options {
            out.push_str(&format!(
                "<option value=\"{}\" {}>{}</option>",
                escape(id),
                if id == text(binding, "queueId") {
                    "selected"
                } else {
                    ""
                },
                escape(name)
            ));
        }
        out.push_str(&format!("</select></label><label>Access<select id=\"queue-access-{index}\" data-queue-binding=\"access\"><option value=\"publish\" {}>Send</option><option value=\"consume\" {}>Receive</option></select></label>{}</div>",if binding["access"] == "publish" { "selected" } else { "" },if binding["access"] == "consume" { "selected" } else { "" },button("Remove","queue-binding-remove",&index.to_string())));
    }
    out.push_str(&format!("<button type=\"button\" class=\"btn\" data-action=\"queue-binding-add\" {}>Add Queue connection</button></div>",if field.options.is_empty() || list(&rows).len() >= 8 { "disabled" } else { "" }));
    out
}
pub fn binding_input(app: &Shared, target: &Element) {
    if !target.has_attribute("data-queue-binding") {
        return;
    }
    let Ok(rows) = crate::document().query_selector_all(".queue-binding-row") else {
        return;
    };
    let mut bindings = Vec::new();
    let mut aliases = std::collections::BTreeSet::new();
    for index in 0..rows.length() {
        let Some(alias) = element(&format!("queue-alias-{index}"))
            .and_then(|e| e.dyn_into::<HtmlInputElement>().ok())
        else {
            continue;
        };
        let name = alias.value();
        alias.set_custom_validity(if !aliases.insert(name.clone()) {
            "Aliases must be unique."
        } else {
            ""
        });
        let value = |key| {
            element(&format!("queue-{key}-{index}"))
                .and_then(|e| e.dyn_into::<HtmlSelectElement>().ok())
                .map(|e| e.value())
                .unwrap_or_default()
        };
        bindings.push(json!({"alias":name,"queueId":value("id"),"access":value("access")}));
    }
    if let Some(input) = element("field-queues").and_then(|e| e.dyn_into::<HtmlInputElement>().ok())
    {
        input.set_value(&json!(bindings).to_string());
    }
    if let Some(field) = app
        .borrow_mut()
        .form
        .as_mut()
        .and_then(|f| f.fields.iter_mut().find(|f| f.key == "queues"))
    {
        field.value = json!(bindings).to_string();
    }
}
pub fn binding_draft(bindings: &Value) -> String {
    json!(
        bindings
            .as_object()
            .into_iter()
            .flatten()
            .map(|(alias, binding)| {
                json!({"alias":alias,"queueId":binding["queueId"],"access":binding["access"]})
            })
            .collect::<Vec<_>>()
    )
    .to_string()
}
pub fn binding_payload(draft: &str) -> Result<Value, String> {
    let rows: Vec<Value> = serde_json::from_str(draft).map_err(|_| "Invalid Queue connections.")?;
    if rows.len() > 8 {
        return Err("A service supports up to eight Queue connections.".into());
    }
    let mut bindings = json!({});
    for row in rows {
        let alias = text(&row, "alias");
        if bindings.get(alias).is_some() {
            return Err("Queue aliases must be unique.".into());
        }
        if alias.is_empty() || !["publish", "consume"].contains(&text(&row, "access")) {
            return Err("Choose an alias and access for each Queue connection.".into());
        }
        bindings[alias] = json!({"queueId":row["queueId"],"access":row["access"]});
    }
    Ok(bindings)
}
fn binding_edit(app: &Shared, action: &str, index: &str) {
    super::capture_form(app);
    let mut state = app.borrow_mut();
    let Some(field) = state
        .form
        .as_mut()
        .and_then(|f| f.fields.iter_mut().find(|f| f.key == "queues"))
    else {
        return;
    };
    let mut rows: Vec<Value> = serde_json::from_str(&field.value).unwrap_or_default();
    if action == "queue-binding-remove" {
        if let Ok(index) = index.parse::<usize>()
            && index < rows.len()
        {
            rows.remove(index);
        }
    } else if rows.len() < 8
        && let Some((id, _)) = field.options.first()
        && let Some(alias) = (1..=9)
            .map(|n| format!("QUEUE_{n}"))
            .find(|key| !rows.iter().any(|r| text(r, "alias") == key))
    {
        rows.push(json!({"alias":alias,"queueId":id,"access":"publish"}));
    }
    field.value = json!(rows).to_string();
    state.dirty = true;
    drop(state);
    render_editor(app);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_aliases_cannot_silently_replace_a_connection() {
        let draft = json!([
            {"alias":"TASKS","queueId":"one","access":"publish"},
            {"alias":"TASKS","queueId":"two","access":"consume"}
        ]);
        assert_eq!(
            binding_payload(&draft.to_string()).unwrap_err(),
            "Queue aliases must be unique."
        );
    }
    #[test]
    fn editing_bindings_retains_queue_identity_and_role() {
        let original = json!({"TASKS":{"queueId":"one","access":"consume"}});
        assert_eq!(
            binding_payload(&binding_draft(&original)).unwrap(),
            original
        );
        assert_eq!(binding_payload("[]").unwrap(), json!({}));
    }
}
