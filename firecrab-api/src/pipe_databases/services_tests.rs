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
        if engine == Engine::Mysql {
            let table = TableRequest {
                schema: settings.summary.database.clone(),
                table: format!("geom_{}", Uuid::new_v4().simple()),
            };
            let qualified = table.qualified(engine);
            execute(&mut c, &format!("CREATE TABLE {qualified}(id bigint PRIMARY KEY, shape GEOMETRYCOLLECTION); INSERT INTO {qualified} VALUES (1,ST_GeomFromText('GEOMETRYCOLLECTION(POINT(1 2))'))")).await;
            let page = c
                .editable_page(&PageRequest {
                    table: table.clone(),
                    offset: 0,
                    limit: 50,
                })
                .await
                .unwrap();
            assert!(page.definition.columns[1].binary);
            let value = page.rows[0].values["shape"].as_ref().unwrap();
            assert!(value.bytes().all(|b| b.is_ascii_hexdigit()));
            let edit = Mutation {
                request_id: Uuid::new_v4(),
                table,
                operation: Operation::Update,
                values: BTreeMap::from([("shape".into(), json!(value))]),
                key: page.rows[0].key.clone(),
                version: Some(page.rows[0].version.clone()),
            };
            assert!(matches!(
                mutate(&mut c, record.id, &edit).await,
                Err(Error::Invalid(_))
            ));
            execute(&mut c, &format!("DROP TABLE {qualified}")).await;
        }
        let limits = if engine == Engine::Postgres {
            let table = TableRequest {
                schema: "public".into(),
                table: format!("limits_{}", Uuid::new_v4().simple()),
            };
            let qualified = table.qualified(engine);
            let sequence = format!("public.seq_{}", Uuid::new_v4().simple());
            execute(&mut c, &format!("CREATE SEQUENCE {sequence}; CREATE TABLE {qualified}(id bigint PRIMARY KEY, v varchar(2), c char(2), b bit varying(2), f bit(2), external_default bigint DEFAULT nextval('{sequence}'))")).await;
            for (column, value) in [("v", "abc"), ("c", "abc"), ("b", "101"), ("f", "101")] {
                let mutation = Mutation {
                    request_id: Uuid::new_v4(),
                    table: table.clone(),
                    operation: Operation::Insert,
                    values: BTreeMap::from([
                        ("id".into(), json!("1")),
                        (column.into(), json!(value)),
                    ]),
                    key: BTreeMap::new(),
                    version: None,
                };
                let result = mutate(&mut c, record.id, &mutation).await;
                assert!(
                    matches!(result, Err(Error::Database(sqlx::Error::Database(ref e))) if matches!(e.code().as_deref(),Some("22001"|"22026"))),
                    "over-length {column} must fail before committing: {result:?}"
                );
            }
            assert_eq!(
                count(&mut c, &format!("SELECT COUNT(*) FROM {qualified}")).await,
                0
            );
            let valid = Mutation {
                request_id: Uuid::new_v4(),
                table,
                operation: Operation::Insert,
                values: BTreeMap::from([
                    ("id".into(), json!("1")),
                    ("v".into(), json!("ab")),
                    ("c".into(), json!("xy")),
                    ("b".into(), json!("10")),
                    ("f".into(), json!("01")),
                ]),
                key: BTreeMap::new(),
                version: None,
            };
            mutate(&mut c, record.id, &valid).await.unwrap();
            Some((qualified, sequence))
        } else {
            None
        };
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
        execute(&mut c, &format!("DROP TABLE {qualified}")).await;
        let mut stale_binding = write_binding.clone();
        stale_binding.tables.push(TableRequest {
            schema: "qa_dropped_schema".into(),
            table: "missing".into(),
        });
        bindings::revoke(&record, &stale_binding).await.unwrap();
        if let Some((table, sequence)) = limits {
            execute(
                &mut c,
                &format!("DROP TABLE {table}; DROP SEQUENCE {sequence}"),
            )
            .await;
        }
        let namespace = quote(engine, &internal_schema(record.id));
        if engine == Engine::Postgres {
            execute(&mut c, &format!("DROP SCHEMA {namespace} CASCADE")).await;
        } else {
            execute(&mut c, &format!("DROP DATABASE {namespace}")).await;
        }
    }
}

