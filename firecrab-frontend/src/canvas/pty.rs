//! Browser reconnect attaches by session UUID; it never creates or replays a shell command.
use super::*;
use web_sys::{CustomEvent, Event, MessageEvent, WebSocket};

pub(super) struct Session {
    project: String,
    id: String,
    metadata: Value,
    control: Option<Value>,
    cursor: u64,
    socket: Option<WebSocket>,
    callbacks: Vec<Closure<dyn FnMut(Event)>>,
    error: String,
    next_retry: f64,
    epoch: u64,
}
impl Drop for Session {
    fn drop(&mut self) {
        if let Some(control) = self.control.take() {
            let url = format!("{}/control", path(&self.project, &self.id));
            spawn_local(async move {
                let _ = workspace_request("DELETE", &url, Some(&control)).await;
            });
        }
        if let Some(s) = self.socket.take() {
            s.set_onopen(None);
            s.set_onmessage(None);
            s.set_onclose(None);
            s.set_onerror(None);
            let _ = s.close();
        }
    }
}
pub(super) fn binding(action: &str, payload: JsValue) -> JsValue {
    let function = js_sys::Reflect::get(&window(), &JsValue::from_str("firecrabCanvasPanes"))
        .ok()
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok());
    function
        .and_then(|f| {
            f.call2(&JsValue::NULL, &JsValue::from_str(action), &payload)
                .ok()
        })
        .unwrap_or(JsValue::UNDEFINED)
}
fn key(project: &str, id: &str) -> String {
    format!("{project}/{id}")
}
fn path(project: &str, id: &str) -> String {
    format!("{}/terminals/{}", base(project), api::encode(id))
}
fn send(session: &Session, value: &Value) -> Result<(), String> {
    let s = session
        .socket
        .as_ref()
        .filter(|s| s.ready_state() == WebSocket::OPEN)
        .ok_or("Terminal is disconnected")?;
    s.send_with_str(&value.to_string()).map_err(api::error)
}
fn bytes(encoded: &str) -> Vec<u8> {
    window()
        .atob(encoded)
        .unwrap_or_default()
        .chars()
        .map(|c| c as u8)
        .collect()
}
fn b64(bytes: &[u8]) -> String {
    window()
        .btoa(&bytes.iter().map(|b| char::from(*b)).collect::<String>())
        .unwrap_or_default()
}
pub(super) fn card(a: &App, t: &Tab, n: &Node) -> String {
    let k = key(&t.identity.project, &n.id);
    let session = a.canvas.ptys.get(&k);
    let controlled = session.is_some_and(|s| s.control.is_some());
    let status = session
        .map(|s| text(&s.metadata, "status"))
        .unwrap_or("connecting");
    format!(
        "<div class=\"cw-pty-status\"><span>{}</span><span>{}</span></div><div class=\"cw-pty\" id=\"cw-pty-{}\" data-key=\"{}\"></div><footer><button data-action=\"cw-control\" data-id=\"{}\">{}</button><button data-action=\"cw-stop\" data-id=\"{}\" {}>{}</button><span>{}</span></footer>",
        escape(status),
        if controlled {
            tr(a, "You control input", "내가 입력 제어 중")
        } else {
            tr(a, "View only", "보기 전용")
        },
        escape(&n.id),
        escape(&k),
        escape(&n.id),
        tr(a, "Take input control", "입력 제어권 가져오기"),
        escape(&n.id),
        if controlled { "" } else { "disabled" },
        tr(a, "Stop", "종료"),
        escape(session.map(|s| s.error.as_str()).unwrap_or(""))
    )
}
pub(super) fn attach(app: &Shared) {
    let a = app.borrow();
    let Some(t) = tab(&a.canvas) else {
        return;
    };
    let project = t.identity.project.clone();
    let nodes: Vec<_> = t
        .layout
        .nodes
        .iter()
        .filter(|n| n.kind == "terminal")
        .map(|n| n.id.clone())
        .collect();
    let surfaces:Vec<_>=nodes.iter().map(|id|json!({"id":format!("{}-{id}",t.identity.id),"element":format!("cw-pty-{id}"),"key":key(&project,id),"options":crate::terminal::Prefs::default().options(id)})).collect();
    let keep: Vec<_> = a
        .canvas
        .tabs
        .iter()
        .flat_map(|tab| {
            tab.layout
                .nodes
                .iter()
                .filter(|n| n.kind == "terminal")
                .map(|n| format!("{}-{}", tab.identity.id, n.id))
        })
        .collect();
    let editor = t
        .editor
        .as_ref()
        .map(|url| json!({"id":t.identity.id,"url":url}));
    let references: std::collections::BTreeSet<_> = a
        .canvas
        .tabs
        .iter()
        .flat_map(|tab| {
            tab.layout
                .nodes
                .iter()
                .filter(|n| n.kind == "terminal")
                .map(|n| key(&tab.identity.project, &n.id))
        })
        .collect();
    drop(a);
    app.borrow_mut()
        .canvas
        .ptys
        .retain(|k, _| references.contains(k));
    binding(
        "attach",
        JsValue::from_str(&json!({"surfaces":surfaces,"editor":editor,"keep":keep}).to_string()),
    );
    for id in nodes {
        connect(app, &project, &id);
    }
}
fn connect(app: &Shared, project: &str, id: &str) {
    let k = key(project, id);
    let mut a = app.borrow_mut();
    let session = a.canvas.ptys.entry(k.clone()).or_insert_with(|| Session {
        project: project.into(),
        id: id.into(),
        metadata: Value::Null,
        control: None,
        cursor: 0,
        socket: None,
        callbacks: vec![],
        error: String::new(),
        next_retry: 0.,
        epoch: 0,
    });
    if session.socket.as_ref().is_some_and(|s| {
        s.ready_state() == WebSocket::CONNECTING || s.ready_state() == WebSocket::OPEN
    }) || session.next_retry > js_sys::Date::now()
    {
        return;
    }
    if let Some(old) = session.socket.take() {
        old.set_onopen(None);
        old.set_onmessage(None);
        old.set_onclose(None);
        old.set_onerror(None);
        let _ = old.close();
    }
    session.callbacks.clear();
    session.epoch += 1;
    let epoch = session.epoch;
    let cursor = session.cursor;
    let protocol = if window().location().protocol().unwrap_or_default() == "https:" {
        "wss"
    } else {
        "ws"
    };
    let host = window().location().host().unwrap_or_default();
    let url = format!(
        "{protocol}://{host}{}/socket?cursor={cursor}",
        path(project, id)
    );
    let Ok(socket) = WebSocket::new_with_str(&url, "firecrab.terminal.v1") else {
        return;
    };
    for name in ["open", "message", "close", "error"] {
        let app = app.clone();
        let k = k.clone();
        let event_kind = name.to_string();
        let callback = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let mut a = app.borrow_mut();
            let Some(s) = a.canvas.ptys.get_mut(&k).filter(|s| s.epoch == epoch) else {
                return;
            };
            let mut redraw = false;
            let mut effects = Vec::new();
            match event_kind.as_str() {
                "message" => {
                    let Some(value) = event
                        .dyn_ref::<MessageEvent>()
                        .and_then(|e| e.data().as_string())
                        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                    else {
                        return;
                    };
                    match text(&value, "type") {
                        "state" => {
                            redraw = s.metadata["status"] != value["terminal"]["status"]
                                || s.metadata["controller"]["clientId"]
                                    != value["terminal"]["controller"]["clientId"];
                            s.metadata = value["terminal"].clone();
                            if let Some(c) = &s.control
                                && (s.metadata["controller"]["clientId"] != c["clientId"]
                                    || s.metadata["controlGeneration"] != c["generation"])
                            {
                                s.control = None;
                            }
                            effects.push(("shared",JsValue::from_str(&json!({"key":k,"rows":s.metadata["rows"],"cols":s.metadata["columns"]}).to_string())));
                        }
                        "output" => {
                            let page = &value["output"];
                            let start = page["start"].as_u64().unwrap_or(s.cursor);
                            let next = page["next"].as_u64().unwrap_or(s.cursor);
                            if start > s.cursor {
                                s.error = "Earlier output was truncated".into();
                            }
                            if next >= s.cursor {
                                let data = bytes(text(page, "contentBase64"));
                                let skip = s.cursor.saturating_sub(start) as usize;
                                if skip < data.len() {
                                    let data = js_sys::Uint8Array::from(&data[skip..]);
                                    let payload = js_sys::Object::new();
                                    let _ = js_sys::Reflect::set(
                                        &payload,
                                        &"key".into(),
                                        &k.clone().into(),
                                    );
                                    let _ = js_sys::Reflect::set(&payload, &"bytes".into(), &data);
                                    effects.push(("write", payload.into()));
                                }
                                s.cursor = next;
                            }
                        }
                        "control_error" => {
                            s.control = None;
                            s.error = text(&value, "message").into();
                            redraw = true;
                        }
                        _ => {}
                    }
                }
                "close" | "error" => {
                    s.control = None;
                    s.error = "Connection lost; input was not replayed".into();
                    s.next_retry = js_sys::Date::now() + 3000.;
                    redraw = true;
                }
                "open" => {
                    s.error.clear();
                }
                _ => {}
            }
            drop(a);
            for (action, payload) in effects {
                binding(action, payload);
            }
            if redraw {
                render(&app);
            }
        });
        match name {
            "open" => socket.set_onopen(Some(callback.as_ref().unchecked_ref())),
            "message" => socket.set_onmessage(Some(callback.as_ref().unchecked_ref())),
            "close" => socket.set_onclose(Some(callback.as_ref().unchecked_ref())),
            _ => socket.set_onerror(Some(callback.as_ref().unchecked_ref())),
        }
        session.callbacks.push(callback);
    }
    session.socket = Some(socket);
}
pub(super) fn create(app: &Shared) {
    let mut a = app.borrow_mut();
    let Some(t) = tab_mut(&mut a.canvas).filter(|t| t.loaded) else {
        return;
    };
    if t.layout.nodes.len() >= 24 {
        return;
    }
    let id = uuid();
    let project = t.identity.project.clone();
    let request = json!({"label":format!("Terminal {}",t.layout.nodes.iter().filter(|n|n.kind=="terminal").count()+1),"workingDirectory":t.layout.directory,"rows":24,"columns":80});
    let offset = t.layout.nodes.len() as f64 * 25.;
    t.layout.nodes.push(Node {
        id: id.clone(),
        kind: "terminal".into(),
        x: 100. + offset,
        y: 100. + offset,
        width: 600.,
        height: 380.,
        content: request.to_string(),
    });
    drop(a);
    changed(app);
    render(app);
    let app = app.clone();
    spawn_local(async move {
        let result = workspace_request("PUT", &path(&project, &id), Some(&request)).await;
        let mut a = app.borrow_mut();
        match result {
            Ok(metadata) => {
                let k = key(&project, &id);
                if let Some(s) = a.canvas.ptys.get_mut(&k) {
                    s.metadata = metadata;
                    s.error.clear();
                }
            }
            Err(e) => {
                if let Some(t) = tab_mut(&mut a.canvas) {
                    t.error = e;
                }
            }
        }
        drop(a);
        render(&app);
    });
}
pub(super) fn action(app: &Shared, action: &str, id: &str) -> bool {
    if !matches!(action, "cw-control" | "cw-stop") {
        return false;
    }
    let a = app.borrow();
    let Some(t) = tab(&a.canvas) else {
        return true;
    };
    let project = t.identity.project.clone();
    let k = key(&project, id);
    let Some(s) = a.canvas.ptys.get(&k) else {
        return true;
    };
    let body = if action == "cw-stop" {
        let Some(c) = s.control.clone() else {
            return true;
        };
        c
    } else {
        json!({"clientId":uuid(),"expectedGeneration":s.metadata["controlGeneration"].as_u64().unwrap_or(0),"takeOver":s.metadata["controller"].is_object()})
    };
    let url = format!(
        "{}/{}",
        path(&project, id),
        if action == "cw-stop" {
            "stop"
        } else {
            "control"
        }
    );
    let claim = action == "cw-control";
    drop(a);
    let app = app.clone();
    spawn_local(async move {
        let result = workspace_request("POST", &url, Some(&body)).await;
        if let Some(s) = app.borrow_mut().canvas.ptys.get_mut(&k) {
            match result {
                Ok(value) => {
                    if claim {
                        s.control = Some(
                            json!({"clientId":value["clientId"],"generation":value["generation"],"token":value["token"]}),
                        );
                    } else {
                        s.control = None;
                    }
                    s.error.clear();
                }
                Err(e) => s.error = e,
            }
        }
        render(&app);
        binding("report", JsValue::from_str(&json!({"key":k}).to_string()));
    });
    true
}
pub(super) fn tick(app: &Shared) {
    let mut retry = vec![];
    let mut a = app.borrow_mut();
    for s in a.canvas.ptys.values_mut() {
        if let Some(control) = s.control.as_ref()
            && let Err(e) = send(s, &json!({"type":"renew","control":control}))
        {
            s.error = e;
            s.control = None;
        }
        if s.socket
            .as_ref()
            .is_none_or(|w| w.ready_state() == WebSocket::CLOSED)
        {
            retry.push((s.project.clone(), s.id.clone()));
        }
    }
    drop(a);
    for (project, id) in retry {
        connect(app, &project, &id);
    }
}
pub(super) fn install(app: &Shared) -> Result<(), JsValue> {
    let app = app.clone();
    let callback = Closure::<dyn FnMut(CustomEvent)>::new(move |event: CustomEvent| {
        let Some(value) = event
            .detail()
            .as_string()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        else {
            return;
        };
        let mut a = app.borrow_mut();
        let Some(s) = a.canvas.ptys.get_mut(text(&value, "key")) else {
            return;
        };
        let Some(control) = s.control.clone() else {
            return;
        };
        let message = if text(&value, "type") == "resize" {
            json!({"type":"resize","control":control,"rows":value["rows"],"columns":value["cols"]})
        } else {
            let bytes = list(&value["bytes"])
                .iter()
                .filter_map(|n| n.as_u64().filter(|n| *n <= 255).map(|n| n as u8))
                .collect::<Vec<_>>();
            if bytes.is_empty() || bytes.len() > 16384 {
                return;
            }
            json!({"type":"input","control":control,"contentBase64":b64(&bytes)})
        };
        if let Err(e) = send(s, &message) {
            s.error = e;
        }
    });
    document().add_event_listener_with_callback(
        "firecrab-canvas-terminal",
        callback.as_ref().unchecked_ref(),
    )?;
    callback.forget();
    Ok(())
}

