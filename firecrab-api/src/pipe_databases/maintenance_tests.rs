use super::*;
use crate::pipe_databases::{
    DatabaseInput,
    bindings::{self, Access, BindingInput, BindingRecord},
    events::{self, Trigger, TriggerInput},
};

#[test]
fn migrations_reject_transaction_escapes_and_foreign_objects() {
    for engine in [Engine::Postgres, Engine::Mysql] {
        assert!(
            migration_statements(
                engine,
                "qa",
                "CREATE TABLE app_test (id bigint PRIMARY KEY); INSERT INTO app_test VALUES (1)"
            )
            .is_ok()
        );
        for sql in [
            "COMMIT",
            "CREATE TABLE app_test(id int); COMMIT",
            "ROLLBACK",
            "SET ROLE admin",
            "DROP SCHEMA public CASCADE",
            "COPY app_test TO PROGRAM 'echo unsafe'",
            "DROP TABLE firecrab_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.audit",
        ] {
            assert!(
                migration_statements(engine, "qa", sql).is_err(),
                "{engine:?}: {sql}"
            );
        }
    }
    for sql in [
        "USE other",
        "ALTER TABLE qa.t RENAME TO other.t",
        "INSERT INTO qa.t SELECT * FROM other.t",
        "DROP TABLE other.t",
    ] {
        assert!(
            migration_statements(Engine::Mysql, "qa", sql).is_err(),
            "{sql}"
        );
    }
    assert!(
        migration_statements(
            Engine::Postgres,
            "qa",
            "ALTER TABLE public.t SET SCHEMA firecrab_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        )
        .is_err()
    );
    assert!(
        migration_statements(
            Engine::Postgres,
            "qa",
            "/* COMMIT */ ALTER TABLE public.t ADD COLUMN note text"
        )
        .is_ok()
    );
}
async fn execute(c: &mut DatabaseConnection, sql: &str) {
    match c {
        DatabaseConnection::Postgres(c) => write::pg_batch(c, sql).await.unwrap(),
        DatabaseConnection::Mysql(c) => write::mysql_command(c, sql).await.unwrap(),
    }
}
async fn count(c: &mut DatabaseConnection, sql: &str) -> i64 {
    match c {
        DatabaseConnection::Postgres(c) => sqlx::query_scalar(sql).fetch_one(c).await.unwrap(),
        DatabaseConnection::Mysql(c) => sqlx::query_scalar(sql).fetch_one(c).await.unwrap(),
    }
}

#[tokio::test]
#[ignore = "requires disposable DB fixtures and matching native backup clients"]
async fn native_backups_migrations_and_restore() {
    let inputs: Vec<DatabaseInput> = serde_json::from_slice(
        &fs::read(std::env::var("FIRECRAB_DB_SERVICES_QA_JSON").unwrap()).unwrap(),
    )
    .unwrap();
    for input in inputs {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::with_db_file(
            crate::templates::TemplateRegistry::load_default().unwrap(),
            dir.path().join("firecrab.db"),
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
        eprintln!("Native maintenance QA: {engine:?}");
        let schema = if engine == Engine::Postgres {
            "public".to_owned()
        } else {
            settings.summary.database.clone()
        };
        let table = write::TableRequest {
            schema: schema.clone(),
            table: format!("restore_{}", Uuid::new_v4().simple()),
        };
        let qualified = table.qualified(engine);
        let mut c = DatabaseConnection::connect_write(&record.input, &settings)
            .await
            .unwrap();
        write::initialize(&mut c, record.id).await.unwrap();
        let identity = if engine == Engine::Postgres {
            "bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY"
        } else {
            "bigint AUTO_INCREMENT PRIMARY KEY"
        };
        execute(
            &mut c,
            &format!("CREATE TABLE {qualified}(id {identity},name varchar(100) NOT NULL)"),
        )
        .await;
        execute(
            &mut c,
            &format!("INSERT INTO {qualified}(name) VALUES ('original')"),
        )
        .await;
        let id = Uuid::new_v4();
        let service_id = Uuid::new_v4();
        let trigger = Trigger {
            id,
            database_id: record.id,
            input: TriggerInput {
                request_id: id,
                schema,
                table: table.table.clone(),
                service_id,
                path: "/events/database".into(),
            },
            paused: true,
            installed: true,
            detached: false,
        };
        events::install(&mut c, &trigger).await.unwrap();
        state.store.save_database_trigger(&trigger).unwrap();
        let mut binding = BindingRecord::new(
            &record,
            BindingInput {
                request_id: Uuid::new_v4(),
                service_id,
                access: Access::Write,
                env_name: "DATABASE_URL".into(),
            },
        )
        .await
        .unwrap();
        bindings::provision(&record, &binding).await.unwrap();
        binding.ready = true;
        state.store.save_database_binding(&binding).unwrap();
        let mut migration = Job::new(&record, Uuid::new_v4(), Kind::Migration).unwrap();
        migration.version = Some("v1".into());
        migration.sql = Some(format!(
            "ALTER TABLE {qualified} ADD COLUMN note varchar(100)"
        ));
        migration.status = "running".into();
        state.store.save_database_job(&migration).unwrap();
        run(&state, &record, &mut migration).await.unwrap();
        migration.status = "succeeded".into();
        persist(&state, &migration).unwrap();
        apply_migration(&record, &migration).await.unwrap();
        let mut changed = migration.clone();
        changed.sql = Some(format!(
            "ALTER TABLE {qualified} ADD COLUMN changed varchar(100)"
        ));
        assert!(apply_migration(&record, &changed).await.is_err());
        let columns = c.table_info(&table).await.unwrap();
        assert!(columns.columns.iter().any(|col| col.name == "note"));
        let safety = state
            .store
            .database_job(migration.safety_backup_id.unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(safety.status, "succeeded");
        assert!(safety.checksum.is_some());
        // All-or-nothing PostgreSQL DDL, and explicit incomplete MySQL receipts.
        let failed_name = format!("partial_{}", Uuid::new_v4().simple());
        let failed_table = write::TableRequest {
            schema: table.schema.clone(),
            table: failed_name.clone(),
        };
        let mut failing = Job::new(&record, Uuid::new_v4(), Kind::Migration).unwrap();
        failing.version = Some("v2".into());
        failing.sql = Some(format!(
            "CREATE TABLE {}(id int); ALTER TABLE {} ADD COLUMN impossible_type firecrab_invalid_type",
            failed_table.qualified(engine),
            qualified
        ));
        assert!(apply_migration(&record, &failing).await.is_err());
        if engine == Engine::Postgres {
            assert!(matches!(
                c.table_info(&failed_table).await,
                Err(Error::NotFound)
            ));
        } else {
            assert!(c.table_info(&failed_table).await.is_ok());
            assert!(apply_migration(&record, &failing).await.is_err());
        }
        // A corrupted archive is rejected before any restore SQL runs.
        let path = backup_path(&state, safety.id).unwrap();
        let bytes = fs::read(&path).unwrap();
        fs::write(&path, b"corrupted archive").unwrap();
        let attempted = Job::new(&record, Uuid::new_v4(), Kind::Restore).unwrap();
        assert!(restore(&state, &record, &attempted, &safety).await.is_err());
        fs::write(&path, bytes).unwrap();
        let mut restore_job = Job::new(&record, Uuid::new_v4(), Kind::Restore).unwrap();
        restore_job.backup_id = Some(safety.id);
        restore_job.status = "running".into();
        persist(&state, &restore_job).unwrap();
        run(&state, &record, &mut restore_job).await.unwrap();
        let columns = c.table_info(&table).await.unwrap();
        assert!(!columns.columns.iter().any(|col| col.name == "note"));
        assert!(matches!(
            c.table_info(&failed_table).await,
            Err(Error::NotFound)
        ));
        assert_eq!(
            count(
                &mut c,
                &format!("SELECT COUNT(*) FROM {qualified} WHERE name='original'")
            )
            .await,
            1
        );
        // Restored service accounts and capture are usable without new user settings.
        let private = binding.connection.settings(binding.id).unwrap();
        let mut service = DatabaseConnection::connect_write(&binding.connection, &private)
            .await
            .unwrap();
        execute(
            &mut service,
            &format!("INSERT INTO {qualified}(name) VALUES ('after restore')"),
        )
        .await;
        let outbox = format!(
            "{}.{}",
            quote(engine, &write::internal_schema(record.id)),
            quote(engine, &format!("outbox_{}", trigger.id.simple()))
        );
        assert_eq!(
            count(
                &mut c,
                &format!("SELECT COUNT(*) FROM {outbox} WHERE status='pending'")
            )
            .await,
            1
        );
        let backup_file = backup_path(&state, safety.id).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(backup_file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        events::detach(&mut c, &trigger).await.unwrap();
        bindings::revoke(&record, &binding).await.unwrap();
        execute(&mut c, &format!("DROP TABLE {qualified}")).await;
        execute(
            &mut c,
            &if engine == Engine::Postgres {
                format!(
                    "DROP SCHEMA {} CASCADE",
                    quote(engine, &write::internal_schema(record.id))
                )
            } else {
                format!(
                    "DROP DATABASE {}",
                    quote(engine, &write::internal_schema(record.id))
                )
            },
        )
        .await;
    }
}
