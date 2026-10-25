use crate::{
    App, Shared, api, element,
    model::{escape, list, text},
    render::{button, request},
    render_content, render_messages, set_html,
};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
pub fn connections(app: &Shared) {
    for path in ["/api/secrets/connections", "/api/pipe/auth"] {
        load_connections(app, path);
    }
}
fn load_connections(app: &Shared, path: &'static str) {
    let a = app.clone();
    let generation = app.borrow().generation;
    spawn_local(async move {
        let result = api::request("GET", path, None).await;
        let mut s = a.borrow_mut();
        if s.generation != generation {
            return;
        }
        match result {
            Ok(v) => {
                s.cache.insert(path.into(), v);
            }
            Err(e) => s.error = e,
        };
        let editing = s.form.is_some();
        drop(s);
        if !editing {
            render_content(&a);
        }
        render_messages(&a);
    });
}
pub fn content(app: &App) -> String {
    let external = app.advanced.secrets_external;
    let mut out = format!(
        "<div role=\"tablist\" aria-label=\"Secret stores\"><button class=\"btn\" role=\"tab\" aria-selected=\"{}\" data-action=\"secret-internal\">Internal</button><button class=\"btn\" role=\"tab\" aria-selected=\"{external}\" data-action=\"secret-external\">External connections</button></div>",
        !external
    );
    out.push_str(&account_card(app.cache.get("/api/pipe/auth"), app.busy));
    if external {
        out.push_str(&button("Add connection", "create", "secret-connection"));
        for link in list(
            app.cache
                .get("/api/secrets/connections")
                .unwrap_or(&Value::Null),
        ) {
            let id = text(link, "id");
            let name = text(link, "name");
            out.push_str(&format!(
                "<article class=\"wasm-card\"><h2>{}</h2><p>{}</p>{}{}{}</article>",
                escape(name),
                escape(text(link, "provider")),
                button(&format!("Import from {name}"), "secret-import", id),
                button(
                    &format!("Edit connection {name}"),
                    "secret-edit-connection",
                    id
                ),
                request(
                    &format!("Delete connection {name}"),
                    "DELETE",
                    &format!("/api/secrets/connections/{}", api::encode(id)),
                    Value::Null
                )
            ));
        }
    } else {
        out.push_str("<section aria-label=\"Internal secrets\">");
        if list(&app.resource).is_empty() {
            out.push_str("<p>No secrets yet.</p>");
        }
        for row in list(&app.resource) {
            let id = text(row, "id");
            let name = text(row, "name");
            let reveal = app.advanced.reveals.get(id);
            out.push_str(&format!("<article class=\"wasm-card\"><label><input type=\"checkbox\" data-secret-id=\"{}\" aria-label=\"Select {}\"> {}</label><p>{}</p><code aria-label=\"{} value\">{}</code>{}{}{}</article>",escape(id),escape(name),escape(name),escape(text(row,"description")),escape(name),escape(reveal.map(|(v,_)|v.as_str()).unwrap_or("••••••••")),button(&format!("{} {name}",if reveal.is_some(){"Hide"}else{"Reveal"}),"secret-reveal",id),button(&format!("Edit {name}"),"secret-edit",id),request(&format!("Delete {name}"),"DELETE",&format!("/api/secrets/{}",api::encode(id)),Value::Null)));
            if let Some(source) = row.get("source").filter(|v| v.is_object()) {
                out.push_str(&format!(
                    "<p>{}: {} · v{}</p>",
                    crate::i18n::t("Imported from"),
                    escape(text(source, "connectionName")),
                    row["version"].as_u64().unwrap_or(1)
                ));
            }
        }
        out.push_str("<label for=\"upload-destination\">Upload destination</label><select id=\"upload-destination\">");
        for link in list(
            app.cache
                .get("/api/secrets/connections")
                .unwrap_or(&Value::Null),
        ) {
            out.push_str(&format!(
                "<option value=\"{}\">{} · {}</option>",
                escape(text(link, "id")),
                escape(text(link, "name")),
                match text(link, "provider") {
                    "firecrab" => "Firecrab · PipeSecrets",
                    "infisical" => "Infisical",
                    _ => "AWS Secrets Manager",
                }
            ));
        }
        out.push_str("</select>");
        out.push_str(&button("Preview upload", "secret-export", ""));
        out.push_str("</section>");
    }
    out.push_str(&format!("<div id=\"secret-plan\">{}</div>", plan(app)));
    out
}
fn account_card(auth: Option<&Value>, busy: bool) -> String {
    use crate::i18n::t;
    let status = match auth {
        None => format!("<p>{}</p>", t("Loading account connection…")),
        Some(auth) if list(auth).iter().any(|a| a["provider"] == "firecrab") => {
            format!(
                "<p>{}</p>{}",
                t(
                    "Preview secrets from the Firecrab account connected in Auth. Only selected secrets are copied locally."
                ),
                button("Preview account secrets", "secret-account-import", "").replace(
                    "type=\"button\"",
                    if busy {
                        "type=\"button\" disabled"
                    } else {
                        "type=\"button\""
                    }
                )
            )
        }
        Some(_) => format!(
            "<p>{}</p><a href=\"#/pipe/auth\">{}</a>",
            t("Connect a Firecrab account in Auth to import account secrets."),
            t("Open Auth")
        ),
    };
    format!(
        "<section class=\"wasm-card\" aria-label=\"Firecrab account secrets\"><h2>{}</h2>{status}</section>",
        t("Firecrab account secrets")
    )
}
pub fn sync_editor() {
    let Some(value) =
        element("field-value").and_then(|el| el.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
    else {
        return;
    };
    let replacing = element("field-replaceValue")
        .is_none_or(|el| el.unchecked_into::<web_sys::HtmlInputElement>().checked());
    if let Some(field) = value
        .closest(".field")
        .ok()
        .flatten()
        .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
    {
        field.set_hidden(!replacing);
    }
    if !replacing {
        value.set_value("");
    }
    let visible = element("secret-show-value")
        .is_some_and(|el| el.unchecked_into::<web_sys::HtmlInputElement>().checked());
    let _ = value.set_attribute(
        "style",
        if visible {
            ""
        } else {
            "-webkit-text-security:disc"
        },
    );
}
fn plan(app: &App) -> String {
    let Some(plan) = &app.advanced.plan else {
        return String::new();
    };
    let import = plan["direction"] == "import";
    let mut out = format!(
        "<section class=\"wasm-card\"><h2>{}</h2>",
        if import {
            "Import preview"
        } else {
            "Upload preview"
        }
    );
    if app.advanced.plan_expired {
        out.push_str("<p role=\"alert\">This preview expired. Create a new preview.</p>");
    }
    for row in list(&plan["changes"]) {
        let name = text(row, "name");
        out.push_str(&format!("<label class=\"wasm-wide\"><input type=\"checkbox\" data-plan-name=\"{}\" aria-label=\"Apply {}\" {} {}> {} · {}</label>",escape(name),escape(name),if app.advanced.plan_names.iter().any(|s|s==name){"checked"}else{""},if row["change"]=="unchanged"||app.advanced.plan_expired{"disabled"}else{""},escape(name),escape(text(row,"change"))));
    }
    out.push_str(
        &button(
            &format!(
                "{} selected ({})",
                if import { "Import" } else { "Upload" },
                app.advanced.plan_names.len()
            ),
            "secret-apply",
            text(plan, "id"),
        )
        .replace(
            "type=\"button\"",
            if app.advanced.plan_expired || app.advanced.plan_names.is_empty() || app.busy {
                "type=\"button\" disabled"
            } else {
                "type=\"button\""
            },
        ),
    );
    out.push_str(&button("Close preview", "secret-plan-close", ""));
    out.push_str("</section>");
    out
}
pub fn render_plan(app: &Shared) {
    set_html("secret-plan", &plan(&app.borrow()));
}
pub fn tick(app: &Shared) {
    let now = js_sys::Date::now();
    let mut s = app.borrow_mut();
    if s.route != "secrets" {
        return;
    }
    let count = s.advanced.reveals.len();
    s.advanced.reveals.retain(|_, (_, until)| *until > now);
    let changed = count != s.advanced.reveals.len();
    let expired = s
        .advanced
        .plan
        .as_ref()
        .is_some_and(|p| p["expiresAt"].as_f64().unwrap_or(0.0) <= now)
        && !s.advanced.plan_expired;
    if expired {
        s.advanced.plan_expired = true;
    }
    let editing = s.form.is_some();
    drop(s);
    if changed && !editing {
        render_content(app);
    } else if expired {
        render_plan(app);
    }
}
pub fn dispatch(app: &Shared, action: &str, id: &str) -> bool {
    match action {
        "secret-internal" | "secret-external" => {
            let mut s = app.borrow_mut();
            s.advanced.reveals.clear();
            s.advanced.secrets_external = action == "secret-external";
            drop(s);
            render_content(app);
            return true;
        }
        "secret-edit" => {
            crate::show_detail(app.clone(), id.into());
            crate::open_form(app.clone(), "secrets", id);
            return true;
        }
        "secret-edit-connection" => {
            crate::open_form(app.clone(), "secret-connection", id);
            return true;
        }
        "secret-plan-close" => {
            app.borrow_mut().advanced.plan = None;
            render_plan(app);
            return true;
        }
        _ => {}
    }
    if ![
        "secret-reveal",
        "secret-import",
        "secret-account-import",
        "secret-export",
        "secret-apply",
    ]
    .contains(&action)
    {
        return false;
    }
    if action == "secret-reveal" && app.borrow_mut().advanced.reveals.remove(id).is_some() {
        render_content(app);
        return true;
    }
    let (path, body) = match action {
        "secret-account-import" => ("/api/secrets/account/preview".into(), json!({})),
        "secret-reveal" => (
            format!("/api/secrets/{}/reveal", api::encode(id)),
            json!({}),
        ),
        "secret-import" => (
            format!("/api/secrets/connections/{}/preview", api::encode(id)),
            json!({"direction":"import","secretIds":[]}),
        ),
        "secret-export" => {
            let connection = element("upload-destination")
                .and_then(|el| el.dyn_into::<web_sys::HtmlSelectElement>().ok())
                .map(|el| el.value())
                .unwrap_or_default();
            let mut ids = Vec::new();
            if let Ok(nodes) = crate::document().query_selector_all("input[data-secret-id]:checked")
            {
                for i in 0..nodes.length() {
                    if let Some(el) = nodes
                        .item(i)
                        .and_then(|el| el.dyn_into::<web_sys::Element>().ok())
                    {
                        ids.push(el.get_attribute("data-secret-id").unwrap_or_default());
                    }
                }
            }
            if connection.is_empty() || ids.is_empty() {
                app.borrow_mut().error = "Select secrets and an upload destination.".into();
                render_messages(app);
                return true;
            }
            (
                format!(
                    "/api/secrets/connections/{}/preview",
                    api::encode(&connection)
                ),
                json!({"direction":"export","secretIds":ids}),
            )
        }
        _ => {
            if app.borrow().advanced.plan_expired {
                return true;
            }
            (
                format!("/api/secrets/plans/{}/apply", api::encode(id)),
                json!({"names":app.borrow().advanced.plan_names}),
            )
        }
    };
    let a = app.clone();
    let action = action.to_string();
    let id = id.to_string();
    let generation = app.borrow().generation;
    let reveal_generation = app.borrow().advanced.reveal_generation;
    let version = list(&app.borrow().resource)
        .iter()
        .find(|r| r["id"] == id)
        .map(|r| r["version"].clone());
    let export = app
        .borrow()
        .advanced
        .plan
        .as_ref()
        .is_some_and(|p| p["direction"] == "export");
    {
        let mut state = app.borrow_mut();
        state.busy = true;
        if ["secret-import", "secret-account-import", "secret-export"].contains(&action.as_str()) {
            state.advanced.plan = None;
            state.advanced.plan_names.clear();
        }
    }
    render_plan(app);
    spawn_local(async move {
        let result = api::request("POST", &path, Some(&body)).await;
        let succeeded = result.is_ok();
        let mut s = a.borrow_mut();
        s.busy = false;
        if s.generation != generation
            || s.route != "secrets"
            || (action == "secret-reveal" && s.advanced.reveal_generation != reveal_generation)
        {
            return;
        }
        match result {
            Ok(v) => {
                s.error.clear();
                if action == "secret-reveal" {
                    if version.as_ref() == Some(&v["version"])
                        && list(&s.resource)
                            .iter()
                            .any(|r| r["id"] == id && Some(&r["version"]) == version.as_ref())
                    {
                        s.advanced.reveals.insert(
                            id,
                            (text(&v, "value").into(), js_sys::Date::now() + 30000.0),
                        );
                    }
                } else if action == "secret-apply" {
                    s.notice = format!("{} secret(s) applied.", list(&v["applied"]).len());
                    if !list(&v["failed"]).is_empty() {
                        s.error = format!(
                            "{} failed: {}",
                            if export { "Upload" } else { "Import" },
                            list(&v["failed"])
                                .iter()
                                .map(|r| text(r, "name"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                    }
                    s.advanced.plan = None;
                } else {
                    s.advanced.plan_names = list(&v["changes"])
                        .iter()
                        .filter(|r| r["change"] == "add")
                        .map(|r| text(r, "name").to_string())
                        .collect();
                    s.advanced.plan_expired = false;
                    s.advanced.plan = Some(v);
                }
            }
            Err(e) => s.error = e,
        };
        drop(s);
        if succeeded && ["secret-apply", "secret-reveal"].contains(&action.as_str()) {
            render_content(&a);
        } else {
            render_plan(&a);
        }
        if succeeded && action == "secret-apply" {
            crate::refresh(a.clone());
        }
        render_messages(&a);
    });
    true
}
