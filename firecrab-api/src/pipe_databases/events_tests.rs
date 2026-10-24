use super::*;
use crate::pipe_databases::DatabaseInput;
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use firecrab_api_types::{
    DeployMicroServiceRequest, MicroServiceDeployment, MicroServiceResponse, MicroServiceSpec,
    ServiceInstance, ServicePhase, VmState,
};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone)]
struct Receiver {
    secret: String,
    fail: Arc<AtomicBool>,
    lose_reply: Arc<AtomicBool>,
    calls: Arc<Mutex<Vec<String>>>,
    effects: Arc<Mutex<std::collections::HashSet<String>>>,
}
async fn receive(State(state): State<Receiver>, headers: HeaderMap, body: Bytes) -> StatusCode {
    let timestamp = headers["x-firecrab-timestamp"].to_str().unwrap();
    let mut signature = Hmac::<Sha256>::new_from_slice(state.secret.as_bytes()).unwrap();
    signature.update(timestamp.as_bytes());
    signature.update(b".");
    signature.update(&body);
    assert_eq!(
        headers["x-firecrab-signature"],
        format!("sha256={:x}", signature.finalize().into_bytes())
    );
    let event: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let id = event["id"].as_str().unwrap().to_owned();
    assert_eq!(headers["x-firecrab-event-id"], id);
    assert!(String::from_utf8_lossy(&body).contains("123456789012345678.123456789"));
    state.calls.lock().unwrap().push(id.clone());
    if state.fail.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    state.effects.lock().unwrap().insert(id);
    if state.lose_reply.swap(false, Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    StatusCode::NO_CONTENT
}
async fn execute(c: &mut DatabaseConnection, sql: &str) {
    match c {
        DatabaseConnection::Postgres(c) => {
            write::pg_batch(c, sql).await.unwrap();
        }
        DatabaseConnection::Mysql(c) => {
            write::mysql_command(c, sql).await.unwrap();
        }
    }
}
async fn number(c: &mut DatabaseConnection, sql: &str) -> i64 {
    match c {
        DatabaseConnection::Postgres(c) => sqlx::query_scalar(sql).fetch_one(c).await.unwrap(),
        DatabaseConnection::Mysql(c) => sqlx::query_scalar(sql).fetch_one(c).await.unwrap(),
    }
}

#[test]
fn raw_event_envelopes_preserve_decimal_precision() {
    let id = Uuid::new_v4();
    let trigger = Trigger {
        id,
        database_id: Uuid::new_v4(),
        input: TriggerInput {
            request_id: id,
            schema: "public".into(),
            table: "amounts".into(),
            service_id: Uuid::new_v4(),
            path: "/events/database".into(),
        },
        paused: false,
        installed: true,
        detached: false,
    };
    let raw: Box<serde_json::value::RawValue> =
        serde_json::from_str("{\"amount\":123456789012345678.123456789}").unwrap();
    let bytes = event_body("test:1", &trigger, &raw).unwrap();
    assert!(
        String::from_utf8(bytes)
            .unwrap()
            .contains("123456789012345678.123456789")
    );
}

async fn restart_checkpoint(engine: Engine) {
    let Ok(dir) = std::env::var("FIRECRAB_QA_RESTART_GATE") else {
        return;
    };
    let name = if engine == Engine::Postgres {
        "postgres"
    } else {
        "mysql"
    };
    let path = std::path::Path::new(&dir);
    tokio::fs::write(
        path.join(format!("{name}.ready")),
        b"restart QA database and RabbitMQ brokers",
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(120), async {
        while !path.join(format!("{name}.continue")).exists() {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("QA restart controller must acknowledge recovery");
}

#[tokio::test]
#[ignore = "requires disposable DB and three TLS RabbitMQ fixtures"]
async fn durable_events_real_rabbitmq() {
    let inputs: Vec<DatabaseInput> = serde_json::from_slice(
        &std::fs::read(std::env::var("FIRECRAB_DB_SERVICES_QA_JSON").unwrap()).unwrap(),
    )
    .unwrap();
    let cluster: super::super::managed_rabbitmq::Cluster = serde_json::from_slice(
        &std::fs::read(std::env::var("FIRECRAB_RABBITMQ_FIXTURE").unwrap()).unwrap(),
    )
    .unwrap();
    let rabbitmq = cluster.config.unwrap();
    let mut untrusted = rabbitmq.clone();
    untrusted.ca_certificate = None;
    assert!(
        untrusted.connect().await.is_err(),
        "an untrusted broker must fail closed"
    );
    let mut client = rabbitmq.connect().await.unwrap();
    for input in inputs {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("firecrab.db");
        let state = AppState::with_db_file(
            crate::templates::TemplateRegistry::load_default().unwrap(),
            path.clone(),
        )
        .await
        .unwrap();
        let record = DatabaseRecord {
            id: Uuid::new_v4(),
            input,
        };
        state
            .store
            .save_pipe_database(record.id, &record.input)
            .unwrap();
        let settings = record.input.settings(record.id).unwrap();
        let engine = settings.summary.engine;
        let schema = if engine == Engine::Postgres {
            "public".into()
        } else {
            settings.summary.database.clone()
        };
        let table = format!("events_{}", Uuid::new_v4().simple());
        let qualified = TableRequest {
            schema: schema.clone(),
            table: table.clone(),
        }
        .qualified(engine);
        let mut c = DatabaseConnection::connect_write(&record.input, &settings)
            .await
            .unwrap();
        write::initialize(&mut c, record.id).await.unwrap();
        let identity = if engine == Engine::Postgres {
            "bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY"
        } else {
            "bigint AUTO_INCREMENT PRIMARY KEY"
        };
        execute(&mut c,&format!("CREATE TABLE {qualified}(id {identity},name varchar(100),amount decimal(38,9) NOT NULL DEFAULT 123456789012345678.123456789)")).await;
        let service_id = Uuid::new_v4();
        let id = Uuid::new_v4();
        let trigger = Trigger {
            id,
            database_id: record.id,
            input: TriggerInput {
                request_id: id,
                schema,
                table,
                service_id,
                path: "/events/database".into(),
            },
            paused: false,
            installed: true,
            detached: false,
        };
        let outbox = trigger.outbox(engine);
        let receiver = Receiver {
            secret: state.store.database_service_secret(service_id).unwrap(),
            fail: Arc::new(AtomicBool::new(true)),
            lose_reply: Arc::new(AtomicBool::new(false)),
            calls: Arc::new(Mutex::new(vec![])),
            effects: Arc::new(Mutex::new(std::collections::HashSet::new())),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = Router::new()
            .route("/events/database", post(receive))
            .with_state(receiver.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let spec:MicroServiceSpec=serde_json::from_value(serde_json::json!({"name":"event-handler","image":"example/handler:1","port":port,"ram":256,"cpu":1})).unwrap();
        state
            .store
            .insert_micro_service(&MicroServiceResponse {
                id: service_id,
                spec: spec.clone(),
            })
            .unwrap();
        state.store.save_database_rabbitmq(&rabbitmq).unwrap();
        let axum::Json(created) = crate::handlers::pipe_database_events::create(
            axum::extract::State(state.clone()),
            axum::Extension(crate::server::RequestId(Uuid::new_v4())),
            axum::extract::Path(record.id.to_string()),
            crate::extract::ValidatedJson(trigger.input.clone()),
        )
        .await
        .unwrap();
        assert!(created.installed && !created.paused);
        install(&mut c, &trigger).await.unwrap();
        let binding = state
            .store
            .database_bindings()
            .unwrap()
            .into_iter()
            .find(|b| b.input.service_id == service_id)
            .unwrap();
        assert!(binding.ready && binding.event_inbox);
        assert_eq!(
            binding.input.access,
            super::super::bindings::Access::Read,
            "enabling events must not promote business privileges"
        );
        let vm = Uuid::new_v4();
        state
            .store
            .link_database_service_vm(vm, service_id)
            .unwrap();
        let (env, _) = super::super::bindings::guest_inputs(&state.store, vm).unwrap();
        assert!(
            env.contains_key("FIRECRAB_INBOX_TABLE"),
            "the handler receives its generated inbox without manual configuration"
        );
        state
            .store
            .save_service_deployment(&MicroServiceDeployment {
                id: Uuid::new_v4(),
                service_id,
                phase: ServicePhase::Running,
                spec,
                request: DeployMicroServiceRequest {
                    micro_network_id: None,
                    host_port: 20000,
                },
                digest: None,
                instances: vec![ServiceInstance {
                    vm_id: Uuid::new_v4(),
                    ipv4: "127.0.0.1".into(),
                    host_port: 20000,
                    state: VmState::Running,
                    healthy: true,
                }],
                log: vec![],
                domain_route_owned: false,
                error: None,
            })
            .unwrap();
        execute(
            &mut c,
            &format!("INSERT INTO {qualified}(name) VALUES ('first'),('second')"),
        )
        .await;
        relay(&state, &record, &trigger, &client, false)
            .await
            .unwrap();
        assert_eq!(
            receiver.calls.lock().unwrap().len(),
            1,
            "a failed event must block later events"
        );
        assert_eq!(
            number(
                &mut c,
                &format!("SELECT COUNT(*) FROM {outbox} WHERE attempts=1 AND status='pending'")
            )
            .await,
            1
        );
        let probe = rabbitmq::channel(&client, &trigger.queue(), false)
            .await
            .unwrap();
        let unacked = probe
            .basic_get(trigger.queue().into(), BasicGetOptions::default())
            .await
            .unwrap()
            .unwrap();
        assert!(
            unacked.redelivered,
            "a handler failure must return the broker delivery"
        );
        assert_eq!(
            unacked.properties.message_id().as_ref().unwrap().as_str(),
            format!("{}:1", trigger.id)
        );
        probe
            .close(200, "simulate receiver channel loss".into())
            .await
            .unwrap();
        // Reconnect the source after an optional real DB/broker restart.
        drop(c);
        restart_checkpoint(engine).await;
        let mut c = DatabaseConnection::connect_write(&record.input, &settings)
            .await
            .unwrap();
        let _ = client.close(200, "restart checkpoint".into()).await;
        client = rabbitmq.connect().await.unwrap();
        let channel = rabbitmq::channel(&client, &trigger.queue(), false)
            .await
            .unwrap();
        // The source repairs a lost broker copy without inventing a new ID.
        channel
            .queue_purge(trigger.queue().into(), QueuePurgeOptions::default())
            .await
            .unwrap();
        receiver.fail.store(false, Ordering::SeqCst);
        execute(&mut c, &format!("UPDATE {outbox} SET next_attempt=0")).await;
        receiver.lose_reply.store(true, Ordering::SeqCst);
        relay(&state, &record, &trigger, &client, false)
            .await
            .unwrap();
        assert_eq!(
            receiver.effects.lock().unwrap().len(),
            1,
            "lost HTTP reply may follow a committed business effect"
        );
        execute(&mut c, &format!("UPDATE {outbox} SET next_attempt=0")).await;
        relay(&state, &record, &trigger, &client, false)
            .await
            .unwrap();
        assert_eq!(receiver.effects.lock().unwrap().len(), 2);
        // Reopen persisted API state and repeat an HTTP-success/checkpoint crash window.
        execute(
            &mut c,
            &format!("UPDATE {outbox} SET status='pending',next_attempt=0 WHERE id=1"),
        )
        .await;
        let reopened = AppState::with_db_file(
            crate::templates::TemplateRegistry::load_default().unwrap(),
            path,
        )
        .await
        .unwrap();
        relay(&reopened, &record, &trigger, &client, false)
            .await
            .unwrap();
        assert_eq!(
            receiver.effects.lock().unwrap().len(),
            2,
            "duplicate delivery must retain its original ID"
        );
        {
            let calls = receiver.calls.lock().unwrap();
            assert_eq!(calls[0], calls[1]);
        }
        // Exhausted retries remain durable and block subsequent business effects.
        execute(
            &mut c,
            &format!("INSERT INTO {qualified}(name) VALUES ('third'),('fourth')"),
        )
        .await;
        execute(
            &mut c,
            &format!("UPDATE {outbox} SET attempts=11,next_attempt=0 WHERE id=3"),
        )
        .await;
        receiver.fail.store(true, Ordering::SeqCst);
        relay(&state, &record, &trigger, &client, false)
            .await
            .unwrap();
        assert!(
            relay(&state, &record, &trigger, &client, false)
                .await
                .is_err()
        );
        let failures = failed(&mut c, &trigger).await.unwrap();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].attempts, 12);
        assert_eq!(failures[0].error.as_deref(), Some("handler_unavailable"));
        let dead = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(message) = channel
                    .basic_get(
                        format!("{}.failed", trigger.queue()).into(),
                        BasicGetOptions::default(),
                    )
                    .await
                    .unwrap()
                {
                    break message;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            dead.properties.message_id().as_ref().unwrap().as_str(),
            format!("{}:3", trigger.id)
        );
        dead.ack(BasicAckOptions::default()).await.unwrap();
        receiver.fail.store(false, Ordering::SeqCst);
        replay(&mut c, &trigger, 3).await.unwrap();
        relay(&state, &record, &trigger, &client, false)
            .await
            .unwrap();
        assert_eq!(receiver.effects.lock().unwrap().len(), 4);
        let status = status(&mut c, trigger.clone()).await.unwrap();
        assert_eq!((status.pending, status.failed, status.delivered), (0, 0, 4));
        execute(
            &mut c,
            &format!("INSERT INTO {outbox}(payload) VALUES (REPEAT('x',524289))"),
        )
        .await;
        execute(
            &mut c,
            &format!("INSERT INTO {qualified}(name) VALUES ('after oversized')"),
        )
        .await;
        relay(&state, &record, &trigger, &client, false)
            .await
            .unwrap();
        assert_eq!(
            failed(&mut c, &trigger).await.unwrap()[0].error.as_deref(),
            Some("payload_too_large")
        );
        replay(&mut c, &trigger, 5).await.unwrap();
        relay(&state, &record, &trigger, &client, false)
            .await
            .unwrap();
        assert!(
            acknowledge(&mut c, &trigger, 6).await.is_err(),
            "pending delivery cannot be skipped"
        );
        acknowledge(&mut c, &trigger, 5).await.unwrap();
        acknowledge(&mut c, &trigger, 5).await.unwrap();
        assert!(replay(&mut c, &trigger, 5).await.is_err());
        relay(&state, &record, &trigger, &client, false)
            .await
            .unwrap();
        assert_eq!(receiver.effects.lock().unwrap().len(), 5);
        let status = super::status(&mut c, trigger.clone()).await.unwrap();
        assert_eq!(
            (
                status.pending,
                status.failed,
                status.delivered,
                status.skipped
            ),
            (0, 0, 5, 1)
        );
        assert_eq!(number(&mut c, &format!("SELECT COUNT(*) FROM {outbox} WHERE id=5 AND OCTET_LENGTH(payload)=524289 AND last_error='operator_acknowledged' AND delivered_at IS NULL")).await, 1);
        detach(&mut c, &trigger).await.unwrap();
        execute(
            &mut c,
            &format!("INSERT INTO {qualified}(name) VALUES ('after detach')"),
        )
        .await;
        assert_eq!(
            number(&mut c, &format!("SELECT COUNT(*) FROM {outbox}")).await,
            6
        );
        server.abort();
        super::super::bindings::revoke(&record, &binding)
            .await
            .unwrap();
        channel
            .queue_delete(trigger.queue().into(), QueueDeleteOptions::default())
            .await
            .unwrap();
        channel
            .queue_delete(
                format!("{}.failed", trigger.queue()).into(),
                QueueDeleteOptions::default(),
            )
            .await
            .unwrap();
        execute(&mut c, &format!("DROP TABLE {qualified}")).await;
        let namespace = quote(engine, &write::internal_schema(record.id));
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
