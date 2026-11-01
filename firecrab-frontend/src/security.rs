//! On-demand explanations of aggregate service security signals.
use crate::{
    Shared, api, document, element,
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

/// Keep disclosures and operator selections stable when async data redraws the page.
pub struct View {
    expanded: Vec<String>,
    selections: Vec<(String, String)>,
}

pub fn capture_view() -> View {
    let mut view = View {
        expanded: Vec::new(),
        selections: Vec::new(),
    };
    if let Ok(items) = document().query_selector_all(".security-page details[open][id]") {
        for index in 0..items.length() {
            if let Some(node) = items.item(index)
                && let Ok(item) = node.dyn_into::<web_sys::Element>()
            {
                view.expanded.push(item.id());
            }
        }
    }
    if let Ok(items) = document().query_selector_all(".security-page select[id]") {
        for index in 0..items.length() {
            if let Some(node) = items.item(index)
                && let Ok(select) = node.dyn_into::<HtmlSelectElement>()
            {
                view.selections.push((select.id(), select.value()));
            }
        }
    }
    view
}

pub fn restore_view(view: View) {
    for id in view.expanded {
        if let Some(item) = element(&id) {
            let _ = item.set_attribute("open", "");
        }
    }
    for (id, value) in view.selections {
        if let Some(item) = element(&id)
            && let Ok(select) = item.dyn_into::<HtmlSelectElement>()
        {
            let fallback = select.value();
            select.set_value(&value);
            if select.selected_index() < 0 {
                select.set_value(&fallback);
            }
        }
    }
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
    if action == "security-database-link" || action == "security-database-unlink" {
        let linking = action == "security-database-link";
        let selected = element(&format!("security-db-{id}"))
            .and_then(|el| el.dyn_into::<HtmlSelectElement>().ok())
            .map(|el| el.value())
            .unwrap_or_default();
        if linking && selected.is_empty() {
            return true;
        }
        let app = app.clone();
        let database = id.to_owned();
        let generation = app.borrow().generation;
        app.borrow_mut().security.busy = true;
        render_content(&app);
        spawn_local(async move {
            let path = format!(
                "/api/service-security/databases/{}/link",
                api::encode(&database)
            );
            let body = json!({"serviceId": selected});
            let result = api::request(
                if linking { "PUT" } else { "DELETE" },
                &path,
                linking.then_some(&body),
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
