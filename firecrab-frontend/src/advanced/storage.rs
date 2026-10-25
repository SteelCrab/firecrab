use crate::{
    App, Shared, api, element,
    model::{escape, list, text},
    render::{button, request},
    render_content, render_messages, window,
};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{Element, File, HtmlInputElement};
#[derive(Default)]
pub struct State {
    pub connection: Option<Value>,
    pub page: Value,
    pub file: Option<File>,
    pub name: String,
    pub overwrite: bool,
}
pub fn content(app: &App) -> String {
    let s = &app.advanced.storage;
    let mut out =
        String::from("<section role=\"region\" aria-label=\"PipeStorage\"><h2>S3 Storage</h2>");
    out.push_str(&format!(
        "<fieldset style=\"border:0;padding:0;min-width:0\" {}>",
        if app.busy { "disabled" } else { "" }
    ));
    if let Some(connection) = &s.connection {
        let id = text(connection, "id");
        let prefix = text(&s.page, "prefix");
        out.push_str(&button("All connections", "storage-back", ""));
        out.push_str(&format!("<section role=\"region\" aria-label=\"S3 object browser\"><h3>{}</h3><p>s3://{}/{}</p><label>Directory prefix<input id=\"storage-prefix\" value=\"{}\"></label>{}{}",escape(text(connection,"name")),escape(text(connection,"bucket")),escape(text(connection,"prefix")),escape(prefix),button("Open prefix","storage-prefix",id),button("Root folder","storage-root",id)));
        for directory in list(&s.page["prefixes"]).iter().filter_map(Value::as_str) {
            out.push_str(&button(directory, "storage-folder", directory));
        }
        out.push_str("<div class=\"table-scroll\"><table class=\"wasm-table\"><thead><tr><th>Name</th><th>Size</th><th>Modified</th><th>Actions</th></tr></thead><tbody>");
        for object in list(&s.page["objects"]) {
            let key = text(object, "key");
            out.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}{}</td></tr>",
                escape(key.strip_prefix(prefix).unwrap_or(key)),
                object["size"],
                escape(text(object, "lastModified")),
                button("Download", "storage-download", key),
                button("Delete file", "storage-delete", key)
            ));
        }
        out.push_str("</tbody></table></div>");
        if list(&s.page["objects"]).is_empty() && list(&s.page["prefixes"]).is_empty() {
            out.push_str("<p>This folder is empty</p>");
        }
        if !text(&s.page, "nextCursor").is_empty() {
            out.push_str(&button("Load more", "storage-more", id));
        }
        out.push_str(&format!("<div class=\"wasm-form wasm-card\"><label>File<input id=\"storage-file\" type=\"file\"></label><label>File name<input id=\"storage-name\" value=\"{}\"></label><label><input id=\"storage-overwrite\" type=\"checkbox\" {}>Allow overwrite</label>{}{}</div></section>",escape(&s.name),if s.overwrite{"checked"}else{""},button("Upload","storage-upload",id),button("Cancel upload","storage-clear","")));
    } else {
        out.push_str(&button("Connect bucket", "create", "pipe-storage"));
        for connection in list(&app.resource) {
            let name = text(connection, "name");
            let id = text(connection, "id");
            let bound = !list(&connection["services"]).is_empty();
            let disabled = |html: String| {
                if bound {
                    html.replace("type=\"button\"", "type=\"button\" disabled")
                } else {
                    html
                }
            };
            out.push_str(&format!(
                "<article class=\"wasm-card\"><h3>{}</h3><p>{} · {}</p>{}{}{}{}",
                button(&format!("Open files in {name}"), "storage-browse", id),
                escape(text(connection, "bucket")),
                escape(text(connection, "prefix")),
                button("Check connection", "storage-verify", id),
                disabled(button("Edit connection", "storage-edit", id)),
                disabled(request(
                    "Disconnect",
                    "DELETE",
                    &format!("/api/pipe/storage/{}", api::encode(id)),
                    Value::Null
                )),
                if bound {
                    format!(
                        "<p>Used by {}. Unbind these services before editing or disconnecting.</p>",
                        list(&connection["services"])
                            .iter()
                            .map(|s| escape(text(s, "name")))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                } else {
                    String::new()
                }
            ));
            out.push_str("</article>");
        }
    }
    out.push_str("</fieldset></section>");
    out
}
pub fn change(app: &Shared, target: &Element) -> bool {
    match target.id().as_str() {
        "storage-file" => {
            let input = target.clone().unchecked_into::<HtmlInputElement>();
            let file = input.files().and_then(|files| files.get(0));
            if let Some(file) = file {
                if file.size() > 512.0 * 1024.0 * 1024.0 {
                    input.set_value("");
                    app.borrow_mut().error = "Files must be at most 512 MiB.".into();
                    render_messages(app);
                    return true;
                }
                let mut s = app.borrow_mut();
                s.advanced.storage.name = file.name();
                s.advanced.storage.file = Some(file);
                s.advanced.storage.overwrite = false;
                s.dirty = true;
                let name = s.advanced.storage.name.clone();
                drop(s);
                if let Some(el) =
                    element("storage-name").and_then(|el| el.dyn_into::<HtmlInputElement>().ok())
                {
                    el.set_value(&name);
                }
            }
            true
        }
        "storage-name" => {
            app.borrow_mut().advanced.storage.name =
                target.clone().unchecked_into::<HtmlInputElement>().value();
            true
        }
        "storage-overwrite" => {
            app.borrow_mut().advanced.storage.overwrite = target
                .clone()
                .unchecked_into::<HtmlInputElement>()
                .checked();
            true
        }
        _ => false,
    }
}
fn browse(app: &Shared, connection: Value, prefix: String, cursor: Option<String>) {
    if app.borrow().busy {
        return;
    }
    app.borrow_mut().busy = true;
    render_content(app);
    let a = app.clone();
    let generation = app.borrow().generation;
    let id = api::encode(text(&connection, "id"));
    spawn_local(async move {
        let mut path = format!(
            "/api/pipe/storage/{id}/objects?prefix={}&limit=100",
            api::encode(&prefix)
        );
        if let Some(cursor) = &cursor {
            path.push_str(&format!("&cursor={}", api::encode(cursor)));
        }
        let result = api::request("GET", &path, None).await;
        let mut s = a.borrow_mut();
        s.busy = false;
        if s.generation != generation {
            return;
        }
        match result {
            Ok(mut value) => {
                s.error.clear();
                if cursor.is_some() && s.advanced.storage.page["prefix"] == value["prefix"] {
                    for key in ["objects", "prefixes"] {
                        let mut old = list(&s.advanced.storage.page[key]).to_vec();
                        for row in list(&value[key]) {
                            if !old.contains(row) {
                                old.push(row.clone());
                            }
                        }
                        value[key] = json!(old);
                    }
                }
                s.advanced.storage.connection = Some(connection);
                s.advanced.storage.page = value;
            }
            Err(e) => s.error = e,
        }
        drop(s);
        render_content(&a);
    });
}
pub fn dispatch(app: &Shared, action: &str, id: &str) -> bool {
    if !action.starts_with("storage-") {
        return false;
    }
    let connection = app.borrow().advanced.storage.connection.clone();
    match action {
        "storage-back" => {
            if !crate::allow_discard(app) {
                return true;
            }
            app.borrow_mut().advanced.storage = State::default();
            app.borrow_mut().dirty = false;
            render_content(app);
        }
        "storage-clear" => {
            app.borrow_mut().advanced.storage.file = None;
            app.borrow_mut().advanced.storage.name.clear();
            app.borrow_mut().dirty = false;
            render_content(app);
        }
        "storage-edit" => {
            let row = list(&app.borrow().resource)
                .iter()
                .find(|r| r["id"] == id)
                .cloned();
            if let Some(row) = row
                && list(&row["services"]).is_empty()
            {
                app.borrow_mut().detail = Some(row);
                crate::open_form(app.clone(), "pipe-storage", id);
            }
        }
        "storage-browse" => {
            let row = list(&app.borrow().resource)
                .iter()
                .find(|r| r["id"] == id)
                .cloned();
            if let Some(row) = row {
                let prefix = text(&row, "prefix").into();
                browse(app, row, prefix, None);
            }
        }
        "storage-prefix" | "storage-folder" | "storage-root" | "storage-more" => {
            if let Some(connection) = connection {
                if !crate::allow_discard(app) {
                    return true;
                }
                let prefix = match action {
                    "storage-root" => text(&connection, "prefix").into(),
                    "storage-folder" => id.into(),
                    "storage-more" => text(&app.borrow().advanced.storage.page, "prefix").into(),
                    _ => {
                        let value = element("storage-prefix")
                            .unwrap()
                            .unchecked_into::<HtmlInputElement>()
                            .value();
                        if value.is_empty() || value.ends_with('/') {
                            value
                        } else {
                            format!("{value}/")
                        }
                    }
                };
                let cursor = if action == "storage-more" {
                    Some(text(&app.borrow().advanced.storage.page, "nextCursor").into())
                } else {
                    None
                };
                browse(app, connection, prefix, cursor);
            }
        }
        "storage-verify" => {
            let a = app.clone();
            let generation = app.borrow().generation;
            let path = format!("/api/pipe/storage/{}/verify", api::encode(id));
            spawn_local(async move {
                let result = api::request("POST", &path, None).await;
                let mut s = a.borrow_mut();
                if s.generation != generation {
                    return;
                }
                match result {
                    Ok(_) => {
                        s.error.clear();
                        s.notice =
                            "Listing is allowed. File permissions are checked when you use them."
                                .into();
                    }
                    Err(e) => s.error = e,
                }
                drop(s);
                render_messages(&a);
            });
        }
        "storage-upload" | "storage-download" | "storage-delete" => {
            if let Some(connection) = connection {
                object(app, connection, action, id);
            }
        }
        _ => {}
    }
    true
}
fn object(app: &Shared, connection: Value, action: &str, key: &str) {
    if app.borrow().busy {
        return;
    }
    let (file, stored_name, prefix) = {
        let state = app.borrow();
        let s = &state.advanced.storage;
        (
            s.file.clone(),
            s.name.clone(),
            text(&s.page, "prefix").to_string(),
        )
    };
    let name = element("storage-name")
        .map(|el| el.unchecked_into::<HtmlInputElement>().value())
        .unwrap_or(stored_name);
    let overwrite = element("storage-overwrite")
        .is_some_and(|el| el.unchecked_into::<HtmlInputElement>().checked());
    let key = if action == "storage-upload" {
        if file.is_none() || name.is_empty() {
            return;
        }
        format!("{prefix}{name}")
    } else {
        key.into()
    };
    if (action == "storage-delete" || (action == "storage-upload" && overwrite))
        && !window()
            .confirm_with_message(&format!(
                "{} {key}?",
                if action == "storage-delete" {
                    "Delete"
                } else {
                    "Overwrite"
                }
            ))
            .unwrap_or(false)
    {
        return;
    }
    let path = format!(
        "/api/pipe/storage/{}/object?key={}&overwrite={overwrite}",
        api::encode(text(&connection, "id")),
        api::encode(&key)
    );
    let action = action.to_string();
    let a = app.clone();
    let generation = app.borrow().generation;
    app.borrow_mut().busy = true;
    app.borrow_mut().advanced.storage.name = name;
    render_content(app);
    spawn_local(async move {
        let method = match action.as_str() {
            "storage-upload" => "PUT",
            "storage-download" => "GET",
            _ => "DELETE",
        };
        let result = api::binary(
            method,
            &path,
            if method == "PUT" {
                file.as_ref().map(|f| f.unchecked_ref())
            } else {
                None
            },
        )
        .await;
        let mut s = a.borrow_mut();
        s.busy = false;
        if s.generation != generation {
            return;
        }
        match result {
            Ok(bytes) => {
                s.error.clear();
                s.notice = if method == "PUT" {
                    "Upload complete."
                } else if method == "DELETE" {
                    "File deleted."
                } else {
                    "Downloaded."
                }
                .into();
                if method == "PUT" {
                    s.advanced.storage.file = None;
                    s.advanced.storage.name.clear();
                    s.dirty = false;
                }
                drop(s);
                if method == "GET" {
                    if let Err(e) = download(&key, &bytes) {
                        a.borrow_mut().error = e;
                    }
                } else {
                    browse(&a, connection, prefix, None);
                }
                render_content(&a);
                render_messages(&a);
            }
            Err(e) => {
                s.error = e;
                drop(s);
                render_content(&a);
                render_messages(&a);
            }
        }
    });
}
fn download(key: &str, blob: &web_sys::Blob) -> Result<(), String> {
    let url = web_sys::Url::create_object_url_with_blob(blob).map_err(api::error)?;
    let link = crate::document()
        .create_element("a")
        .map_err(api::error)?
        .unchecked_into::<web_sys::HtmlAnchorElement>();
    link.set_href(&url);
    link.set_download(key.rsplit('/').next().unwrap_or("download"));
    crate::document()
        .body()
        .ok_or("Document body is unavailable.")?
        .append_child(&link)
        .map_err(api::error)?;
    link.click();
    link.remove();
    let cleanup = wasm_bindgen::closure::Closure::once_into_js(move || {
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    window()
        .set_timeout_with_callback_and_timeout_and_arguments_0(cleanup.unchecked_ref(), 1000)
        .map_err(api::error)?;
    Ok(())
}
