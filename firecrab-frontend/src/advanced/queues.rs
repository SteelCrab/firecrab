//! Queue lifecycle and short-lived credential reveals. Secrets never enter the cache.
use crate::{
    App, Shared, api, element,
    forms::Field,
    model::{escape, list, text},
    render::{Icon, button, chips, empty_state, icon_button, status_dot, status_line},
    render_content, render_editor, render_messages, set_html,
};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{Element, FormData, HtmlFormElement, HtmlInputElement, HtmlSelectElement};

#[derive(Default)]
pub struct State {
    selected: String,
    metrics: Value,
    reveal: Option<(String, f64)>,
    reveal_generation: u64,
}
fn title(value: &str) -> String {
    let mut chars = value.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}
fn input(name: &str, label: &str, value: &str, min: u32, max: u32) -> String {
    format!(
        "<label>{label}<input name=\"{name}\" type=\"number\" value=\"{value}\" min=\"{min}\" max=\"{max}\" required></label>"
    )
}
fn controls_disabled(app: &App) -> &'static str {
    if app.busy || !app.connection_error.is_empty() {
        "disabled"
    } else {
        ""
    }
}
/// The tone of a queue or engine phase.
fn phase_tone(phase: &str) -> &'static str {
    match phase {
        "running" => "ok",
        "failed" => "error",
        _ => "busy",
    }
}
pub fn content(app: &App) -> String {
    let ko = app.locale == "ko";
    let engine = app.cache.get("/api/queue/engine").unwrap_or(&Value::Null);
    let disabled = controls_disabled(app);
    let mut out = String::from("<div class=\"queue-page\">");
    if engine.is_null() {
        // Not installed: the install form is the first thing to do.
        let mut form = format!(
            "<form id=\"queue-engine-form\" class=\"queue-form\"><fieldset {disabled}><div class=\"queue-inputs\"><label>MicroNetwork<select name=\"microNetworkId\" required><option value=\"\">Select a network</option>"
        );
        for network in list(app.cache.get("/api/micro-networks").unwrap_or(&Value::Null)) {
            form.push_str(&format!(
                "<option value=\"{}\">{}</option>",
                escape(text(network, "id")),
                escape(text(network, "name"))
            ));
        }
        form.push_str("</select></label>");
        form.push_str(&input("cpu", "CPU", "4", 1, 32));
        form.push_str(&input("ram", "Memory (MiB)", "4096", 128, 32768));
        form.push_str(&input("diskGb", "Disk (GiB)", "8", 4, 1024));
        form.push_str("</div><button type=\"submit\" class=\"btn primary\">Install RabbitMQ</button></fieldset></form>");
        out.push_str(&empty_state(
            "queues",
            if ko { "큐를 쓰려면 엔진을 설치합니다" } else { "Install the engine to use queues" },
            if ko {
                "RabbitMQ 엔진은 MicroVM 하나에서 실행되고 모든 큐가 함께 씁니다. 한 번만 설치하면 됩니다."
            } else {
                "The RabbitMQ engine runs in one MicroVM and every queue shares it. You install it once."
            },
            &form,
        ));
    } else {
        let phase = text(engine, "phase");
        let mut actions = format!("<fieldset class=\"queue-actions\" {disabled}>");
        if ["stopped", "failed"].contains(&phase) {
            actions.push_str(&icon_button(
                "Start / recover",
                "queue-start",
                "",
                Icon::Start,
            ));
        }
        if phase == "running" {
            actions.push_str(&icon_button("Stop", "queue-stop", "", Icon::Stop));
        }
        actions.push_str("</fieldset>");
        let message = if phase == "running" {
            ""
        } else if ko {
            "엔진이 실행 중일 때 큐를 만들 수 있습니다."
        } else {
            "Queues can be created once the engine is running."
        };
        out.push_str(&status_line(
            phase_tone(phase),
            "Internal RabbitMQ",
            &[status_dot(phase_tone(phase), &title(phase))],
            &actions,
            if text(engine, "error").is_empty() {
                message
            } else {
                text(engine, "error")
            },
        ));
        if !text(engine, "log").is_empty() {
            out.push_str(&format!("<details><summary>Installation log</summary><pre class=\"queue-log\">{}</pre></details>",escape(text(engine,"log"))));
        }
    }
    let ready = text(engine, "phase") == "running" && disabled.is_empty();
    // New queue: a flat form row above the list.
    out.push_str(&format!("<form id=\"queue-create-form\" class=\"queue-form queue-create\"><fieldset {}><div class=\"queue-inputs\"><label>Queue name<input name=\"name\" required pattern=\"[a-z0-9-]{{1,40}}\" maxlength=\"40\"></label>",if ready { "" } else { "disabled" }));
    out.push_str(&input("deliveryLimit", "Delivery limit", "5", 1, 100));
    out.push_str(&input("maxMib", "Capacity per queue (MiB)", "64", 1, 1024));
    out.push_str("</div><button class=\"btn primary\" type=\"submit\">Create Queue</button></fieldset></form>");
    let queues = list(&app.resource);
    if queues.is_empty() {
        if !engine.is_null() {
            out.push_str(&empty_state(
                "queues",
                if ko {
                    "아직 큐가 없습니다"
                } else {
                    "No queues yet"
                },
                if ko {
                    "엔진이 시작되면 위 양식으로 첫 큐를 만듭니다."
                } else {
                    "Create the first queue with the form above once the engine starts."
                },
                "",
            ));
        }
    } else {
        out.push_str(&format!(
            "<div class=\"table-scroll\"><table class=\"wasm-table queue-table\" aria-label=\"{}\"><thead><tr><th>{}</th><th>{}</th><th class=\"col-secondary\">{}</th><th class=\"row-actions\"><span class=\"sr-only\">{}</span></th></tr></thead><tbody>",
            if ko { "큐" } else { "Queues" },
            if ko { "이름" } else { "Name" },
            if ko { "상태" } else { "Status" },
            if ko { "사용 서비스" } else { "Used by" },
            crate::i18n::t("Actions")
        ));
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
                    if ko {
                        "전달 한도"
                    } else {
                        "Delivery limit"
                    },
                    queue["deliveryLimit"],
                    if ko { "최대" } else { "Max" },
                    queue["maxBytes"].as_u64().map_or(0, |b| b / 1_048_576)
                )
            } else {
                error.to_string()
            };
            let mut actions = format!("<fieldset class=\"queue-actions\" {disabled}>");
            if phase == "failed" {
                actions.push_str(&icon_button("Retry", "queue-retry", id, Icon::Retry));
            }
            actions.push_str(&format!(
                "<button type=\"button\" class=\"icon-btn danger\" aria-label=\"Delete\" title=\"{}\" data-action=\"queue-delete\" data-id=\"{}\" {}>{}</button></fieldset>",
                if services.is_empty() {
                    "Delete"
                } else {
                    "Used by a MicroService. Remove its connection before deletion."
                },
                escape(id),
                if services.is_empty() { "" } else { "disabled" },
                Icon::Delete.svg()
            ));
            out.push_str(&format!(
                "<tr data-id=\"{}\"><td><div class=\"cell-name\"><b><button type=\"button\" class=\"cell-link\" data-action=\"queue-select\" data-id=\"{}\">{}</button></b><span class=\"cell-sub\" data-tone=\"{}\">{}</span></div></td><td>{}</td><td class=\"col-secondary\">{}</td><td class=\"row-actions\">{actions}</td></tr>",
                escape(id),
                escape(id),
                escape(text(queue, "name")),
                if error.is_empty() { "" } else { "error" },
                escape(&sub),
                status_dot(phase_tone(phase), &title(phase)),
                chips(&used)
            ));
        }
        out.push_str("</tbody></table></div>");
    }
    if !app.advanced.queues.selected.is_empty() {
        out.push_str(&format!("<section class=\"queue-detail\"><h2>Queue detail</h2><div id=\"queue-metrics\">{}</div><fieldset class=\"queue-actions\" {disabled}>{}{}{}</fieldset>",metrics(&app.advanced.queues.metrics),button("Show sender connection","queue-reveal-publish",""),button("Show receiver connection","queue-reveal-consume",""),button("Hide","queue-hide","")));
        if let Some((url, _)) = &app.advanced.queues.reveal {
            out.push_str(&format!(
                "<div class=\"queue-connection\"><code>{}</code></div>",
                escape(url)
            ));
        }
        out.push_str("</section>");
    }
    out.push_str("</div>");
    out
}
fn metrics(value: &Value) -> String {
    if value.is_null() {
        return String::new();
    }
    let mut out = "<dl class=\"queue-metrics\">".to_string();
    for (key, label) in [
        ("ready", "Waiting"),
        ("unacknowledged", "Unacknowledged"),
        ("consumers", "Consumers"),
        ("failed", "Failed"),
    ] {
        out.push_str(&format!(
            "<div><dt>{label}</dt><dd>{}</dd></div>",
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
                if !list(&state.resource)
                    .iter()
                    .any(|q| text(q, "id") == state.advanced.queues.selected)
                {
                    state.advanced.queues = State::default();
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
                &metrics(&app.borrow().advanced.queues.metrics),
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
    let generation = app.borrow().generation;
    render_content(app);
    let app = app.clone();
    spawn_local(async move {
        let result = api::request(method, &path, body.as_ref()).await;
        let mut state = app.borrow_mut();
        if state.generation != generation {
            return;
        }
        state.busy = false;
        state.error = result.err().unwrap_or_default();
        drop(state);
        render_content(&app);
        refresh(app);
    });
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
