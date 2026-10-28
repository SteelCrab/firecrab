pub(crate) mod edit;
mod view;
use crate::{
    App, Shared, api, element,
    forms::{Field, Form},
    model::{escape, list, text},
    render_content, render_editor, render_messages, window,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{
    Element, FormData, HtmlFormElement, HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement,
};

#[derive(Default)]
pub struct State {
    selected: String,
    table: String,
    tables: Value,
    page: Value,
    offset: u64,
    visible_offset: u64,
    epoch: u64,
    loading: bool,
    aux_loading: bool,
    rows_error: String,
    tables_error: String,
    notice: String,
    advanced: bool,
    maintenance: bool,
    aux: BTreeMap<String, Value>,
    failures: BTreeMap<String, Value>,
    pub(crate) row: Option<Row>,
}
pub(crate) struct Row {
    pub(crate) uncertain: bool,
    operation: String,
    request: String,
    original: Value,
    values: BTreeMap<String, String>,
    modes: BTreeMap<String, String>,
}
fn tr(en: &'static str, ko: &'static str) -> &'static str {
    if crate::i18n::t("Name") == "이름" {
        ko
    } else {
        en
    }
}
fn uuid() -> Result<String, String> {
    let mut b = [0u8; 16];
    window()
        .crypto()
        .map_err(api::error)?
        .get_random_values_with_u8_array(&mut b)
        .map_err(api::error)?;
    b[6] = (b[6] & 15) | 64;
    b[8] = (b[8] & 63) | 128;
    let h: String = b.iter().map(|v| format!("{v:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    ))
}
fn selected(app: &App) -> Value {
    list(&app.resource)
        .iter()
        .find(|v| text(v, "id") == app.databases.selected)
        .cloned()
        .unwrap_or(Value::Null)
}
fn table(app: &App) -> Option<(String, String)> {
    serde_json::from_str(&app.databases.table).ok()
}
fn field_value(id: &str) -> String {
    element(id)
        .and_then(|e| {
            if let Some(i) = e.dyn_ref::<HtmlInputElement>() {
                Some(i.value())
            } else if let Some(i) = e.dyn_ref::<HtmlSelectElement>() {
                Some(i.value())
            } else {
                e.dyn_ref::<HtmlTextAreaElement>().map(|i| i.value())
            }
        })
        .unwrap_or_default()
}
fn valid(app: &App, generation: u64, epoch: u64) -> bool {
    app.route == "pipe/database" && app.generation == generation && app.databases.epoch == epoch
}
fn redraw(app: &Shared) {
    if app.borrow().route == "pipe/database" {
        render_content(app);
    }
}
pub fn content(app: &App) -> String {
    view::content(app)
}
pub fn tick(app: &Shared) {
    let s = app.borrow();
    let load = s.route == "pipe/database"
        && !s.databases.selected.is_empty()
        && (s.databases.advanced || s.databases.maintenance)
        && s.form.is_none()
        && !s.busy
        && !s.databases.aux_loading;
    drop(s);
    if load {
        load_aux(app.clone());
    }
}
pub fn after_refresh(app: &Shared) {
    let mut s = app.borrow_mut();
    if s.route != "pipe/database"
        || s.databases.selected.is_empty()
        || !s.connection_error.is_empty()
    {
        return;
    }
    let db = selected(&s);
    if db.is_null() {
        s.databases = State {
            epoch: s.databases.epoch + 1,
            ..State::default()
        };
        return;
    }
    let reload = s.form.is_none()
        && !s.databases.loading
        && !s.databases.page.is_null()
        && (s.databases.page.get("definition").is_some() != (db["writeEnabled"] == true));
    drop(s);
    if reload {
        load_page(app.clone());
    }
}
pub fn after_mutation(app: &Shared) {
    let mut s = app.borrow_mut();
    if s.route != "pipe/database" {
        return;
    }
    s.databases.epoch += 1;
    s.databases.loading = false;
    s.databases.aux_loading = false;
    s.databases.failures.clear();
    let table = !s.databases.table.is_empty();
    let aux = s.databases.advanced || s.databases.maintenance;
    drop(s);
    if table {
        load_page(app.clone());
    }
    if aux {
        load_aux(app.clone());
    }
}
fn load_tables(app: Shared) {
    let mut s = app.borrow_mut();
    s.databases.epoch += 1;
    s.databases.aux_loading = false;
    s.databases.loading = true;
    s.databases.tables = Value::Null;
    s.databases.page = Value::Null;
    s.databases.tables_error.clear();
    let (g, e, id) = (
        s.generation,
        s.databases.epoch,
        s.databases.selected.clone(),
    );
    drop(s);
    redraw(&app);
    spawn_local(async move {
        let result = api::request(
            "GET",
            &format!("/api/pipe/databases/{}/tables", api::encode(&id)),
            None,
        )
        .await;
        let mut s = app.borrow_mut();
        if !valid(&s, g, e) {
            return;
        }
        s.databases.loading = false;
        match result {
            Ok(v) => s.databases.tables = v,
            Err(err) => s.databases.tables_error = err,
        };
        drop(s);
        redraw(&app);
    });
}
fn load_page(app: Shared) {
    let mut s = app.borrow_mut();
    let Some((schema, table)) = table(&s) else {
        return;
    };
    s.databases.epoch += 1;
    s.databases.aux_loading = false;
    s.databases.loading = true;
    s.databases.page = Value::Null;
    s.databases.rows_error.clear();
    let (g, e, id, offset, write) = (
        s.generation,
        s.databases.epoch,
        s.databases.selected.clone(),
        s.databases.offset,
        selected(&s)["writeEnabled"] == true,
    );
    drop(s);
    redraw(&app);
    spawn_local(async move {
        let result = api::request(
            "POST",
            &format!(
                "/api/pipe/databases/{}/{}",
                api::encode(&id),
                if write { "rows" } else { "preview" }
            ),
            Some(&json!({"schema":schema,"table":table,"offset":offset,"limit":50})),
        )
        .await;
        let mut s = app.borrow_mut();
        if !valid(&s, g, e) {
            return;
        }
        s.databases.loading = false;
        match result {
            Ok(v) => {
                s.databases.page = v;
                s.databases.visible_offset = offset;
            }
            Err(err) => {
                s.databases.rows_error = err;
                s.databases.offset = s.databases.visible_offset;
            }
        };
        drop(s);
        redraw(&app);
    });
}
fn load_aux(app: Shared) {
    let mut s = app.borrow_mut();
    if s.databases.aux_loading || s.databases.selected.is_empty() {
        return;
    }
    s.databases.aux_loading = true;
    let (g, e, id, a, m) = (
        s.generation,
        s.databases.epoch,
        s.databases.selected.clone(),
        s.databases.advanced,
        s.databases.maintenance,
    );
    drop(s);
    spawn_local(async move {
        let mut paths = Vec::new();
        if a {
            paths.extend([
                ("services", "/api/micro-services".into()),
                ("bindings", format!("/api/pipe/databases/{id}/bindings")),
                ("triggers", format!("/api/pipe/databases/{id}/triggers")),
                ("rabbitmq", "/api/pipe/database-rabbitmq".into()),
                ("managed", "/api/pipe/database-rabbitmq/managed".into()),
            ]);
        }
        if m {
            paths.extend([
                ("jobs", format!("/api/pipe/databases/{id}/jobs")),
                ("tools", "/api/pipe/database-maintenance/tools".into()),
            ]);
        }
        let mut changed = false;
        let mut errors = Vec::new();
        for (key, path) in paths {
            let result = api::request("GET", &path, None).await;
            let mut s = app.borrow_mut();
            if !valid(&s, g, e) {
                return;
            }
            match result {
                Ok(value) => {
                    changed |= s.databases.aux.get(key) != Some(&value);
                    s.databases.aux.insert(key.into(), value);
                }
                Err(err) => errors.push(err),
            }
        }
        let mut s = app.borrow_mut();
        if !valid(&s, g, e) {
            return;
        }
        s.databases.aux_loading = false;
        if !errors.is_empty() {
            s.error = errors.join(" · ");
        }
        let draw = (changed || !s.error.is_empty()) && s.form.is_none() && !s.dirty;
        drop(s);
        if draw {
            redraw(&app);
        }
    });
}
fn perform(app: Shared, method: &str, path: String, body: Option<Value>, confirm: Option<&str>) {
    if confirm.is_some_and(|q| !window().confirm_with_message(q).unwrap_or(false)) {
        return;
    }
    crate::mutate(app, method.into(), path, body, None);
}
fn open(app: &Shared, form: Form) {
    if !crate::allow_discard(app) {
        return;
    }
    let mut s = app.borrow_mut();
    s.form = Some(form);
    s.panel_open = false;
    s.dirty = false;
    s.error.clear();
    s.field_errors.clear();
    s.databases.notice.clear();
    drop(s);
    render_editor(app);
}
fn connection_form(app: &Shared, id: &str) {
    let mut form = crate::forms::resource_form("database", &Value::Null, "").unwrap();
    if !id.is_empty() {
        let s = app.borrow();
        let db = list(&s.resource)
            .iter()
            .find(|v| text(v, "id") == id)
            .cloned()
            .unwrap_or(Value::Null);
        form.title = "Update connection credentials".into();
        form.method = "PUT";
        form.path = format!("/api/pipe/databases/{}", api::encode(id));
        for field in &mut form.fields {
            if field.key != "uri" && field.key != "caCertificate" {
                field.value = db[field.key]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| db[field.key].to_string());
            }
        }
    }
    if form
        .fields
        .iter()
        .any(|f| f.key == "provider" && f.value != "local")
        && let Some(f) = form.fields.iter_mut().find(|f| f.key == "tlsMode")
    {
        f.options.retain(|(v, _)| v != "disable");
    }
    open(app, form);
}
pub fn dispatch(app: &Shared, action: &str, id: &str, button: &Element) -> bool {
    if app.borrow().route != "pipe/database" {
        return false;
    }
    match action {
        "create" if id == "database" => connection_form(app, ""),
        "cancel" => {
            if app
                .borrow()
                .databases
                .row
                .as_ref()
                .is_some_and(|r| r.uncertain)
            {
                return true;
            }
            let mut s = app.borrow_mut();
            s.form = None;
            s.databases.row = None;
            s.dirty = false;
            s.error.clear();
            s.field_errors.clear();
            s.databases.notice.clear();
            drop(s);
            redraw(app);
        }
        "db-browse" => {
            if !crate::allow_discard(app) {
                return true;
            }
            let mut s = app.borrow_mut();
            s.databases = State {
                epoch: s.databases.epoch + 1,
                selected: id.into(),
                ..State::default()
            };
            s.form = None;
            s.dirty = false;
            drop(s);
            load_tables(app.clone());
        }
        "db-close" => {
            if !crate::allow_discard(app) {
                return true;
            }
            {
                let mut s = app.borrow_mut();
                s.databases = State {
                    epoch: s.databases.epoch + 1,
                    ..State::default()
                };
            }
            redraw(app);
        }
        "db-refresh" => load_page(app.clone()),
        "db-tables" => load_tables(app.clone()),
        "db-retry-list" => crate::refresh(app.clone()),
        "db-prev" | "db-next" => {
            let mut s = app.borrow_mut();
            s.databases.offset = if action == "db-next" {
                s.databases.offset + 50
            } else {
                s.databases.offset.saturating_sub(50)
            };
            drop(s);
            load_page(app.clone());
        }
        "db-edit" => connection_form(app, id),
        "db-menu" => {
            let s = app.borrow();
            let db = list(&s.resource)
                .iter()
                .find(|v| text(v, "id") == id)
                .cloned()
                .unwrap_or(Value::Null);
            let rect = button.get_bounding_client_rect();
            let label = if db["writeEnabled"] == true {
                tr("Disable writes", "쓰기 끄기")
            } else {
                tr("Enable writes", "쓰기 사용")
            };
            crate::set_html(
                "menu-root",
                &format!(
                    "<div role=\"menu\" class=\"menu\" style=\"position:fixed;top:{}px;right:{}px\"><button role=\"menuitem\" data-action=\"db-edit\" data-id=\"{}\">{}</button><button role=\"menuitem\" data-action=\"db-access\" data-id=\"{}\">{}</button><button role=\"menuitem\" data-action=\"db-remove\" data-id=\"{}\">{}</button></div>",
                    rect.bottom(),
                    (window()
                        .inner_width()
                        .ok()
                        .and_then(|v| v.as_f64())
                        .unwrap_or(1440.0)
                        - rect.right())
                    .max(8.0),
                    escape(id),
                    tr("Update connection credentials", "연결 정보 변경"),
                    escape(id),
                    label,
                    escape(id),
                    escape(&format!(
                        "{} {}",
                        tr("Remove connection", "연결 제거"),
                        text(&db, "name")
                    ))
                ),
            );
        }
        "db-remove" => perform(
            app.clone(),
            "DELETE",
            format!("/api/pipe/databases/{}", api::encode(id)),
            None,
            Some(tr(
                "Remove this saved connection? The source database and its data are kept.",
                "저장한 연결을 제거하는가? 원본 DB와 데이터는 유지한다.",
            )),
        ),
        "db-access" => {
            let s = app.borrow();
            let enabled = list(&s.resource)
                .iter()
                .find(|v| text(v, "id") == id)
                .is_some_and(|v| v["writeEnabled"] == true);
            drop(s);
            perform(
                app.clone(),
                "PUT",
                format!("/api/pipe/databases/{}/access", api::encode(id)),
                Some(json!({"writeEnabled":!enabled})),
                Some(tr(
                    "Change operator write access for this database?",
                    "이 DB의 쓰기 권한을 변경하는가?",
                )),
            );
        }
        "db-show-uri" => {
            if let Some(input) =
                element("field-uri").and_then(|el| el.dyn_into::<HtmlInputElement>().ok())
            {
                let show = input.type_() == "password";
                input.set_type(if show { "text" } else { "password" });
                button.set_text_content(Some(if show {
                    tr("Hide URI", "URI 숨기기")
                } else {
                    tr("Show URI", "URI 표시")
                }));
                let _ =
                    button.set_attribute("aria-label", if show { "Hide URI" } else { "Show URI" });
            }
        }
        "database-test" => edit::test_connection(app.clone()),
        "db-services" | "db-maintenance" => {
            let mut s = app.borrow_mut();
            if action == "db-services" {
                s.databases.advanced = !s.databases.advanced;
            } else {
                s.databases.maintenance = !s.databases.maintenance;
            }
            drop(s);
            redraw(app);
            load_aux(app.clone());
        }
        "db-prepare" => perform(
            app.clone(),
            "POST",
            "/api/pipe/database-rabbitmq/managed".into(),
            Some(json!({"development":false})),
            None,
        ),
        "db-binding" | "db-trigger" | "db-external" | "db-backup" | "db-migration"
        | "db-restore" => edit::open_operation(app, action),
        "db-unbind" => perform(
            app.clone(),
            "DELETE",
            format!("/api/pipe/database-bindings/{}", api::encode(id)),
            None,
            Some(tr(
                "Revoke this service account? Stop its service first.",
                "이 서비스 계정을 회수하는가? 먼저 서비스를 중지한다.",
            )),
        ),
        "db-delete-backup" => perform(
            app.clone(),
            "DELETE",
            format!("/api/pipe/database-jobs/{}/backup", api::encode(id)),
            None,
            Some(tr(
                "Remove this retained backup?",
                "보관한 백업을 제거하는가?",
            )),
        ),
        "db-pause" => {
            let s = app.borrow();
            let paused = list(s.databases.aux.get("triggers").unwrap_or(&Value::Null))
                .iter()
                .find(|t| text(t, "id") == id)
                .is_some_and(|t| t["paused"] == true);
            drop(s);
            perform(
                app.clone(),
                "PUT",
                format!("/api/pipe/database-triggers/{}", api::encode(id)),
                Some(json!({"paused":!paused})),
                None,
            );
        }
        "db-detach" => perform(
            app.clone(),
            "POST",
            format!("/api/pipe/database-triggers/{}/detach", api::encode(id)),
            None,
            Some(tr(
                "Stop capturing new changes? Retained events continue to drain.",
                "새 변경 캡처를 중단하는가? 보관한 이벤트는 계속 전달한다.",
            )),
        ),
        "db-delete-trigger" => perform(
            app.clone(),
            "DELETE",
            format!("/api/pipe/database-triggers/{}", api::encode(id)),
            None,
            Some(tr(
                "Remove this drained trigger and its queues?",
                "전달을 마친 트리거와 큐를 제거하는가?",
            )),
        ),
        "db-failures" => edit::failures(app.clone(), id.into()),
        "db-replay" | "db-skip" => {
            if let Ok((trigger, event)) = serde_json::from_str::<(String, String)>(id) {
                perform(
                    app.clone(),
                    "POST",
                    format!(
                        "/api/pipe/database-triggers/{}/{}/{}",
                        api::encode(&trigger),
                        if action == "db-replay" {
                            "replay"
                        } else {
                            "acknowledge"
                        },
                        api::encode(event.rsplit(':').next().unwrap_or(&event))
                    ),
                    if action == "db-skip" {
                        Some(json!({"confirmEventId":event}))
                    } else {
                        None
                    },
                    if action == "db-skip" {
                        Some(tr(
                            "Skip this failed event? Its business effect will not run.",
                            "실패한 이벤트를 건너뛰는가? 해당 업무 처리를 실행하지 않는다.",
                        ))
                    } else {
                        None
                    },
                );
            }
        }
        "db-insert" | "db-update" | "db-delete-row" => edit::open_row(app, action, id),
        "db-retry-row" => edit::save_row(app.clone()),
        _ => return false,
    }
    true
}
pub fn change(app: &Shared, target: &Element) -> bool {
    if app.borrow().route != "pipe/database" {
        return false;
    }
    if matches!(target.id().as_str(), "field-development" | "field-tls")
        && app
            .borrow()
            .form
            .as_ref()
            .is_some_and(|f| f.title == "Use existing RabbitMQ")
    {
        let development = element("field-development")
            .and_then(|e| e.dyn_into::<HtmlInputElement>().ok())
            .is_some_and(|i| i.checked());
        if let Some(tls) = element("field-tls").and_then(|e| e.dyn_into::<HtmlInputElement>().ok())
        {
            if !development {
                tls.set_checked(true);
            }
            tls.set_disabled(!development);
        }
        crate::advanced::capture_form(app);
        app.borrow_mut().dirty = true;
        return true;
    }
    if target.id() == "db-table" {
        if !crate::allow_discard(app) {
            return true;
        }
        let mut s = app.borrow_mut();
        s.databases.table = target.clone().unchecked_into::<HtmlSelectElement>().value();
        s.databases.offset = 0;
        drop(s);
        load_page(app.clone());
        return true;
    }
    if target.id() == "field-provider"
        && app.borrow().form.as_ref().is_some_and(|f| {
            f.title == "Connect database" || f.title == "Update connection credentials"
        })
    {
        crate::advanced::capture_form(app);
        let local = field_value("field-provider") == "local";
        if let Some(select) = element("field-tlsMode") {
            select.set_inner_html(&format!("<option value=\"\">{}</option>{}<option value=\"require\">{}</option><option value=\"verify_full\">{}</option>",tr("Automatic","자동"),if local{format!("<option value=\"disable\">{}</option>",tr("Disabled","사용 안 함"))}else{String::new()},tr("Encrypted","암호화"),tr("Verified","검증")));
            select.unchecked_into::<HtmlSelectElement>().set_value("");
        }
        {
            let mut s = app.borrow_mut();
            s.databases.notice.clear();
            if let Some(form) = s.form.as_mut()
                && let Some(f) = form.fields.iter_mut().find(|f| f.key == "tlsMode")
            {
                f.value.clear();
                f.options.retain(|(v, _)| v != "disable");
                if local {
                    f.options.insert(0, ("disable".into(), "Disabled".into()));
                }
            }
        }
        return true;
    }
    false
}
pub fn input(app: &Shared, target: &Element) -> bool {
    if app.borrow().route != "pipe/database" {
        return false;
    }
    if target.closest("#db-row-editor").ok().flatten().is_some() {
        edit::capture_row(app);
        app.borrow_mut().dirty = true;
        return true;
    }
    if target.closest("#editor").ok().flatten().is_some() {
        if app.borrow().form.as_ref().is_some_and(|f| {
            matches!(
                f.title.as_str(),
                "Connect database" | "Update connection credentials"
            )
        }) {
            let disabled = app.borrow().busy
                || field_value("field-name").trim().is_empty()
                || field_value("field-uri").trim().is_empty();
            if let Some(form) = element("editor") {
                for sel in ["button[type=submit]", "[data-action=database-test]"] {
                    if let Ok(Some(b)) = form.query_selector(sel) {
                        if disabled {
                            let _ = b.set_attribute("disabled", "");
                        } else {
                            let _ = b.remove_attribute("disabled");
                        }
                    }
                }
            }
        }
        app.borrow_mut().databases.notice.clear();
        if let Some(node) = element("db-notice") {
            node.remove();
        }
        return false;
    }
    false
}
pub fn submit(app: &Shared, form: &HtmlFormElement) -> bool {
    if form.id() == "db-row-editor" {
        edit::capture_row(app);
        edit::save_row(app.clone());
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_connection_form_shows_its_heading_once() {
        let app = App {
            route: "pipe/database".into(),
            form: crate::forms::resource_form("database", &Value::Null, ""),
            ..Default::default()
        };
        let html = view::content(&app);
        assert_eq!(
            html.matches("<h2>Connect database</h2>").count(),
            1,
            "{html}"
        );
    }
    #[test]
    fn connection_payload_omits_automatic_tls_and_absent_ca() {
        let form = crate::forms::resource_form("database", &Value::Null, "").unwrap();
        let values = BTreeMap::from([
            ("name".into(), " App ".into()),
            ("provider".into(), "rds".into()),
            ("uri".into(), " postgres://example/db ".into()),
            ("tlsMode".into(), "".into()),
            ("caCertificate".into(), " \n".into()),
            ("writeEnabled".into(), "true".into()),
        ]);
        let body = form.payload(&values).unwrap();
        assert_eq!(body["name"], "App");
        assert_eq!(body["writeEnabled"], true);
        assert!(body.get("tlsMode").is_none());
        assert!(body.get("caCertificate").is_none());
    }
    #[test]
    fn row_payload_preserves_text_precision_null_and_defaults() {
        let row = Row {
            uncertain: false,
            operation: "insert".into(),
            request: "retry-id".into(),
            original: Value::Null,
            values: BTreeMap::from([
                ("id".into(), "9007199254740993123".into()),
                ("empty".into(), "".into()),
                ("nullable".into(), "ignored".into()),
                ("defaulted".into(), "ignored".into()),
            ]),
            modes: BTreeMap::from([
                ("id".into(), "value".into()),
                ("empty".into(), "value".into()),
                ("nullable".into(), "null".into()),
                ("defaulted".into(), "default".into()),
            ]),
        };
        let body = edit::row_payload(&row, "schema.with.dots", "table");
        assert_eq!(body["requestId"], "retry-id");
        assert_eq!(body["schema"], "schema.with.dots");
        assert_eq!(body["values"]["id"], "9007199254740993123");
        assert_eq!(body["values"]["empty"], "");
        assert!(body["values"]["nullable"].is_null());
        assert!(body["values"].get("defaulted").is_none());
        assert_eq!(body, edit::row_payload(&row, "schema.with.dots", "table"));
    }
    #[test]
    fn event_table_selection_preserves_identifiers_without_splitting_dots() {
        let mut form = Form::new(
            "Enable database events",
            "/events".into(),
            "POST",
            vec![Field::new("tableKey", "Table", "text", "")],
            "pipe/database",
        );
        form.base = json!({"requestId":"stable"});
        let values = BTreeMap::from([(
            "tableKey".into(),
            json!(["schema.with.dots", "한글.table"]).to_string(),
        )]);
        let body = form.payload(&values).unwrap();
        assert_eq!(body["schema"], "schema.with.dots");
        assert_eq!(body["table"], "한글.table");
        assert!(body.get("tableKey").is_none());
    }
    #[test]
    fn database_rows_escape_server_supplied_names_and_values() {
        let mut app = App {
            route: "pipe/database".into(),
            loaded: true,
            resource: json!([{"id":"db","name":"<script>name</script>","engine":"postgres","host":"localhost","port":5432,"database":"app","tlsMode":"verify_full"}]),
            ..App::default()
        };
        app.databases.selected = "db".into();
        app.databases.tables = json!({"tables":[]});
        app.databases.page = json!({"columns":[{"name":"<b>column</b>"}],"rows":[["<img src=x onerror=1>"]],"hasMore":false});
        let html = content(&app);
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<img src=x"));
        assert!(html.contains("&lt;img src=x onerror=1&gt;"));
    }
}
