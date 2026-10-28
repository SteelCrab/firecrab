use super::*;
use std::fmt::Write;
fn button(label: &str, action: &str, id: &str, disabled: bool) -> String {
    let html = crate::render::button(label, action, id);
    if disabled {
        html.replace("type=\"button\"", "type=\"button\" disabled")
    } else {
        html
    }
}
fn number(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}
fn alert(value: &str) -> String {
    format!(
        "<p class=\"wasm-error\" role=\"alert\">{}</p>",
        escape(value)
    )
}
pub fn content(app: &App) -> String {
    let d = &app.databases;
    let editing = app.form.is_some();
    let locked = app.busy || d.row.is_some() || !app.connection_error.is_empty();
    let mut out = format!(
        "<section class=\"database-screen\" aria-label=\"{}\"><div class=\"wasm-actions\"><h2>{}</h2>{}</div>",
        tr("Databases", "데이터베이스"),
        tr("Databases", "데이터베이스"),
        button(
            tr("Add database", "DB 가져오기"),
            "create",
            "database",
            locked || editing
        )
    );
    if !app.connection_error.is_empty() {
        out.push_str(&alert(&app.connection_error));
        out.push_str(&button(
            tr("Retry database list", "DB 목록 다시 불러오기"),
            "db-retry-list",
            "",
            app.busy,
        ));
    }
    if !app.error.is_empty() && app.field_errors.is_empty() {
        out.push_str(&alert(&app.error));
    }
    let notice = if d.notice.is_empty() {
        &app.notice
    } else {
        &d.notice
    };
    if !notice.is_empty() {
        let _ = write!(
            out,
            "<p id=\"db-notice\" role=\"status\">{}</p>",
            escape(notice)
        );
    }
    if let Some(form) = &app.form
        && d.row.is_none()
    {
        out.push_str(&form.heading());
        out.push_str(&form.html(app.busy, &app.field_errors));
    }
    if list(&app.resource).is_empty() && !editing && app.connection_error.is_empty() {
        let _ = write!(
            out,
            "<p>{}</p>",
            tr(
                "No database connections. Add a local DB URI, Supabase or Amazon RDS database.",
                "DB 연결이 없다. 로컬 DB URI, Supabase 또는 Amazon RDS DB를 추가한다."
            )
        );
    }
    if app.connection_error.is_empty() && !list(&app.resource).is_empty() {
        let _ = write!(
            out,
            "<div class=\"table-scroll\"><table class=\"wasm-table pipe-database-table\" aria-label=\"{}\"><thead><tr><th>{}</th><th>{}</th><th>{}</th><th>{}</th><th>TLS</th><th>{}</th></tr></thead><tbody>",
            tr("Saved databases", "저장한 DB"),
            tr("Name", "이름"),
            tr("Source", "위치"),
            tr("Engine", "엔진"),
            tr("Database address", "DB 주소"),
            tr("Actions", "작업")
        );
        for db in list(&app.resource) {
            let id = text(db, "id");
            let name = text(db, "name");
            let source = match text(db, "provider") {
                "local" => tr("Local DB URI", "로컬 DB URI"),
                "rds" => "Amazon RDS",
                _ => "Supabase",
            };
            let tls = match text(db, "tlsMode") {
                "verify_full" => tr("Verified", "검증"),
                "require" => tr("Encrypted", "암호화"),
                _ => tr("Disabled", "사용 안 함"),
            };
            let browse = button(tr("Browse", "조회"), "db-browse", id, locked || editing).replace(
                "type=\"button\"",
                &format!(
                    "type=\"button\" aria-label=\"{}\"",
                    escape(&format!("Browse {name}"))
                ),
            );
            let menu = button("⋯", "db-menu", id, locked || editing).replace(
                "type=\"button\"",
                &format!(
                    "type=\"button\" aria-label=\"{}\"",
                    escape(&format!("{name} actions"))
                ),
            );
            let _ = write!(
                out,
                "<tr><td>{}</td><td>{source}</td><td>{}</td><td class=\"mono\">{}:{}/{}</td><td>{tls}</td><td><div class=\"wasm-actions\">{browse}{menu}</div></td></tr>",
                escape(name),
                if text(db, "engine") == "postgres" {
                    "PostgreSQL"
                } else {
                    "MySQL"
                },
                escape(text(db, "host")),
                db["port"].as_u64().unwrap_or(0),
                escape(text(db, "database"))
            );
        }
        out.push_str("</tbody></table></div>");
    }
    let db = selected(app);
    if !db.is_null() && app.connection_error.is_empty() {
        out.push_str(&browser(app, &db));
    }
    out.push_str("</section>");
    out
}
fn browser(app: &App, db: &Value) -> String {
    let d = &app.databases;
    let locked = app.busy || app.form.is_some();
    let mut out = format!(
        "<section class=\"wasm-card database-browser\" aria-label=\"{}\"><div class=\"wasm-actions\"><h3>{}</h3>{}</div><div class=\"database-controls\"><label for=\"db-table\">{}</label><select id=\"db-table\" {}><option value=\"\">{}</option>",
        tr("Database browser", "DB 조회"),
        escape(text(db, "name")),
        button(tr("Close browser", "조회 닫기"), "db-close", "", locked),
        tr("Table", "테이블"),
        if locked || d.loading || !d.tables_error.is_empty() || list(&d.tables["tables"]).is_empty()
        {
            "disabled"
        } else {
            ""
        },
        tr("Select table", "테이블 선택")
    );
    for t in list(&d.tables["tables"]) {
        let value = json!([text(t, "schema"), text(t, "name")]).to_string();
        let _ = write!(
            out,
            "<option value=\"{}\" {}>{}.{}</option>",
            escape(&value),
            if d.table == value { "selected" } else { "" },
            escape(text(t, "schema")),
            escape(text(t, "name"))
        );
    }
    let _ = write!(
        out,
        "</select>{}{}</div>",
        button(
            tr("Refresh tables", "테이블 새로고침"),
            "db-tables",
            "",
            locked || d.loading
        ),
        button(
            tr("Refresh rows", "데이터 새로고침"),
            "db-refresh",
            "",
            locked || d.loading || d.table.is_empty()
        )
    );
    if !d.tables_error.is_empty() {
        out.push_str(&alert(&d.tables_error));
    } else if d.tables.is_null() {
        out.push_str(&format!(
            "<p role=\"status\">{}</p>",
            tr("Loading tables…", "테이블을 불러오는 중…")
        ));
    } else if list(&d.tables["tables"]).is_empty() {
        out.push_str(&format!(
            "<p>{}</p>",
            tr(
                "No accessible tables. Check SELECT permissions or refresh the table list.",
                "테이블이 없다."
            )
        ));
    }
    if d.tables["truncated"] == true {
        out.push_str("<p>Showing the first 500 tables.</p>");
    }
    if !d.rows_error.is_empty() {
        out.push_str(&alert(&d.rows_error));
        out.push_str(&button(
            tr("Retry rows", "데이터 다시 불러오기"),
            "db-refresh",
            "",
            locked,
        ));
    }
    if d.loading && !d.table.is_empty() {
        out.push_str(&format!(
            "<p role=\"status\">{}</p>",
            tr("Loading rows…", "데이터를 불러오는 중…")
        ));
    }
    if !d.page.is_null() {
        out.push_str(&rows(app, db));
    }
    if let Some(row) = &d.row {
        out.push_str(&row_editor(app, row));
    }
    let _ = write!(
        out,
        "<details {}><summary data-action=\"db-services\">{}</summary>",
        if d.advanced { "open" } else { "" },
        tr("Services & reliable events", "서비스·안전한 이벤트")
    );
    if d.advanced {
        out.push_str(&services(app));
    }
    out.push_str("</details>");
    let _ = write!(
        out,
        "<details {}><summary data-action=\"db-maintenance\">{}</summary>",
        if d.maintenance { "open" } else { "" },
        tr("Backups & schema changes", "백업·스키마 변경")
    );
    if d.maintenance {
        out.push_str(&maintenance(app));
    }
    out.push_str("</details></section>");
    out
}
fn rows(app: &App, db: &Value) -> String {
    let d = &app.databases;
    let write = db["writeEnabled"] == true;
    let page = &d.page;
    let columns = if write {
        &page["definition"]["columns"]
    } else {
        &page["columns"]
    };
    let locked = app.busy || app.form.is_some();
    let editable = write
        && page["definition"]["transactional"] == true
        && !list(&page["definition"]["primaryKey"]).is_empty();
    let mut out = String::new();
    if write {
        out.push_str(&button(
            tr("Add row", "행 추가"),
            "db-insert",
            "",
            locked || !editable,
        ));
        if !editable {
            out.push_str(
                "<p>Row edits require a transactional table with a supported primary key.</p>",
            );
        }
    } else {
        out.push_str(&format!(
            "<p>{}</p>",
            tr(
                "Read only. Enable writes in the connection actions to edit rows.",
                "읽기 전용이다. 행을 편집하려면 연결 메뉴에서 쓰기를 사용한다."
            )
        ));
    }
    let _ = write!(
        out,
        "<div class=\"table-scroll\"><table class=\"wasm-table pipe-database-preview\" aria-label=\"{}\"><thead><tr>",
        tr("Data preview", "데이터 미리보기")
    );
    for c in list(columns) {
        let _ = write!(out, "<th>{}</th>", escape(text(c, "name")));
    }
    if write {
        let _ = write!(out, "<th>{}</th>", tr("Actions", "작업"));
    }
    out.push_str("</tr></thead><tbody>");
    for (i, row) in list(&page["rows"]).iter().enumerate() {
        out.push_str("<tr>");
        for (j, column) in list(columns).iter().enumerate() {
            let value = if write {
                &row["values"][text(column, "name")]
            } else {
                &row[j]
            };
            let cell = if value.is_null() {
                "<span class=\"database-null\" title=\"SQL NULL\">NULL</span>".into()
            } else {
                escape(&number(value))
            };
            let _ = write!(
                out,
                "<td><span class=\"pipe-database-cell\" tabindex=\"0\">{cell}</span></td>"
            );
        }
        if write {
            let disabled = locked || !editable || !list(&row["truncatedColumns"]).is_empty();
            let _ = write!(
                out,
                "<td><div class=\"wasm-actions\">{}{}</div></td>",
                button(
                    tr("Edit row", "행 편집"),
                    "db-update",
                    &i.to_string(),
                    disabled
                ),
                button(
                    tr("Delete row", "행 삭제"),
                    "db-delete-row",
                    &i.to_string(),
                    disabled
                )
            );
        }
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table></div>");
    if list(&page["rows"]).is_empty() {
        let _ = write!(
            out,
            "<p>{}</p>",
            tr(
                "This table has no rows on this page.",
                "이 페이지에 데이터가 없다."
            )
        );
    }
    let _ = write!(
        out,
        "<div class=\"wasm-actions\">{}<span>{} {}</span>{}</div>",
        button(
            tr("Previous rows", "이전 행"),
            "db-prev",
            "",
            locked || d.loading || d.offset == 0
        ),
        tr("Offset", "시작 위치"),
        d.offset,
        button(
            tr("Next rows", "다음 행"),
            "db-next",
            "",
            locked || d.loading || page["hasMore"] != true
        )
    );
    out
}
fn row_editor(app: &App, row: &Row) -> String {
    let mut out = format!(
        "<form id=\"db-row-editor\" class=\"wasm-form\" aria-label=\"{}\"><h4 class=\"wasm-wide\">{}</h4><fieldset class=\"wasm-form wasm-wide\" {}>",
        tr("Row editor", "행 편집"),
        match row.operation.as_str() {
            "insert" => tr("Add row", "행 추가"),
            "delete" => tr("Delete row", "행 삭제"),
            _ => tr("Edit row", "행 편집"),
        },
        if app.busy || row.uncertain {
            "disabled"
        } else {
            ""
        }
    );
    if row.operation != "delete" {
        for (i, column) in list(&app.databases.page["definition"]["columns"])
            .iter()
            .enumerate()
        {
            let name = text(column, "name");
            let mode = row.modes.get(name).map(String::as_str).unwrap_or("keep");
            let disabled = column["generated"] == true
                || column["binary"] == true
                || (!row.original.is_null() && column["primaryKey"] == true);
            let _ = write!(
                out,
                "<div class=\"field\"><label for=\"row-value-{i}\">{} <small>{}</small></label><select name=\"mode-{i}\" aria-label=\"{} value mode\" {}>",
                escape(name),
                escape(text(column, "dataType")),
                escape(name),
                if disabled { "disabled" } else { "" }
            );
            for (value, label) in [
                ("value", tr("Value", "값")),
                ("null", "SQL NULL"),
                ("default", tr("Use default", "기본값 사용")),
                ("keep", tr("Unchanged", "변경 안 함")),
            ] {
                if value == "default" && !row.original.is_null()
                    || value == "null" && column["nullable"] != true
                    || value == "keep" && row.original.is_null()
                {
                    continue;
                }
                let _ = write!(
                    out,
                    "<option value=\"{value}\" {}>{label}</option>",
                    if mode == value { "selected" } else { "" }
                );
            }
            let _ = write!(
                out,
                "</select><textarea id=\"row-value-{i}\" name=\"value-{i}\" aria-label=\"{} value\" {}>{}</textarea></div>",
                escape(name),
                if disabled { "disabled" } else { "" },
                escape(row.values.get(name).map(String::as_str).unwrap_or(""))
            );
        }
    }
    // Cancel first, as in the shared form footer.
    let _ = write!(
        out,
        "<div class=\"wasm-actions wasm-wide\"><button type=\"button\" class=\"btn ghost\" data-action=\"cancel\"{}>{}</button><button class=\"btn primary\" type=\"submit\">{}</button></div></fieldset>",
        if app.busy { " disabled" } else { "" },
        tr("Cancel", "취소"),
        if row.operation == "delete" {
            tr("Delete row", "행 삭제")
        } else {
            tr("Save row", "행 저장")
        }
    );
    if row.uncertain {
        out.push_str("<p>The result may be unknown. Repeating this request keeps its ID and cannot apply a committed change twice.</p>");
        out.push_str(&button(
            tr("Retry same request", "같은 요청 재시도"),
            "db-retry-row",
            "",
            app.busy,
        ));
    }
    out.push_str("</form>");
    out
}
fn services(app: &App) -> String {
    let d = &app.databases;
    let locked = app.busy || app.form.is_some();
    let mut out = format!(
        "<section aria-label=\"{}\"><h4>{}</h4>{}",
        tr("Database service bindings", "DB 서비스 연결"),
        tr("Service access", "서비스 접근"),
        button(
            tr("Connect service", "서비스 연결"),
            "db-binding",
            "",
            locked
        )
    );
    for binding in list(d.aux.get("bindings").unwrap_or(&Value::Null)) {
        let id = text(binding, "serviceId");
        let name = list(d.aux.get("services").unwrap_or(&Value::Null))
            .iter()
            .find(|v| text(v, "id") == id)
            .map(|v| text(v, "name"))
            .unwrap_or(id);
        let _ = write!(
            out,
            "<p>{} · <span>{}</span> · {} {}</p>",
            escape(name),
            escape(text(binding, "access")),
            if binding["ready"] == true {
                tr("Ready", "준비 완료")
            } else {
                tr("Setup incomplete", "설정 미완료")
            },
            button(
                tr("Revoke access", "접근 회수"),
                "db-unbind",
                text(binding, "id"),
                locked
            )
        );
    }
    let rabbit = d.aux.get("rabbitmq").unwrap_or(&Value::Null);
    let managed = d.aux.get("managed").unwrap_or(&Value::Null);
    let _ = write!(
        out,
        "</section><section aria-label=\"{}\"><h4>{}</h4><p>{}</p>",
        tr("Reliable database events", "안전한 DB 이벤트"),
        tr("Database events", "DB 이벤트"),
        tr(
            "Firecrab prepares the handler account and duplicate tracking automatically. Use the provided receiver for duplicate protection. Choose read/write access if the handler changes application data.",
            "Firecrab이 처리 계정과 중복 추적을 자동 준비한다. 제공한 수신기로 중복 처리를 방지한다. 업무 데이터를 변경하는 서비스는 읽기/쓰기 권한을 선택한다."
        )
    );
    if rabbit["configured"] == true {
        let _ = write!(
            out,
            "<p>RabbitMQ · {}</p><p>RabbitMQ · {} · {}</p>",
            escape(if text(managed, "phase").is_empty() {
                "ready"
            } else {
                text(managed, "phase")
            }),
            if rabbit["tls"] == true {
                "TLS enabled"
            } else {
                "development TCP"
            },
            escape(
                &list(&rabbit["brokers"])
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        );
    } else {
        out.push_str(&button(
            tr("Prepare RabbitMQ automatically", "RabbitMQ 자동 준비"),
            "db-prepare",
            "",
            locked || matches!(text(managed, "phase"), "queued" | "starting" | "importing"),
        ));
    }
    if !text(managed, "error").is_empty() {
        out.push_str(&alert(text(managed, "error")));
    }
    let _ = write!(
        out,
        "<details><summary>{}</summary>{}</details>",
        tr("Use existing RabbitMQ", "기존 RabbitMQ 사용"),
        button(
            tr("Configure RabbitMQ", "RabbitMQ 설정"),
            "db-external",
            "",
            locked
        )
    );
    for trigger in list(d.aux.get("triggers").unwrap_or(&Value::Null)) {
        let id = text(trigger, "id");
        let _ = write!(
            out,
            "<article class=\"database-trigger\"><p>{}.{} → {}</p><small>{} {} · {} {} · {} {}</small><div class=\"wasm-actions\">{}{}{}{} </div>",
            escape(text(trigger, "schema")),
            escape(text(trigger, "table")),
            escape(text(trigger, "serviceId")),
            tr("Pending", "대기"),
            trigger["pending"].as_u64().unwrap_or(0),
            tr("Failed", "실패"),
            trigger["failed"].as_u64().unwrap_or(0),
            tr("Delivered", "전달"),
            trigger["delivered"].as_u64().unwrap_or(0),
            button(
                if trigger["paused"] == true {
                    tr("Resume", "재개")
                } else {
                    tr("Pause", "일시 중지")
                },
                "db-pause",
                id,
                locked
            ),
            if trigger["detached"] != true {
                button(tr("Detach capture", "캡처 중단"), "db-detach", id, locked)
            } else {
                button(
                    tr("Remove trigger", "트리거 제거"),
                    "db-delete-trigger",
                    id,
                    locked
                        || trigger["pending"].as_u64().unwrap_or(0) > 0
                        || trigger["failed"].as_u64().unwrap_or(0) > 0,
                )
            },
            button(
                tr("Failed events", "실패 이벤트"),
                "db-failures",
                id,
                locked
            ),
            if text(trigger, "lastError").is_empty() {
                String::new()
            } else {
                alert(text(trigger, "lastError"))
            }
        );
        let _ = write!(
            out,
            "<p>{} {}</p>",
            tr("Skipped", "건너뜀"),
            trigger["skipped"].as_u64().unwrap_or(0)
        );
        if let Some(failures) = d.failures.get(id) {
            for f in list(failures) {
                let value = json!([id, text(f, "eventId")]).to_string();
                let _ = write!(
                    out,
                    "<p>{} · <span>{}</span> · {} {}</p>",
                    escape(text(f, "eventId")),
                    escape(text(f, "error")),
                    button(tr("Replay", "재처리"), "db-replay", &value, locked),
                    button(
                        tr("Skip failed event", "실패 이벤트 건너뛰기"),
                        "db-skip",
                        &value,
                        locked
                    )
                );
            }
        }
        out.push_str("</article>");
    }
    out.push_str(&button(
        tr("Enable events", "이벤트 사용"),
        "db-trigger",
        "",
        locked || rabbit["configured"] != true,
    ));
    out.push_str("</section>");
    out
}
fn maintenance(app: &App) -> String {
    let d = &app.databases;
    let locked = app.busy || app.form.is_some();
    let mut out = format!(
        "<div class=\"wasm-actions\">{}{}{}</div><p>{}</p>",
        button(tr("Create backup", "백업 만들기"), "db-backup", "", locked),
        button(
            tr("Apply schema change", "스키마 변경"),
            "db-migration",
            "",
            locked
        ),
        button(
            tr("Restore snapshot", "백업 복구"),
            "db-restore",
            "",
            locked
        ),
        tr(
            "Schema changes and restores use a safety backup. Review the job result before resuming writers.",
            "스키마 변경과 복구 전에 안전 백업을 만든다. 쓰기 서비스를 재개하기 전에 작업 결과를 확인한다."
        )
    );
    let jobs = d.aux.get("jobs").unwrap_or(&Value::Null);
    if list(jobs).is_empty() {
        out.push_str("<p>No database jobs.</p>");
    } else {
        out.push_str("<div class=\"table-scroll\"><table class=\"wasm-table\" aria-label=\"Database jobs\"><thead><tr><th>Operation</th><th>Status</th><th>Result</th></tr></thead><tbody>");
        for job in list(jobs) {
            let id = api::encode(text(job, "id"));
            let _ = write!(
                out,
                "<tr><td>{}</td><td>{}</td><td>{}",
                escape(text(job, "kind")),
                escape(text(job, "status")),
                escape(text(job, "error"))
            );
            if text(job, "kind") == "backup" && text(job, "status") == "succeeded" {
                let _ = write!(
                    out,
                    "<a class=\"btn\" href=\"/api/pipe/database-jobs/{id}/backup\" download>Download backup</a>"
                );
            }
            if text(job, "kind") == "backup"
                && matches!(text(job, "status"), "succeeded" | "failed")
            {
                out.push_str(&button(
                    tr("Remove backup", "백업 제거"),
                    "db-delete-backup",
                    text(job, "id"),
                    locked,
                ));
            }
            out.push_str("</td></tr>");
        }
        out.push_str("</tbody></table></div>");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_row_editor_cancels_with_a_ghost_button_before_the_submit_button() {
        let row = Row {
            uncertain: false,
            operation: "insert".into(),
            request: "request".into(),
            original: Value::Null,
            values: BTreeMap::new(),
            modes: BTreeMap::new(),
        };
        let html = row_editor(&App::default(), &row);
        let cancel =
            "<button type=\"button\" class=\"btn ghost\" data-action=\"cancel\">Cancel</button>";
        let submit = "<button class=\"btn primary\" type=\"submit\">Save row</button>";
        let (cancel_at, submit_at) = (html.find(cancel), html.find(submit));
        assert!(cancel_at.is_some() && submit_at.is_some(), "{html}");
        assert!(cancel_at < submit_at, "{html}");
        // While a request runs, Cancel is disabled like the other buttons; Save row stays as is.
        let busy = App {
            busy: true,
            ..Default::default()
        };
        let html = row_editor(&busy, &row);
        assert!(
            html.contains("data-action=\"cancel\" disabled>Cancel</button>"),
            "{html}"
        );
    }
}