/// Discover existing retained PTYs without dispatching a new shell.
pub(super) fn catalog(app: Shared) {
    let a = app.borrow();
    let Some(t) = tab(&a.canvas).filter(|t| t.loaded) else {
        return;
    };
    let identity = t.identity.clone();
    drop(a);
    spawn_local(async move {
        let result = workspace_request(
            "GET",
            &format!("{}/terminals", base(&identity.project)),
            None,
        )
        .await;
        if let Some(t) = app
            .borrow_mut()
            .canvas
            .tabs
            .iter_mut()
            .find(|t| t.identity.id == identity.id)
        {
            match result {
                Ok(rows) => t.terminals = list(&rows).to_vec(),
                Err(e) => t.error = e,
            }
        }
        render(&app);
    });
}
/// Attach a retained identity selected from the authorized project catalog.
pub(super) fn attach_existing(app: &Shared) {
    let id = element("cw-existing-terminal")
        .and_then(|e| e.dyn_into::<HtmlSelectElement>().ok())
        .map(|e| e.value())
        .unwrap_or_default();
    let mut a = app.borrow_mut();
    let Some(t) = tab_mut(&mut a.canvas) else {
        return;
    };
    if !t.terminals.iter().any(|r| text(r, "id") == id)
        || t.layout.nodes.iter().any(|n| n.id == id)
        || t.layout.nodes.len() >= 24
    {
        return;
    }
    t.layout.nodes.push(Node {
        id,
        kind: "terminal".into(),
        x: 80.,
        y: 80.,
        width: 600.,
        height: 380.,
        content: String::new(),
    });
    drop(a);
    changed(app);
    render(app);
}
