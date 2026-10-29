//! Project-scoped collaboration messages use the server's current project grants.
use super::*;
use std::fmt::Write;

#[derive(Default)]
pub(super) struct State {
    pub(super) input: String,
    messages: Vec<Value>,
    after: u64,
    can_write: bool,
    loaded: bool,
    polling: bool,
    sending: Option<String>,
    pending: Option<(String, String)>,
    load_error: String,
    send_error: String,
}

pub(super) fn input(app: &Shared, node: &Element) -> bool {
    if node.id() != "canvas-chat-input" {
        return false;
    }
    let Some(input) = node.dyn_ref::<HtmlTextAreaElement>() else {
        return false;
    };
    let mut a = app.borrow_mut();
    a.sandbox.canvas.input = input.value();
    dirty(&mut a);
    let disabled = !can_send(&a.sandbox.canvas);
    let limit_message = if a.sandbox.canvas.input.trim().len() > 4096 {
        tr(
            &a,
            "Use at most 4,096 UTF-8 bytes.",
            "메시지는 UTF-8 기준 4,096바이트까지 입력한다.",
        )
    } else {
        ""
    };
    if let Some(limit) = element("canvas-chat-limit") {
        limit.set_text_content(Some(limit_message));
    }
    drop(a);
    if let Some(button) =
        element("canvas-chat-send").and_then(|n| n.dyn_into::<web_sys::HtmlButtonElement>().ok())
    {
        button.set_disabled(disabled);
    }
    true
}

fn can_send(s: &State) -> bool {
    s.loaded
        && s.can_write
        && s.sending.is_none()
        && (s.pending.is_some() || (!s.input.trim().is_empty() && s.input.trim().len() <= 4096))
}

fn merge(s: &mut State, message: Value) -> bool {
    if text(&message, "id").is_empty() || message["sequence"].as_u64().is_none() {
        return false;
    }
    if !s.messages.iter().any(|m| m["id"] == message["id"]) {
        s.messages.push(message);
        s.messages
            .sort_by_key(|m| m["sequence"].as_u64().unwrap_or(0));
        if s.messages.len() > 200 {
            s.messages.drain(..s.messages.len() - 200);
        }
        return true;
    }
    false
}

/// Poll from the last server cursor, independently of AI task polling.
pub(super) fn tick(app: &Shared) {
    let mut a = app.borrow_mut();
    if a.route != "ai-canvas" || a.sandbox.project.is_empty() || a.sandbox.canvas.polling {
        return;
    }
    let project = a.sandbox.project.clone();
    let context = a.sandbox.context;
    let generation = a.generation;
    let canvas = &mut a.sandbox.canvas;
    canvas.polling = true;
    let after = canvas.after;
    drop(a);
    let app = app.clone();
    spawn_local(async move {
        let path = format!(
            "/api/ai-sandbox/projects/{}/canvas/messages?after={after}",
            api::encode(&project)
        );
        let result = api::request_detailed("GET", &path, None, None).await;
        if !live(&app, generation) || app.borrow().route != "ai-canvas" {
            return;
        }
        let mut a = app.borrow_mut();
        if a.sandbox.context != context {
            return;
        }
        let s = &mut a.sandbox.canvas;
        s.polling = false;
        let mut changed = false;
        let mut more = false;
        match result {
            Ok(page) => {
                let can_write = page["canWrite"].as_bool().unwrap_or(false);
                changed |= !s.loaded || s.can_write != can_write || !s.load_error.is_empty();
                s.loaded = true;
                s.can_write = can_write;
                s.load_error.clear();
                for message in list(&page["messages"]) {
                    let Some(sequence) = message["sequence"].as_u64() else {
                        continue;
                    };
                    s.after = s.after.max(sequence);
                    if s.pending
                        .as_ref()
                        .is_some_and(|(id, _)| message["id"] == *id)
                    {
                        s.pending = None;
                        s.sending = None;
                        s.input.clear();
                        s.send_error.clear();
                        changed = true;
                    }
                    changed |= merge(s, message.clone());
                }
                more = list(&page["messages"]).len() == 200 && s.after > after;
            }
            Err(error) => {
                let denied = matches!(error.status, Some(401 | 403));
                if denied {
                    s.messages.clear();
                    s.after = 0;
                    s.can_write = false;
                    s.loaded = false;
                }
                let message = error.to_string();
                changed |= s.load_error != message || denied;
                s.load_error = message;
            }
        }
        dirty(&mut a);
        drop(a);
        if changed {
            render(&app);
        }
        if more {
            tick(&app);
        }
    });
}

