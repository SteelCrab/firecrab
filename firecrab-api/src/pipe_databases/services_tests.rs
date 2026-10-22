//! Run against disposable PostgreSQL/MySQL servers; never a production database.
use super::{
    bindings::{self, Access, BindingInput, BindingRecord},
    events::{self, Trigger, TriggerInput},
    write::*,
    *,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

async fn execute(c: &mut DatabaseConnection, sql: &str) {
    match c {
        DatabaseConnection::Postgres(c) => {
            sqlx::Executor::execute(c, sql).await.unwrap();
        }
        DatabaseConnection::Mysql(c) => {
            sqlx::Executor::execute(c, sql).await.unwrap();
        }
    }
}
async fn count(c: &mut DatabaseConnection, sql: &str) -> i64 {
    match c {
        DatabaseConnection::Postgres(c) => sqlx::query_scalar(sql).fetch_one(c).await.unwrap(),
        DatabaseConnection::Mysql(c) => sqlx::query_scalar(sql).fetch_one(c).await.unwrap(),
    }
}

#[tokio::test]
#[ignore = "requires FIRECRAB_DB_SERVICES_QA_JSON with disposable DB credentials"]
async fn database_services_real_engines() {
    let inputs: Vec<DatabaseInput> = serde_json::from_slice(
        &std::fs::read(std::env::var("FIRECRAB_DB_SERVICES_QA_JSON").expect("QA fixture file"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(inputs.len(), 2);
    for input in inputs {
        assert!(input.write_enabled);
        let record = DatabaseRecord {
            id: Uuid::new_v4(),
            input,
        };
        let settings = record.input.settings(record.id).unwrap();
        let engine = settings.summary.engine;
        let mut c = DatabaseConnection::connect_write(&record.input, &settings)
            .await
            .unwrap();
        initialize(&mut c, record.id).await.unwrap();
        initialize(&mut c, record.id).await.unwrap();
        let schema = if engine == Engine::Postgres {
            "public".to_owned()
        } else {
            settings.summary.database.clone()
        };
        let table = TableRequest {
            schema: schema.clone(),
            table: format!("qa_{}", Uuid::new_v4().simple()),
        };
        let qualified = table.qualified(engine);
        let id = if engine == Engine::Postgres {
            "bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY"
        } else {
            "bigint AUTO_INCREMENT PRIMARY KEY"
        };
        let json_type = if engine == Engine::Postgres {
            "jsonb"
        } else {
            "json"
        };
        execute(&mut c,&format!("CREATE TABLE {qualified}(id {id},name varchar(100) NOT NULL,note text NULL,amount decimal(12,2) NOT NULL DEFAULT 0,metadata {json_type})")).await;
        let make = |operation, values, key, version| Mutation {
            request_id: Uuid::new_v4(),
            table: table.clone(),
            operation,
            values,
            key,
            version,
        };
        let insert = make(
            Operation::Insert,
            BTreeMap::from([
                ("name".into(), json!("한글 ' \\ literal")),
                ("amount".into(), json!("12.50")),
                (
                    "metadata".into(),
                    json!(r#"{"enabled":true,"exact":1234567890123456789.123456789}"#),
                ),
                ("note".into(), Value::Null),
            ]),
            BTreeMap::new(),
            None,
        );
        let receipt = mutate(&mut c, record.id, &insert).await.unwrap();
        assert_eq!(receipt.affected_rows, 1);
        mutate(&mut c, record.id, &insert).await.unwrap();
        assert_eq!(
            count(&mut c, &format!("SELECT COUNT(*) FROM {qualified}")).await,
            1
        );
        let mut reused = make(
            Operation::Insert,
            BTreeMap::from([("name".into(), json!("different"))]),
            BTreeMap::new(),
            None,
        );
        reused.request_id = insert.request_id;
        assert!(matches!(
            mutate(&mut c, record.id, &reused).await,
            Err(Error::RequestConflict)
        ));
        let page = c
            .editable_page(&PageRequest {
                table: table.clone(),
                offset: 0,
                limit: 50,
            })
            .await
            .unwrap();
        let row = &page.rows[0];
        assert_eq!(row.values["name"].as_deref(), Some("한글 ' \\ literal"));
        assert!(row.values["note"].is_none());
        let update = make(
            Operation::Update,
            BTreeMap::from([("note".into(), json!("updated"))]),
            row.key.clone(),
            Some(row.version.clone()),
        );
        mutate(&mut c, record.id, &update).await.unwrap();
        let stale = make(
            Operation::Delete,
            BTreeMap::new(),
            row.key.clone(),
            Some(row.version.clone()),
        );
        assert!(matches!(
            mutate(&mut c, record.id, &stale).await,
            Err(Error::Conflict)
        ));
        let unchanged = c
            .editable_page(&PageRequest {
                table: table.clone(),
                offset: 0,
                limit: 50,
            })
            .await
            .unwrap();
        let equal = make(
            Operation::Update,
            BTreeMap::from([("note".into(), json!("updated"))]),
            unchanged.rows[0].key.clone(),
            Some(unchanged.rows[0].version.clone()),
        );
        mutate(&mut c, record.id, &equal).await.unwrap();
        // JSON literal null and SQL NULL must remain distinct; raw JSON text
        // also keeps PostgreSQL numeric precision through the HTTP contract.
        for value in [
            json!("null"),
            Value::Null,
            json!(r#"{"exact":1234567890123456789.123456789}"#),
        ] {
            let current = c
                .editable_page(&PageRequest {
                    table: table.clone(),
                    offset: 0,
                    limit: 50,
                })
                .await
                .unwrap();
            let update = make(
                Operation::Update,
                BTreeMap::from([("metadata".into(), value.clone())]),
                current.rows[0].key.clone(),
                Some(current.rows[0].version.clone()),
            );
            mutate(&mut c, record.id, &update).await.unwrap();
            let page = c
                .editable_page(&PageRequest {
                    table: table.clone(),
                    offset: 0,
                    limit: 50,
                })
                .await
                .unwrap();
            if value.is_null() {
                assert!(page.rows[0].values["metadata"].is_none());
            } else if value == json!("null") {
                assert_eq!(page.rows[0].values["metadata"].as_deref(), Some("null"));
            } else if engine == Engine::Postgres {
                assert!(
                    page.rows[0].values["metadata"]
                        .as_ref()
                        .unwrap()
                        .contains("1234567890123456789.123456789")
                );
            }
        }
        let mut readonly = record.input.clone();
        readonly.write_enabled = false;
        assert!(
            DatabaseConnection::connect_write(&readonly, &settings)
                .await
                .is_err()
        );
        let mut ro = DatabaseConnection::connect(&record.input, &settings)
            .await
            .unwrap();
        let denied = match &mut ro {
            DatabaseConnection::Postgres(c) => sqlx::query(&format!("DELETE FROM {qualified}"))
                .execute(c)
                .await
                .is_err(),
            DatabaseConnection::Mysql(c) => {
                let mut tx = sqlx::Connection::begin(c).await.unwrap();
                sqlx::query(&format!("DELETE FROM {qualified}"))
                    .execute(&mut *tx)
                    .await
                    .is_err()
            }
        };
        assert!(denied, "read-only sessions must reject writes");
        let trigger = Trigger {
            id: Uuid::new_v4(),
            database_id: record.id,
            input: TriggerInput {
                request_id: Uuid::new_v4(),
                schema: schema.clone(),
                table: table.table.clone(),
                service_id: Uuid::new_v4(),
                path: "/events/database".into(),
            },
            paused: false,
            installed: false,
            detached: false,
        };
        events::install(&mut c, &trigger).await.unwrap();
        let outbox = format!(
            "{}.{}",
            quote(engine, &internal_schema(record.id)),
            quote(engine, &format!("outbox_{}", trigger.id.simple()))
        );
        execute(&mut c, "BEGIN").await;
        execute(
            &mut c,
            &format!("INSERT INTO {qualified}(name) VALUES ('rolled back')"),
        )
        .await;
        execute(&mut c, "ROLLBACK").await;
        assert_eq!(
            count(&mut c, &format!("SELECT COUNT(*) FROM {outbox}")).await,
            0
        );
        execute(
            &mut c,
            &format!("INSERT INTO {qualified}(name) VALUES ('event committed')"),
        )
        .await;
        assert_eq!(
            count(
                &mut c,
                &format!("SELECT COUNT(*) FROM {outbox} WHERE status='pending'")
            )
            .await,
            1
        );
        let service = Uuid::new_v4();
        let binding = BindingRecord::new(
            &record,
            BindingInput {
                request_id: Uuid::new_v4(),
                service_id: service,
                access: Access::Read,
                env_name: "DATABASE_URL".into(),
            },
        )
        .await
        .unwrap();
        bindings::provision(&record, &binding).await.unwrap();
        bindings::provision(&record, &binding).await.unwrap();
        let private = binding.connection.settings(binding.id).unwrap();
        let mut restricted = DatabaseConnection::connect_write(&binding.connection, &private)
            .await
            .unwrap();
        let denied = match &mut restricted {
            DatabaseConnection::Postgres(c) => sqlx::query(&format!(
                "INSERT INTO {qualified}(name) VALUES ('forbidden')"
            ))
            .execute(c)
            .await
            .is_err(),
            DatabaseConnection::Mysql(c) => sqlx::query(&format!(
                "INSERT INTO {qualified}(name) VALUES ('forbidden')"
            ))
            .execute(c)
            .await
            .is_err(),
        };
        assert!(denied, "a read binding must be enforced by the DB account");
        bindings::revoke(&record, &binding).await.unwrap();
        let write_binding = BindingRecord::new(
            &record,
            BindingInput {
                request_id: Uuid::new_v4(),
                service_id: service,
                access: Access::Write,
                env_name: "DATABASE_URL".into(),
            },
        )
        .await
        .unwrap();
        bindings::provision(&record, &write_binding).await.unwrap();
        let private = write_binding.connection.settings(write_binding.id).unwrap();
        let mut service = DatabaseConnection::connect_write(&write_binding.connection, &private)
            .await
            .unwrap();
        execute(
            &mut service,
            &format!("INSERT INTO {qualified}(name) VALUES ('service write')"),
        )
        .await;
        assert_eq!(
            count(&mut c, &format!("SELECT COUNT(*) FROM {outbox}")).await,
            2
        );
        bindings::revoke(&record, &write_binding).await.unwrap();
        execute(&mut c, &format!("DROP TABLE {qualified}")).await;
        let namespace = quote(engine, &internal_schema(record.id));
        if engine == Engine::Postgres {
            execute(&mut c, &format!("DROP SCHEMA {namespace} CASCADE")).await;
        } else {
            execute(&mut c, &format!("DROP DATABASE {namespace}")).await;
        }
    }
}

#[tokio::test]
#[ignore = "requires disposable DB fixtures and native backup clients"]
async fn database_services_api_real_engines() {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    async fn request(
        app: &axum::Router,
        method: &str,
        path: &str,
        value: Value,
        status: u16,
    ) -> Value {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("origin", "http://localhost:8080")
                    .header("content-type", "application/json")
                    .body(Body::from(value.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status, "{method} {path}");
        let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
    let inputs: Vec<DatabaseInput> = serde_json::from_slice(
        &std::fs::read(std::env::var("FIRECRAB_DB_SERVICES_QA_JSON").unwrap()).unwrap(),
    )
    .unwrap();
    for input in inputs {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::state::AppState::with_db_file(
            crate::templates::TemplateRegistry::load_default().unwrap(),
            dir.path().join("api.db"),
        )
        .await
        .unwrap();
        let app =
            crate::server::build_router(state.clone(), &crate::server::HttpConfig::load().unwrap());
        let settings = input.settings(Uuid::new_v4()).unwrap();
        let engine = settings.summary.engine;
        let table = TableRequest {
            schema: if engine == Engine::Postgres {
                "public".into()
            } else {
                settings.summary.database.clone()
            },
            table: format!("api_{}", Uuid::new_v4().simple()),
        };
        let qualified = table.qualified(engine);
        let mut connection = DatabaseConnection::connect_write(&input, &settings)
            .await
            .unwrap();
        execute(
            &mut connection,
            &format!("CREATE TABLE {qualified}(id bigint PRIMARY KEY,name varchar(100) NOT NULL)"),
        )
        .await;
        let created = request(
            &app,
            "POST",
            "/api/pipe/databases",
            serde_json::to_value(&input).unwrap(),
            201,
        )
        .await;
        let id = Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();
        assert!(created.get("uri").is_none() && created.get("caCertificate").is_none());
        let base = format!("/api/pipe/databases/{id}");
        let mutation = json!({"requestId":Uuid::new_v4(),"schema":table.schema,"table":table.table,"operation":"insert","values":{"id":"9007199254740993","name":"API insert"},"key":{}});
        request(
            &app,
            "POST",
            &format!("{base}/mutations"),
            mutation.clone(),
            200,
        )
        .await;
        request(&app, "POST", &format!("{base}/mutations"), mutation, 200).await;
        let page = request(
            &app,
            "POST",
            &format!("{base}/rows"),
            json!({"schema":table.schema,"table":table.table}),
            200,
        )
        .await;
        assert_eq!(page["rows"].as_array().unwrap().len(), 1);
        assert_eq!(page["rows"][0]["key"]["id"], "9007199254740993");
        request(&app,"POST",&format!("{base}/mutations"),json!({"requestId":Uuid::new_v4(),"schema":table.schema,"table":table.table,"operation":"insert","values":{"id":"invalid-number","name":"invalid input"},"key":{}}),400).await;
        // A credential update keeps the already saved CA when the form is blank.
        let mut update = serde_json::to_value(&input).unwrap();
        update.as_object_mut().unwrap().remove("caCertificate");
        request(&app, "PUT", &base, update, 200).await;
        assert_eq!(
            state
                .store
                .pipe_database(id)
                .unwrap()
                .unwrap()
                .input
                .ca_certificate,
            input.ca_certificate
        );
        let service = request(
            &app,
            "POST",
            "/api/micro-services",
            json!({"name":"db-api-qa","image":"example/handler:1","port":3000,"ram":256,"cpu":1}),
            201,
        )
        .await;
        let service_id = service["id"].as_str().unwrap();
        let binding_input =
            json!({"requestId":Uuid::new_v4(),"serviceId":service_id,"access":"write"});
        let binding = request(
            &app,
            "POST",
            &format!("{base}/bindings"),
            binding_input.clone(),
            200,
        )
        .await;
        assert_eq!(binding["ready"], true);
        assert!(binding.get("connection").is_none() && binding.get("role").is_none());
        request(
            &app,
            "POST",
            &format!("{base}/bindings"),
            binding_input,
            200,
        )
        .await;
        request(
            &app,
            "PUT",
            &format!("{base}/access"),
            json!({"writeEnabled":false}),
            400,
        )
        .await;
        let job = request(
            &app,
            "POST",
            &format!("{base}/backups"),
            json!({"requestId":Uuid::new_v4()}),
            202,
        )
        .await;
        super::maintenance::spawn(state.clone());
        let mut done = false;
        for _ in 0..150 {
            let jobs = request(&app, "GET", &format!("{base}/jobs"), Value::Null, 200).await;
            let job = jobs
                .as_array()
                .unwrap()
                .iter()
                .find(|j| j["id"] == job["id"])
                .unwrap();
            if job["status"] == "succeeded" {
                done = true;
                break;
            }
            assert_ne!(job["status"], "failed", "backup API job: {job}");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(done, "backup API worker did not finish");
        let backup_id = job["id"].as_str().unwrap();
        request(
            &app,
            "DELETE",
            &format!("/api/pipe/database-jobs/{backup_id}/backup"),
            Value::Null,
            200,
        )
        .await;
        request(
            &app,
            "DELETE",
            &format!(
                "/api/pipe/database-bindings/{}",
                binding["id"].as_str().unwrap()
            ),
            Value::Null,
            200,
        )
        .await;
        request(&app, "DELETE", &base, Value::Null, 200).await;
        assert_eq!(
            count(
                &mut connection,
                &format!("SELECT COUNT(*) FROM {qualified}")
            )
            .await,
            1,
            "removing the connection must preserve source rows"
        );
        execute(&mut connection, &format!("DROP TABLE {qualified}")).await;
        let namespace = quote(engine, &internal_schema(id));
        execute(
            &mut connection,
            &if engine == Engine::Postgres {
                format!("DROP SCHEMA {namespace} CASCADE")
            } else {
                format!("DROP DATABASE {namespace}")
            },
        )
        .await;
    }
}
