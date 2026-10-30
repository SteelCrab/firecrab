use crate::{
    App, Shared, api,
    i18n::t,
    model::{escape, list, text},
    render,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use wasm_bindgen_futures::spawn_local;

#[derive(Default)]
pub struct State {
    pub installs: BTreeMap<String, Value>,
    polling: Option<u64>,
}

pub fn install_version(path: &str) -> Option<&str> {
    path.strip_prefix("/api/kernels/")?.strip_suffix("/install")
}

/// Read server-owned jobs on entry and on each catalog refresh, including jobs
/// started in another browser. Reading a snapshot never starts a new build.
pub fn refresh(app: &Shared) {
    let mut state = app.borrow_mut();
    if state.route != "kernels" || state.kernels.polling == Some(state.generation) {
        return;
    }
    let generation = state.generation;
    let versions: Vec<String> = list(&state.resource)
        .iter()
        .map(|row| text(row, "version").to_owned())
        .collect();
    state.kernels.polling = Some(generation);
    drop(state);
    let app = app.clone();
    spawn_local(async move {
        for version in versions {
            let result = api::request(
                "GET",
                &format!("/api/kernels/{}/install", api::encode(&version)),
                None,
            )
            .await;
            let mut state = app.borrow_mut();
            if state.generation != generation {
                return;
            }
            let snapshot =
                result.unwrap_or_else(|error| json!({"status":"unavailable", "error":error}));
            state.kernels.installs.insert(version, snapshot);
        }
        let mut state = app.borrow_mut();
        state.kernels.polling = None;
        if let Some(version) = state
            .detail
            .as_ref()
            .map(|data| text(data, "version").to_owned())
            && let Some(current) = list(&state.resource)
                .iter()
                .find(|row| row["version"] == version)
                .cloned()
        {
            state.detail = Some(current);
        }
        drop(state);
        if !app.borrow().busy {
            crate::render_content(&app);
        }
    });
}

pub fn status(app: &App, data: &Value) -> &'static str {
    let snapshot = app.kernels.installs.get(text(data, "version"));
    match snapshot.map(|value| text(value, "status")) {
        Some("running") => "Installing kernel",
        Some("failed") => "Installation failed",
        Some("unavailable") => "Status unavailable",
        Some("succeeded") => "Installed",
        _ if data["installed"] == true => "Installed",
        Some("idle") => "Not installed",
        _ => "Checking status…",
    }
}

pub fn can_install(app: &App, data: &Value) -> bool {
    !app.busy
        && app
            .kernels
            .installs
            .get(text(data, "version"))
            .is_some_and(|snapshot| {
                matches!(text(snapshot, "status"), "idle" | "failed" | "succeeded")
            })
}

pub fn panel(app: &App, data: &Value) -> String {
    let snapshot = app.kernels.installs.get(text(data, "version"));
    let mut summary = json!({"architecture": data["architecture"], "status": t(status(app, data))});
    if let Some(bytes) = snapshot.and_then(|value| value["downloadedBytes"].as_u64()) {
        summary["downloaded"] = render::human_bytes(bytes).into();
    }
    let mut html = render::properties(&summary);
    if let Some(snapshot) = snapshot {
        if let Some(error) = snapshot["error"].as_str() {
            html.push_str(&format!("<p role=\"alert\">{}</p>", escape(error)));
        }
        let log = text(snapshot, "log");
        if !log.is_empty() {
            let label = escape(t("Installation log"));
            html.push_str(&format!("<section class=\"kernel-install\"><h3>{label}</h3><pre id=\"kernel-install-log\" class=\"kernel-install-log\" tabindex=\"0\" role=\"region\" aria-label=\"{label}\">{}</pre></section>", escape(log)));
        }
    }
    html
}
