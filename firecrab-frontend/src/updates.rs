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
    let phase = text(progress, "phase");
    let ko = app.locale == "ko";
    let target = if text(progress, "target").is_empty() {
        text(check, "latest")
    } else {
        text(progress, "target")
    };
    let following = state.update_running || state.update_pid.is_some();
    let title = if !following {
        if ko {
            format!("v{target}로 업데이트")
        } else {
            format!("Update to v{target}")
        }
    } else {
        match phase {
            "failed" => t("Update failed").into(),
            "done" => {
                if ko {
                    format!("v{target}로 업데이트 완료")
                } else {
                    format!("Updated to v{target}")
                }
            }
            _ => {
                if ko {
                    format!("v{target}로 업데이트 중…")
                } else {
                    format!("Updating to v{target}…")
                }
            }
        }
    };
    let mut out = format!("<h2>{}</h2>", escape(&title));
    if !following {
        out.push_str(&format!(
            "<p>Installed v{}</p><h3>{}</h3><div class=\"release-notes\">{}</div>",
            escape(text(check, "current")),
            t("What's new"),
            if text(check, "notes").is_empty() {
                format!("<p>{}</p>", t("No release notes available."))
            } else {
                crate::notes::render(text(check, "notes"))
            }
        ));
        let url = text(check, "releaseUrl");
        if url.starts_with("https://") {
            out.push_str(&format!(
                "<a href=\"{}\" target=\"_blank\" rel=\"noreferrer noopener\">{}</a>",
                escape(url),
                t("View the release on GitHub")
            ));
        }
    }
    if !progress.is_null() && phase != "idle" {
        let percent = progress["percent"]
            .as_f64()
            .unwrap_or_default()
            .clamp(0.0, 100.0);
        let description = match phase {
            "checking" if progress["updatedAtMs"].is_null() => {
                t("Starting the updater…").to_string()
            }
            "checking" => t("Looking up the newest release…").to_string(),
            "downloading" => match (
                progress["downloadedBytes"].as_f64(),
                progress["totalBytes"].as_f64(),
            ) {
                (Some(done), Some(total)) => {
                    if ko {
                        format!(
                            "내려받는 중 — {:.1} MiB / {:.1} MiB",
                            done / 1048576.0,
                            total / 1048576.0
                        )
                    } else {
                        format!(
                            "Downloading — {:.1} MiB of {:.1} MiB",
                            done / 1048576.0,
                            total / 1048576.0
                        )
                    }
                }
                _ => t("Downloading").into(),
            },
            "verifying" => t("Verifying the bundle").into(),
            "applying" => t("Applying the update").into(),
            "restarting" => t("Restarting the services").into(),
            "done" => t("Completed").into(),
            "failed" => t("Update failed").into(),
            _ => phase.to_string(),
        };
        out.push_str(&format!("<p role=\"status\">{}</p><progress max=\"100\" value=\"{percent}\" aria-valuenow=\"{percent}\" aria-label=\"{}\"></progress><span class=\"gauge-percent\">{percent:.0}%</span>", escape(&description), t("Update progress")));
        let active = match phase {
            "downloading" => 1,
            "verifying" => 2,
            "applying" => 3,
            "restarting" => 4,
            "done" => 5,
            "failed" if percent >= 90.0 => 4,
            "failed" if percent >= 85.0 => 3,
            "failed" if percent >= 75.0 => 2,
            "failed" if percent > 0.0 => 1,
            _ => 0,
        };
        out.push_str("<ol>");
        for (index, label) in [
            "Find release",
            "Download bundle",
            "Verify bundle",
            "Apply update",
            "Restart services",
        ]
        .iter()
        .enumerate()
        {
            let class = if index < active {
                "is-done"
            } else if index == active {
                if phase == "failed" {
                    "is-failed"
                } else {
                    "is-active"
                }
            } else {
                "is-pending"
            };
            out.push_str(&format!(
                "<li class=\"update-stage {class}\">{}</li>",
                t(label)
            ));
        }
        out.push_str("</ol>");
        if !text(progress, "error").is_empty() {
            out.push_str(&format!(
                "<p role=\"alert\">{}</p>",
                escape(text(progress, "error"))
            ));
        }
    }
    if state.update_running {
        out.push_str(&format!("<p>{}</p>", t("Keep this page open")));
    } else {
        out.push_str(&render::button(
            if following { "Close" } else { "Cancel" },
            "update-close",
            "",
        ));
        if phase == "done" && following {
            out.push_str(&render::button("Reload", "update-reload", ""));
        } else if check["updateAvailable"] == true || phase == "failed" {
            out.push_str(&render::button(
                if phase == "failed" {
                    "Try again"
                } else {
                    "Update now"
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
        } else if state.workflows.update_running {
            state.workflows.update_progress["phase"] = json!("restarting");
        }
        drop(state);
        redraw(&app);
    });
}
pub fn resume(app: Shared) {
    spawn_local(async move {
        let Ok(progress) = api::request("GET", "/api/update/progress", None).await else {
            return;
        };
        if !phase_active(text(&progress, "phase")) {
            return;
        }
        let mut state = app.borrow_mut();
        if state.workflows.update_open {
            return;
        }
        state.workflows.update_pid = progress["pid"].as_u64();
        state.workflows.update_progress = progress;
        state.workflows.update_open = true;
        state.workflows.update_running = true;
        drop(state);
        redraw(&app);
    });
}
pub fn tick(app: &Shared) {
    if app.borrow().workflows.update_running {
        poll(app.clone());
    }
    if app.borrow().workflows.manager_pending || app.borrow().route == "host" {
        crate::load_manager(app.clone());
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
        "update-reload" => {
            let _ = crate::window().location().reload();
        }
        "update-close" => {
            close_on_escape(app);
        }
        _ => return false,
    }
    true
}
