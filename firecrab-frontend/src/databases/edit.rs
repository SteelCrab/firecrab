use super::*;

pub fn test_connection(app: Shared) {
    crate::advanced::capture_form(&app);
    let Some(form) = app.borrow().form.clone() else {
        return;
    };
    let Some(node) = element("editor").and_then(|el| el.dyn_into::<HtmlFormElement>().ok()) else {
        return;
    };
    let Ok(data) = FormData::new_with_form(&node) else {
        return;
    };
    let values = form
        .fields
        .iter()
        .map(|f| {
            (
                f.key.into(),
                data.get(f.key).as_string().unwrap_or_default(),
            )
        })
        .collect();
    let body = match form.payload(&values) {
        Ok(v) => v,
        Err(e) => {
            app.borrow_mut().error = e;
            redraw(&app);
            return;
        }
    };
    {
        let mut s = app.borrow_mut();
        if s.busy {
            return;
        }
        s.busy = true;
        s.error.clear();
        s.databases.notice.clear();
    }
    redraw(&app);
    let generation = app.borrow().generation;
    spawn_local(async move {
        let result =
            api::request_detailed("POST", "/api/pipe/databases/test", Some(&body), None).await;
        let mut s = app.borrow_mut();
        if s.generation != generation {
            return;
        }
        s.busy = false;
        match result {
            Ok(_) => {
                s.databases.notice =
                    tr("Database connection succeeded.", "DB 연결에 성공했다.").into()
            }
            Err(err) => {
                if let Some(row) = s.databases.row.as_mut() {
                    row.uncertain = err
                        .status
                        .is_none_or(|status| status >= 500 || status == 408);
                }
                s.error = err.message;
                s.field_errors = err.fields;
            }
        }
        drop(s);
        redraw(&app);
    });
}
fn field(key: &'static str, label: &'static str, kind: &'static str, value: &str) -> Field {
    Field::new(key, label, kind, value)
}
fn resource_field(key: &'static str, label: &'static str, items: &Value) -> Field {
    field(key, label, "select", "").resources(items, "id", "name")
}
pub fn open_operation(app: &Shared, action: &str) {
    let s = app.borrow();
    let d = &s.databases;
    let id = &d.selected;
    let path = format!("/api/pipe/databases/{}", api::encode(id));
    let request = match uuid() {
        Ok(v) => v,
        Err(err) => {
            drop(s);
            app.borrow_mut().error = err;
            render_messages(app);
            return;
        }
    };
    let mut form = match action {
        "db-binding" => Form::new(
            "Database service access",
            format!("{path}/bindings"),
            "POST",
            vec![
                resource_field(
                    "serviceId",
                    tr("Service", "서비스"),
                    d.aux.get("services").unwrap_or(&Value::Null),
                ),
                field("access", tr("Access", "접근 권한"), "select", "read")
                    .choices(&[("read", "Read only"), ("write", "Read/write")]),
            ],
            "pipe/database",
        ),
        "db-trigger" => {
            let tables=json!(list(&d.tables["tables"]).iter().map(|v|json!({"id":json!([text(v,"schema"),text(v,"name")]).to_string(),"name":format!("{}.{}",text(v,"schema"),text(v,"name"))})).collect::<Vec<_>>());
            let mut source = resource_field("tableKey", tr("Source table", "원본 테이블"), &tables);
            source.value = d.table.clone();
            Form::new(
                "Enable database events",
                format!("{path}/triggers"),
                "POST",
                vec![
                    source,
                    resource_field(
                        "serviceId",
                        tr("Handler service", "처리 서비스"),
                        d.aux.get("services").unwrap_or(&Value::Null),
                    ),
                    field(
                        "path",
                        tr("Endpoint path", "엔드포인트 경로"),
                        "text",
                        "/events/database",
                    ),
                ],
                "pipe/database",
            )
        }
        "db-external" => Form::new(
            "Use existing RabbitMQ",
            "/api/pipe/database-rabbitmq".into(),
            "PUT",
            vec![
                field(
                    "brokers",
                    tr("RabbitMQ endpoints", "RabbitMQ 주소"),
                    "textarea",
                    "",
                ),
                field("username", tr("Username", "사용자 이름"), "text", ""),
                field("password", tr("Password", "비밀번호"), "password", ""),
                field(
                    "virtualHost",
                    tr("Virtual host", "가상 호스트"),
                    "text",
                    "firecrab",
                ),
                field("tls", "TLS", "checkbox", "true").optional(),
                field(
                    "caCertificate",
                    tr("CA certificate (optional)", "CA 인증서 (선택)"),
                    "textarea",
                    "",
                )
                .optional(),
                field(
                    "clientCertificate",
                    tr("Client certificate (optional)", "클라이언트 인증서 (선택)"),
                    "textarea",
                    "",
                )
                .optional(),
                field(
                    "clientKey",
                    tr("Client key (optional)", "클라이언트 키 (선택)"),
                    "textarea",
                    "",
                )
                .optional(),
                field(
                    "development",
                    tr("Single-node development only", "단일 노드 개발 환경"),
                    "checkbox",
                    "false",
                )
                .optional(),
            ],
            "pipe/database",
        ),
        "db-backup" => Form::new(
            "Create database backup",
            format!("{path}/backups"),
            "POST",
            Vec::new(),
            "pipe/database",
        ),
        "db-migration" => Form::new(
            "Apply database schema",
            format!("{path}/migrations"),
            "POST",
            vec![
                field("version", tr("Version", "버전"), "text", ""),
                field("sql", "SQL", "textarea", ""),
            ],
            "pipe/database",
        ),
        "db-restore" => {
            let jobs=json!(list(d.aux.get("jobs").unwrap_or(&Value::Null)).iter().filter(|j|text(j,"kind")=="backup"&&text(j,"status")=="succeeded").map(|j|json!({"id":j["id"],"name":format!("{} · {}",text(j,"id"),text(j,"createdAt"))})).collect::<Vec<_>>());
            Form::new(
                "Restore database snapshot",
                format!("{path}/restore"),
                "POST",
                vec![resource_field("backupId", tr("Backup", "백업"), &jobs)],
                "pipe/database",
            )
        }
        _ => return,
    };
    if matches!(action, "db-restore" | "db-migration") {
        form.fields.push(field(
            "confirmDatabase",
            tr("Confirm database name", "DB 이름 확인"),
            "text",
            "",
        ));
        form.fields.push(
            field(
                "externalWritersStopped",
                tr(
                    "I stopped all external database writers.",
                    "외부 DB 쓰기를 모두 중지했다.",
                ),
                "checkbox",
                "false",
            )
            .optional(),
        );
        form.base["databaseName"] = json!(text(&selected(&s), "database"));
    }
    if action != "db-external" {
        form.base["requestId"] = json!(request);
    }
    drop(s);
    open(app, form);
}
pub fn payload(form: &Form, body: &Value) -> Option<Result<Value, String>> {
    let mut result = body.clone();
    match form.title.as_str() {
        "Connect database" | "Update connection credentials" => {
            result["name"] = json!(text(body, "name").trim());
            result["uri"] = json!(text(body, "uri").trim());
            if text(body, "tlsMode").is_empty() {
                result.as_object_mut().unwrap().remove("tlsMode");
            }
            if text(body, "caCertificate").trim().is_empty() {
                result.as_object_mut().unwrap().remove("caCertificate");
            } else {
                result["caCertificate"] = json!(text(body, "caCertificate").trim());
            }
        }
        "Enable database events" => {
            let pair: Vec<String> = match serde_json::from_str(text(body, "tableKey")) {
                Ok(pair) => pair,
                Err(_) => return Some(Err("Select a source table.".into())),
            };
            if pair.len() != 2 {
                return Some(Err("Select a source table.".into()));
            }
            result.as_object_mut().unwrap().remove("tableKey");
            result["schema"] = json!(pair[0]);
            result["table"] = json!(pair[1]);
        }
        "Use existing RabbitMQ" => {
            if body["development"] != true {
                result["tls"] = json!(true);
            }
            result["brokers"] = json!(
                text(body, "brokers")
                    .split([',', '\n'])
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .collect::<Vec<_>>()
            );
            for key in ["caCertificate", "clientCertificate", "clientKey"] {
                if text(body, key).trim().is_empty() {
                    result.as_object_mut().unwrap().remove(key);
                }
            }
        }
        "Database service access" => {
            result["envName"] = json!("DATABASE_URL");
        }
        "Create database backup" => {}
        "Apply database schema" | "Restore database snapshot" => {
            if text(body, "confirmDatabase") != text(&form.base, "databaseName")
                || body["externalWritersStopped"] != true
            {
                return Some(Err(tr(
                    "Type the database name and confirm external writers are stopped.",
                    "DB 이름을 입력하고 외부 쓰기 중지를 확인한다.",
                )
                .into()));
            }
            result.as_object_mut().unwrap().remove("databaseName");
        }
        _ => return None,
    }
    Some(Ok(result))
}
pub fn before_submit(form: &Form) -> bool {
    match form.title.as_str(){"Restore database snapshot"=>window().confirm_with_message(tr("Restore replaces this database's snapshot state. Firecrab takes a fresh safety backup first. Continue?","복구하면 DB를 백업 시점의 상태로 변경한다. 먼저 안전 백업을 만든다. 계속하는가?")).unwrap_or(false),"Apply database schema"=>window().confirm_with_message(tr("Apply this schema version after taking a safety backup?","안전 백업을 만든 뒤 이 스키마 버전을 적용하는가?")).unwrap_or(false),_=>true}
}
pub fn failures(app: Shared, id: String) {
    let (g, e) = {
        let s = app.borrow();
        (s.generation, s.databases.epoch)
    };
    spawn_local(async move {
        let result = api::request(
            "GET",
            &format!("/api/pipe/database-triggers/{}/failures", api::encode(&id)),
            None,
        )
        .await;
        let mut s = app.borrow_mut();
        if !valid(&s, g, e) {
            return;
        }
        match result {
            Ok(value) => {
                s.databases.failures.insert(id, value);
            }
            Err(err) => s.error = err,
        };
        drop(s);
        redraw(&app);
    });
}
pub fn open_row(app: &Shared, action: &str, id: &str) {
    if !crate::allow_discard(app) {
        return;
    }
    let mut s = app.borrow_mut();
    let page = &s.databases.page;
    let columns = list(&page["definition"]["columns"]);
    let original = if action == "db-insert" {
        Value::Null
    } else {
        list(&page["rows"])
            .get(id.parse::<usize>().unwrap_or(usize::MAX))
            .cloned()
            .unwrap_or(Value::Null)
    };
    if action != "db-insert"
        && (original.is_null() || !list(&original["truncatedColumns"]).is_empty())
    {
        return;
    }
    let request = match uuid() {
        Ok(id) => id,
        Err(err) => {
            s.error = err;
            return;
        }
    };
    let mut values = BTreeMap::new();
    let mut modes = BTreeMap::new();
    for column in columns {
        let name = text(column, "name");
        values.insert(name.into(), text(&original["values"], name).into());
        modes.insert(
            name.into(),
            if column["generated"] == true
                || column["binary"] == true
                || (!original.is_null() && column["primaryKey"] == true)
            {
                "keep"
            } else if original.is_null() {
                if !column["defaultValue"].is_null() || column["generated"] == true {
                    "default"
                } else if column["nullable"] == true {
                    "null"
                } else {
                    "value"
                }
            } else if original["values"][name].is_null() {
                "null"
            } else {
                "value"
            }
            .into(),
        );
    }
    let operation = match action {
        "db-insert" => "insert",
        "db-delete-row" => "delete",
        _ => "update",
    };
    s.databases.row = Some(Row {
        uncertain: false,
        operation: operation.into(),
        request,
        original,
        values,
        modes,
    });
    s.form = Some(Form::new(
        "Database row",
        String::new(),
        "POST",
        Vec::new(),
        "pipe/database",
    ));
    s.dirty = false;
    s.error.clear();
    drop(s);
    redraw(app);
}
pub fn capture_row(app: &Shared) {
    let Some(form) = element("db-row-editor").and_then(|e| e.dyn_into::<HtmlFormElement>().ok())
    else {
        return;
    };
    let Ok(data) = FormData::new_with_form(&form) else {
        return;
    };
    let mut s = app.borrow_mut();
    let columns = list(&s.databases.page["definition"]["columns"]).to_vec();
    if let Some(row) = s.databases.row.as_mut() {
        if row.uncertain {
            return;
        }
        let old = (row.values.clone(), row.modes.clone());
        for (i, column) in columns.iter().enumerate() {
            let name = text(column, "name");
            if let Some(mode) = data.get(&format!("mode-{i}")).as_string() {
                row.modes.insert(name.into(), mode);
            }
            if let Some(value) = data.get(&format!("value-{i}")).as_string() {
                row.values.insert(name.into(), value);
            }
        }
        if old != (row.values.clone(), row.modes.clone())
            && let Ok(id) = uuid()
        {
            row.request = id;
        }
    }
}
pub(super) fn row_payload(row: &Row, schema: &str, table: &str) -> Value {
    let values = row
        .values
        .iter()
        .filter_map(
            |(key, value)| match row.modes.get(key).map(String::as_str) {
                Some("value") => Some((key.clone(), json!(value))),
                Some("null") => Some((key.clone(), Value::Null)),
                _ => None,
            },
        )
        .collect::<serde_json::Map<_, _>>();
    json!({"requestId":row.request,"schema":schema,"table":table,"operation":row.operation,"key":if row.original.is_null(){json!({})}else{row.original["key"].clone()},"version":row.original["version"],"values":if row.operation=="delete"{json!({})}else{json!(values)}})
}
pub fn save_row(app: Shared) {
    let mut s = app.borrow_mut();
    if s.busy {
        return;
    }
    let Some((schema, table)) = table(&s) else {
        return;
    };
    let Some(row) = s.databases.row.as_ref() else {
        return;
    };
    if row.operation == "delete"
        && !window()
            .confirm_with_message(tr(
                "Delete this row? This changes the source database.",
                "이 행을 삭제하는가? 원본 DB가 변경된다.",
            ))
            .unwrap_or(false)
    {
        return;
    }
    let body = row_payload(row, &schema, &table);
    let path = format!(
        "/api/pipe/databases/{}/mutations",
        api::encode(&s.databases.selected)
    );
    s.busy = true;
    s.error.clear();
    let g = s.generation;
    drop(s);
    redraw(&app);
    spawn_local(async move {
        let result = api::request_detailed("POST", &path, Some(&body), None).await;
        let mut s = app.borrow_mut();
        if s.generation != g {
            return;
        }
        s.busy = false;
        match result {
            Ok(_) => {
                s.form = None;
                s.dirty = false;
                s.databases.row = None;
                s.databases.notice = tr("Row saved.", "행을 저장했다.").into();
            }
            Err(err) => {
                if let Some(row) = s.databases.row.as_mut() {
                    row.uncertain = err
                        .status
                        .is_none_or(|status| status >= 500 || status == 408);
                }
                s.error = err.message;
                s.field_errors = err.fields;
            }
        }
        let success = s.form.is_none();
        drop(s);
        if success {
            load_page(app);
        } else {
            redraw(&app);
        }
    });
}
