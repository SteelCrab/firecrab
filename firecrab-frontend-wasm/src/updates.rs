use crate::{
    App, Shared, api, element,
    i18n::t,
    model::{escape, text},
    render, set_html,
};
use serde_json::{Value, json};
use wasm_bindgen_futures::spawn_local;
fn phase_active(phase: &str) -> bool {
    [
        "checking",
        "downloading",
        "verifying",
        "applying",
        "restarting",
    ]
    .contains(&phase)
}
fn body(app: &App) -> String {
    let state = &app.workflows;
    let check = app.cache.get("/api/update").unwrap_or(&Value::Null);
    let progress = &state.update_progress;
    let mut out = format!(
        "<h2>{}</h2><p>{} → {}</p><h3>{}</h3><div class=\"release-notes\"><pre>{}</pre></div>",
        t("Updates"),
        escape(text(check, "current")),
        escape(text(check, "latest")),
        t("Release notes"),
        escape(if text(check, "notes").is_empty() {
            t("No release notes available.")
        } else {
            text(check, "notes")
        })
    );
    let url = text(check, "releaseUrl");
    if url.starts_with("https://") {
        out.push_str(&format!(
            "<a href=\"{}\" target=\"_blank\" rel=\"noreferrer\">GitHub release</a>",
            escape(url)
        ));
    }
    if !progress.is_null() {
        let percent = progress["percent"]
            .as_f64()
            .unwrap_or_default()
            .clamp(0.0, 100.0);
        out.push_str(&format!("<p role=\"status\">{} · {percent:.0}%</p><progress max=\"100\" value=\"{percent}\" aria-label=\"{}\"></progress>",t(text(progress,"phase")),t("Update progress")));
        if let Some(bytes) = progress["downloadedBytes"].as_u64() {
            out.push_str(&format!(
                "<p>{bytes} / {} bytes</p>",
                progress["totalBytes"]
            ));
        }
        if !text(progress, "error").is_empty() {
            out.push_str(&format!(
                "<p role=\"alert\">{}</p>",
                escape(text(progress, "error"))
            ));
        }
    }
    if !state.update_running {
        out.push_str(&render::button("Close", "update-close", ""));
        if check["updateAvailable"] == true || progress["phase"] == "failed" {
            out.push_str(&render::button(
                if progress["phase"] == "failed" {
                    "Retry"
                } else {
                    "Install update"
                },
                "update-start",
                "",
            ));
        }
    }
    out
}
pub fn redraw(app: &Shared) {
    let state = app.borrow();
    if !state.workflows.update_open {
        return;
    }
    let html = body(&state);
    drop(state);
    if element("update-content").is_some() {
        set_html("update-content", &html);
    } else {
        set_html(
            "modal-root",
            &format!(
                "<dialog open class=\"wasm-modal\" aria-modal=\"true\" aria-label=\"{}\"><div id=\"update-content\">{html}</div></dialog>",
                t("Updates")
            ),
        );
    }
}
fn open(app: Shared) {
    app.borrow_mut().workflows.update_open = true;
    redraw(&app);
    let app_copy = app.clone();
    spawn_local(async move {
        if let Ok(check) = api::request("GET", "/api/update", None).await {
            app_copy
                .borrow_mut()
                .cache
                .insert("/api/update".into(), check);
            redraw(&app_copy);
        }
    });
    poll(app);
}
fn start(app: Shared) {
    if app.borrow().workflows.update_running {
        return;
    }
    {
        let mut state = app.borrow_mut();
        state.workflows.update_running = true;
        state.workflows.update_after = js_sys::Date::now();
        state.workflows.update_pid = None;
        state.workflows.update_progress = json!({"phase":"checking","percent":0});
    }
    redraw(&app);
    spawn_local(async move {
        let result = api::request("POST", "/api/update", None).await;
        let mut state = app.borrow_mut();
        match result {
            Ok(response) => {
                state.workflows.update_pid = response["pid"].as_u64();
                if state.workflows.update_pid.is_none() {
                    state.workflows.update_running = false;
                    state.workflows.update_progress = json!({"phase":"failed","percent":0,"error":"Updater did not return a run PID."});
                }
            }
            Err(error) => {
                let rejected = error.starts_with("HTTP ");
                state.workflows.update_running = !rejected;
                state.workflows.update_progress = json!({"phase":if rejected{"failed"}else{"checking"},"percent":0,"error":error});
            }
        }
        drop(state);
        redraw(&app);
        poll(app);
    });
}
fn poll(app: Shared) {
    let mut state = app.borrow_mut();
    if !state.workflows.update_open || state.workflows.update_polling {
        return;
    }
    state.workflows.update_polling = true;
    let expected = state.workflows.update_pid;
    let after = state.workflows.update_after;
    drop(state);
    spawn_local(async move {
        let result = api::request("GET", "/api/update/progress", None).await;
        let mut state = app.borrow_mut();
        state.workflows.update_polling = false;
        if !state.workflows.update_open || state.workflows.update_pid != expected {
            return;
        }
        if let Ok(progress) = result {
            if expected.is_some() && progress["pid"].as_u64() != expected {
                return;
            }
            if expected.is_none()
                && after > 0.0
                && progress["updatedAtMs"].as_f64().unwrap_or_default() < after
            {
                return;
            }
            let phase = text(&progress, "phase");
            if expected.is_none() && phase_active(phase) {
                state.workflows.update_pid = progress["pid"].as_u64();
            }
            state.workflows.update_running = phase_active(phase);
            state.workflows.update_progress = progress;
        }
        drop(state);
        redraw(&app);
    });
}
pub fn tick(app: &Shared) {
    if app.borrow().workflows.update_running {
        poll(app.clone());
    }
    if app.borrow().workflows.manager_pending {
        let app = app.clone();
        spawn_local(async move {
            if let Ok(value) = api::request("GET", &crate::manager_endpoint(), None).await {
                let mut state = app.borrow_mut();
                state.workflows.manager_pending = value["operation"]["state"] == "applying";
                state.cache.insert("manager".into(), value);
                let show = state.route == "host" && state.form.is_none();
                drop(state);
                if show {
                    crate::render_content(&app);
                }
            }
        });
    }
}
pub fn close_on_escape(app: &Shared) -> bool {
    let mut state = app.borrow_mut();
    if !state.workflows.update_open {
        return false;
    }
    if state.workflows.update_running {
        return true;
    }
    state.workflows.update_open = false;
    drop(state);
    set_html("modal-root", "");
    true
}
pub fn dispatch(app: &Shared, action: &str) -> bool {
    match action {
        "update-open" => open(app.clone()),
        "update-start" => start(app.clone()),
        "update-close" => {
            close_on_escape(app);
        }
        _ => return false,
    }
    true
}
