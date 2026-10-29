use crate::{
    App, Shared, api,
    model::{escape, list, text},
    render::{Icon, button, empty_state, icon_button, icon_link, name_cell, request, status_dot},
    render_content, render_messages, window,
};
use serde_json::{Value, json};
use wasm_bindgen_futures::spawn_local;
pub fn content(app: &App) -> String {
    let rows = list(&app.resource);
    if rows.is_empty() {
        return empty_state(
            "pools",
            tr(app, "No pools yet", "아직 풀이 없습니다"),
            tr(
                app,
                "A pool keeps MicroVMs warm so a VM is ready in seconds.",
                "풀은 MicroVM을 미리 켜 두어 몇 초 안에 쓸 수 있게 합니다.",
            ),
            &button("New pool", "create", "pools"),
        );
    }
    let mut out = format!(
        "<div class=\"table-scroll\"><table class=\"wasm-table pool-table\" aria-label=\"{}\"><thead><tr><th>{}</th><th>{}</th><th class=\"col-secondary\">{}</th><th class=\"col-secondary\">{}</th><th class=\"row-actions\"><span class=\"sr-only\">{}</span></th></tr></thead><tbody>",
        tr(app, "Pools", "풀"),
        tr(app, "Pool", "풀"),
        tr(app, "Ready", "준비됨"),
        tr(app, "In use", "사용 중"),
        tr(app, "Lease time", "임대 시간"),
        crate::i18n::t("Actions")
    );
    for pool in rows {
        let id = text(pool, "id");
        let members = list(&pool["members"]);
        let ready = members.iter().filter(|m| m["state"] == "ready").count();
        let in_use = match pool["leases"].as_array() {
            Some(leases) => leases
                .iter()
                .filter(|l| l["state"] == "active")
                .count()
                .to_string(),
            None => "—".into(),
        };
        let selected = app.detail.as_ref().is_some_and(|d| d["id"] == id);
        let sub = format!(
            "{} · {}",
            text(pool, "template"),
            crate::forms::size_label(&pool["cpu"].to_string(), &pool["ramMib"].to_string())
        );
        let sub = if pool["cpu"].is_null() || pool["ramMib"].is_null() {
            text(pool, "template").to_string()
        } else {
            sub
        };
        let lease = pool["leaseTtlSeconds"]
            .as_u64()
            .map_or("—".to_string(), |s| {
                format!("{} {}", s / 60, tr(app, "min", "분"))
            });
        out.push_str(&format!(
            "<tr data-id=\"{}\" aria-selected=\"{selected}\"><td>{}</td><td>{ready} / {}</td><td class=\"col-secondary\">{in_use}</td><td class=\"col-secondary\">{lease}</td><td class=\"row-actions\">{}</td></tr>",
            escape(id),
            name_cell(text(pool, "name"), &sub, "", false),
            members.len(),
            icon_button("Use VM", "pool-acquire", id, Icon::Start)
        ));
    }
    out.push_str("</tbody></table></div>");
    out
}
fn tr<'a>(app: &App, en: &'a str, ko: &'a str) -> &'a str {
    if app.locale == "ko" { ko } else { en }
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
    let settings = crate::details::section(
        "Settings",
        "",
        &crate::render::properties(
            &json!({"template":data["template"],"minReady":data["minReady"],"maxSize":data["maxSize"],"leaseTtlSeconds":data["leaseTtlSeconds"]}),
        ),
    );
    let active: Vec<_> = list(&data["leases"])
        .iter()
        .filter(|l| l["state"] == "active")
        .collect();
    let mut sessions = String::new();
    if active.is_empty() {
        sessions
            .push_str("<p class=\"detail-note\">Nothing in use. Choose Use VM to get started.</p>");
    }
    for lease in active {
        sessions.push_str(&format!(
            "<li><span class=\"detail-list-main\">{}</span><span class=\"detail-list-sub\">Expires: {}</span>{}{}</li>",
            escape(text(&lease["vm"], "name")),
            lease["expiresAtMs"],
            icon_link(
                "Open terminal",
                &format!("#/console/{}", api::encode(text(lease, "vmId"))),
                true,
                Icon::Terminal
            ),
            icon_button(
                "End session",
                "pool-release",
                &format!("{}/{}", text(data, "id"), text(lease, "id")),
                Icon::Close
            )
        ));
    }
    let members = list(&data["members"]);
    let mut member_rows = String::new();
    for member in members {
        let ready = member["state"] == "ready";
        member_rows.push_str(&format!(
            "<li><span class=\"detail-list-main\">{}</span>{}</li>",
            escape(text(member, "name")),
            status_dot(if ready { "ok" } else { "busy" }, text(member, "state"))
        ));
    }
    if member_rows.is_empty() {
        member_rows.push_str("<p class=\"detail-note\">No members yet.</p>");
    }
    settings
        + &crate::details::section("Active sessions", "", &list_of(&sessions))
        + &crate::details::section("Members", "", &list_of(&member_rows))
}
/// Rows (`<li>`) in a panel list; a plain note stays a note.
fn list_of(rows: &str) -> String {
    if rows.starts_with("<li>") {
        format!("<ul class=\"detail-list\">{rows}</ul>")
    } else {
        rows.to_string()
    }
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