/// Preserve a message identity after an uncertain response so retry is safe.
pub(super) fn send(app: &Shared) {
    let mut a = app.borrow_mut();
    if a.route != "ai-canvas" || !can_send(&a.sandbox.canvas) {
        return;
    }
    let project = a.sandbox.project.clone();
    let context = a.sandbox.context;
    let generation = a.generation;
    let s = &mut a.sandbox.canvas;
    let (id, message) = if let Some(pending) = &s.pending {
        pending.clone()
    } else {
        let Ok(id) = uuid() else {
            s.send_error = "Cannot create a message ID".into();
            drop(a);
            render(app);
            return;
        };
        let pending = (id, s.input.trim().to_owned());
        s.pending = Some(pending.clone());
        pending
    };
    s.sending = Some(id.clone());
    s.send_error.clear();
    drop(a);
    render(app);
    let app = app.clone();
    spawn_local(async move {
        let path = format!(
            "/api/ai-sandbox/projects/{}/canvas/messages/{}",
            api::encode(&project),
            api::encode(&id)
        );
        let result =
            api::request_detailed("PUT", &path, Some(&json!({"text":message})), None).await;
        if !live(&app, generation) || app.borrow().route != "ai-canvas" {
            return;
        }
        let mut a = app.borrow_mut();
        if a.sandbox.context != context {
            return;
        }
        let s = &mut a.sandbox.canvas;
        if s.sending.as_deref() != Some(&id) {
            return;
        }
        s.sending = None;
        match result {
            Ok(message) => {
                merge(s, message);
                if s.pending.as_ref().is_some_and(|(key, _)| *key == id) {
                    s.pending = None;
                    s.input.clear();
                }
                s.send_error.clear();
            }
            Err(error) if s.pending.as_ref().is_some_and(|(key, _)| *key == id) => {
                if matches!(error.status, Some(401 | 403)) {
                    s.can_write = false;
                }
                if matches!(error.status, Some(400 | 401 | 403 | 409 | 413 | 415 | 422)) {
                    s.pending = None;
                }
                s.send_error = error.to_string();
            }
            Err(_) => {}
        }
        dirty(&mut a);
        drop(a);
        render(&app);
        tick(&app);
    });
}

/// Render server-provided message text as escaped plain text.
pub(super) fn content(a: &App, out: &mut String) {
    let s = &a.sandbox.canvas;
    let _ = write!(
        out,
        "<section class=\"canvas-chat\" aria-label=\"{}\"><header class=\"canvas-panel-header\"><div>{}<h3>{}</h3></div></header><div id=\"canvas-chat-log\" class=\"canvas-chat-log\" role=\"log\" aria-live=\"polite\">",
        tr(a, "Shared chat", "공유 대화"),
        icon("message"),
        tr(a, "Shared chat", "공유 대화")
    );
    if !s.load_error.is_empty() {
        let _ = write!(
            out,
            "<p class=\"canvas-chat-error\" role=\"alert\">{}</p>",
            escape(&s.load_error)
        );
    } else if s.loaded && s.messages.is_empty() {
        let _ = write!(
            out,
            "<p class=\"canvas-chat-empty\">{}</p>",
            tr(a, "No shared messages yet.", "공유 대화가 아직 없다.")
        );
    }
    for message in &s.messages {
        let author = if message["author"] == "local" {
            tr(a, "Local operator", "로컬 사용자").to_owned()
        } else {
            let host = text(&message["author"], "host");
            format!(
                "{} {}",
                tr(a, "Host", "호스트"),
                host.chars().take(8).collect::<String>()
            )
        };
        let _ = write!(
            out,
            "<article class=\"canvas-chat-message\"><strong>{}</strong><p>{}</p></article>",
            escape(&author),
            escape(text(message, "text"))
        );
    }
    let _ = write!(
        out,
        "</div><form id=\"canvas-chat-form\" class=\"canvas-chat-form\"><label class=\"sr-only\" for=\"canvas-chat-input\">{}</label><textarea id=\"canvas-chat-input\" rows=\"2\" maxlength=\"4096\" placeholder=\"{}\"{}>{}</textarea><button id=\"canvas-chat-send\" class=\"btn\" type=\"submit\"{}>{}</button></form>",
        tr(a, "Shared message", "공유 메시지"),
        if s.loaded && !s.can_write {
            tr(a, "Read-only access", "읽기 전용")
        } else {
            tr(a, "Write a shared message…", "함께 볼 메시지를 입력한다…")
        },
        if s.sending.is_some() || s.pending.is_some() || !s.can_write {
            " disabled"
        } else {
            ""
        },
        escape(&s.input),
        if can_send(s) { "" } else { " disabled" },
        if s.pending.is_some() && s.sending.is_none() {
            tr(a, "Retry", "다시 시도")
        } else {
            tr(a, "Send", "전송")
        },
    );
    let _ = write!(
        out,
        "<p id=\"canvas-chat-limit\" class=\"canvas-chat-error\" role=\"status\">{}</p>",
        if s.input.trim().len() > 4096 {
            tr(
                a,
                "Use at most 4,096 UTF-8 bytes.",
                "메시지는 UTF-8 기준 4,096바이트까지 입력한다.",
            )
        } else {
            ""
        }
    );
    if !s.send_error.is_empty() {
        let _ = write!(
            out,
            "<p class=\"canvas-chat-error\" role=\"alert\">{}</p>",
            escape(&s.send_error)
        );
    }
    out.push_str("</section>");
}
