//! Hosts and VM placements use controller APIs, never host profile switching.
mod services;

use crate::{
    App, Shared, api,
    model::{escape, list, text},
    render::{
        Icon, active_subtab, button, empty_state, icon_button, name_cell, status_dot, subtabs,
    },
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
    service_pending: Option<Value>,
    editing_service: Option<(String, u64)>,
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

/// "Ready", "Draining" or "Offline" as a status dot.
fn host_status(host: &Value, ko: bool) -> String {
    if host["online"] != true {
        status_dot("error", if ko { "연결 끊김" } else { "Disconnected" })
    } else if host["draining"] == true {
        status_dot("busy", if ko { "비우는 중" } else { "Draining" })
    } else {
        status_dot("ok", if ko { "정상" } else { "Healthy" })
    }
}
/// A bar and "used / total" for a reserved resource; amber from 80 %.
fn reserved(used: f64, total: f64, label: String) -> String {
    let percent = if total > 0.0 {
        (used / total * 100.0).min(100.0)
    } else {
        0.0
    };
    format!(
        "<div class=\"meter\"><span class=\"meter-bar\"{}><i style=\"width:{percent:.0}%\"></i></span><span class=\"meter-label\">{label}</span></div>",
        if percent >= 80.0 {
            " data-warn=\"true\""
        } else {
            ""
        }
    )
}
fn hosts_tab(app: &App, hosts: &[Value]) -> String {
    let ko = app.locale == "ko";
    if hosts.is_empty() {
        return empty_state(
            "cluster",
            if ko {
                "등록된 작업 호스트가 없습니다"
            } else {
                "No workload hosts registered"
            },
            if ko {
                "다른 Mac이나 Linux 호스트를 이 컨트롤러에 등록하면 VM을 나눠 실행할 수 있습니다."
            } else {
                "Register another Mac or Linux host with this controller to spread VMs across hosts."
            },
            "",
        );
    }
    let mut out = format!(
        "<div class=\"table-scroll\"><table class=\"wasm-table cluster-hosts\" aria-label=\"{}\"><thead><tr><th>{}</th><th>{}</th><th class=\"col-secondary\">{}</th><th class=\"col-secondary\">{}</th><th class=\"row-actions\"><span class=\"sr-only\">{}</span></th></tr></thead><tbody>",
        if ko {
            "작업 호스트"
        } else {
            "Workload hosts"
        },
        if ko { "호스트" } else { "Host" },
        if ko { "상태" } else { "Status" },
        if ko { "CPU 예약" } else { "CPU reserved" },
        if ko {
            "메모리 예약"
        } else {
            "Memory reserved"
        },
        crate::i18n::t("Actions")
    );
    for host in hosts {
        let report = &host["report"];
        let id = text(report, "id");
        let draining = host["draining"] == true;
        let (used_cpu, cpu) = (
            host["reserved"]["cpu"].as_f64().unwrap_or(0.0),
            report["capacity"]["cpu"].as_f64().unwrap_or(0.0),
        );
        let (used_ram, ram) = (
            host["reserved"]["ramMib"].as_f64().unwrap_or(0.0),
            report["capacity"]["ramMib"].as_f64().unwrap_or(0.0),
        );
        out.push_str(&format!(
            "<tr data-cluster-host=\"{}\"><td>{}</td><td>{}</td><td class=\"col-secondary\">{}</td><td class=\"col-secondary\">{}</td><td class=\"row-actions\">{}</td></tr>",
            escape(id),
            name_cell(text(report, "name"), id, "", true),
            host_status(host, ko),
            reserved(used_cpu, cpu, format!("{used_cpu} / {cpu}")),
            reserved(
                used_ram,
                ram,
                format!("{} / {} GiB", crate::render::gib(used_ram as u64), crate::render::gib(ram as u64))
            ),
            if draining {
                icon_button("Resume placement", "cluster-resume", id, Icon::Start)
            } else {
                icon_button("Drain host", "cluster-drain", id, Icon::Drain)
            }
        ));
    }
    out.push_str("</tbody></table></div>");
    out.push_str(&format!(
        "<p class=\"cluster-note\">{}</p>",
        if ko {
            "드레인하면 새 배치를 받지 않습니다. 이미 있는 VM은 기록된 호스트에 남습니다."
        } else {
            "Drain prevents new placements. Existing VMs stay on their recorded host."
        }
    ));
    for host in hosts {
        out.push_str(&format!(
            "<details class=\"cluster-inventory\"><summary>{} · {}</summary>{}</details>",
            if ko { "호스트 정보" } else { "Host info" },
            escape(text(&host["report"], "name")),
            crate::render::properties(&host["report"])
        ));
    }
    out
}
fn placements_tab(app: &App, hosts: &[Value], placements: &[Value]) -> String {
    let ko = app.locale == "ko";
    let state = &app.advanced.cluster;
    let mut out = String::new();
    out.push_str("<div class=\"cluster-place\">");
    out.push_str(&format!(
        "<h3 class=\"cluster-subhead\">{}</h3>",
        if app.locale == "ko" {
            "VM 배치"
        } else {
            "Place a VM"
        }
    ));
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
    out.push_str("</div>");
    if placements.is_empty() {
        out.push_str(&empty_state(
            "cluster",
            if ko {
                "아직 VM 배치가 없습니다"
            } else {
                "No VM placements yet"
            },
            if ko {
                "위 양식으로 VM을 호스트에 배치합니다."
            } else {
                "Place a VM on a host with the form above."
            },
            "",
        ));
        return out;
    }
    out.push_str(&format!(
        "<div class=\"table-scroll\"><table class=\"wasm-table cluster-placements\" aria-label=\"{}\"><thead><tr><th>{}</th><th class=\"col-secondary\">{}</th><th>{}</th><th class=\"row-actions\"><span class=\"sr-only\">{}</span></th></tr></thead><tbody>",
        if ko { "VM 배치" } else { "VM placements" },
        "VM",
        if ko { "호스트" } else { "Host" },
        if ko { "상태" } else { "Status" },
        crate::i18n::t("Actions")
    ));
    for placement in placements {
        let id = text(placement, "id");
        let phase = text(placement, "phase");
        let owner = text(&placement["vm"], "hostId");
        let host = hosts
            .iter()
            .find(|h| text(&h["report"], "id") == owner)
            .map(|h| text(&h["report"], "name"))
            .unwrap_or(owner);
        let error = text(placement, "error");
        let tone = match phase {
            "confirmed" => "ok",
            "rejected" => "error",
            "deleted" => "off",
            _ => "busy",
        };
        let mut actions = String::new();
        if phase != "deleted" {
            if phase == "confirmed" {
                actions.push_str(&icon_button("Start", "cluster-start", id, Icon::Start));
                actions.push_str(&icon_button("Stop", "cluster-stop", id, Icon::Stop));
            }
            if phase != "rejected" {
                actions.push_str(&icon_button(
                    "Reconcile",
                    "cluster-reconcile",
                    id,
                    Icon::Retry,
                ));
            }
            actions.push_str(&icon_button("Delete", "cluster-delete", id, Icon::Delete));
        }
        out.push_str(&format!(
            "<tr data-cluster-placement=\"{}\"><td>{}</td><td class=\"col-secondary\">{}</td><td>{}</td><td class=\"row-actions\">{actions}</td></tr>",
            escape(id),
            name_cell(text(&placement["spec"], "name"), error, if error.is_empty() { "" } else { "error" }, false),
            escape(host),
            status_dot(tone, format!("{phase} {}", text(&placement["observed"], "state")).trim())
        ));
    }
    out.push_str("</tbody></table></div>");
    out
}

pub fn content(app: &App) -> String {
    let ko = app.locale == "ko";
    let hosts = list(&app.resource);
    let placements = app
        .cache
        .get("/api/cluster/placements")
        .map(list)
        .unwrap_or_default();
    let services = app
        .cache
        .get("/api/cluster/services")
        .map(list)
        .unwrap_or_default();
    let active = active_subtab(app, "cluster", "hosts");
    let tabs = subtabs(
        "cluster",
        if ko {
            "클러스터 구역"
        } else {
            "Cluster sections"
        },
        &[
            (
                "hosts",
                if ko {
                    "작업 호스트"
                } else {
                    "Workload hosts"
                },
                hosts.len(),
            ),
            (
                "placements",
                if ko { "VM 배치" } else { "VM placements" },
                placements.len(),
            ),
            (
                "services",
                if ko {
                    "컨트롤러 서비스"
                } else {
                    "Controller services"
                },
                services.len(),
            ),
        ],
        active,
    );
    let body = match active {
        "placements" => placements_tab(app, hosts, placements),
        "services" => services::content(app),
        _ => hosts_tab(app, hosts),
    };
    format!(
        "<section class=\"cluster-screen\" aria-label=\"Cluster\">{tabs}<div role=\"tabpanel\" class=\"cluster-body\">{body}</div></section>"
    )
}

pub fn restore(app: &Shared) {
    services::restore(app);
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
    if services::dispatch(app, action, id) {
        return true;
    }
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
        let mut app = app();
        let html = content(&app);
        assert!(html.contains("&lt;host&gt;"));
        assert!(!html.contains("<host>"));
        assert!(html.contains("1 / 4"));
        assert!(html.contains("0.5 / 4 GiB"));
        assert!(html.contains("Drain host"));
        // The form that places a VM is on the placements tab.
        app.subtabs.insert("cluster".into(), "placements".into());
        let html = content(&app);
        assert!(html.contains("private"));
        assert!(html.contains("default"));
    }
    #[test]
    fn an_unresolved_request_offers_the_same_retry_instead_of_another_new_placement() {
        let mut app = app();
        app.advanced.cluster.pending =
            Some(json!({"key":"retry-key","spec":{"name":"original-vm"}}));
        app.subtabs.insert("cluster".into(), "placements".into());
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
            app.subtabs.insert("cluster".into(), "placements".into());
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
