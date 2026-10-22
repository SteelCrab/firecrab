use super::*;

fn input(uri: &str, provider: Provider) -> DatabaseInput {
    DatabaseInput {
        write_enabled: false,
        name: "Test DB".into(),
        provider,
        uri: uri.into(),
        tls_mode: None,
        ca_certificate: None,
    }
}

#[test]
fn parses_both_engines_cloud_providers_and_encoded_passwords() {
    for (uri, provider, engine, port, tls) in [
        (
            "mysql://user:p%40ss@localhost/app",
            Provider::Local,
            Engine::Mysql,
            3306,
            TlsMode::Disable,
        ),
        (
            "postgres://user:secret@[::1]:5544/app",
            Provider::Local,
            Engine::Postgres,
            5544,
            TlsMode::Disable,
        ),
        (
            "postgresql://postgres.project:secret@pooler.supabase.com:5432/postgres?sslmode=require",
            Provider::Supabase,
            Engine::Postgres,
            5432,
            TlsMode::Require,
        ),
        (
            "postgresql://user:secret@db.rds.amazonaws.com/app",
            Provider::Rds,
            Engine::Postgres,
            5432,
            TlsMode::VerifyFull,
        ),
        (
            "mysql://user:secret@db.rds.amazonaws.com/app?ssl-mode=VERIFY_IDENTITY",
            Provider::Rds,
            Engine::Mysql,
            3306,
            TlsMode::VerifyFull,
        ),
    ] {
        let settings = input(uri, provider).settings(Uuid::nil()).unwrap();
        assert_eq!(settings.summary.engine, engine);
        assert_eq!(settings.summary.port, port);
        assert_eq!(settings.summary.tls_mode, tls);
    }
    let settings = input("mysql://user:p%40ss@localhost/app", Provider::Local)
        .settings(Uuid::nil())
        .unwrap();
    assert_eq!(settings.password, "p@ss");
}

#[test]
fn rejects_incompatible_uris_tls_downgrades_and_file_parameters() {
    for (uri, provider) in [
        ("https://project.supabase.co", Provider::Supabase),
        ("mysql://user:secret@localhost/app", Provider::Supabase),
        (
            "postgres://user:secret@localhost/app?sslmode=disable",
            Provider::Rds,
        ),
        (
            "postgres://user:secret@localhost/app?sslrootcert=/etc/shadow",
            Provider::Local,
        ),
        (
            "mysql://user:secret@localhost/app?socket=/tmp/mysql.sock",
            Provider::Local,
        ),
        (
            "postgres://user:secret@localhost/app?host=evil.example",
            Provider::Local,
        ),
        (
            "postgres://user:secret@localhost/app?sslmode=require&sslmode=disable",
            Provider::Local,
        ),
        (
            "postgres://user:secret@localhost/app#secret",
            Provider::Local,
        ),
        ("postgres://user:secret@localhost:0/app", Provider::Local),
        ("postgres://user:secret@localhost/", Provider::Local),
        ("postgres://localhost/app", Provider::Local),
    ] {
        assert!(
            input(uri, provider).settings(Uuid::nil()).is_err(),
            "accepted {uri}"
        );
    }
}

#[test]
fn summaries_and_debug_output_never_contain_credentials() {
    let spec = input(
        "postgres://private-user:private-password@localhost/app",
        Provider::Local,
    );
    let summary = serde_json::to_string(&spec.settings(Uuid::nil()).unwrap().summary).unwrap();
    assert!(!summary.contains("private-user"));
    assert!(!summary.contains("private-password"));
    assert!(!summary.contains("uri"));
    assert!(!format!("{spec:?}").contains("private-password"));
}

#[test]
fn identifiers_are_quoted_and_preview_limits_are_enforced() {
    let request = PreviewRequest {
        schema: "public".into(),
        table: "users\"; DROP TABLE users; --".into(),
        limit: 50,
        offset: 0,
    };
    let columns = vec![Column {
        name: "odd\"column".into(),
        data_type: "text".into(),
    }];
    let sql = preview_sql(Engine::Postgres, &request, &columns);
    assert!(sql.contains("\"users\"\"; DROP TABLE users; --\""));
    assert!(sql.contains("LEFT(\"odd\"\"column\"::text, 2048)"));
    assert_eq!(quote(Engine::Mysql, "a`b"), "`a``b`");
    for limit in [0, 101, u32::MAX] {
        assert!(
            PreviewRequest {
                schema: "public".into(),
                table: "users".into(),
                limit,
                offset: 0
            }
            .validate()
            .is_err()
        );
    }
    let mut preview = Preview {
        columns,
        rows: vec![],
        has_more: false,
        offset: 0,
    };
    let mut bytes = 0;
    assert!(append_row(&mut preview, vec![None], 1, &mut bytes));
    assert!(!append_row(
        &mut preview,
        vec![Some("second".into())],
        1,
        &mut bytes
    ));
    assert!(preview.has_more);
    assert_eq!(preview.rows.len(), 1);
}

#[test]
fn saved_connections_survive_reopening_and_can_be_removed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("firecrab.db");
    let id = Uuid::new_v4();
    let spec = input("mysql://user:secret@localhost/app", Provider::Local);
    let store = crate::persistence::Store::open(&path).unwrap();
    store.save_pipe_database(id, &spec).unwrap();
    drop(store);
    let store = crate::persistence::Store::open(&path).unwrap();
    assert_eq!(store.list_pipe_databases().unwrap().len(), 1);
    let record = store.pipe_database(id).unwrap().unwrap();
    assert_eq!(record.input.uri, spec.uri);
    assert!(store.delete_pipe_database(id).unwrap());
    assert!(!store.delete_pipe_database(id).unwrap());
    assert!(store.pipe_database(id).unwrap().is_none());
}

