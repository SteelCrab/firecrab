use crate::{
    App, Shared, api,
    model::{escape, list, text},
    render::{button, properties},
    render_content, render_messages,
};
use serde_json::{Value, json};
use wasm_bindgen_futures::spawn_local;
pub fn load(app: &Shared) {
    let a = app.clone();
    let generation = app.borrow().generation;
    spawn_local(async move {
        let result = api::request("GET", "/api/pipe/registry-images", None).await;
        let mut s = a.borrow_mut();
        if s.generation != generation || s.route != "images" {
            return;
        }
        let old = s.cache.get("pulled-images").cloned();
        match result {
            Ok(v) => {
                s.cache.insert("pulled-images".into(), v);
                s.cache.remove("pulled-images-error");
            }
            Err(e) => {
                s.cache.insert("pulled-images-error".into(), json!(e));
            }
        }
        let changed = old != s.cache.get("pulled-images").cloned()
            || s.cache.contains_key("pulled-images-error");
        let editing = s.form.is_some();
        drop(s);
        if changed && !editing {
            render_content(&a);
        }
        render_messages(&a);
    });
}
pub fn sources(app: &App) -> String {
    let mut out = String::from(
        "<section aria-label=\"Pulled registry images\"><h2>Pulled registry images</h2>",
    );
    if let Some(error) = app.cache.get("pulled-images-error") {
        out.push_str(&format!(
            "<p role=\"alert\">{}</p>",
            escape(error.as_str().unwrap_or_default())
        ));
    }
    for image in list(app.cache.get("pulled-images").unwrap_or(&Value::Null))
        .iter()
        .filter(|i| {
            !list(&app.resource)
                .iter()
                .any(|built| built["alias"] == i["templateAlias"] && built["installed"] == true)
        })
    {
        let alias = text(image, "templateAlias");
        let reference = text(image, "reference");
        let job = app
            .job
            .as_ref()
            .filter(|(_, v)| v["alias"] == alias)
            .map(|(_, v)| v);
        let running = job.is_some_and(|v| v["status"] == "running");
        out.push_str(&format!("<article class=\"wasm-card pulled-image-row\" data-alias=\"{}\"><h3>{}</h3>{}<p>{}</p>{}</article>",escape(alias),escape(reference),properties(&json!({"digest":image["digest"],"architecture":image["architecture"]})),if running{"Building…"}else if job.is_some_and(|v|v["status"]=="failed"){"Build failed"}else{"Build required"},button("Build M2Image","image-build",alias).replace("type=\"button\"",if running{"type=\"button\" disabled"}else{"type=\"button\""})));
    }
    out.push_str("</section>");
    out
}
pub fn dispatch(app: &Shared, action: &str, id: &str) -> bool {
    if action != "image-build" {
        return false;
    }
    if !crate::allow_discard(app) {
        return true;
    }
    let image = list(
        app.borrow()
            .cache
            .get("pulled-images")
            .unwrap_or(&Value::Null),
    )
    .iter()
    .find(|i| i["templateAlias"] == id)
    .cloned();
    if let Some(image) = image {
        let reference = text(&image, "reference");
        let repository = reference.split('@').next().unwrap_or(reference);
        let repository = match repository.rsplit_once(':') {
            Some((repo, tag)) if !tag.contains('/') => repo,
            _ => repository,
        };
        let reference = format!("{repository}@{}", text(&image, "digest"));
        crate::mutate(
            app.clone(),
            "POST".into(),
            "/api/oci/import".into(),
            Some(json!({"reference":reference})),
            None,
        );
    }
    true
}
