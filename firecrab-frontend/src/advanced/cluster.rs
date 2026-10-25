//! Hosts and VM placements use controller APIs, never host profile switching.
use crate::{
    App, Shared, api,
    model::{escape, list, text},
    render::button,
    render_content, render_messages, window,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{Element, HtmlInputElement, HtmlSelectElement};

const SAVED_REQUEST: &str = "firecrab.cluster.placement.v1";

/// Preserve cursor and focus while heartbeat changes refresh the host list.
pub struct Focus {
    key: String,
    selection: Option<(u32, u32)>,
}
pub fn focused_input() -> Option<Focus> {
    let element = crate::document().active_element()?;
    let key = element.get_attribute("data-cluster-input")?;
    let selection = element.dyn_ref::<HtmlInputElement>().and_then(|input| {
        input
            .selection_start()
            .ok()
            .flatten()
            .zip(input.selection_end().ok().flatten())
    });
    Some(Focus { key, selection })
}
pub fn restore_focus(focus: Option<Focus>) {
    let Some(focus) = focus else {
        return;
    };
    let Ok(elements) = crate::document().query_selector_all("[data-cluster-input]") else {
        return;
    };
    for i in 0..elements.length() {
        let Some(element) = elements
            .get(i)
            .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
        else {
            continue;
        };
        if element.get_attribute("data-cluster-input").as_deref() != Some(&focus.key) {
            continue;
        }
        let _ = element.focus();
        if let Some((start, end)) = focus.selection
            && let Some(input) = element.dyn_ref::<HtmlInputElement>()
        {
            let _ = input.set_selection_range(start, end);
        }
        break;
    }
}
#[derive(Default)]
pub struct State {
    values: BTreeMap<String, String>,
    pending: Option<Value>,
}

fn value<'a>(state: &'a State, key: &str, default: &'a str) -> &'a str {
    state.values.get(key).map(String::as_str).unwrap_or(default)
}
fn field(state: &State, key: &str, label: &str, default: &str, kind: &str) -> String {
    format!(
        "<label class=\"field\">{}<input id=\"cluster-{}\" data-cluster-input=\"{}\" type=\"{}\" value=\"{}\"></label>",
        escape(label),
        escape(key),
        escape(key),
        kind,
        escape(value(state, key, default))
    )
}
fn options(items: &[Value], key: &str, label: &str, selected: &str) -> String {
    items
        .iter()
        .map(|item| {
            let id = text(item, key);
            format!(
                "<option value=\"{}\" {}>{}</option>",
                escape(id),
                if id == selected { "selected" } else { "" },
                escape(text(item, label))
            )
        })
        .collect()
}

pub fn content(app: &App) -> String {
    let hosts = list(&app.resource);
    let state = &app.advanced.cluster;
    let mut out = String::from(
        "<section class=\"cluster-screen\" aria-label=\"Cluster\"><h2>Workload hosts</h2><p>Drain prevents new placements. Existing VMs stay on their recorded host.</p>",
    );
    out.push_str("<div class=\"wasm-cards cluster-hosts\">");
    for host in hosts {
        let report = &host["report"];
        let id = text(report, "id");
        let status = if host["online"] != true {
            "Offline"
        } else if host["draining"] == true {
            "Draining"
        } else {
            "Ready"
        };
        let label = if host["draining"] == true {
            "Resume placement"
        } else {
            "Drain host"
        };
        let action = if host["draining"] == true {
            "cluster-resume"
        } else {
            "cluster-drain"
        };
        out.push_str(&format!("<article class=\"wasm-card\" data-cluster-host=\"{}\"><h3>{}</h3><p>{status} · {}</p><p>Reserved CPU: {} / {} · Memory: {} / {} MiB</p>{}<details><summary>Host inventory</summary>{}</details></article>",
            escape(id),escape(text(report,"name")),escape(id),host["reserved"]["cpu"],report["capacity"]["cpu"],
            host["reserved"]["ramMib"],report["capacity"]["ramMib"],button(label,action,id),crate::render::properties(report)));
    }
    out.push_str("</div>");
    if hosts.is_empty() {
        out.push_str("<p class=\"empty-state\">No workload hosts registered. Configure worker enrollment with the controller's host UUID and HTTPS endpoint.</p>");
    }
    out.push_str("<h2>Place a VM</h2>");
    if let Some(saved) = &state.pending {
        out.push_str(&format!("<article class=\"wasm-card\"><p>A saved request for <strong>{}</strong> has no acknowledged result. Retry to recover the same placement.</p>{}</article>",
            escape(text(&saved["spec"],"name")),button("Retry saved placement","cluster-place","")));
    } else if !hosts.is_empty() {
        out.push_str("<section class=\"wasm-card cluster-form\" aria-label=\"VM placement form\"><div class=\"cluster-fields\">");
        out.push_str(&field(state, "name", "VM name", "", "text"));
        let images: BTreeSet<_> = hosts
            .iter()
            .flat_map(|h| list(&h["report"]["images"]))
            .map(Value::to_string)
            .collect();
        let default = images.first().map(String::as_str).unwrap_or_default();
        let selected = value(state, "image", default);
        out.push_str(
            "<label class=\"field\">Installed image<select id=\"cluster-image\" data-cluster-input=\"image\">",
        );
        for image in &images {
            let pin: Value = serde_json::from_str(image).unwrap_or_default();
            out.push_str(&format!(
                "<option value=\"{}\" {}>{} · {} · {}</option>",
                escape(image),
                if selected == image { "selected" } else { "" },
                escape(text(&pin, "alias")),
                escape(text(&pin, "version")),
                escape(
                    &text(&pin, "rootfsSha256")
                        .chars()
                        .take(12)
                        .collect::<String>()
                )
            ));
        }
        out.push_str("</select></label>");
        for (key, label, default) in [
            ("cpu", "vCPUs", "1"),
            ("ram", "Memory (MiB)", "512"),
            ("disk", "Disk (GiB)", "2"),
        ] {
            out.push_str(&field(state, key, label, default, "number"));
        }
        out.push_str("</div><h3>Candidate hosts</h3><p>Select a local network and storage on each candidate. Networks and disks remain host-local. Only hosts with the selected image and available capacity are eligible.</p>");
        for host in hosts {
            let report = &host["report"];
            let id = text(report, "id");
            let networks = list(&report["networks"]);
            let storages = list(&report["storages"]);
            let eligible = host["online"] == true
                && host["draining"] != true
                && !networks.is_empty()
                && !storages.is_empty();
            let chosen = eligible
                && value(
                    state,
                    &format!("candidate-{id}"),
                    if eligible { "true" } else { "false" },
                ) == "true";
            let network_key = format!("network-{id}");
            let storage_key = format!("storage-{id}");
            let id_attr = escape(id);
            let network_attr = escape(&network_key);
            let storage_attr = escape(&storage_key);
            out.push_str(&format!("<fieldset class=\"wasm-card cluster-target\"><legend><label><input type=\"checkbox\" data-cluster-input=\"candidate-{id_attr}\" {} {}>{}</label></legend><label class=\"field\">Network<select data-cluster-input=\"{network_attr}\">{}</select></label><label class=\"field\">Storage<select data-cluster-input=\"{storage_attr}\">{}</select></label></fieldset>",
                if chosen {"checked"}else{""},if eligible {""}else{"disabled"},escape(text(report,"name")),
                options(networks,"id","name",value(state,&network_key,networks.first().map(|v|text(v,"id")).unwrap_or_default())),
                options(storages,"id","id",value(state,&storage_key,storages.first().map(|v|text(v,"id")).unwrap_or_default()))));
        }
        out.push_str(&button("Place VM", "cluster-place", ""));
        out.push_str("</section>");
    }
    out.push_str("<h2>VM placements</h2>");
    let placements = app
        .cache
        .get("/api/cluster/placements")
        .map(list)
        .unwrap_or_default();
    for placement in placements {
        let id = text(placement, "id");
        let phase = text(placement, "phase");
        let owner = text(&placement["vm"], "hostId");
        let name = hosts
            .iter()
            .find(|h| text(&h["report"], "id") == owner)
            .map(|h| text(&h["report"], "name"))
            .unwrap_or(owner);
        out.push_str(&format!("<article class=\"wasm-card\" data-cluster-placement=\"{}\"><h3>{}</h3><p>{} · {}</p><p>Host: {} · VM: {}</p><p>{}</p>",
            escape(id),escape(text(&placement["spec"],"name")),escape(phase),escape(text(&placement["observed"],"state")),escape(name),
            escape(text(&placement["vm"],"resourceId")),escape(text(placement,"error"))));
        if phase != "deleted" {
            out.push_str("<div class=\"cluster-actions\">");
            if phase == "confirmed" {
                out.push_str(&button("Start", "cluster-start", id));
                out.push_str(&button("Stop", "cluster-stop", id));
            }
            if phase != "rejected" {
                out.push_str(&button("Reconcile", "cluster-reconcile", id));
            }
            out.push_str(&button("Delete", "cluster-delete", id));
            out.push_str("</div>");
        }
        out.push_str("</article>");
    }
    if placements.is_empty() {
        out.push_str("<p class=\"empty-state\">No VM placements yet.</p>");
    }
    out.push_str("</section>");
    out
}

pub fn restore(app: &Shared) {
    if app.borrow().advanced.cluster.pending.is_some() {
        return;
    }
    if let Ok(Some(storage)) = window().session_storage()
        && let Ok(Some(saved)) = storage.get_item(SAVED_REQUEST)
        && let Ok(saved) = serde_json::from_str::<Value>(&saved)
        && saved["key"].as_str().is_some_and(|v| !v.is_empty())
        && saved["spec"].is_object()
    {
        app.borrow_mut().advanced.cluster.pending = Some(saved);
    }
}

pub fn input(app: &Shared, target: &Element) -> bool {
    let Some(key) = target.get_attribute("data-cluster-input") else {
        return false;
    };
    let value = if let Some(input) = target.dyn_ref::<HtmlInputElement>() {
        if input.type_() == "checkbox" {
            input.checked().to_string()
        } else {
            input.value()
        }
    } else if let Some(select) = target.dyn_ref::<HtmlSelectElement>() {
        select.value()
    } else {
        return false;
    };
    app.borrow_mut().advanced.cluster.values.insert(key, value);
    true
}

fn placement_spec(app: &App) -> Result<Value, String> {
    let state = &app.advanced.cluster;
    let hosts = list(&app.resource);
    let image_default = hosts
        .iter()
        .flat_map(|h| list(&h["report"]["images"]))
        .map(Value::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .next()
        .unwrap_or_default();
    let image: Value = serde_json::from_str(value(state, "image", &image_default))
        .map_err(|_| "Choose an installed image.")?;
    let mut targets = Vec::new();
    for host in hosts {
        let report = &host["report"];
        let id = text(report, "id");
        let networks = list(&report["networks"]);
        let storages = list(&report["storages"]);
        let eligible = host["online"] == true
            && host["draining"] != true
            && !networks.is_empty()
            && !storages.is_empty();
        if !eligible || value(state, &format!("candidate-{id}"), "true") != "true" {
            continue;
        }
        let network = value(state, &format!("network-{id}"), text(&networks[0], "id")).to_string();
        let storage = value(state, &format!("storage-{id}"), text(&storages[0], "id")).to_string();
        targets.push(json!({"network":{"hostId":id,"resourceId":network},"storage":{"hostId":id,"resourceId":storage}}));
    }
    let number = |key, default| {
        value(state, key, default)
            .parse::<u64>()
            .map_err(|_| format!("Enter a valid {key}."))
    };
    let spec = json!({"name":value(state,"name",""),"image":image,"cpu":number("cpu","1")?,"ram":number("ram","512")?,"diskGb":number("disk","2")?,"targets":targets});
    Ok(spec)
}

fn new_request(app: &App) -> Result<Value, String> {
    let spec = placement_spec(app)?;
    let mut bytes = [0u8; 16];
    window()
        .crypto()
        .map_err(api::error)?
        .get_random_values_with_u8_array(&mut bytes)
        .map_err(api::error)?;
    Ok(json!({"key":bytes.iter().map(|b|format!("{b:02x}")).collect::<String>(),"spec":spec}))
}

pub fn dispatch(app: &Shared, action: &str, id: &str) -> bool {
    if !action.starts_with("cluster-") {
        return false;
    }
    if app.borrow().busy {
        return true;
    }
    let action = action.to_string();
    let id = id.to_string();
    let app = app.clone();
    let saved = if action == "cluster-place" {
        let pending = app.borrow().advanced.cluster.pending.clone();
        let request = match pending {
            Some(saved) => Ok(saved),
            None => new_request(&app.borrow()),
        };
        match request.and_then(|saved| {
            let storage = window()
                .session_storage()
                .map_err(api::error)?
                .ok_or("Session storage is required to preserve placement retries.")?;
            storage
                .set_item(SAVED_REQUEST, &saved.to_string())
                .map_err(api::error)?;
            Ok(saved)
        }) {
            Ok(saved) => {
                app.borrow_mut().advanced.cluster.pending = Some(saved.clone());
                Some(saved)
            }
            Err(error) => {
                app.borrow_mut().error = error;
                render_messages(&app);
                return true;
            }
        }
    } else {
        None
    };
    app.borrow_mut().busy = true;
    let generation = app.borrow().generation;
    spawn_local(async move {
        let result = if let Some(saved) = &saved {
            api::post_idempotent(
                "/api/cluster/placements",
                &saved["spec"],
                text(saved, "key"),
            )
            .await
        } else {
            let (method, path, body) = if action == "cluster-drain" || action == "cluster-resume" {
                (
                    "PATCH",
                    format!("/api/cluster/hosts/{}", api::encode(&id)),
                    Some(json!({"draining":action=="cluster-drain"})),
                )
            } else {
                (
                    "POST",
                    format!(
                        "/api/cluster/placements/{}/{}",
                        api::encode(&id),
                        action.trim_start_matches("cluster-")
                    ),
                    None,
                )
            };
            api::request_detailed(method, &path, body.as_ref(), None).await
        };
        let clear = saved.is_some()
            && (result.is_ok()
                || result.as_ref().err().is_some_and(|e| {
                    e.status == Some(400) || e.code.as_deref() == Some("insufficient_capacity")
                }));
        if clear {
            if let Ok(Some(storage)) = window().session_storage() {
                let _ = storage.remove_item(SAVED_REQUEST);
            }
            app.borrow_mut().advanced.cluster.pending = None;
        }
        let mut state = app.borrow_mut();
        state.busy = false;
        if state.generation != generation {
            return;
        }
        match result {
            Ok(_) => {
                state.error.clear();
                state.notice =
                    "Cluster operation recorded. Status updates from the owning host.".into();
            }
            Err(error) => {
                state.error = error.to_string();
                if !error.fields.is_empty() {
                    state.error.push_str(&format!(
                        " — {}",
                        error
                            .fields
                            .values()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("; ")
                    ));
                }
            }
        }
        drop(state);
        render_content(&app);
        crate::refresh(app.clone());
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> App {
        App {
            route: "cluster".into(),
            resource: json!([{
                "report":{"id":"00000000-0000-0000-0000-000000000001","name":"<host>","capacity":{"cpu":4,"ramMib":4096},
                    "images":[{"alias":"fixture","version":"v1","kernelSha256":"a".repeat(64),"rootfsSha256":"b".repeat(64),"bootArgsSha256":"c".repeat(64)}],
                    "networks":[{"id":"00000000-0000-0000-0000-000000000010","name":"private"}],"storages":[{"id":"default"}]},
                "online":true,"draining":false,"reserved":{"cpu":1,"ramMib":512}
            }]),
            ..App::default()
        }
    }
    #[test]
    fn hosts_show_health_reservations_and_host_local_resources_without_html_injection() {
        let app = app();
        let html = content(&app);
        assert!(html.contains("&lt;host&gt;"));
        assert!(!html.contains("<host>"));
        assert!(html.contains("Reserved CPU: 1 / 4"));
        assert!(html.contains("Memory: 512 / 4096 MiB"));
        assert!(html.contains("private"));
        assert!(html.contains("default"));
        assert!(html.contains("Drain host"));
    }
    #[test]
    fn an_unresolved_request_offers_the_same_retry_instead_of_another_new_placement() {
        let mut app = app();
        app.advanced.cluster.pending =
            Some(json!({"key":"retry-key","spec":{"name":"original-vm"}}));
        let html = content(&app);
        assert!(html.contains("Retry saved placement"));
        assert!(html.contains("original-vm"));
        assert!(!html.contains("VM placement form"));
    }
    #[test]
    fn offline_and_draining_hosts_cannot_be_selected_as_new_candidates() {
        for (online, draining) in [(false, false), (true, true)] {
            let mut app = app();
            app.advanced.cluster.values.insert(
                "candidate-00000000-0000-0000-0000-000000000001".into(),
                "true".into(),
            );
            app.resource[0]["online"] = json!(online);
            app.resource[0]["draining"] = json!(draining);
            let html = content(&app);
            assert!(html.contains("disabled"));
            assert!(!html.contains("checked disabled"));
        }
    }

    #[test]
    fn placement_form_matches_the_shared_wire_contract_and_scopes_every_resource() {
        let mut app = app();
        app.advanced
            .cluster
            .values
            .insert("name".into(), "cluster-vm".into());
        let spec: firecrab_api_types::PlacementSpec =
            serde_json::from_value(placement_spec(&app).unwrap()).unwrap();
        assert_eq!(spec.name, "cluster-vm");
        assert_eq!(spec.image.alias, "fixture");
        assert_eq!(spec.targets.len(), 1);
        assert_eq!(
            spec.targets[0].network.host_id,
            spec.targets[0].storage.host_id
        );
        assert_eq!(spec.targets[0].storage.resource_id, "default");
        assert_eq!(
            spec.targets[0].network.resource_id,
            "00000000-0000-0000-0000-000000000010"
        );
    }
}
