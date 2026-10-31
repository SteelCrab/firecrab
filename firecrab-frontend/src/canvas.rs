//! Real canvas views use project APIs; local storage contains only tab identities.
mod pty;
mod view;
use crate::{
    App, Shared, api, document, element,
    model::{escape, list, text},
    set_html, window,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::spawn_local;
use web_sys::{Element, HtmlSelectElement, HtmlTextAreaElement, PointerEvent, WheelEvent};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Identity {
    id: String,
    project: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct Node {
    id: String,
    kind: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    #[serde(default)]
    content: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Layout {
    schema: u32,
    nodes: Vec<Node>,
    x: f64,
    y: f64,
    zoom: f64,
    #[serde(default)]
    directory: String,
}
impl Default for Layout {
    fn default() -> Self {
        Self {
            schema: 1,
            nodes: vec![],
            x: 40.,
            y: 55.,
            zoom: 1.,
            directory: String::new(),
        }
    }
}
impl Layout {
    fn validate(&self) -> bool {
        self.schema == 1
            && self.nodes.len() <= 24
            && (0.25..=2.).contains(&self.zoom)
            && self.x.is_finite()
            && self.y.is_finite()
            && self.nodes.iter().all(|n| {
                n.x.is_finite()
                    && n.y.is_finite()
                    && (240. ..=1200.).contains(&n.width)
                    && (180. ..=1200.).contains(&n.height)
                    && matches!(n.kind.as_str(), "folder" | "note" | "history" | "terminal")
            })
    }
}
struct Tab {
    identity: Identity,
    layout: Layout,
    revision: u64,
    changed: u64,
    saved: u64,
    saving: bool,
    retryable: bool,
    pending: Option<(u64, Value)>,
    editor: Option<String>,
    workspace: Value,
    files: Vec<Value>,
    terminals: Vec<Value>,
    views: Vec<Value>,
    history: Vec<Value>,
    loaded: bool,
    busy: bool,
    error: String,
}
impl Tab {
    fn new(identity: Identity) -> Self {
        Self {
            identity,
            layout: Layout::default(),
            revision: 0,
            changed: 0,
            saved: 0,
            saving: false,
            retryable: false,
            pending: None,
            editor: None,
            workspace: Value::Null,
            files: vec![],
            terminals: vec![],
            views: vec![],
            history: vec![],
            loaded: false,
            busy: false,
            error: String::new(),
        }
    }
}
struct Drag {
    pointer: i32,
    node: Option<String>,
    resize: bool,
    x: f64,
    y: f64,
    start_x: f64,
    start_y: f64,
}
#[derive(Default)]
pub struct State {
    tabs: Vec<Tab>,
    active: String,
    projects: Vec<Value>,
    loaded: bool,
    error: String,
    drag: Option<Drag>,
    ptys: std::collections::BTreeMap<String, pty::Session>,
}
pub fn tr<'a>(a: &App, en: &'a str, ko: &'a str) -> &'a str {
    if a.locale == "ko" { ko } else { en }
}
fn uuid() -> String {
    window()
        .crypto()
        .map(|c| c.random_uuid())
        .unwrap_or_default()
}
fn tab(s: &State) -> Option<&Tab> {
    s.tabs.iter().find(|t| t.identity.id == s.active)
}
fn tab_mut(s: &mut State) -> Option<&mut Tab> {
    s.tabs.iter_mut().find(|t| t.identity.id == s.active)
}
fn base(project: &str) -> String {
    format!(
        "/api/ai-sandbox/projects/{}/workspace",
        api::encode(project)
    )
}
fn endpoint(t: &Tab) -> String {
    format!(
        "{}/views/{}",
        base(&t.identity.project),
        api::encode(&t.identity.id)
    )
}
fn remember(s: &State) {
    if let Ok(Some(storage)) = window().local_storage() {
        let identities: Vec<_> = s.tabs.iter().map(|t| &t.identity).collect();
        let _ = storage.set_item(
            "firecrab.canvas.tabs",
            &json!({"tabs":identities,"active":s.active}).to_string(),
        );
    }
}
fn dirty(a: &mut App) {
    a.dirty = a
        .canvas
        .tabs
        .iter()
        .any(|t| t.changed != t.saved || t.pending.is_some());
}
pub fn page(a: &App) -> String {
    view::page(a)
}
pub fn before_render() {
    pty::binding("prepare", JsValue::UNDEFINED);
}
pub fn render(app: &Shared) {
    if app.borrow().route != "ai-canvas" {
        return;
    }
    pty::binding("prepare", JsValue::UNDEFINED);
    let focused = document().active_element();
    let focus = focused.as_ref().map(|e| e.id()).unwrap_or_default();
    let selection = focused
        .as_ref()
        .and_then(|e| e.dyn_ref::<HtmlTextAreaElement>())
        .and_then(|e| Some((e.selection_start().ok()??, e.selection_end().ok()??)));
    let html = view::page(&app.borrow());
    app.borrow_mut().rendering = true;
    set_html("root", &html);
    app.borrow_mut().rendering = false;
    pty::attach(app);
    if let Some(e) = element(&focus)
        && let Some(field) = e.dyn_ref::<HtmlTextAreaElement>()
    {
        let _ = field.focus();
        if let Some((start, end)) = selection {
            let _ = field.set_selection_range(start, end);
        }
    }
}
pub fn load(app: Shared) {
    if !app.borrow().canvas.loaded {
        let saved = window()
            .local_storage()
            .ok()
            .flatten()
            .and_then(|s| s.get_item("firecrab.canvas.tabs").ok().flatten())
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or(Value::Null);
        let mut a = app.borrow_mut();
        for item in list(&saved["tabs"]).iter().take(24) {
            if let Ok(id) = serde_json::from_value::<Identity>(item.clone())
                && !id.id.is_empty()
                && !a.canvas.tabs.iter().any(|t| t.identity.id == id.id)
            {
                a.canvas.tabs.push(Tab::new(id));
            }
        }
        if a.canvas.tabs.is_empty() {
            a.canvas.tabs.push(Tab::new(Identity {
                id: uuid(),
                project: String::new(),
            }));
        }
        a.canvas.active = if a
            .canvas
            .tabs
            .iter()
            .any(|t| t.identity.id == text(&saved, "active"))
        {
            text(&saved, "active").into()
        } else {
            a.canvas.tabs[0].identity.id.clone()
        };
        a.canvas.loaded = true;
    }
    render(&app);
    spawn_local(async move {
        let result = workspace_request("GET", "/api/ai-sandbox/projects", None).await;
        match result {
            Ok(projects) => app.borrow_mut().canvas.projects = list(&projects).to_vec(),
            Err(e) => app.borrow_mut().canvas.error = e,
        }
        render(&app);
        open_tab(app);
    });
}
// Retry only an explicit workspace lock rejection. Unknown write responses are not replayed.
async fn workspace_request(
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, String> {
    for attempt in 0..8 {
        match api::request_detailed(method, path, body, None).await {
            Ok(value) => return Ok(value),
            Err(error) if error.code.as_deref() == Some("workspace_busy") && attempt < 7 => {
                let pause = js_sys::Promise::new(&mut |resolve, reject| {
                    if let Err(error) = window()
                        .set_timeout_with_callback_and_timeout_and_arguments_0(
                            &resolve,
                            100 * (attempt + 1),
                        )
                    {
                        let _ = reject.call1(&JsValue::UNDEFINED, &error);
                    }
                });
                wasm_bindgen_futures::JsFuture::from(pause)
                    .await
                    .map_err(api::error)?;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    unreachable!()
}
fn open_tab(app: Shared) {
    let mut a = app.borrow_mut();
    let Some(t) = tab_mut(&mut a.canvas) else {
        return;
    };
    if t.identity.project.is_empty() || t.loaded || t.busy {
        return;
    }
    t.busy = true;
    let identity = t.identity.clone();
    let path = endpoint(t);
    drop(a);
    render(&app);
    spawn_local(async move {
        let result = async {
            let workspace = workspace_request("GET", &base(&identity.project), None).await?;
            let saved = match api::request_detailed("GET", &path, None, None).await {
                Ok(v) => v,
                Err(e) if e.status == Some(404) => {
                    json!({"revision":0,"document":Layout::default()})
                }
                Err(e) => return Err(e.to_string()),
            };
            let layout = serde_json::from_value::<Layout>(saved["document"].clone())
                .map_err(|_| "Saved canvas format is unsupported".to_string())?;
            if !layout.validate() {
                return Err("Saved canvas bounds are invalid".into());
            }
            Ok((workspace, saved["revision"].as_u64().unwrap_or(0), layout))
        }
        .await;
        let mut a = app.borrow_mut();
        let Some(t) = a
            .canvas
            .tabs
            .iter_mut()
            .find(|t| t.identity.id == identity.id && t.identity.project == identity.project)
        else {
            return;
        };
        t.busy = false;
        match result {
            Ok((workspace, revision, layout)) => {
                t.workspace = workspace;
                t.revision = revision;
                t.layout = layout;
                t.loaded = true;
                if revision == 0 {
                    t.changed = 1;
                }
            }
            Err(e) => t.error = e,
        };
        drop(a);
        render(&app);
        save(app.clone(), &identity.id);
        pty::catalog(app.clone());
        retained_views(app.clone());
        refresh_files(app);
    });
}
fn save(app: Shared, id: &str) {
    let mut a = app.borrow_mut();
    let Some(t) = a.canvas.tabs.iter_mut().find(|t| t.identity.id == id) else {
        return;
    };
    if !t.loaded
        || t.saving
        || t.identity.project.is_empty()
        || t.changed == t.saved && t.pending.is_none()
    {
        return;
    }
    t.saving = true;
    let path = endpoint(t);
    let identity = t.identity.clone();
    let (changed, request) = t.pending.clone().unwrap_or_else(|| {
        (
            t.changed,
            json!({"operationId":uuid(),"expectedRevision":t.revision,"document":t.layout}),
        )
    });
    t.pending = Some((changed, request.clone()));
    drop(a);
    spawn_local(async move {
        let result = api::request_detailed("PUT", &path, Some(&request), None).await;
        let mut a = app.borrow_mut();
        let Some(t) = a
            .canvas
            .tabs
            .iter_mut()
            .find(|t| t.identity.id == identity.id && t.identity.project == identity.project)
        else {
            return;
        };
        t.saving = false;
        match result {
            Ok(value) => {
                t.revision = value["revision"].as_u64().unwrap_or(t.revision);
                t.saved = changed;
                t.pending = None;
                t.retryable = false;
                t.error.clear();
            }
            Err(e) => {
                t.retryable = e.status.is_none() || e.code.as_deref() == Some("workspace_busy");
                t.error = e.to_string();
            }
        }
        dirty(&mut a);
        let message = if a.dirty {
            tr(&a, "Unsaved changes", "미저장 변경")
        } else {
            tr(&a, "Saved privately", "개인 화면 저장됨")
        };
        if let Some(e) = element("cw-save-state") {
            e.set_text_content(Some(message));
        }
        if let Some(e) = element("cw-error") {
            e.set_text_content(tab(&a.canvas).map(|t| t.error.as_str()));
        }
    });
}
fn refresh_files(app: Shared) {
    let a = app.borrow();
    let Some(t) = tab(&a.canvas) else {
        return;
    };
    if !t.loaded || text(&t.workspace, "status") != "ready" {
        return;
    }
    let identity = t.identity.clone();
    let path = format!(
        "{}/files/tree?path={}",
        base(&identity.project),
        api::encode(&t.layout.directory)
    );
    drop(a);
    spawn_local(async move {
        let result = workspace_request("GET", &path, None).await;
        let mut a = app.borrow_mut();
        let Some(t) = a
            .canvas
            .tabs
            .iter_mut()
            .find(|t| t.identity.id == identity.id)
        else {
            return;
        };
        match result {
            Ok(files) => t.files = list(&files).to_vec(),
            Err(e) => t.error = e,
        }
        drop(a);
        render(&app);
    });
}
fn changed(app: &Shared) {
    let mut a = app.borrow_mut();
    if let Some(t) = tab_mut(&mut a.canvas) {
        t.changed = t.changed.saturating_add(1);
    }
    dirty(&mut a);
}
pub fn tick(app: &Shared) {
    let ids: Vec<_> = app
        .borrow()
        .canvas
        .tabs
        .iter()
        .filter(|t| t.changed != t.saved && (t.error.is_empty() || t.retryable))
        .map(|t| t.identity.id.clone())
        .collect();
    for id in ids {
        save(app.clone(), &id);
    }
    pty::tick(app);
}
pub fn input(app: &Shared, target: &Element) -> bool {
    let Some(id) = target.get_attribute("data-cw-note") else {
        return false;
    };
    if let Some(field) = target.dyn_ref::<HtmlTextAreaElement>() {
        let mut a = app.borrow_mut();
        if let Some(t) = tab_mut(&mut a.canvas)
            && let Some(n) = t.layout.nodes.iter_mut().find(|n| n.id == id)
        {
            n.content = field.value();
        }
        drop(a);
        changed(app);
    }
    true
}
pub fn action(app: &Shared, action: &str, id: &str) -> bool {
    if !action.starts_with("cw-") {
        return false;
    }
    match action {
        "cw-views" => retained_views(app.clone()),
        "cw-reopen" => {
            let selected = element("cw-saved-view")
                .and_then(|e| e.dyn_into::<HtmlSelectElement>().ok())
                .map(|e| e.value())
                .unwrap_or_default();
            let mut a = app.borrow_mut();
            let Some(current) = tab(&a.canvas) else {
                return true;
            };
            if !current.views.iter().any(|v| text(v, "id") == selected) {
                return true;
            }
            let project = current.identity.project.clone();
            if a.canvas
                .tabs
                .iter()
                .any(|t| t.identity.id == selected && t.identity.project != project)
            {
                if let Some(t) = tab_mut(&mut a.canvas) {
                    t.error = "This saved canvas conflicts with an open project tab. Close that tab before reopening.".into();
                }
                drop(a);
                render(app);
                return true;
            }
            if !a.canvas.tabs.iter().any(|t| t.identity.id == selected) {
                if a.canvas.tabs.len() >= 24 {
                    return true;
                }
                a.canvas.tabs.push(Tab::new(Identity {
                    id: selected.clone(),
                    project,
                }));
            }
            a.canvas.active = selected;
            remember(&a.canvas);
            drop(a);
            render(app);
            open_tab(app.clone());
        }
        "cw-retry" => open_tab(app.clone()),
        "cw-fork" => {
            let mut a = app.borrow_mut();
            if a.canvas.tabs.len() >= 24 {
                return true;
            }
            if let Some(t) = tab(&a.canvas).filter(|t| t.loaded) {
                let mut copied = Tab::new(Identity {
                    id: uuid(),
                    project: t.identity.project.clone(),
                });
                copied.layout = t.layout.clone();
                copied.workspace = t.workspace.clone();
                copied.files = t.files.clone();
                copied.loaded = true;
                copied.changed = 1;
                a.canvas.active = copied.identity.id.clone();
                a.canvas.tabs.push(copied);
                remember(&a.canvas);
                dirty(&mut a);
            }
            drop(a);
            render(app);
        }
        "cw-new" => {
            let mut a = app.borrow_mut();
            if a.canvas.tabs.len() >= 24 {
                return true;
            }
            let id = uuid();
            a.canvas.tabs.push(Tab::new(Identity {
                id: id.clone(),
                project: String::new(),
            }));
            a.canvas.active = id;
            remember(&a.canvas);
            drop(a);
            render(app);
        }
        "cw-tab" => {
            app.borrow_mut().canvas.active = id.into();
            remember(&app.borrow().canvas);
            render(app);
            open_tab(app.clone());
        }
        "cw-close" => {
            let mut a = app.borrow_mut();
            let Some(t) = a.canvas.tabs.iter().find(|t| t.identity.id == id) else {
                return true;
            };
            if t.changed != t.saved || t.pending.is_some() {
                a.canvas.error = "Save this tab before closing it".into();
                drop(a);
                render(app);
                return true;
            }
            a.canvas.tabs.retain(|t| t.identity.id != id);
            if a.canvas.active == id {
                a.canvas.active = a
                    .canvas
                    .tabs
                    .first()
                    .map(|t| t.identity.id.clone())
                    .unwrap_or_default();
            }
            remember(&a.canvas);
            drop(a);
            if app.borrow().canvas.tabs.is_empty() {
                action_new(app);
            } else {
                render(app);
                open_tab(app.clone());
            }
        }
        "cw-open" => {
            let project = element("cw-project-select")
                .and_then(|e| e.dyn_into::<HtmlSelectElement>().ok())
                .map(|e| e.value())
                .unwrap_or_default();
            if project.is_empty() {
                return true;
            }
            let mut a = app.borrow_mut();
            if let Some(t) = tab_mut(&mut a.canvas) {
                t.identity.project = project;
                t.loaded = false;
                t.error.clear();
            }
            remember(&a.canvas);
            drop(a);
            open_tab(app.clone());
        }
        "cw-save" => {
            let id = app.borrow().canvas.active.clone();
            if let Some(t) = tab_mut(&mut app.borrow_mut().canvas) {
                t.error.clear();
            }
            save(app.clone(), &id);
        }
        "cw-add" => {
            if id == "terminal" {
                pty::create(app);
                return true;
            }
            let mut a = app.borrow_mut();
            if let Some(t) = tab_mut(&mut a.canvas)
                && t.loaded
                && t.layout.nodes.len() < 24
                && (id != "note" || t.layout.nodes.iter().filter(|n| n.kind == "note").count() < 6)
            {
                let n = t.layout.nodes.len() as f64;
                t.layout.nodes.push(Node {
                    id: uuid(),
                    kind: id.into(),
                    x: 40. + (n % 2.) * 640.,
                    y: 40. + (n / 2.).floor() * 420.,
                    width: 380.,
                    height: 330.,
                    content: String::new(),
                });
            }
            drop(a);
            changed(app);
            render(app);
            if id == "history" {
                history(app.clone(), None);
            }
        }
        "cw-remove" => {
            if let Some(t) = tab_mut(&mut app.borrow_mut().canvas) {
                t.layout.nodes.retain(|n| n.id != id);
            }
            changed(app);
            render(app);
        }
        "cw-directory" => {
            if let Some(t) = tab_mut(&mut app.borrow_mut().canvas) {
                t.layout.directory = id.into();
            }
            changed(app);
            refresh_files(app.clone());
        }
        "cw-refresh" => refresh_files(app.clone()),
        "cw-terminals" => pty::catalog(app.clone()),
        "cw-attach" => pty::attach_existing(app),
        "cw-history" => history(app.clone(), (!id.is_empty()).then_some(id.to_string())),
        "cw-editor" => open_editor(app.clone(), (!id.is_empty()).then_some(id.to_string())),
        "cw-reconcile" => {
            let a = app.borrow();
            let Some(t) = tab(&a.canvas) else {
                return true;
            };
            let identity = t.identity.clone();
            drop(a);
            let app = app.clone();
            spawn_local(async move {
                let result = workspace_request(
                    "POST",
                    &format!("{}/reconcile", base(&identity.project)),
                    Some(&json!({})),
                )
                .await;
                if let Some(t) = app
                    .borrow_mut()
                    .canvas
                    .tabs
                    .iter_mut()
                    .find(|t| t.identity.id == identity.id)
                {
                    match result {
                        Ok(w) => {
                            t.workspace = w;
                            t.error.clear();
                        }
                        Err(e) => t.error = e,
                    }
                }
                refresh_files(app.clone());
                render(&app);
            });
        }
        "cw-back" => {
            if let Some(t) = tab_mut(&mut app.borrow_mut().canvas) {
                t.editor = None;
            }
            render(app);
        }
        "cw-reset" => {
            if let Some(t) = tab_mut(&mut app.borrow_mut().canvas) {
                t.layout.x = 40.;
                t.layout.y = 55.;
                t.layout.zoom = 1.;
            }
            changed(app);
            render(app);
        }
        _ => return pty::action(app, action, id),
    }
    true
}
fn action_new(app: &Shared) {
    action(app, "cw-new", "");
}
fn retained_views(app: Shared) {
    let a = app.borrow();
    let Some(t) = tab(&a.canvas) else {
        return;
    };
    let identity = t.identity.clone();
    drop(a);
    spawn_local(async move {
        let result =
            workspace_request("GET", &format!("{}/views", base(&identity.project)), None).await;
        if let Some(t) = app
            .borrow_mut()
            .canvas
            .tabs
            .iter_mut()
            .find(|t| t.identity.id == identity.id && t.identity.project == identity.project)
        {
            match result {
                Ok(rows) => t.views = list(&rows).to_vec(),
                Err(error) => t.error = error,
            }
        }
        render(&app);
    });
}
fn history(app: Shared, path: Option<String>) {
    let a = app.borrow();
    let Some(t) = tab(&a.canvas) else {
        return;
    };
    let identity = t.identity.clone();
    drop(a);
    spawn_local(async move {
        let query = path
            .map(|p| format!("?path={}", api::encode(&p)))
            .unwrap_or_default();
        let result = workspace_request(
            "GET",
            &format!("{}/files/history{query}", base(&identity.project)),
            None,
        )
        .await;
        if let Some(t) = app
            .borrow_mut()
            .canvas
            .tabs
            .iter_mut()
            .find(|t| t.identity.id == identity.id)
        {
            match result {
                Ok(rows) => t.history = list(&rows).to_vec(),
                Err(e) => t.error = e,
            }
        }
        render(&app);
    });
}
fn open_editor(app: Shared, file: Option<String>) {
    let mut a = app.borrow_mut();
    let Some(t) = tab_mut(&mut a.canvas) else {
        return;
    };
    if t.busy {
        return;
    }
    t.busy = true;
    t.error.clear();
    let identity = t.identity.clone();
    let folder = t.layout.directory.clone();
    drop(a);
    render(&app);
    spawn_local(async move {
        let result=workspace_request("PUT",&format!("{}/editors/{}",base(&identity.project),identity.id),Some(&json!({"viewId":identity.id,"workingDirectory":folder,"file":file,"requestId":uuid()}))).await;
        if let Some(t) = app
            .borrow_mut()
            .canvas
            .tabs
            .iter_mut()
            .find(|t| t.identity.id == identity.id)
        {
            t.busy = false;
        }
        match result {
            Ok(editor) => {
                let Some(host) = editor["host"].as_str() else {
                    return;
                };
                // A guest editor must always use a distinct, project-scoped origin.
                if !host.ends_with(".editor.localhost") || host.contains(['/', ':']) {
                    return;
                }
                let protocol = window()
                    .location()
                    .protocol()
                    .unwrap_or_else(|_| "http:".into());
                let port = window().location().port().unwrap_or_default();
                let uri = format!(
                    "{protocol}//{host}{}/?folder={}",
                    if port.is_empty() {
                        String::new()
                    } else {
                        format!(":{port}")
                    },
                    api::encode(&format!(
                        "firecrab://{}/{}",
                        text(&editor, "id"),
                        text(&editor, "workingDirectory")
                    ))
                );
                if let Some(t) = app
                    .borrow_mut()
                    .canvas
                    .tabs
                    .iter_mut()
                    .find(|t| t.identity.id == identity.id)
                {
                    t.editor = Some(uri);
                }
                render(&app);
            }
            Err(e) => {
                if let Some(t) = app
                    .borrow_mut()
                    .canvas
                    .tabs
                    .iter_mut()
                    .find(|t| t.identity.id == identity.id)
                {
                    t.error = e;
                }
                render(&app);
            }
        }
    });
}
pub fn install(app: &Shared) -> Result<(), JsValue> {
    pty::install(app)?;
    for kind in ["pointerdown", "pointermove", "pointerup", "pointercancel"] {
        let app = app.clone();
        let name = kind.to_string();
        let callback = Closure::<dyn FnMut(PointerEvent)>::new(move |e: PointerEvent| {
            if app.borrow().route != "ai-canvas" {
                return;
            }
            if name == "pointerdown" {
                let Some(target) = e.target().and_then(|e| e.dyn_into::<Element>().ok()) else {
                    return;
                };
                let node = target
                    .closest("[data-cw-drag],[data-cw-resize]")
                    .ok()
                    .flatten();
                let background = target.id() == "cw-board" || target.id() == "cw-world";
                if node.is_none() && !background {
                    return;
                }
                if e.button() != 0 && e.button() != 1 {
                    return;
                }
                let mut a = app.borrow_mut();
                let Some(t) = tab(&a.canvas) else {
                    return;
                };
                let node_id = node.as_ref().and_then(|n| n.get_attribute("data-id"));
                let resize = node
                    .as_ref()
                    .is_some_and(|n| n.has_attribute("data-cw-resize"));
                let Some((x, y)) = node_id
                    .as_ref()
                    .map(|id| {
                        t.layout.nodes.iter().find(|n| &n.id == id).map(|n| {
                            if resize {
                                (n.width, n.height)
                            } else {
                                (n.x, n.y)
                            }
                        })
                    })
                    .unwrap_or(Some((t.layout.x, t.layout.y)))
                else {
                    return;
                };
                a.canvas.drag = Some(Drag {
                    pointer: e.pointer_id(),
                    node: node_id,
                    resize,
                    x,
                    y,
                    start_x: f64::from(e.client_x()),
                    start_y: f64::from(e.client_y()),
                });
                if let Some(board) = element("cw-board") {
                    let _ = board.set_pointer_capture(e.pointer_id());
                }
                e.prevent_default();
            } else if name == "pointermove" {
                let mut a = app.borrow_mut();
                let Some(d) = a
                    .canvas
                    .drag
                    .as_ref()
                    .filter(|d| d.pointer == e.pointer_id())
                else {
                    return;
                };
                let (id, resize, x, y, dx, dy) = (
                    d.node.clone(),
                    d.resize,
                    d.x,
                    d.y,
                    f64::from(e.client_x()) - d.start_x,
                    f64::from(e.client_y()) - d.start_y,
                );
                let Some(t) = tab_mut(&mut a.canvas) else {
                    return;
                };
                if let Some(id) = id {
                    if let Some(n) = t.layout.nodes.iter_mut().find(|n| n.id == id) {
                        if resize {
                            n.width = (x + dx / t.layout.zoom).clamp(240., 1200.);
                            n.height = (y + dy / t.layout.zoom).clamp(180., 1200.);
                        } else {
                            n.x = x + dx / t.layout.zoom;
                            n.y = y + dy / t.layout.zoom;
                        }
                        if let Some(card) = element(&format!("cw-node-{id}")) {
                            let _ = card.set_attribute("style", &view::position(n));
                        }
                    }
                } else {
                    t.layout.x = x + dx;
                    t.layout.y = y + dy;
                    if let Some(world) = element("cw-world") {
                        let _ = world.set_attribute("style", &view::transform(&t.layout));
                    }
                }
            } else {
                let finished = app.borrow_mut().canvas.drag.take().is_some();
                if finished {
                    changed(&app);
                    let id = app.borrow().canvas.active.clone();
                    save(app.clone(), &id);
                }
            }
        });
        document().add_event_listener_with_callback(kind, callback.as_ref().unchecked_ref())?;
        callback.forget();
    }
    let app = app.clone();
    let callback = Closure::<dyn FnMut(WheelEvent)>::new(move |e: WheelEvent| {
        if !e.ctrl_key() && !e.meta_key() {
            return;
        }
        let Some(target) = e.target().and_then(|n| n.dyn_into::<Element>().ok()) else {
            return;
        };
        if target.closest("#cw-board").ok().flatten().is_none() {
            return;
        }
        let mut a = app.borrow_mut();
        let Some(t) = tab_mut(&mut a.canvas) else {
            return;
        };
        t.layout.zoom = (t.layout.zoom * (-e.delta_y() * 0.002).exp()).clamp(0.25, 2.);
        if let Some(world) = element("cw-world") {
            let _ = world.set_attribute("style", &view::transform(&t.layout));
        }
        drop(a);
        changed(&app);
        e.prevent_default();
    });
    let options = web_sys::AddEventListenerOptions::new();
    options.set_passive(false);
    document().add_event_listener_with_callback_and_add_event_listener_options(
        "wheel",
        callback.as_ref().unchecked_ref(),
        &options,
    )?;
    callback.forget();
    Ok(())
}