#[tokio::test]
#[ignore = "requires disposable MySQL fixture"]
async fn cancelled_mysql_setup_repairs_only_a_host_claimed_namespace() {
    let inputs: Vec<DatabaseInput> = serde_json::from_slice(
        &std::fs::read(std::env::var("FIRECRAB_DB_SERVICES_QA_JSON").unwrap()).unwrap(),
    )
    .unwrap();
    let input = inputs
        .into_iter()
        .find(|input| input.settings(Uuid::nil()).unwrap().summary.engine == Engine::Mysql)
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let store = crate::persistence::Store::open(&directory.path().join("state.db")).unwrap();
    for owner_table in [false, true] {
        let record = DatabaseRecord {
            id: Uuid::new_v4(),
            input: input.clone(),
        };
        let settings = record.input.settings(record.id).unwrap();
        let mut c = DatabaseConnection::connect_write(&record.input, &settings)
            .await
            .unwrap();
        let schema = quote(Engine::Mysql, &internal_schema(record.id));
        execute(&mut c, &format!("CREATE DATABASE {schema}")).await;
        if owner_table {
            execute(
                &mut c,
                &format!("CREATE TABLE {schema}.owner(id char(36) PRIMARY KEY) ENGINE=InnoDB"),
            )
            .await;
        }
        assert!(
            initialize_saved(&mut c, &record, &store).await.is_err(),
            "unclaimed namespaces must not be adopted"
        );
        let target = super::maintenance::target_hash(&record).unwrap();
        store.set_database_setup_claim(record.id, &target).unwrap();
        initialize_saved(&mut c, &record, &store).await.unwrap();
        verify_owner(&mut c, record.id).await.unwrap();
        assert!(!store.database_setup_claim(record.id, &target).unwrap());
        execute(&mut c, &format!("DROP DATABASE {schema}")).await;
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
        state
            .store
            .put_pipe_auth(&crate::pipe_auth::SavedAuth::Github {
                username: "database-qa".into(),
                token: "fixture-unused-for-deployment".into(),
            })
            .unwrap();
        let service = request(
            &app,
            "POST",
            "/api/micro-services",
            json!({"name":"db-api-qa","image":"ghcr.io/database-qa/handler:1","port":3000,"ram":256,"cpu":1}),
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

// Exercise the public control API as well as the underlying SQL operations.
async fn api_call(
    app: &axum::Router,
    method: &str,
    path: &str,
    input: Value,
    status: u16,
) -> Value {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("origin", "http://localhost:8080")
                .header("content-type", "application/json")
                .body(Body::from(input.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), status, "{method} {path}");
    serde_json::from_slice(
        &to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap()
}
async fn api_job(app: &axum::Router, base: &str, id: &Value) -> Value {
    for _ in 0..300 {
        let jobs = api_call(app, "GET", &format!("{base}/jobs"), Value::Null, 200).await;
        let job = jobs
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["id"] == *id)
            .unwrap();
        if job["status"] == "succeeded" {
            return job.clone();
        }
        assert!(
            !matches!(job["status"].as_str(), Some("failed" | "needs_recovery")),
            "maintenance did not succeed: {job}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("maintenance API job did not finish");
}

#[tokio::test]
#[ignore = "requires disposable DB and three TLS RabbitMQ fixtures"]
async fn database_event_and_maintenance_api_real_engines() {
    let inputs: Vec<DatabaseInput> = serde_json::from_slice(
        &std::fs::read(std::env::var("FIRECRAB_DB_SERVICES_QA_JSON").unwrap()).unwrap(),
    )
    .unwrap();
    let cluster: super::managed_rabbitmq::Cluster = serde_json::from_slice(
        &std::fs::read(std::env::var("FIRECRAB_RABBITMQ_FIXTURE").unwrap()).unwrap(),
    )
    .unwrap();
    for input in inputs {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::state::AppState::with_db_file(
            crate::templates::TemplateRegistry::load_default().unwrap(),
            dir.path().join("workflow.db"),
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
            table: format!("workflow_{}", Uuid::new_v4().simple()),
        };
        let qualified = table.qualified(engine);
        let mut c = DatabaseConnection::connect_write(&input, &settings)
            .await
            .unwrap();
        execute(&mut c,&format!("CREATE TABLE {qualified}(id bigint PRIMARY KEY,name varchar(100) NOT NULL); INSERT INTO {qualified} VALUES(1,'original')")).await;
        let saved = api_call(
            &app,
            "POST",
            "/api/pipe/databases",
            serde_json::to_value(&input).unwrap(),
            201,
        )
        .await;
        let id = Uuid::parse_str(saved["id"].as_str().unwrap()).unwrap();
        let base = format!("/api/pipe/databases/{id}");
        state
            .store
            .put_pipe_auth(&crate::pipe_auth::SavedAuth::Github {
                username: "database-qa".into(),
                token: "fixture-unused-for-deployment".into(),
            })
            .unwrap();
        let service = api_call(
            &app,
            "POST",
            "/api/micro-services",
            json!({"name":"workflow-handler","image":"ghcr.io/database-qa/handler:1","port":3000,"ram":256,"cpu":1}),
            201,
        )
        .await;
        let service_id = service["id"].as_str().unwrap();

        // External configuration disables managed supervision and never exposes credentials.
        assert_eq!(
            api_call(&app, "GET", "/api/pipe/database-rabbitmq", Value::Null, 200).await["configured"],
            false
        );
        assert!(
            api_call(
                &app,
                "GET",
                "/api/pipe/database-rabbitmq/managed",
                Value::Null,
                200
            )
            .await
            .is_null()
        );
        let tid = Uuid::new_v4();
        let trigger_input = json!({"requestId":tid,"schema":table.schema,"table":table.table,"serviceId":service_id});
        api_call(&app,"POST",&format!("{base}/triggers"),json!({"requestId":Uuid::nil(),"schema":table.schema,"table":table.table,"serviceId":service_id}),400).await;
        api_call(
            &app,
            "POST",
            &format!("{base}/triggers"),
            trigger_input.clone(),
            400,
        )
        .await;
        let mut managed = cluster.clone();
        managed.enabled = true;
        managed.phase = "ready".into();
        state
            .store
            .save_managed_database_rabbitmq(&managed)
            .unwrap();
        let metadata = api_call(
            &app,
            "GET",
            "/api/pipe/database-rabbitmq/managed",
            Value::Null,
            200,
        )
        .await;
        assert_eq!(metadata["brokers"].as_array().unwrap().len(), 3);
        assert!(metadata.get("config").is_none() && metadata["brokers"][0].get("files").is_none());
        api_call(
            &app,
            "POST",
            "/api/pipe/database-rabbitmq/managed",
            json!({"development":false}),
            409,
        )
        .await;
        api_call(
            &app,
            "POST",
            "/api/pipe/database-rabbitmq/managed",
            json!({"development":true}),
            409,
        )
        .await;
        let config = managed.config.as_ref().unwrap();
        api_call(
            &app,
            "PUT",
            "/api/pipe/database-rabbitmq",
            serde_json::to_value(config).unwrap(),
            200,
        )
        .await;
        assert!(
            !state
                .store
                .managed_database_rabbitmq()
                .unwrap()
                .unwrap()
                .enabled
        );
        let metadata = api_call(&app, "GET", "/api/pipe/database-rabbitmq", Value::Null, 200).await;
        assert_eq!(metadata["configured"], true);
        assert_eq!(metadata["tls"], true);
        for field in ["password", "username", "caCertificate", "clientKey"] {
            assert!(metadata.get(field).is_none());
        }

        // A retry keeps capture ownership and the auto-created read-only handler account.
        let created = api_call(
            &app,
            "POST",
            &format!("{base}/triggers"),
            trigger_input.clone(),
            200,
        )
        .await;
        assert_eq!(created["installed"], true);
        let binding =
            api_call(&app, "GET", &format!("{base}/bindings"), Value::Null, 200).await[0].clone();
        assert_eq!(binding["access"], "read");
        assert!(binding.get("eventInbox").is_none());
        assert!(
            state
                .store
                .database_bindings()
                .unwrap()
                .iter()
                .any(|b| b.id.to_string() == binding["id"].as_str().unwrap() && b.event_inbox)
        );
        let path = format!("/api/pipe/database-triggers/{tid}");
        api_call(&app, "PUT", &path, json!({"paused":true}), 200).await;
        assert_eq!(
            api_call(
                &app,
                "POST",
                &format!("{base}/triggers"),
                trigger_input.clone(),
                200
            )
            .await["paused"],
            true
        );
        let mut changed = trigger_input.clone();
        changed["path"] = json!("/different");
        api_call(&app, "POST", &format!("{base}/triggers"), changed, 409).await;
        api_call(
            &app,
            "PUT",
            "/api/pipe/database-rabbitmq",
            serde_json::to_value(config).unwrap(),
            400,
        )
        .await;
        api_call(
            &app,
            "POST",
            "/api/pipe/database-rabbitmq/managed",
            json!({"development":false}),
            400,
        )
        .await;
        api_call(&app, "DELETE", &path, Value::Null, 400).await;
        execute(
            &mut c,
            &format!("UPDATE {qualified} SET name='retained by API' WHERE id=1"),
        )
        .await;
        let outbox = format!(
            "{}.{}",
            quote(engine, &internal_schema(id)),
            quote(engine, &format!("outbox_{}", tid.simple()))
        );
        let mark_failed = format!(
            "UPDATE {outbox} SET status='failed',attempts=12,last_error='handler_unavailable' WHERE id=1"
        );
        execute(&mut c, &mark_failed).await;
        state
            .store
            .set_database_trigger_health(tid, Some("rabbitmq_unavailable"))
            .unwrap();
        let status = api_call(&app, "GET", &format!("{base}/triggers"), Value::Null, 200).await;
        assert_eq!(status[0]["failed"], 1);
        assert_eq!(status[0]["lastError"], "rabbitmq_unavailable");
        state.store.set_database_trigger_health(tid, None).unwrap();
        let failures = api_call(&app, "GET", &format!("{path}/failures"), Value::Null, 200).await;
        let event_id = format!("{tid}:1");
        assert_eq!(failures[0]["eventId"], event_id);
        assert!(failures[0].get("payload").is_none());
        api_call(&app, "POST", &format!("{path}/replay/1"), Value::Null, 200).await;
        assert!(
            api_call(&app, "GET", &format!("{path}/failures"), Value::Null, 200)
                .await
                .as_array()
                .unwrap()
                .is_empty()
        );
        api_call(
            &app,
            "POST",
            &format!("{path}/replay/999"),
            Value::Null,
            404,
        )
        .await;
        execute(&mut c, &mark_failed).await;
        api_call(
            &app,
            "POST",
            &format!("{path}/acknowledge/1"),
            json!({"confirmEventId":"another-event"}),
            400,
        )
        .await;
        assert_eq!(
            api_call(&app, "GET", &format!("{base}/triggers"), Value::Null, 200).await[0]["failed"],
            1
        );
        api_call(
            &app,
            "POST",
            &format!("{path}/acknowledge/1"),
            json!({"confirmEventId":event_id}),
            200,
        )
        .await;
        // Repeating the same explicit skip is safe, but cannot skip a different event.
        api_call(
            &app,
            "POST",
            &format!("{path}/acknowledge/1"),
            json!({"confirmEventId":event_id}),
            200,
        )
        .await;
        assert_eq!(
            api_call(&app, "GET", &format!("{base}/triggers"), Value::Null, 200).await[0]["skipped"],
            1
        );
        api_call(&app, "POST", &format!("{path}/detach"), Value::Null, 200).await;
        api_call(&app, "PUT", &path, json!({"paused":true}), 200).await;
        assert_eq!(
            api_call(&app, "PUT", &path, json!({"paused":false}), 200).await["detached"],
            true
        );
        api_call(&app, "DELETE", &path, Value::Null, 200).await;
        assert!(
            api_call(&app, "GET", &format!("{base}/triggers"), Value::Null, 200)
                .await
                .as_array()
                .unwrap()
                .is_empty()
        );

        // Failed setup retains ownership, is visible, cannot resume, and can be cleaned.
        let partial = Uuid::new_v4();
        let partial_path = format!("/api/pipe/database-triggers/{partial}");
        api_call(&app,"POST",&format!("{base}/triggers"),json!({"requestId":partial,"schema":table.schema,"table":format!("missing_{}",partial.simple()),"serviceId":service_id}),404).await;
        assert_eq!(
            api_call(&app, "GET", &format!("{base}/triggers"), Value::Null, 200).await[0]["lastError"],
            "setup_incomplete"
        );
        api_call(&app, "PUT", &partial_path, json!({"paused":false}), 400).await;
        api_call(
            &app,
            "POST",
            &format!("{partial_path}/detach"),
            Value::Null,
            200,
        )
        .await;
        api_call(&app, "DELETE", &partial_path, Value::Null, 200).await;

        // Backups must finish before download/removal; retries reuse their receipt.
        let capabilities = api_call(
            &app,
            "GET",
            "/api/pipe/database-maintenance/tools",
            Value::Null,
            200,
        )
        .await;
        assert_eq!(capabilities["postgres"], true);
        assert_eq!(capabilities["mysql"], true);
        let backup_input = json!({"requestId":Uuid::new_v4()});
        let backup = api_call(
            &app,
            "POST",
            &format!("{base}/backups"),
            backup_input.clone(),
            202,
        )
        .await;
        let backup_id = backup["id"].as_str().unwrap();
        let archive = format!("/api/pipe/database-jobs/{backup_id}/backup");
        api_call(&app, "DELETE", &archive, Value::Null, 400).await;
        api_call(&app, "GET", &archive, Value::Null, 404).await;
        api_call(
            &app,
            "POST",
            &format!("{base}/backups"),
            backup_input.clone(),
            200,
        )
        .await;
        super::maintenance::spawn(state.clone());
        let snapshot = api_job(&app, &base, &backup["id"]).await;
        use axum::{
            body::{Body, to_bytes},
            http::Request,
        };
        use tower::ServiceExt;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&archive)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            response.headers()["content-type"],
            "application/octet-stream"
        );
        assert!(
            response.headers()["content-disposition"]
                .to_str()
                .unwrap()
                .contains(backup_id)
        );
        let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024)
            .await
            .unwrap();
        use sha2::Digest;
        assert_eq!(
            format!("{:x}", sha2::Sha256::digest(&bytes)),
            snapshot["checksum"].as_str().unwrap()
        );
        api_call(&app, "POST", &format!("{base}/backups"), backup_input, 200).await;
        api_call(&app, "DELETE", &base, Value::Null, 400).await;

        let migration = json!({"requestId":Uuid::new_v4(),"version":"api-v1","sql":format!("ALTER TABLE {qualified} ADD COLUMN note varchar(100)"),"confirmDatabase":settings.summary.database,"externalWritersStopped":true});
        let mut denied = migration.clone();
        denied["externalWritersStopped"] = json!(false);
        api_call(&app, "POST", &format!("{base}/migrations"), denied, 400).await;
        let mut escaped = migration.clone();
        escaped["sql"] = json!("COMMIT");
        api_call(&app, "POST", &format!("{base}/migrations"), escaped, 400).await;
        let mut reused = migration.clone();
        reused["requestId"] = backup["id"].clone();
        api_call(&app, "POST", &format!("{base}/migrations"), reused, 409).await;
        let queued = api_call(
            &app,
            "POST",
            &format!("{base}/migrations"),
            migration.clone(),
            202,
        )
        .await;
        api_job(&app, &base, &queued["id"]).await;
        assert!(
            c.table_info(&table)
                .await
                .unwrap()
                .columns
                .iter()
                .any(|col| col.name == "note")
        );
        api_call(
            &app,
            "POST",
            &format!("{base}/migrations"),
            migration.clone(),
            200,
        )
        .await;
        let mut changed = migration;
        changed["sql"] = json!(format!("ALTER TABLE {qualified} ADD COLUMN changed text"));
        api_call(&app, "POST", &format!("{base}/migrations"), changed, 409).await;

        let restore = json!({"requestId":Uuid::new_v4(),"backupId":backup_id,"confirmDatabase":settings.summary.database,"externalWritersStopped":true});
        let mut denied = restore.clone();
        denied["confirmDatabase"] = json!("wrong database");
        api_call(&app, "POST", &format!("{base}/restore"), denied, 400).await;
        let mut missing = restore.clone();
        missing["backupId"] = json!(Uuid::new_v4());
        api_call(&app, "POST", &format!("{base}/restore"), missing, 404).await;
        let restored = api_call(
            &app,
            "POST",
            &format!("{base}/restore"),
            restore.clone(),
            202,
        )
        .await;
        api_job(&app, &base, &restored["id"]).await;
        assert!(
            !c.table_info(&table)
                .await
                .unwrap()
                .columns
                .iter()
                .any(|col| col.name == "note")
        );
        assert_eq!(
            count(
                &mut c,
                &format!("SELECT COUNT(*) FROM {qualified} WHERE id=1 AND name='retained by API'")
            )
            .await,
            1
        );
        api_call(&app, "POST", &format!("{base}/restore"), restore, 200).await;
        let jobs = api_call(&app, "GET", &format!("{base}/jobs"), Value::Null, 200).await;
        for job in jobs.as_array().unwrap().iter().filter(|j| {
            j["kind"] == "backup" && matches!(j["status"].as_str(), Some("succeeded" | "failed"))
        }) {
            api_call(
                &app,
                "DELETE",
                &format!(
                    "/api/pipe/database-jobs/{}/backup",
                    job["id"].as_str().unwrap()
                ),
                Value::Null,
                200,
            )
            .await;
        }
        api_call(&app, "GET", &archive, Value::Null, 404).await;
        api_call(
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
        api_call(&app, "DELETE", &base, Value::Null, 200).await;
        execute(&mut c, &format!("DROP TABLE {qualified}")).await;
        let namespace = quote(engine, &internal_schema(id));
        execute(
            &mut c,
            &if engine == Engine::Postgres {
                format!("DROP SCHEMA {namespace} CASCADE")
            } else {
                format!("DROP DATABASE {namespace}")
            },
        )
        .await;
    }
}
