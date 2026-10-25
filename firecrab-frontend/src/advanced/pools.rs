use crate::{
    App, Shared, api,
    model::{escape, list, text},
    render::{button, request},
    render_content, render_messages, window,
};
use serde_json::{Value, json};
use wasm_bindgen_futures::spawn_local;
pub fn content(app: &App) -> String {
    let mut out = String::from("<section class=\"pools-screen\" aria-label=\"Pools\">");
    for pool in list(&app.resource) {
        let id = text(pool, "id");
        let ready = list(&pool["members"])
            .iter()
            .filter(|m| m["state"] == "ready")
            .count();
        out.push_str(&format!("<article class=\"wasm-card\"><h2>{}</h2><p>{ready} ready · {} members · {}</p>{}</article>",button(text(pool,"name"),"detail",id),list(&pool["members"]).len(),escape(text(pool,"template")),button("Use VM","pool-acquire",id)));
    }
    if list(&app.resource).is_empty() {
        out.push_str("<p class=\"empty-state\">No pools yet.</p>");
    }
    out.push_str("</section>");
    out
}
pub fn actions(data: &Value) -> String {
    button("Pool settings", "edit", "pools")
        + &if list(&data["leases"]).iter().any(|l| l["state"] == "active") {
            String::new()
        } else {
            request(
                "Delete pool",
                "DELETE",
                &format!("/api/pools/{}", api::encode(text(data, "id"))),
                Value::Null,
            )
        }
}
pub fn detail(_app: &App, data: &Value) -> String {
    let mut out = crate::render::properties(
        &json!({"template":data["template"],"minReady":data["minReady"],"maxSize":data["maxSize"],"leaseTtlSeconds":data["leaseTtlSeconds"]}),
    );
    let active: Vec<_> = list(&data["leases"])
        .iter()
        .filter(|l| l["state"] == "active")
        .collect();
    if active.is_empty() {
        out.push_str("<p>Nothing in use. Choose Use VM to get started.</p>");
    }
    for lease in active {
        out.push_str(&format!("<section class=\"wasm-card\"><h3>{}</h3><p>Expires: {}</p><a class=\"btn\" href=\"#/console/{}\" target=\"_blank\" rel=\"noopener\">Open terminal</a>{}</section>",escape(text(&lease["vm"],"name")),lease["expiresAtMs"],api::encode(text(lease,"vmId")),button("End session","pool-release",&format!("{}/{}",text(data,"id"),text(lease,"id")))));
    }
    out
}
fn key(pool: &str) -> Result<String, String> {
    let storage = window().session_storage().map_err(api::error)?.ok_or(
        "Session storage is unavailable. Retry in a browser with session storage enabled.",
    )?;
    let name = format!("firecrab.pool.acquire.{pool}");
    if let Some(key) = storage.get_item(&name).map_err(api::error)? {
        return Ok(key);
    }
    let mut bytes = [0u8; 16];
    window()
        .crypto()
        .map_err(api::error)?
        .get_random_values_with_u8_array(&mut bytes)
        .map_err(api::error)?;
    let key = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    storage.set_item(&name, &key).map_err(api::error)?;
    Ok(key)
}
pub fn dispatch(app: &Shared, action: &str, id: &str) -> bool {
    if !["pool-acquire", "pool-release"].contains(&action) {
        return false;
    }
    if app.borrow().busy {
        return true;
    }
    app.borrow_mut().busy = true;
    let a = app.clone();
    let id = id.to_string();
    let acquire = action == "pool-acquire";
    let generation = app.borrow().generation;
    spawn_local(async move {
        let result = if acquire {
            match key(&id) {
                Ok(key) => {
                    api::acquire(&format!("/api/pools/{}/acquire", api::encode(&id)), &key).await
                }
                Err(e) => Err(e),
            }
        } else {
            let (pool, lease) = id.split_once('/').unwrap_or_default();
            api::request(
                "DELETE",
                &format!(
                    "/api/pools/{}/leases/{}",
                    api::encode(pool),
                    api::encode(lease)
                ),
                None,
            )
            .await
        };
        if acquire
            && result.is_ok()
            && let Ok(Some(storage)) = window().session_storage()
        {
            let _ = storage.remove_item(&format!("firecrab.pool.acquire.{id}"));
        }
        let mut s = a.borrow_mut();
        s.busy = false;
        if s.generation != generation {
            return;
        }
        match result {
            Ok(lease) => {
                s.error.clear();
                s.notice = "Session updated.".into();
                if acquire && s.detail.is_none() {
                    s.panel_open = true;
                    s.detail = list(&s.resource).iter().find(|p| p["id"] == id).cloned();
                }
                if let Some(d) = s.detail.as_mut() {
                    let leases = d["leases"].as_array_mut();
                    if let Some(leases) = leases {
                        leases.retain(|l| l["id"] != lease["id"]);
                        leases.push(lease);
                    } else {
                        d["leases"] = json!([lease]);
                    }
                }
            }
            Err(e) => {
                s.error = if acquire {
                    format!("Unable to connect. Retry Use VM to recover the same session. {e}")
                } else {
                    e
                }
            }
        }
        drop(s);
        render_content(&a);
        super::detail_selected(&a);
        render_messages(&a);
    });
    true
}
