use crate::{Shared, api, model::text, render_content};
use serde_json::Value;
use wasm_bindgen_futures::spawn_local;

pub fn load(app: Shared) {
    if app.borrow().workflows.manager_polling {
        return;
    }
    app.borrow_mut().workflows.manager_polling = true;
    spawn_local(async move {
        let result = api::request("GET", &crate::manager_endpoint(), None).await;
        let mut state = app.borrow_mut();
        state.workflows.manager_polling = false;
        let Ok(value) = result else {
            return;
        };
        let owner = state
            .cache
            .get("/api/host")
            .map(|host| text(&host["platform"], "controllerId"))
            .unwrap_or("");
        if !owner.is_empty() && owner != text(&value, "controllerId") {
            state.cache.remove("manager");
            state.workflows.manager_pending = false;
            state.form = None;
            state.dirty = false;
            state.notice.clear();
            state.error = "This controller belongs to a different host".into();
        } else {
            let operation = &value["operation"];
            let id = text(operation, "id");
            let follows = state
                .workflows
                .manager_operation
                .as_deref()
                .is_some_and(|expected| expected == id)
                || state.workflows.manager_pending
                    && state.workflows.manager_operation.is_none()
                    && (id != state.workflows.manager_previous || id.is_empty());
            if follows {
                if !id.is_empty() {
                    state.workflows.manager_operation = Some(id.into());
                }
                match text(operation, "state") {
                    "applying" => state.workflows.manager_pending = true,
                    "complete" => {
                        state.workflows.manager_pending = false;
                        state.notice = "Resources applied. The management VM is ready.".into();
                        state.error.clear();
                    }
                    "failed" => {
                        state.workflows.manager_pending = false;
                        state.notice.clear();
                        state.error = text(operation, "error").into();
                    }
                    _ => {}
                }
            }
            state.cache.insert("manager".into(), value);
        }
        let redraw = state.route == "host" && state.form.is_none();
        drop(state);
        if redraw {
            render_content(&app);
        }
    });
}

pub fn apply(app: Shared) {
    let mut state = app.borrow_mut();
    if state.workflows.manager_pending {
        return;
    }
    let manager = state.cache.get("manager").cloned().unwrap_or(Value::Null);
    let controller = text(&manager, "controllerId").to_string();
    if controller.is_empty() {
        return;
    }
    state.workflows.manager_pending = true;
    state.workflows.manager_previous = text(&manager["operation"], "id").into();
    state.workflows.manager_operation = None;
    state.error.clear();
    state.notice.clear();
    drop(state);
    render_content(&app);
    spawn_local(async move {
        let result = api::request_detailed(
            "POST",
            &format!("{}/apply", crate::manager_endpoint()),
            None,
            Some(&controller),
        )
        .await;
        let mut state = app.borrow_mut();
        match result {
            Ok(value) => {
                if !text(&value, "id").is_empty() {
                    state.workflows.manager_operation = Some(text(&value, "id").into());
                }
            }
            Err(error) if error.message.starts_with("HTTP ") => {
                state.workflows.manager_pending = false;
                state.error = error.message;
            }
            // The local controller can drop the request while restarting the VM.
            Err(_) => {}
        }
        drop(state);
        load(app);
    });
}