/// Run against fresh disposable databases, never an operator's existing DB.
#[tokio::test]
#[ignore = "requires disposable FIRECRAB_TEST_MARIADB_URI"]
async fn mariadb_sessions_apply_native_timeout_and_access_mode() {
    let mut spec = input(
        &std::env::var("FIRECRAB_TEST_MARIADB_URI").unwrap(),
        Provider::Local,
    );
    for write in [false, true] {
        spec.write_enabled = write;
        let settings = spec.settings(Uuid::new_v4()).unwrap();
        let connection = DatabaseConnection::connect_mode(&spec, &settings, write)
            .await
            .unwrap();
        let DatabaseConnection::Mysql(mut c) = connection else {
            panic!("MariaDB fixture must use MySQL")
        };
        let timeout: f64 = sqlx::query_scalar("SELECT @@max_statement_time")
            .fetch_one(&mut c)
            .await
            .unwrap();
        let readonly: i64 = sqlx::query_scalar("SELECT CAST(@@tx_read_only AS SIGNED)")
            .fetch_one(&mut c)
            .await
            .unwrap();
        assert_eq!(timeout, 5.0);
        assert_eq!(readonly, i64::from(!write));
    }
}

/// Run against fresh disposable databases, never an operator's existing DB.
#[tokio::test]
#[ignore = "requires disposable FIRECRAB_TEST_POSTGRES_URI and FIRECRAB_TEST_MYSQL_URI databases"]
async fn reads_real_postgres_and_mysql_tables() {
    for (variable, engine, schema) in [
        ("FIRECRAB_TEST_POSTGRES_URI", Engine::Postgres, "public"),
        ("FIRECRAB_TEST_MYSQL_URI", Engine::Mysql, "pipe_test"),
    ] {
        let uri = std::env::var(variable).expect("disposable test database URI is required");
        let spec = input(&uri, Provider::Local);
        let settings = spec.settings(Uuid::nil()).unwrap();
        let table_name = "pipe\"`fixture";
        let table = format!("{}.{}", quote(engine, schema), quote(engine, table_name));
        let column = quote(engine, "odd\"`name");
        match engine {
            Engine::Postgres => {
                let mut connection = PgConnection::connect(&uri).await.unwrap();
                sqlx::raw_sql(&format!("CREATE TABLE {table} (id integer, {column} text, payload bytea); INSERT INTO {table} VALUES (1, 'café', decode('0102', 'hex')), (2, NULL, NULL)"))
                    .execute(&mut connection).await.unwrap();
            }
            Engine::Mysql => {
                let mut connection = MySqlConnection::connect(&uri).await.unwrap();
                sqlx::raw_sql(&format!("CREATE TABLE {table} (id integer, {column} text, payload blob); INSERT INTO {table} VALUES (1, 'café', X'0102'), (2, NULL, NULL)"))
                    .execute(&mut connection).await.unwrap();
            }
        }
        let mut connection = DatabaseConnection::connect(&spec, &settings).await.unwrap();
        connection.ping().await.unwrap();
        let tables = connection.tables().await.unwrap();
        assert!(
            tables
                .tables
                .iter()
                .any(|table| table.schema == schema && table.name == table_name)
        );
        let mut request = PreviewRequest {
            schema: schema.into(),
            table: table_name.into(),
            limit: 50,
            offset: 0,
        };
        let preview = connection.preview(&request).await.unwrap();
        assert_eq!(preview.columns[1].name, "odd\"`name");
        assert_eq!(preview.rows.len(), 2);
        assert!(!preview.has_more);
        assert!(
            preview
                .rows
                .iter()
                .any(|row| row[1].as_deref() == Some("café"))
        );
        assert!(
            preview
                .rows
                .iter()
                .any(|row| row[1].is_none() && row[2].is_none())
        );
        let binary = if engine == Engine::Postgres {
            "\\x0102"
        } else {
            "0102"
        };
        assert!(
            preview
                .rows
                .iter()
                .any(|row| row[2].as_deref() == Some(binary))
        );
        request.limit = 1;
        assert!(connection.preview(&request).await.unwrap().has_more);
        request.offset = 100;
        assert!(connection.preview(&request).await.unwrap().rows.is_empty());
    }
}

#[test]
fn supabase_transaction_pooling_is_read_only_and_spatial_previews_stay_binary() {
    let uri = format!(
        "postgresql://user:{}@aws-0-ap-northeast-2.pooler.supabase.com:6543/postgres",
        "qa-fixture"
    );
    let mut connection = input(&uri, Provider::Supabase);
    assert!(connection.settings(Uuid::nil()).is_ok());
    connection.write_enabled = true;
    assert!(connection.settings(Uuid::nil()).is_err());
    for kind in [
        "geometry",
        "point",
        "linestring",
        "polygon",
        "multipoint",
        "multilinestring",
        "multipolygon",
        "geomcollection",
        "geometrycollection",
    ] {
        let sql = preview_sql(
            Engine::Mysql,
            &PreviewRequest {
                schema: "app".into(),
                table: "places".into(),
                limit: 50,
                offset: 0,
            },
            &[Column {
                name: "shape".into(),
                data_type: kind.into(),
            }],
        );
        assert!(sql.contains("HEX(`shape`)"), "{kind}: {sql}");
    }
}
