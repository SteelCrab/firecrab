use crate::{
    App, Shared, api, element,
    model::{escape, list, text},
    render::{Icon, empty_state, icon_button, icon_request, name_cell},
    render_content, render_messages, set_html,
};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;

fn text_button(label: &str, caption: &str, action: &str, id: &str) -> String {
    let label = escape(crate::i18n::t(label));
    format!(
        "<button type=\"button\" class=\"btn\" aria-label=\"{label}\" title=\"{label}\" data-action=\"{}\" data-id=\"{}\">{}</button>",
        escape(action),
        escape(id),
        escape(crate::i18n::t(caption))
    )
}

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
    use crate::i18n::t;
    let external = app.advanced.secrets_external;
    let connection_count = list(
        app.cache
            .get("/api/secrets/connections")
            .unwrap_or(&Value::Null),
    )
    .len();
    let mut out = format!(
        "<div class=\"secrets-page\"><div class=\"subtabs secret-tabs\" role=\"tablist\" aria-label=\"Secret stores\"><button class=\"subtab\" type=\"button\" role=\"tab\" aria-label=\"{}\" aria-selected=\"{}\" data-action=\"secret-internal\">{} <em>{}</em></button><button class=\"subtab\" type=\"button\" role=\"tab\" aria-label=\"{}\" aria-selected=\"{external}\" data-action=\"secret-external\">{} <em>{connection_count}</em></button></div>",
        t("Internal secrets"),
        !external,
        t("Secrets"),
        list(&app.resource).len(),
        t("External connections"),
        t("Connections")
    );
    out.push_str(&account_card(app.cache.get("/api/pipe/auth"), app.busy));
    if external {
        out.push_str(&text_button(
            "Add connection",
            "Connection",
            "create",
            "secret-connection",
        ));
        let links = list(
            app.cache
                .get("/api/secrets/connections")
                .unwrap_or(&Value::Null),
        );
        if links.is_empty() {
            out.push_str(&empty_state(
                "secrets",
                t("No connections yet"),
                t("Connect a secret store to import its secrets."),
                "",
            ));
        } else {
            out.push_str(&format!(
                "<div class=\"table-scroll\"><table class=\"wasm-table secret-table\"><thead><tr><th>{}</th><th class=\"col-secondary\">{}</th><th class=\"row-actions\"><span class=\"sr-only\">{}</span></th></tr></thead><tbody>",
                t("Name"),
                t("Provider"),
                t("Actions")
            ));
            for link in links {
                let id = text(link, "id");
                let name = text(link, "name");
                out.push_str(&format!(
                    "<tr><td>{}</td><td class=\"col-secondary\">{}</td><td class=\"row-actions\">{}{}{}</td></tr>",
                    name_cell(name, "", "", false),
                    escape(text(link, "provider")),
                    icon_button(
                        &format!("{} {name}", t("Import from")),
                        "secret-import",
                        id,
                        Icon::Download
                    ),
                    icon_button(
                        &format!("{} {name}", t("Edit connection")),
                        "secret-edit-connection",
                        id,
                        Icon::Edit
                    ),
                    icon_request(
                        &format!("{} {name}", t("Delete connection")),
                        &format!("/api/secrets/connections/{}", api::encode(id))
                    )
                ));
            }
            out.push_str("</tbody></table></div>");
        }
    } else {
        out.push_str("<section class=\"secret-list\" aria-label=\"Internal secrets\"><p>Value changes automatically redeploy running services with a domain that use the secret. Follow progress under Deploy.</p>");
        if list(&app.resource).is_empty() {
            out.push_str(&empty_state(
                "secrets",
                t("No secrets yet"),
                t("Store values your services need, such as passwords and tokens."),
                "",
            ));
        } else {
            out.push_str(&format!(
                "<div class=\"table-scroll\"><table class=\"wasm-table secret-table\"><thead><tr><th class=\"secret-check\"><span class=\"sr-only\">{}</span></th><th>{}</th><th class=\"col-secondary\">{}</th><th class=\"row-actions\"><span class=\"sr-only\">{}</span></th></tr></thead><tbody>",
                t("Select"),
                t("Name"),
                t("Value"),
                t("Actions")
            ));
            for row in list(&app.resource) {
                let id = text(row, "id");
                let name = text(row, "name");
                let reveal = app.advanced.reveals.get(id);
                let sub = match row.get("source").filter(|v| v.is_object()) {
                    Some(source) => format!(
                        "{}: {} · v{}",
                        t("Imported from"),
                        text(source, "connectionName"),
                        row["version"].as_u64().unwrap_or(1)
                    ),
                    None => text(row, "description").to_string(),
                };
                out.push_str(&format!(
                    "<tr data-id=\"{}\"><td class=\"secret-check\"><input type=\"checkbox\" data-secret-id=\"{}\" aria-label=\"Select {}\"></td><td>{}</td><td class=\"col-secondary\"><code aria-label=\"{} value\">{}</code></td><td class=\"row-actions\">{}{}{}</td></tr>",
                    escape(id),
                    escape(id),
                    escape(name),
                    name_cell(name, &sub, "", true),
                    escape(name),
                    escape(reveal.map(|(value, _)| value.as_str()).unwrap_or("••••••••")),
                    icon_button(&format!("{} {name}", t(if reveal.is_some() { "Hide" } else { "Reveal" })), "secret-reveal", id, if reveal.is_some() { Icon::Hide } else { Icon::Reveal }),
                    icon_button(&format!("{} {name}", t("Edit")), "secret-edit", id, Icon::Edit),
                    icon_request(&format!("{} {name}", t("Delete")), &format!("/api/secrets/{}", api::encode(id)))
                ));
            }
            out.push_str("</tbody></table></div>");
        }
        out.push_str("<div class=\"secret-export\"><label for=\"upload-destination\">Upload destination</label><select id=\"upload-destination\">");
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
                    _ => "AWS Secrets Manager",
                }
            ));
        }
        out.push_str("</select>");
        out.push_str(&text_button(
            "Preview upload",
            "Preview",
            "secret-export",
            "",
        ));
        out.push_str("</div></section>");
    }
    out.push_str(&format!(
        "<div id=\"secret-plan\">{}</div></div>",
        plan(app)
    ));
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
                text_button(
                    "Preview account secrets",
                    "Preview",
                    "secret-account-import",
                    ""
                )
                .replace(
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
            "<p>{}</p><a href=\"#/pipe/auth\" aria-label=\"{}\">Auth</a>",
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
        &text_button(
            &format!(
                "{} selected ({})",
                if import { "Import" } else { "Upload" },
                app.advanced.plan_names.len()
            ),
            &format!(
                "{} ({})",
                crate::i18n::t(if import { "Import" } else { "Upload" }),
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
    out.push_str(&icon_button(
        "Close preview",
        "secret-plan-close",
        "",
        Icon::Close,
    ));
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
