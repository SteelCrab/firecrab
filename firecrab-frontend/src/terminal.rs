use crate::{
    App, Shared, api, details, document, element,
    i18n::t,
    model::{escape, text},
    render, set_html, window,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    BinaryType, CloseEvent, CustomEvent, Element, Event, KeyboardEvent, MessageEvent, WebSocket,
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Prefs {
    pub font_size: u32,
    pub theme_id: String,
    pub inspect_open: bool,
}
impl Default for Prefs {
    fn default() -> Self {
        Self {
            font_size: 13,
            theme_id: "ember".into(),
            inspect_open: true,
        }
    }
}
impl Prefs {
    pub fn load() -> Self {
        let value = window()
            .local_storage()
            .ok()
            .flatten()
            .and_then(|storage| storage.get_item("firecrab.console.prefs").ok().flatten())
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .unwrap_or(Value::Null);
        Self::from_value(&value)
    }
    pub fn from_value(value: &Value) -> Self {
        let font = value["fontSize"].as_u64().unwrap_or(13) as u32;
        let theme = text(value, "themeId");
        Self {
            font_size: if [12, 13, 14, 16, 18].contains(&font) {
                font
            } else {
                13
            },
            theme_id: if ["ember", "classic", "light", "solarized"].contains(&theme) {
                theme
            } else {
                "ember"
            }
            .into(),
            inspect_open: value["inspectOpen"].as_bool().unwrap_or(true),
        }
    }
    fn save(&self) {
        if let Ok(Some(storage)) = window().local_storage() {
            let _ = storage.set_item(
                "firecrab.console.prefs",
                &serde_json::to_string(self).unwrap_or_default(),
            );
        }
    }
    fn options(&self, id: &str) -> Value {
        let (background, foreground, cursor, selection) = match self.theme_id.as_str() {
            "classic" => ("#0c0c0c", "#cccccc", "#ffffff", "rgba(255,255,255,.25)"),
            "light" => ("#fafafb", "#18181b", "#e8492f", "rgba(232,73,47,.2)"),
            "solarized" => ("#002b36", "#839496", "#93a1a1", "rgba(147,161,161,.3)"),
            _ => ("#18181b", "#e4e4e7", "#e8492f", "rgba(232,73,47,.32)"),
        };
        json!({"id":id,"fontSize":self.font_size,"theme":{"background":background,"foreground":foreground,"cursor":cursor,"selectionBackground":selection},"fontFamily":"\"JetBrains Mono\", \"Noto Sans KR\", \"Noto Sans JP\", \"Noto Sans SC\", \"D2Coding\", \"NanumGothicCoding\", \"Malgun Gothic\", \"Apple SD Gothic Neo\", \"Yu Gothic\", \"Microsoft YaHei\", ui-monospace, monospace","cols":80,"rows":24,"scrollback":8388608,"convertEol":false,"cursorBlink":true,"rescaleOverlappingGlyphs":true,"macOptionIsMeta":false})
    }
}
pub struct State {
    pub id: String,
    pub vm: Value,
    pub prefs: Prefs,
    pub status: String,
    pub only: bool,
    pub surface: String,
    pub settings: bool,
    guest_resize: Option<Value>,
    socket: Option<WebSocket>,
    callbacks: Vec<Closure<dyn FnMut(Event)>>,
    retry: Option<i32>,
    attempt: u32,
    epoch: u64,
}
impl Drop for State {
    fn drop(&mut self) {
        if let Some(timer) = self.retry.take() {
            window().clear_timeout_with_handle(timer);
        }
        if let Some(socket) = self.socket.take() {
            socket.set_onopen(None);
            socket.set_onmessage(None);
            socket.set_onerror(None);
            socket.set_onclose(None);
            let _ = socket.close_with_code(1000);
        }
        self.callbacks.clear();
    }
}
fn binding(action: &str, payload: JsValue) -> JsValue {
    js_sys::Reflect::get(&window(), &JsValue::from_str("firecrabTerminal"))
        .and_then(|value| value.dyn_into::<js_sys::Function>())
        .and_then(|function| function.call2(&JsValue::NULL, &JsValue::from_str(action), &payload))
        .unwrap_or(JsValue::UNDEFINED)
}
#[wasm_bindgen]
pub fn terminal_key_allowed(event: KeyboardEvent) -> bool {
    !event.is_composing() && event.key() != "Process" && event.key_code() != 229
}
pub fn html(app: &App, id: &str) -> String {
    let prefs = app
        .console
        .as_ref()
        .map(|state| state.prefs.clone())
        .unwrap_or_default();
    let name = app
        .console
        .as_ref()
        .map(|state| text(&state.vm, "name"))
        .filter(|name| !name.is_empty())
        .unwrap_or(id);
    let reconnect = render::button("Apply theme and reconnect", "console-reconnect", "");
    format!(
        "<main class=\"console-page wasm-console\" id=\"console-page\"><header class=\"console-header\" id=\"console-header\"><a class=\"btn\" href=\"#/vms\">← MicroVM</a><h1 class=\"console-title\" id=\"console-title\">{}</h1><span id=\"console-address\"></span><p id=\"console-geometry\" class=\"console-geometry\" aria-live=\"polite\" hidden></p><span id=\"console-status\" class=\"console-status\" role=\"status\">{}</span><div role=\"tablist\" aria-label=\"Console surface\"><button role=\"tab\" aria-selected=\"true\" id=\"console-tab-serial\" data-action=\"console-tab\" data-id=\"serial\">{}</button><button role=\"tab\" aria-selected=\"false\" id=\"console-tab-ssh\" data-action=\"console-tab\" data-id=\"ssh\">SSH</button></div><div class=\"wasm-actions\" role=\"group\" aria-label=\"Export log\">{}{}{}{}{}<span id=\"console-retry\"></span></div></header><div id=\"messages\" aria-live=\"polite\"></div><div id=\"console-settings\" class=\"wasm-card\" role=\"dialog\" aria-label=\"{}\" hidden><label for=\"console-font\">{}</label><select id=\"console-font\">{}</select><label for=\"console-theme\">{}</label><select id=\"console-theme\">{}</select>{reconnect}</div><div class=\"wasm-console-layout\"><div id=\"terminal-surface\" class=\"console-surface\"><div id=\"terminal\"></div></div><section id=\"console-ssh\" hidden></section><aside id=\"console-inspector\" aria-label=\"VM details\" {}></aside></div><button class=\"btn console-exit-only\" id=\"console-exit-only\" data-action=\"console-only\" hidden>{}</button></main><div id=\"modal-root\"></div>",
        escape(name),
        t("Connecting…"),
        t("Serial"),
        render::button("Copy log", "console-copy-log", ""),
        render::button("Save log", "console-save-log", ""),
        render::button("Display settings", "console-settings", ""),
        render::button(
            if prefs.inspect_open {
                "Hide details"
            } else {
                "Show details"
            },
            "console-inspect",
            ""
        ),
        render::button("Terminal only", "console-only", ""),
        t("Terminal display settings"),
        t("Font size"),
        [12, 13, 14, 16, 18]
            .iter()
            .map(|font| format!(
                "<option value=\"{font}\" {}>{font}px</option>",
                if *font == prefs.font_size {
                    "selected"
                } else {
                    ""
                }
            ))
            .collect::<Vec<_>>()
            .join(""),
        t("Color theme"),
        [
            ("ember", "Firecrab"),
            ("classic", "Classic"),
            ("light", "Light"),
            ("solarized", "Solarized")
        ]
        .iter()
        .map(|(id, label)| format!(
            "<option value=\"{id}\" {}>{}</option>",
            if *id == prefs.theme_id {
                "selected"
            } else {
                ""
            },
            t(label)
        ))
        .collect::<Vec<_>>()
        .join(""),
        if prefs.inspect_open { "" } else { "hidden" },
        t("Exit terminal only")
    )
}
pub fn start(app: Shared, id: String) {
    stop(&app);
    app.borrow_mut().console = Some(State {
        id: id.clone(),
        vm: Value::Null,
        prefs: Prefs::load(),
        status: "connecting".into(),
        only: false,
        surface: "serial".into(),
        settings: false,
        guest_resize: None,
        socket: None,
        callbacks: Vec::new(),
        retry: None,
        attempt: 0,
        epoch: 0,
    });
    app.borrow_mut().loaded = true;
    crate::render_page(&app);
    let options = app
        .borrow()
        .console
        .as_ref()
        .map(|state| state.prefs.options(&id))
        .unwrap_or(Value::Null);
    binding("mount", JsValue::from_str(&options.to_string()));
    tick(&app);
}
pub fn stop(app: &Shared) {
    let state = app.borrow_mut().console.take();
    drop(state);
    binding("dispose", JsValue::UNDEFINED);
}
fn connect(app: Shared) {
    let mut root = app.borrow_mut();
    let Some(state) = root.console.as_mut() else {
        return;
    };
    if let Some(timer) = state.retry.take() {
        window().clear_timeout_with_handle(timer);
    }
    if let Some(socket) = state.socket.take() {
        socket.set_onopen(None);
        socket.set_onclose(None);
        socket.set_onmessage(None);
        socket.set_onerror(None);
        let _ = socket.close_with_code(1000);
    }
    state.callbacks.clear();
    state.epoch += 1;
    let epoch = state.epoch;
    state.guest_resize = None;
    let id = state.id.clone();
    state.status = if state.attempt == 0 {
        "connecting"
    } else {
        "reconnecting"
    }
    .into();
    drop(root);
    binding("shared", JsValue::from_str("null"));
    redraw(&app);
    let protocol = if window().location().protocol().unwrap_or_default() == "https:" {
        "wss"
    } else {
        "ws"
    };
    let host = window().location().host().unwrap_or_default();
    let socket = match WebSocket::new_with_str(
        &format!("{protocol}://{host}/ws/vms/{}/console", api::encode(&id)),
        "firecrab.console.v1",
    ) {
        Ok(socket) => socket,
        Err(error) => {
            app.borrow_mut().error = api::error(error);
            return;
        }
    };
    socket.set_binary_type(BinaryType::Arraybuffer);
    let opened = app.clone();
    let onopen = Closure::<dyn FnMut(Event)>::new(move |_| {
        let mut root = opened.borrow_mut();
        let Some(state) = root.console.as_mut().filter(|state| state.epoch == epoch) else {
            return;
        };
        state.status = "connected".into();
        state.attempt = 0;
        drop(root);
        redraw(&opened);
        binding("focus", JsValue::UNDEFINED);
    });
    let received = app.clone();
    let onmessage = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        if !received
            .borrow()
            .console
            .as_ref()
            .is_some_and(|state| state.epoch == epoch)
        {
            return;
        }
        if let Ok(message) = event.dyn_into::<MessageEvent>() {
            if let Some(raw) = message.data().as_string() {
                let mut root = received.borrow_mut();
                let state = root.console.as_mut().unwrap();
                if state
                    .socket
                    .as_ref()
                    .is_some_and(|socket| socket.protocol() == "firecrab.console.v1")
                {
                    if let Some(update) = guest_resize_message(&raw) {
                        let first = state.guest_resize.is_none();
                        let size = update["size"].to_string();
                        state.guest_resize = Some(update);
                        drop(root);
                        binding("shared", JsValue::from_str(&size));
                        redraw(&received);
                        if first {
                            binding("reportDimensions", JsValue::UNDEFINED);
                        }
                        return;
                    }
                    if state.guest_resize.is_some() {
                        if let Some(socket) = &state.socket {
                            let _ = socket.close_with_code(4002);
                        }
                        root.error = "Invalid console control message".into();
                        drop(root);
                        crate::render_messages(&received);
                        return;
                    }
                }
            }
            binding("write", message.data());
        }
    });
    let failed = app.clone();
    let onerror = Closure::<dyn FnMut(Event)>::new(move |_| {
        if let Some(state) = failed
            .borrow_mut()
            .console
            .as_mut()
            .filter(|state| state.epoch == epoch)
        {
            state.status = "failed".into();
        }
        redraw(&failed);
    });
    let closed = app.clone();
    let onclose = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        let Ok(event) = event.dyn_into::<CloseEvent>() else {
            return;
        };
        let mut root = closed.borrow_mut();
        let Some(state) = root.console.as_mut().filter(|state| state.epoch == epoch) else {
            return;
        };
        state.guest_resize = None;
        if event.code() == 4000 {
            state.status = "ended".into();
            drop(root);
            binding(
                "write",
                JsValue::from_str(&format!(
                    "\r\n\x1b[2m{}\x1b[0m\r\n",
                    t("Guest session ended. Use New session to open another.")
                )),
            );
            redraw(&closed);
            return;
        }
        state.status = "reconnecting".into();
        let delay = (1000u32.saturating_mul(2u32.saturating_pow(state.attempt.min(5)))).min(15000);
        state.attempt += 1;
        let callback = root.console_retry.as_ref().map(|callback| {
            callback
                .as_ref()
                .unchecked_ref::<js_sys::Function>()
                .clone()
        });
        if let Some(callback) = callback
            && let Ok(timer) = window()
                .set_timeout_with_callback_and_timeout_and_arguments_0(&callback, delay as i32)
            && let Some(state) = root.console.as_mut()
        {
            state.retry = Some(timer);
        }
        drop(root);
        redraw(&closed);
    });
    socket.set_onopen(Some(onopen.as_ref().unchecked_ref()));
    socket.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
    socket.set_onerror(Some(onerror.as_ref().unchecked_ref()));
    socket.set_onclose(Some(onclose.as_ref().unchecked_ref()));
    if let Some(state) = app.borrow_mut().console.as_mut() {
        state.socket = Some(socket);
        state.callbacks = vec![onopen, onmessage, onerror, onclose];
    }
}
fn redraw(app: &Shared) {
    let root = app.borrow();
    let Some(state) = root.console.as_ref() else {
        return;
    };
    if let Some(geometry) =
        element("console-geometry").and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
    {
        geometry
            .set_hidden(state.only || state.surface != "serial" || state.guest_resize.is_none());
        if let Some(update) = &state.guest_resize {
            let status = text(update, "status");
            let _ = geometry.set_attribute("data-status", status);
            geometry.set_text_content(Some(&if status == "applied" {
                format!(
                    "{}: {} × {}",
                    t("Guest size"),
                    update["size"]["cols"],
                    update["size"]["rows"]
                )
            } else {
                t(if status == "unavailable" {
                    "Guest size not synchronized"
                } else {
                    "Synchronizing guest size…"
                })
                .into()
            }));
        }
    }
    let label = match state.status.as_str() {
        "connected" => "Connected",
        "reconnecting" => "Reconnecting…",
        "ended" => "Session ended",
        "failed" => "Connection failed",
        _ => "Connecting…",
    };
    if let Some(status) = element("console-status") {
        status.set_text_content(Some(t(label)));
    }
    if let Some(title) = element("console-title") {
        let name = text(&state.vm, "name");
        if !name.is_empty() {
            title.set_text_content(Some(name));
            document().set_title(&format!("terminal — {name}"));
        }
    }
    if let Some(address) = element("console-address") {
        address.set_text_content(Some(text(&state.vm, "ipv4")));
    }
    set_html(
        "console-retry",
        &if ["ended", "failed", "reconnecting", "disconnected"].contains(&state.status.as_str()) {
            render::button(
                if state.status == "ended" {
                    "New session"
                } else {
                    "Reconnect"
                },
                "console-reconnect",
                "",
            )
        } else {
            String::new()
        },
    );
    for (id, hidden) in [
        ("console-header", state.only),
        ("console-settings", state.only || !state.settings),
        ("console-inspector", state.only || !state.prefs.inspect_open),
        ("console-exit-only", !state.only),
        ("terminal-surface", state.surface != "serial"),
        ("console-ssh", state.surface != "ssh"),
    ] {
        if let Some(element) = element(id).and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok()) {
            element.set_hidden(hidden);
        }
    }
    if let Ok(Some(toggle)) = document().query_selector("[data-action=console-inspect]") {
        toggle.set_text_content(Some(t(if state.prefs.inspect_open {
            "Hide details"
        } else {
            "Show details"
        })));
        let _ = toggle.set_attribute(
            "aria-expanded",
            if state.prefs.inspect_open {
                "true"
            } else {
                "false"
            },
        );
    }
    if state.prefs.inspect_open && !state.vm.is_null() {
        let ports = crate::model::list(&state.vm["portForwards"])
            .iter()
            .map(|rule| {
                format!(
                    "<p>:{} → :{}/{}</p>",
                    rule["guestPort"],
                    rule["hostPort"],
                    escape(text(rule, "protocol"))
                )
            })
            .collect::<String>();
        set_html(
            "console-inspector",
            &format!(
                "<h2>{}</h2>{}{ports}",
                t("Resources"),
                details::overview(&state.vm)
            ),
        );
    }
    for surface in ["serial", "ssh"] {
        if let Some(tab) = element(&format!("console-tab-{surface}")) {
            let _ = tab.set_attribute(
                "aria-selected",
                if state.surface == surface {
                    "true"
                } else {
                    "false"
                },
            );
        }
    }
    let ssh = state.surface == "ssh";
    drop(root);
    if ssh {
        crate::workflows::redraw_ssh(app);
    }
    binding("fit", JsValue::UNDEFINED);
}
pub fn tick(app: &Shared) {
    let Some(id) = app.borrow().console.as_ref().map(|state| state.id.clone()) else {
        return;
    };
    let app = app.clone();
    spawn_local(async move {
        let result = api::request("GET", &format!("/api/vms/{}", api::encode(&id)), None).await;
        let mut root = app.borrow_mut();
        let Some(state) = root.console.as_mut().filter(|state| state.id == id) else {
            return;
        };
        match result {
            Ok(vm) => {
                state.vm = vm.clone();
                root.workflows.ssh_vm = vm;
            }
            Err(error) => root.error = error,
        }
        drop(root);
        redraw(&app);
    });
}
pub fn change(app: &Shared, target: &Element) -> bool {
    if !["console-font", "console-theme"].contains(&target.id().as_str()) {
        return false;
    }
    let value = target
        .clone()
        .dyn_into::<web_sys::HtmlSelectElement>()
        .ok()
        .map(|select| select.value())
        .unwrap_or_default();
    let mut root = app.borrow_mut();
    let Some(state) = root.console.as_mut() else {
        return false;
    };
    if target.id() == "console-font" {
        let font = value.parse().unwrap_or(13);
        if [12, 13, 14, 16, 18].contains(&font) {
            state.prefs.font_size = font;
        }
    } else if ["ember", "classic", "light", "solarized"].contains(&value.as_str()) {
        state.prefs.theme_id = value;
    }
    state.prefs.save();
    let options = state.prefs.options(&state.id);
    drop(root);
    binding("options", JsValue::from_str(&options.to_string()));
    true
}
pub fn close_on_escape(app: &Shared) -> bool {
    let mut root = app.borrow_mut();
    if let Some(state) = root.console.as_mut() {
        state.only = false;
        state.settings = false;
        drop(root);
        redraw(app);
        return true;
    }
    false
}
pub fn dispatch(app: &Shared, action: &str, _id: &str) -> bool {
    if !action.starts_with("console-") && action != "export-log" {
        return false;
    }
    match action {
        "console-settings" | "console-only" | "console-inspect" => {
            let mut root = app.borrow_mut();
            let Some(state) = root.console.as_mut() else {
                return false;
            };
            match action {
                "console-settings" => state.settings = !state.settings,
                "console-only" => state.only = !state.only,
                _ => {
                    state.prefs.inspect_open = !state.prefs.inspect_open;
                    state.prefs.save();
                }
            }
            drop(root);
            redraw(app);
        }
        "console-tab" => {
            let mut root = app.borrow_mut();
            if let Some(state) = root.console.as_mut() {
                state.surface = _id.into();
            }
            drop(root);
            redraw(app);
        }
        "console-reconnect" => {
            if let Some(state) = app.borrow_mut().console.as_mut() {
                state.attempt = 0;
            }
            remount(app);
        }
        "console-copy-log" | "console-save-log" | "export-log" => {
            let root = app.borrow();
            let Some(state) = root.console.as_ref() else {
                return false;
            };
            let id = state.id.clone();
            let vm = state.vm.clone();
            let copy = action == "console-copy-log";
            drop(root);
            let app = app.clone();
            spawn_local(async move {
                let log =
                    api::request("GET", &format!("/api/vms/{}/log", api::encode(&id)), None).await;
                let snapshot = binding("snapshot", JsValue::UNDEFINED);
                let live = if snapshot.is_instance_of::<js_sys::Promise>() {
                    JsFuture::from(snapshot.unchecked_into::<js_sys::Promise>())
                        .await
                        .ok()
                        .and_then(|value| value.as_string())
                        .unwrap_or_default()
                } else {
                    snapshot.as_string().unwrap_or_default()
                };
                let bundle = details::export_bundle(
                    &vm,
                    &id,
                    log.as_ref()
                        .map(|log| text(log, "consoleLog"))
                        .unwrap_or(""),
                    Some(&live),
                );
                if copy {
                    crate::workflows::copy_action(app, bundle);
                } else {
                    download_text(
                        &format!(
                            "firecrab-console-{}.txt",
                            details::safe_name(text(&vm, "name"))
                        ),
                        &bundle,
                    );
                }
            });
        }
        _ => return false,
    }
    true
}
pub fn install(app: &Shared) -> Result<(), JsValue> {
    let resized = app.clone();
    let callback = Closure::<dyn FnMut(CustomEvent)>::new(move |event: CustomEvent| {
        let Some(size) = event
            .detail()
            .as_string()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        else {
            return;
        };
        if !valid_dimension(&size["rows"]) || !valid_dimension(&size["cols"]) {
            return;
        }
        let root = resized.borrow();
        let Some(state) = root
            .console
            .as_ref()
            .filter(|state| size["id"] == state.id && state.guest_resize.is_some())
        else {
            return;
        };
        if let Some(socket) = state
            .socket
            .as_ref()
            .filter(|socket| socket.ready_state() == WebSocket::OPEN)
        {
            let _ = socket.send_with_str(
                &json!({"type":"resize", "rows":size["rows"], "cols":size["cols"]}).to_string(),
            );
        }
    });
    document().add_event_listener_with_callback(
        "firecrab-terminal-dimensions",
        callback.as_ref().unchecked_ref(),
    )?;
    callback.forget();
    let retry = app.clone();
    app.borrow_mut().console_retry =
        Some(Closure::<dyn FnMut()>::new(move || connect(retry.clone())));
    let ready = app.clone();
    let callback = Closure::<dyn FnMut(CustomEvent)>::new(move |event: CustomEvent| {
        if ready
            .borrow()
            .console
            .as_ref()
            .is_some_and(|state| event.detail().as_string().as_deref() == Some(&state.id))
        {
            connect(ready.clone());
        }
    });
    document().add_event_listener_with_callback(
        "firecrab-terminal-ready",
        callback.as_ref().unchecked_ref(),
    )?;
    callback.forget();
    let failed = app.clone();
    let callback = Closure::<dyn FnMut(CustomEvent)>::new(move |event: CustomEvent| {
        let Some(error) = event
            .detail()
            .as_string()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        else {
            return;
        };
        let mut root = failed.borrow_mut();
        let Some(state) = root
            .console
            .as_mut()
            .filter(|state| error["id"] == state.id)
        else {
            return;
        };
        if let Some(timer) = state.retry.take() {
            window().clear_timeout_with_handle(timer);
        }
        if let Some(socket) = state.socket.take() {
            socket.set_onopen(None);
            socket.set_onclose(None);
            socket.set_onmessage(None);
            socket.set_onerror(None);
            let _ = socket.close_with_code(1000);
        }
        state.callbacks.clear();
        state.epoch += 1;
        state.status = "failed".into();
        root.error = text(&error, "message").into();
        drop(root);
        redraw(&failed);
        crate::render_messages(&failed);
    });
    document().add_event_listener_with_callback(
        "firecrab-terminal-error",
        callback.as_ref().unchecked_ref(),
    )?;
    callback.forget();
    let input = app.clone();
    let callback = Closure::<dyn FnMut(CustomEvent)>::new(move |event: CustomEvent| {
        let detail = event.detail();
        let data = if let Some(text) = detail.as_string() {
            text.into_bytes()
        } else if detail.is_instance_of::<js_sys::Uint8Array>() {
            js_sys::Uint8Array::new(&detail).to_vec()
        } else {
            return;
        };
        if let Some(socket) = input
            .borrow()
            .console
            .as_ref()
            .and_then(|state| state.socket.as_ref())
            .filter(|socket| socket.ready_state() == WebSocket::OPEN)
        {
            let _ = socket.send_with_u8_array(&data);
        }
    });
    document().add_event_listener_with_callback(
        "firecrab-terminal-input",
        callback.as_ref().unchecked_ref(),
    )?;
    callback.forget();
    Ok(())
}
fn remount(app: &Shared) {
    let mut root = app.borrow_mut();
    root.error.clear();
    let Some(state) = root.console.as_mut() else {
        return;
    };
    if let Some(timer) = state.retry.take() {
        window().clear_timeout_with_handle(timer);
    }
    if let Some(socket) = state.socket.take() {
        socket.set_onopen(None);
        socket.set_onclose(None);
        socket.set_onmessage(None);
        socket.set_onerror(None);
        let _ = socket.close_with_code(1000);
    }
    state.callbacks.clear();
    state.epoch += 1;
    state.status = "connecting".into();
    let options = state.prefs.options(&state.id);
    drop(root);
    redraw(app);
    binding("mount", JsValue::from_str(&options.to_string()));
}
pub fn download_text(name: &str, text: &str) {
    let parts = js_sys::Array::new();
    parts.push(&JsValue::from_str(text));
    let options = web_sys::BlobPropertyBag::new();
    options.set_type("text/plain;charset=utf-8");
    let Ok(blob) = web_sys::Blob::new_with_str_sequence_and_options(&parts, &options) else {
        return;
    };
    let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else {
        return;
    };
    if let Ok(anchor) = document().create_element("a").and_then(|a| {
        a.dyn_into::<web_sys::HtmlAnchorElement>()
            .map_err(Into::into)
    }) {
        anchor.set_href(&url);
        anchor.set_download(name);
        if let Some(body) = document().body() {
            let _ = body.append_child(&anchor);
            anchor.click();
            anchor.remove();
        }
    }
    let cleanup = Closure::once_into_js(move || {
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    let _ = window()
        .set_timeout_with_callback_and_timeout_and_arguments_0(cleanup.unchecked_ref(), 1000);
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preferences_reject_unknown_fonts_and_themes() {
        let prefs =
            Prefs::from_value(&json!({"fontSize":900,"themeId":"injected","inspectOpen":false}));
        assert_eq!(prefs.font_size, 13);
        assert_eq!(prefs.theme_id, "ember");
        assert!(!prefs.inspect_open);
    }
    #[test]
    fn guest_resize_accepts_status_and_bounded_dimensions() {
        assert!(
            guest_resize_message(r#"{"type":"resize","status":"pending","size":null,"viewers":1}"#)
                .is_some()
        );
        assert!(
            guest_resize_message(
                r#"{"type":"resize","status":"applied","size":{"rows":2,"cols":1000},"viewers":2}"#
            )
            .is_some()
        );
        assert!(
            guest_resize_message(
                r#"{"type":"resize","status":"unavailable","size":null,"viewers":1}"#
            )
            .is_some()
        );
    }
    #[test]
    fn guest_resize_rejects_invalid_control_frames() {
        for raw in [
            r#"{"type":"resize","status":"applied","size":null,"viewers":1}"#,
            r#"{"type":"resize","status":"pending","size":{"rows":1,"cols":80},"viewers":1}"#,
            r#"{"type":"resize","status":"pending","size":null,"viewers":0}"#,
            r#"{"type":"resize","status":"pending","viewers":1}"#,
            "terminal output",
        ] {
            assert!(guest_resize_message(raw).is_none(), "{raw}");
        }
    }
}

fn valid_dimension(value: &Value) -> bool {
    value.as_u64().is_some_and(|n| (2..=1000).contains(&n))
}
fn guest_resize_message(raw: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(raw).ok()?;
    if value["type"] != "resize"
        || !["pending", "applied", "unavailable"].contains(&text(&value, "status"))
        || value["viewers"].as_u64().is_none_or(|n| n == 0)
    {
        return None;
    }
    let size = value.get("size")?;
    if (value["status"] == "applied" && size.is_null())
        || (!size.is_null() && (!valid_dimension(&size["rows"]) || !valid_dimension(&size["cols"])))
    {
        return None;
    }
    Some(value)
}
