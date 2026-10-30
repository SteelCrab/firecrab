//! On-demand explanations of aggregate service security signals.
use crate::{
    Shared, api, element,
    model::{list, text},
    render_content,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlSelectElement;

#[derive(Default)]
pub struct State {
    pub models: Vec<Value>,
    pub explanations: BTreeMap<String, Value>,
    pub busy: bool,
}

pub fn load(app: &Shared) {
    app.borrow_mut().security.busy = false;
    let app = app.clone();
    let generation = app.borrow().generation;
    spawn_local(async move {
        let mut models = Vec::new();
        if let Ok(accounts) = api::request("GET", "/api/pipe/ai/connections", None).await {
            for account in list(&accounts) {
                let id = text(account, "id");
                if let Ok(catalog) = api::request(
                    "GET",
                    &format!("/api/ai-sandbox/connections/{}/models", api::encode(id)),
                    None,
                )
                .await
                {
                    for model in list(&catalog["models"])
                        .iter()
                        .filter(|model| model["verified"] == true)
                    {
                        models.push(json!({"connectionId":id,"model":model["id"],"account":account["name"],"name":model["name"]}));
                    }
                }
            }
        }
        let mut state = app.borrow_mut();
        if state.generation != generation || state.route != "security" {
            return;
        }
        state.security.models = models;
        drop(state);
        render_content(&app);
    });
}

pub fn dispatch(app: &Shared, action: &str, id: &str) -> bool {
    if action == "security-scan" {
        let app = app.clone();
        let service = id.to_owned();
        let generation = app.borrow().generation;
        app.borrow_mut().security.busy = true;
        render_content(&app);
        spawn_local(async move {
            let result = api::request(
                "POST",
                &format!("/api/service-security/{}/scan", api::encode(&service)),
                None,
            )
            .await;
            let mut state = app.borrow_mut();
            if state.generation != generation || state.route != "security" {
                return;
            }
            state.security.busy = false;
            match result {
                Ok(_) => state.error.clear(),
                Err(error) => state.error = error,
            }
            drop(state);
            crate::refresh(app);
        });
        return true;
    }
    if action != "security-explain" {
        return false;
    }
    let selection = element("security-model")
        .and_then(|el| el.dyn_into::<HtmlSelectElement>().ok())
        .and_then(|el| serde_json::from_str::<Value>(&el.value()).ok());
    let Some(selection) = selection else {
        return true;
    };
    let app = app.clone();
    let service = id.to_owned();
    let generation = app.borrow().generation;
    app.borrow_mut().security.busy = true;
    render_content(&app);
    spawn_local(async move {
        let path = format!("/api/service-security/{}/analysis", api::encode(&service));
        let result = api::request("POST", &path, Some(&selection)).await;
        let mut state = app.borrow_mut();
        if state.generation != generation || state.route != "security" {
            return;
        }
        state.security.busy = false;
        match result {
            Ok(value) => {
                state.security.explanations.insert(service, value);
                state.error.clear();
            }
            Err(error) => state.error = error,
        }
        drop(state);
        render_content(&app);
    });
    true
}
