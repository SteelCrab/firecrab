//! Explicit CODE lifecycle requests use the existing durable VM/checkout API.
mod operations;
mod view;
use super::*;
use serde_json::json;
pub(super) use view::content;
use web_sys::HtmlSelectElement;

const JOURNAL: &str = "firecrab.code.runtime.v1.";
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Image {
    alias: String,
    version: String,
    installed: bool,
    min_disk_gb: u32,
}
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Network {
    id: String,
    name: String,
    internet_enabled: bool,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Storage {
    id: String,
    name: String,
    kind: String,
}
#[derive(Clone, PartialEq)]
struct Catalog {
    images: Vec<Image>,
    networks: Vec<Network>,
    storage: Vec<Storage>,
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Placement {
    template: String,
    micro_network_id: String,
    storage_root: String,
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Action {
    Create,
    Start,
    Stop,
}
impl Action {
    fn expected(&self) -> &str {
        if *self == Self::Stop { "stop" } else { "start" }
    }
    fn suffix(&self) -> &str {
        match self {
            Self::Create => "",
            Self::Start => "/start",
            Self::Stop => "/stop",
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Before {
    vm: String,
    operation: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Pending {
    id: String,
    revision: u64,
    spec: Spec,
    action: Action,
    placement: Option<Placement>,
    before: Option<Before>,
}
#[derive(Default)]
pub(super) struct State {
    catalog: Option<Catalog>,
    loading: bool,
    unavailable: bool,
    epoch: u64,
    choice: Option<(String, Placement)>,
    request: Option<Pending>,
    busy: bool,
    error: &'static str,
    notice: bool,
    retryable: bool,
}
pub(super) fn busy(app: &App) -> bool {
    app.code.lifecycle.busy
}
fn key(id: &str) -> String {
    format!("{JOURNAL}{id}")
}
fn remember(id: &str, pending: Option<&Pending>) {
    if let Ok(Some(storage)) = crate::window().session_storage() {
        if let Some(p) = pending {
            if let Ok(raw) = serde_json::to_string(p) {
                let _ = storage.set_item(&key(id), &raw);
            }
        } else {
            let _ = storage.remove_item(&key(id));
        }
    }
}
fn restore(id: &str) -> Option<Pending> {
    let raw = crate::window()
        .session_storage()
        .ok()
        .flatten()?
        .get_item(&key(id))
        .ok()
        .flatten()?;
    let p: Pending = serde_json::from_str(&raw).ok()?;
    let valid = p.id == id
        && uuid(id)
        && p.revision > 0
        && p.revision <= i64::MAX as u64
        && p.before
            .as_ref()
            .is_none_or(|b| uuid(&b.vm) && uuid(&b.operation))
        && match (&p.action, &p.placement, &p.before) {
            (Action::Create, Some(v), None) => {
                !v.template.is_empty()
                    && v.template.len() <= 100
                    && uuid(&v.micro_network_id)
                    && !v.storage_root.is_empty()
                    && v.storage_root.len() <= 128
            }
            (Action::Start | Action::Stop, None, Some(_)) => true,
            _ => false,
        };
    valid.then_some(p)
}
fn eligible_image(image: &Image, w: &Workspace) -> bool {
    image.installed
        && image.alias != "__microboot"
        && !image.alias.is_empty()
        && image.min_disk_gb <= w.spec.resources.disk_gb
}
fn placement(app: &App, w: &Workspace) -> Option<Placement> {
    let c = app.code.lifecycle.catalog.as_ref()?;
    if let Some((id, p)) = &app.code.lifecycle.choice
        && id == &w.id
    {
        return Some(p.clone());
    }
    Some(Placement {
        template: c
            .images
            .iter()
            .find(|i| eligible_image(i, w))
            .map(|i| i.alias.clone())
            .unwrap_or_default(),
        micro_network_id: c
            .networks
            .iter()
            .find(|n| n.internet_enabled && uuid(&n.id))
            .map(|n| n.id.clone())
            .unwrap_or_default(),
        storage_root: c
            .storage
            .iter()
            .find(|s| s.kind == "default")
            .or_else(|| c.storage.first())
            .map(|s| s.id.clone())
            .unwrap_or_default(),
    })
}
fn valid_placement(app: &App, w: &Workspace, p: &Placement) -> bool {
    app.code.lifecycle.catalog.as_ref().is_some_and(|c| {
        c.images
            .iter()
            .any(|i| i.alias == p.template && eligible_image(i, w))
            && c.networks
                .iter()
                .any(|n| n.id == p.micro_network_id && n.internet_enabled && uuid(&n.id))
            && c.storage.iter().any(|s| s.id == p.storage_root)
    })
}
fn pending(r: &Runtime) -> bool {
    ["queued", "provisioning", "preparing", "stopping"].contains(&r.phase.as_str())
}
fn known(r: &Runtime) -> bool {
    ["ready", "stopped", "failed", "reconcile_required"].contains(&r.phase.as_str())
        && ["start", "stop"].contains(&r.action.as_str())
}
pub(super) fn unresolved(app: &App) -> bool {
    busy(app) || app.code.lifecycle.request.is_some()
}
fn available(app: &App, w: &Workspace, action: &Action) -> bool {
    if busy(app)
        || app.code.lifecycle.request.is_some()
        || app.code.binding.is_some()
        || auth::is_open(app)
    {
        return false;
    }
    match (&app.code.runtime, action) {
        (RuntimeState::Absent, Action::Create) => {
            w.connections.github == "present_unverified"
                && placement(app, w).is_some_and(|p| valid_placement(app, w, &p))
        }
        (RuntimeState::Current(r), Action::Start) => {
            known(r)
                && !pending(r)
                && ["stopped", "failed", "reconcile_required"].contains(&r.phase.as_str())
                && (r.initial_commit.is_some() || w.connections.github == "present_unverified")
                && (r.action == "start" || r.phase != "failed")
        }
        (RuntimeState::Current(r), Action::Stop) => {
            known(r) && !pending(r) && r.vm_state.as_deref() == Some("running")
        }
        _ => false,
    }
}
pub(super) fn load(app: Shared) {
    let mut a = app.borrow_mut();
    if auth::is_open(&a) || busy(&a) {
        return;
    }
    let Some(w) = binding::selected(&a) else {
        return;
    };
    let id = w.id.clone();
    if a.code.lifecycle.request.is_none() {
        a.code.lifecycle.request = restore(&id);
        if a.code.lifecycle.request.is_some() {
            a.code.selection = Some(id.clone());
        }
    }
    if !matches!(a.code.runtime, RuntimeState::Absent)
        || a.code.lifecycle.loading
        || a.code.lifecycle.catalog.is_some()
        || a.code.lifecycle.unavailable
    {
        return;
    }
    a.code.lifecycle.loading = true;
    a.code.lifecycle.epoch += 1;
    let (epoch, generation) = (a.code.lifecycle.epoch, a.generation);
    drop(a);
    spawn_local(async move {
        let images = api::request("GET", "/api/images", None)
            .await
            .ok()
            .and_then(|v| serde_json::from_value::<Vec<Image>>(v).ok());
        let networks = api::request("GET", "/api/micro-networks", None)
            .await
            .ok()
            .and_then(|v| serde_json::from_value::<Vec<Network>>(v).ok());
        let storage = api::request("GET", "/api/storage", None)
            .await
            .ok()
            .and_then(|v| serde_json::from_value::<Vec<Storage>>(v).ok());
        let mut a = app.borrow_mut();
        if a.generation != generation || a.code.lifecycle.epoch != epoch {
            return;
        }
        let catalog = images
            .zip(networks)
            .zip(storage)
            .map(|((images, networks), storage)| Catalog {
                images,
                networks,
                storage,
            });
        a.code.lifecycle.loading = false;
        a.code.lifecycle.unavailable = catalog.is_none();
        a.code.lifecycle.catalog = catalog;
        drop(a);
        render_content(&app);
    });
}
pub(super) fn change(app: &Shared, node: &Element) -> bool {
    let id = node.id();
    let Some(field) = id.strip_prefix("code-placement-") else {
        return false;
    };
    let Some(select) = node.dyn_ref::<HtmlSelectElement>() else {
        return false;
    };
    let mut a = app.borrow_mut();
    if busy(&a) || a.code.lifecycle.request.is_some() {
        return true;
    }
    let Some(w) = binding::selected(&a) else {
        return true;
    };
    let Some(mut p) = placement(&a, w) else {
        return true;
    };
    let workspace = w.id.clone();
    match field {
        "template" => p.template = select.value(),
        "network" => p.micro_network_id = select.value(),
        "storage" => p.storage_root = select.value(),
        _ => return false,
    }
    a.code.selection = Some(workspace.clone());
    a.code.lifecycle.choice = Some((workspace, p));
    let generation = a.generation;
    drop(a);
    let app = app.clone();
    spawn_local(async move {
        if app.borrow().generation == generation {
            render_content(&app)
        }
    });
    true
}
pub(super) fn action(app: &Shared, action: &str) -> bool {
    if !action.starts_with("code-runtime-") || !is_route(&app.borrow().route) {
        return false;
    }
    if busy(&app.borrow()) {
        return true;
    }
    match action {
        "code-runtime-create" => operations::begin(app, Action::Create),
        "code-runtime-start" => operations::begin(app, Action::Start),
        "code-runtime-stop" => operations::begin(app, Action::Stop),
        "code-runtime-check" => operations::recover(app, false),
        "code-runtime-retry" => operations::recover(app, true),
        "code-runtime-dismiss" => {
            let mut a = app.borrow_mut();
            if let Some(p) = a.code.lifecycle.request.take() {
                remember(&p.id, None)
            }
            a.code.lifecycle.error = "";
            a.code.lifecycle.retryable = false;
            drop(a);
            super::load(app.clone());
            render_content(app);
        }
        "code-runtime-catalog" => {
            let mut a = app.borrow_mut();
            a.code.lifecycle.catalog = None;
            a.code.lifecycle.unavailable = false;
            a.code.lifecycle.epoch += 1;
            a.code.lifecycle.loading = false;
            drop(a);
            load(app.clone());
            render_content(app);
        }
        _ => return false,
    };
    true
}
