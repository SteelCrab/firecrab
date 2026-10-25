use crate::{
    App, Shared, api, element,
    i18n::t,
    model::{escape, list, text},
    render::button,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{Element, HtmlInputElement, HtmlSelectElement};

pub const ENDPOINT: &str = "/api/microblockstorage";

#[derive(Default)]
pub struct State {
    pub draft: Option<Draft>,
    loading: Option<u64>,
    error: String,
}

pub struct Draft {
    vm: String,
    volume: String,
    mount: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Mount {
    volume_id: String,
    mount_path: String,
}

fn available(volume: &Value) -> bool {
    volume["state"] == "available" && volume["attachedVmId"].is_null()
}

fn editable(vm: &Value) -> bool {
    matches!(text(vm, "purpose"), "" | "instance")
        && matches!(text(vm, "state"), "created" | "stopped" | "error")
}

fn disabled(value: bool) -> &'static str {
    if value { "disabled" } else { "" }
}

pub fn content(app: &App) -> String {
    let mut html = format!(
        "<p>{}</p>",
        t("Data volumes survive VM deletion. Attach them in the Volumes tab of a stopped MicroVM.")
    );
    if list(&app.resource).is_empty() {
        html.push_str(&format!(
            "<p class=\"empty-state\">{}</p>",
            t("No data volumes yet.")
        ));
        return html;
    }
    html.push_str(
        "<div class=\"volume-table-scroll\"><table class=\"wasm-table volume-table\"><thead><tr>",
    );
    for label in [
        "Name",
        "Size (GiB)",
        "State",
        "Storage",
        "MicroVM",
        "Mount path",
        "Actions",
    ] {
        html.push_str(&format!("<th>{}</th>", t(label)));
    }
    html.push_str("</tr></thead><tbody>");
    for volume in list(&app.resource) {
        let vm = text(volume, "attachedVmId");
        let owner = if vm.is_empty() {
            "—".into()
        } else {
            let label = app
                .vms
                .iter()
                .find(|item| item.id == vm)
                .map(|item| item.name.as_str())
                .unwrap_or(vm);
            button(label, "navigate-vm", vm)
        };
        html.push_str(&format!(
            "<tr><td><strong>{}</strong><div class=\"mono\">{}</div></td><td>{}</td><td>{}</td><td>{}</td><td>{owner}</td><td class=\"mono\">{}</td><td><button type=\"button\" class=\"btn danger\" data-action=\"volume-delete\" data-id=\"{}\" {}>{}</button></td></tr>",
            escape(text(volume,"name")), escape(text(volume,"id")), volume["sizeGib"], t(text(volume,"state")),
            escape(text(volume,"storageRoot")), escape(text(volume,"mountPath")), escape(text(volume,"id")),
            disabled(app.busy || !available(volume)), t("Delete")
        ));
    }
    html.push_str("</tbody></table></div>");
    html
}

pub fn load(app: &Shared) {
    let mut state = app.borrow_mut();
    if state.route != "vms" || state.volumes.loading == Some(state.generation) {
        return;
    }
    let generation = state.generation;
    state.volumes.loading = Some(generation);
    drop(state);
    let app = app.clone();
    spawn_local(async move {
        let result = api::request("GET", ENDPOINT, None).await;
        let mut state = app.borrow_mut();
        if state.volumes.loading == Some(generation) {
            state.volumes.loading = None;
        }
        if state.generation != generation || state.route != "vms" {
            return;
        }
        match result {
            Ok(value) => {
                state.cache.insert(ENDPOINT.into(), value);
                state.volumes.error.clear();
            }
            Err(error) => state.volumes.error = error,
        }
        let visible = state.form.is_none() && state.detail_tab == "volumes" && state.panel_open;
        drop(state);
        if visible {
            crate::render_detail(&app);
        }
    });
}

pub fn tick(app: &Shared) {
    let state = app.borrow();
    let visible = state.route == "vms"
        && state.form.is_none()
        && state.panel_open
        && state.detail_tab == "volumes"
        && state.detail.is_some();
    drop(state);
    if visible {
        load(app);
    }
}

pub fn after_mutation(app: &Shared, path: &str) {
    if path.starts_with(ENDPOINT) {
        if let Some(draft) = &mut app.borrow_mut().volumes.draft {
            draft.volume.clear();
        }
        load(app);
    }
}

pub fn panel(app: &App, vm: &Value) -> String {
    let cached = app.cache.get(ENDPOINT);
    let volumes = cached.map(list).unwrap_or(&[]);
    let attached: Vec<_> = volumes
        .iter()
        .filter(|volume| volume["attachedVmId"] == vm["id"])
        .collect();
    let enabled = editable(vm) && !app.busy && cached.is_some() && app.volumes.error.is_empty();
    let mut html = format!(
        "<section><h3>{}</h3><p>{}</p>",
        t("Data volumes"),
        t("Stop this VM to attach or detach a volume. Data stays after VM deletion.")
    );
    if !app.volumes.error.is_empty() {
        html.push_str(&format!(
            "<p role=\"alert\">{}</p>{}",
            escape(&app.volumes.error),
            button("Retry", "volume-refresh", "")
        ));
    } else if cached.is_none() {
        html.push_str(&format!("<p>{}</p>", t("Loading volumes…")));
    } else if attached.is_empty() {
        html.push_str(&format!("<p>{}</p>", t("No attached volumes.")));
    }
    for volume in &attached {
        html.push_str(&format!(
            "<section class=\"wasm-card\"><strong>{}</strong> · {} GiB<p class=\"mono\">{}</p><button class=\"btn\" type=\"button\" data-action=\"volume-detach\" data-id=\"{}\" {}>{}</button></section>",
            escape(text(volume,"name")),volume["sizeGib"],escape(text(volume,"mountPath")),escape(text(volume,"id")),disabled(!enabled),t("Detach")
        ));
    }
    let draft = app
        .volumes
        .draft
        .as_ref()
        .filter(|draft| draft.vm == text(vm, "id"));
    let selected = draft.map(|draft| draft.volume.as_str()).unwrap_or("");
    let selection_available = volumes
        .iter()
        .any(|volume| volume["id"] == selected && available(volume));
    let mount = draft.map(|draft| draft.mount.as_str()).unwrap_or("/data");
    html.push_str(&format!("<div class=\"wasm-form\"><div class=\"field wasm-wide\"><label for=\"vm-volume\">{}</label><select id=\"vm-volume\" {}><option value=\"\">{}</option>",t("Available volume"),disabled(!enabled || attached.len() >= 8),t("Select a volume")));
    for volume in volumes.iter().filter(|volume| available(volume)) {
        html.push_str(&format!(
            "<option value=\"{}\" {}>{} · {} GiB</option>",
            escape(text(volume, "id")),
            if volume["id"] == selected {
                "selected"
            } else {
                ""
            },
            escape(text(volume, "name")),
            volume["sizeGib"]
        ));
    }
    html.push_str(&format!("</select></div><div class=\"field wasm-wide\"><label for=\"vm-volume-path\">{}</label><input id=\"vm-volume-path\" value=\"{}\" {}></div><button id=\"vm-volume-attach\" class=\"btn primary\" type=\"button\" data-action=\"volume-attach\" {}>{}</button></div><p><a href=\"#/volumes\">{}</a></p></section>",t("Mount path"),escape(mount),disabled(!enabled),disabled(!enabled || attached.len()>=8 || !selection_available),t("Attach volume"),t("Manage data volumes")));
    html
}

pub fn parse_mounts(value: &str) -> Result<Value, String> {
    let mounts: Vec<Mount> = serde_json::from_str(value).map_err(|_| "Invalid volume mounts.")?;
    if mounts.len() > 8 {
        return Err("A VM can have at most eight volumes.".into());
    }
    for (index, mount) in mounts.iter().enumerate() {
        if mount.volume_id.is_empty() || !mount.mount_path.starts_with('/') {
            return Err("Select a volume and an absolute mount path.".into());
        }
        if mounts[..index]
            .iter()
            .any(|other| other.volume_id == mount.volume_id)
        {
            return Err("A volume can be selected only once.".into());
        }
    }
    serde_json::to_value(mounts).map_err(|error| error.to_string())
}

pub fn mount_editor(value: &str, options: &[(String, String)]) -> String {
    let count = serde_json::from_str::<Vec<Mount>>(value)
        .unwrap_or_default()
        .len();
    let add = format!(
        "<button id=\"volume-add\" class=\"btn\" type=\"button\" data-action=\"volume-add\" {}>{}</button>",
        disabled(count >= 8 || count >= options.len()),
        t("Add volume")
    );
    format!(
        "<div class=\"wasm-wide\"><h3>{}</h3><p>{}</p><input type=\"hidden\" id=\"field-blockStorages\" name=\"blockStorages\" value=\"{}\"><div id=\"volume-mount-rows\">{}</div>{}</div>",
        t("Data volumes"),
        t("New volumes start empty. Existing files at the mount path are hidden."),
        escape(value),
        mount_rows(value, options),
        add
    )
}

fn mount_rows(value: &str, options: &[(String, String)]) -> String {
    let mounts: Vec<Mount> = serde_json::from_str(value).unwrap_or_default();
    let mut html = String::new();
    for (index, mount) in mounts.iter().enumerate() {
        html.push_str(&format!("<div class=\"volume-mount-row\"><div class=\"field\"><label for=\"mount-{index}-volume\">{}</label><select id=\"mount-{index}-volume\" data-mount-index=\"{index}\" data-mount-key=\"volumeId\"><option value=\"\">{}</option>",t("Volume"),t("Select a volume")));
        for (id, label) in options {
            html.push_str(&format!(
                "<option value=\"{}\" {} {}>{}</option>",
                escape(id),
                if *id == mount.volume_id {
                    "selected"
                } else {
                    ""
                },
                disabled(
                    mounts
                        .iter()
                        .enumerate()
                        .any(|(other, entry)| other != index && entry.volume_id == *id)
                ),
                escape(label)
            ));
        }
        html.push_str(&format!("</select></div><div class=\"field\"><label for=\"mount-{index}-path\">{}</label><input id=\"mount-{index}-path\" value=\"{}\" data-mount-index=\"{index}\" data-mount-key=\"mountPath\"></div>{}</div>",t("Mount path"),escape(&mount.mount_path),button("Remove","volume-remove",&index.to_string())));
    }
    html
}

fn update_mounts(app: &Shared, mounts: &[Mount], redraw: bool) {
    let value = serde_json::to_string(mounts).unwrap_or_else(|_| "[]".into());
    if let Some(input) =
        element("field-blockStorages").and_then(|el| el.dyn_into::<HtmlInputElement>().ok())
    {
        input.set_value(&value);
    }
    let mut state = app.borrow_mut();
    state.dirty = true;
    let Some(field) = state.form.as_mut().and_then(|form| {
        form.fields
            .iter_mut()
            .find(|field| field.key == "blockStorages")
    }) else {
        return;
    };
    field.value = value.clone();
    if let Some(button) = element("volume-add") {
        if mounts.len() >= 8 || mounts.len() >= field.options.len() {
            let _ = button.set_attribute("disabled", "");
        } else {
            let _ = button.remove_attribute("disabled");
        }
    }
    if redraw {
        crate::set_html("volume-mount-rows", &mount_rows(&value, &field.options));
    }
}

pub fn input(app: &Shared, target: &Element) -> bool {
    let value = target
        .dyn_ref::<HtmlInputElement>()
        .map(|input| input.value())
        .or_else(|| {
            target
                .dyn_ref::<HtmlSelectElement>()
                .map(|input| input.value())
        })
        .unwrap_or_default();
    if target.has_attribute("data-mount-index") {
        let index = target
            .get_attribute("data-mount-index")
            .and_then(|value| value.parse::<usize>().ok());
        let mut mounts: Vec<Mount> = element("field-blockStorages")
            .and_then(|el| el.dyn_into::<HtmlInputElement>().ok())
            .and_then(|input| serde_json::from_str(&input.value()).ok())
            .unwrap_or_default();
        if let Some(mount) = index.and_then(|index| mounts.get_mut(index)) {
            if target.get_attribute("data-mount-key").as_deref() == Some("volumeId") {
                mount.volume_id = value;
            } else {
                mount.mount_path = value;
            }
            update_mounts(app, &mounts, target.tag_name() == "SELECT");
        }
        return true;
    }
    if !["vm-volume", "vm-volume-path"].contains(&target.id().as_str()) {
        return false;
    }
    let mut state = app.borrow_mut();
    let vm = state
        .detail
        .as_ref()
        .map(|vm| text(vm, "id").to_string())
        .unwrap_or_default();
    let draft = state.volumes.draft.get_or_insert_with(|| Draft {
        vm,
        volume: String::new(),
        mount: "/data".into(),
    });
    if target.id() == "vm-volume" {
        draft.volume = value;
    } else {
        draft.mount = value;
    }
    let empty = draft.volume.is_empty();
    drop(state);
    if let Some(button) = element("vm-volume-attach") {
        if empty {
            let _ = button.set_attribute("disabled", "");
        } else {
            let _ = button.remove_attribute("disabled");
        }
    }
    true
}

pub fn dispatch(app: &Shared, action: &str, id: &str) -> bool {
    if action == "volume-add" || action == "volume-remove" {
        let mut mounts: Vec<Mount> = element("field-blockStorages")
            .and_then(|el| el.dyn_into::<HtmlInputElement>().ok())
            .and_then(|input| serde_json::from_str(&input.value()).ok())
            .unwrap_or_default();
        if action == "volume-add" {
            if mounts.len() >= 8 {
                return true;
            }
            let state = app.borrow();
            let selected = state
                .form
                .as_ref()
                .and_then(|form| {
                    form.fields
                        .iter()
                        .find(|field| field.key == "blockStorages")
                })
                .and_then(|field| {
                    field
                        .options
                        .iter()
                        .find(|(id, _)| !mounts.iter().any(|mount| mount.volume_id == *id))
                });
            let Some((id, _)) = selected else {
                return true;
            };
            mounts.push(Mount {
                volume_id: id.clone(),
                mount_path: "/data".into(),
            });
        } else if let Some(index) = id
            .parse::<usize>()
            .ok()
            .filter(|index| *index < mounts.len())
        {
            mounts.remove(index);
        }
        update_mounts(app, &mounts, true);
        return true;
    }
    match action {
        "volume-refresh" => load(app),
        "volume-delete" => {
            let state = app.borrow();
            let Some(volume) = list(&state.resource)
                .iter()
                .find(|volume| volume["id"] == id && available(volume))
            else {
                return true;
            };
            let message = format!(
                "{} \"{}\"?",
                t("Permanently delete this volume and all its data"),
                text(volume, "name")
            );
            drop(state);
            if crate::window()
                .confirm_with_message(&message)
                .unwrap_or(false)
            {
                crate::mutate(
                    app.clone(),
                    "DELETE".into(),
                    format!("{ENDPOINT}/{}", api::encode(id)),
                    None,
                    None,
                );
            }
        }
        "volume-attach" | "volume-detach" => {
            let state = app.borrow();
            let Some(vm) = state.detail.as_ref().filter(|vm| editable(vm)) else {
                return true;
            };
            let Some(volumes) = state
                .cache
                .get(ENDPOINT)
                .filter(|_| state.volumes.error.is_empty())
            else {
                return true;
            };
            let vm_id = text(vm, "id");
            let (volume_id, body) = if action == "volume-attach" {
                let Some(draft) = state
                    .volumes
                    .draft
                    .as_ref()
                    .filter(|draft| draft.vm == vm_id)
                else {
                    return true;
                };
                if !list(volumes)
                    .iter()
                    .any(|volume| volume["id"] == draft.volume && available(volume))
                    || list(volumes)
                        .iter()
                        .filter(|volume| volume["attachedVmId"] == vm_id)
                        .count()
                        >= 8
                {
                    return true;
                }
                (
                    draft.volume.clone(),
                    Some(json!({"vmId":vm_id,"mountPath":draft.mount})),
                )
            } else {
                if !list(volumes)
                    .iter()
                    .any(|volume| volume["id"] == id && volume["attachedVmId"] == vm_id)
                {
                    return true;
                }
                (id.to_string(), None)
            };
            drop(state);
            let operation = if action == "volume-attach" {
                "attach"
            } else {
                "detach"
            };
            crate::mutate(
                app.clone(),
                "POST".into(),
                format!("{ENDPOINT}/{}/{operation}", api::encode(&volume_id)),
                body,
                None,
            );
        }
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_permissions_honor_the_default_vm_purpose_and_inactive_state() {
        assert!(editable(&json!({"state":"stopped"})));
        assert!(editable(&json!({"state":"created","purpose":"instance"})));
        for vm in [
            json!({"state":"paused"}),
            json!({"state":"running"}),
            json!({"state":"starting"}),
            json!({"state":"stopped","purpose":"pool"}),
            json!({"state":"stopped","purpose":"builder"}),
        ] {
            assert!(!editable(&vm));
        }
    }

    #[test]
    fn selected_mounts_match_the_backend_and_reject_invalid_ownership_requests() {
        let id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let mount = json!({"volumeId":id,"mountPath":"/var/lib/postgresql/data"});
        let value = parse_mounts(&json!([mount.clone()]).to_string()).unwrap();
        let mounts: Vec<firecrab_api_types::VmBlockStorageMount> =
            serde_json::from_value(value).unwrap();
        assert_eq!(mounts[0].volume_id.to_string(), id);
        assert_eq!(mounts[0].mount_path, "/var/lib/postgresql/data");
        assert_eq!(parse_mounts("[]").unwrap(), json!([]));
        for invalid in [
            json!([mount.clone(), mount.clone()]),
            json!([{"volumeId":"","mountPath":"/data"}]),
            json!([{"volumeId":id,"mountPath":"relative/path"}]),
            json!(vec![mount; 9]),
        ] {
            assert!(parse_mounts(&invalid.to_string()).is_err());
        }
    }

    #[test]
    fn volume_creation_uses_the_api_capacity_and_storage_pool_identity() {
        let form = crate::forms::resource_form("volumes", &Value::Null, "").unwrap();
        let mut values = form
            .fields
            .iter()
            .map(|field| (field.key.to_string(), field.value.clone()))
            .collect::<std::collections::BTreeMap<_, _>>();
        values.insert("name".into(), "  database-data  ".into());
        values.insert("storageRoot".into(), "fast-ssd".into());
        let body = form.payload(&values).unwrap();
        let request: firecrab_api_types::CreateMicroBlockStorageRequest =
            serde_json::from_value(body).unwrap();
        assert_eq!(request.name, "database-data");
        assert_eq!(request.size_gib, 10);
        assert_eq!(request.storage_root.as_deref(), Some("fast-ssd"));
        for capacity in ["0", "65536"] {
            values.insert("sizeGib".into(), capacity.into());
            assert!(form.validate(&values).contains_key("sizeGib"));
        }
    }
}
