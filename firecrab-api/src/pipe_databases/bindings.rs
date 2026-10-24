//! Per-service accounts and private guest inputs. No credentials in service specs.
use super::{
    DatabaseConnection, DatabaseInput, DatabaseRecord, Engine, TlsMode, quote,
    write::{self, Error, TableRequest},
};
use crate::persistence::{PersistenceError, Store};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Privileges granted to one generated service account.
pub enum Access {
    Read,
    Write,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Minimal service binding intent with a stable retry ID.
pub struct BindingInput {
    /// Client UUID reused for retries of the same operation.
    pub request_id: Uuid,
    /// MicroService that receives the connection or events.
    pub service_id: Uuid,
    /// Privileges selected for the generated service account.
    pub access: Access,
    #[serde(default = "env_name")]
    /// Guest environment variable that receives the private URI.
    pub env_name: String,
}
/// Default private URI variable for database drivers.
fn env_name() -> String {
    "DATABASE_URL".into()
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
/// Private binding state, account credentials and granted tables.
pub struct BindingRecord {
    /// Stable UUID for this owned resource.
    pub id: Uuid,
    /// Saved source connection that owns this resource.
    pub database_id: Uuid,
    /// Original setup intent used to detect conflicting retries.
    pub input: BindingInput,
    /// Generated account credentials; private host storage only.
    pub connection: DatabaseInput,
    /// Generated account name whose ownership must be verified.
    pub role: String,
    /// Application tables covered by this operation.
    pub tables: Vec<TableRequest>,
    /// True only after account creation and grants finish.
    pub ready: bool,
    #[serde(default)]
    /// Grant only INSERT on the generated deduplication inbox for event handlers.
    pub event_inbox: bool,
}
impl std::fmt::Debug for BindingRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BindingRecord")
            .field("id", &self.id)
            .field("ready", &self.ready)
            .finish_non_exhaustive()
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
/// Public binding metadata with credentials removed.
pub struct BindingSummary {
    /// Stable UUID for this owned resource.
    pub id: Uuid,
    /// Saved source connection that owns this resource.
    pub database_id: Uuid,
    #[serde(flatten)]
    /// Original setup intent used to detect conflicting retries.
    pub input: BindingInput,
    /// True only after account creation and grants finish.
    pub ready: bool,
    /// Number of application tables granted to the account.
    pub table_count: usize,
}
impl BindingRecord {
    /// Return public metadata without credentials or private SQL.
    pub fn summary(&self) -> BindingSummary {
        BindingSummary {
            id: self.id,
            database_id: self.database_id,
            input: self.input.clone(),
            ready: self.ready,
            table_count: self.tables.len(),
        }
    }
    /// Validate a binding and generate a separate private service account.
    pub async fn new(record: &DatabaseRecord, input: BindingInput) -> Result<Self, Error> {
        if input.request_id.is_nil()
            || input.service_id.is_nil()
            || input.env_name.len() > 64
            || !input
                .env_name
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_uppercase() || b == b'_')
            || !input
                .env_name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            || matches!(
                input.env_name.as_str(),
                "PATH" | "HOME" | "LD_PRELOAD" | "LD_LIBRARY_PATH" | "FIRECRAB_EVENT_SECRET"
            )
        {
            return Err(Error::Invalid(
                "Choose a service and a valid environment variable name",
            ));
        }
        if input.access == Access::Write && !record.input.write_enabled {
            return Err(Error::Invalid(
                "Enable database writes before adding a write binding",
            ));
        }
        let settings = record.input.settings(record.id).map_err(Error::Invalid)?;
        if settings.summary.host.ends_with(".pooler.supabase.com") && settings.summary.port == 6543
        {
            return Err(Error::Invalid(
                "Service bindings require a direct or session-pool database endpoint",
            ));
        }
        let mut connection = DatabaseConnection::connect(&record.input, &settings).await?;
        let tables = connection.tables().await?;
        if tables.truncated || tables.tables.is_empty() {
            return Err(Error::Invalid(
                "Bindings require between 1 and 500 accessible application tables",
            ));
        }
        let tables = tables
            .tables
            .into_iter()
            .filter(|t| !(t.schema.starts_with("firecrab_") && t.schema.len() == 41))
            .map(|t| TableRequest {
                schema: t.schema,
                table: t.name,
            })
            .collect::<Vec<_>>();
        if tables.is_empty() {
            return Err(Error::Invalid("No application tables are available"));
        }
        if input.access == Access::Write && settings.summary.engine == Engine::Mysql {
            for table in &tables {
                if !connection.table_info(table).await?.transactional {
                    return Err(Error::Invalid("Write bindings require InnoDB tables"));
                }
            }
        }
        let role = format!("fc{}", &input.request_id.simple().to_string()[..30]);
        let password = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let mut url = reqwest::Url::parse(&record.input.uri)
            .map_err(|_| Error::Invalid("Invalid saved URI"))?;
        url.set_username(&role)
            .map_err(|_| Error::Invalid("Invalid account name"))?;
        url.set_password(Some(&password))
            .map_err(|_| Error::Invalid("Invalid account password"))?;
        let mut private = record.input.clone();
        private.tls_mode = Some(settings.summary.tls_mode);
        let mut query = url
            .query_pairs()
            .filter(|(key, _)| !matches!(key.as_ref(), "sslmode" | "ssl-mode"))
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect::<Vec<_>>();
        let mode = match (settings.summary.engine, settings.summary.tls_mode) {
            (_, TlsMode::Disable) => "disable",
            (_, TlsMode::Require) => "require",
            (Engine::Postgres, TlsMode::VerifyFull) => "verify-full",
            (Engine::Mysql, TlsMode::VerifyFull) => "verify_identity",
        };
        query.push((
            if settings.summary.engine == Engine::Mysql {
                "ssl-mode"
            } else {
                "sslmode"
            }
            .into(),
            mode.into(),
        ));
        url.set_query(None);
        url.query_pairs_mut().extend_pairs(query);
        private.uri = url.into();
        Ok(Self {
            id: input.request_id,
            database_id: record.id,
            input,
            connection: private,
            role,
            tables,
            ready: false,
            event_inbox: false,
        })
    }
}

/// Create only an owned account and grant its selected application privileges.
pub async fn provision(admin: &DatabaseRecord, binding: &BindingRecord) -> Result<(), Error> {
    let mut privileged = admin.input.clone();
    privileged.write_enabled = true;
    let settings = privileged.settings(admin.id).map_err(Error::Invalid)?;
    let mut connection = DatabaseConnection::connect_write(&privileged, &settings).await?;
    write::initialize(&mut connection, admin.id).await?;
    let private = binding
        .connection
        .settings(binding.id)
        .map_err(Error::Invalid)?;
    let password = &private.password;
    let privileges = if binding.input.access == Access::Read {
        "SELECT"
    } else {
        "SELECT, INSERT, UPDATE, DELETE"
    };
    let marker = format!("firecrab-binding:{}", binding.id);
    match &mut connection {
        DatabaseConnection::Postgres(c) => {
            let mut tx = write::pg_transaction(c).await?;
            let existing: Option<Option<String>> = sqlx::query_scalar(
                "SELECT shobj_description(oid,'pg_authid') FROM pg_roles WHERE rolname=$1",
            )
            .bind(&binding.role)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(comment) = existing {
                if comment.as_deref() != Some(&marker) {
                    return Err(Error::Invalid(
                        "The service account already exists and is not owned by this binding",
                    ));
                }
            } else {
                sqlx::query(&format!("CREATE ROLE {} LOGIN PASSWORD '{password}' NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT CONNECTION LIMIT 20",quote(Engine::Postgres,&binding.role))).execute(&mut *tx).await?;
                sqlx::query(&format!(
                    "COMMENT ON ROLE {} IS '{marker}'",
                    quote(Engine::Postgres, &binding.role)
                ))
                .execute(&mut *tx)
                .await?;
            }
            let role = quote(Engine::Postgres, &binding.role);
            if binding.input.access == Access::Write || binding.event_inbox {
                let schema = quote(Engine::Postgres, &write::internal_schema(admin.id));
                let inbox = format!(
                    "{schema}.{}",
                    quote(Engine::Postgres, &format!("inbox_{}", binding.id.simple()))
                );
                sqlx::query(&format!("CREATE TABLE IF NOT EXISTS {inbox}(event_id varchar(128) PRIMARY KEY,received_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP)")).execute(&mut *tx).await?;
                sqlx::query(&format!("GRANT USAGE ON SCHEMA {schema} TO {role}"))
                    .execute(&mut *tx)
                    .await?;
                sqlx::query(&format!(
                    "GRANT SELECT,INSERT(event_id) ON {inbox} TO {role}"
                ))
                .execute(&mut *tx)
                .await?;
            }
            sqlx::query(&format!(
                "GRANT CONNECT ON DATABASE {} TO {role}",
                quote(Engine::Postgres, &settings.summary.database)
            ))
            .execute(&mut *tx)
            .await?;
            for table in &binding.tables {
                table.validate()?;
                sqlx::query(&format!(
                    "GRANT USAGE ON SCHEMA {} TO {role}",
                    quote(Engine::Postgres, &table.schema)
                ))
                .execute(&mut *tx)
                .await?;
                sqlx::query(&format!(
                    "GRANT {privileges} ON TABLE {} TO {role}",
                    table.qualified(Engine::Postgres)
                ))
                .execute(&mut *tx)
                .await?;
                if binding.input.access == Access::Write {
                    // Only sequences owned by columns of a granted table are included.
                    let sequences:Vec<String>=sqlx::query_scalar("SELECT pg_get_serial_sequence(format('%I.%I',table_schema,table_name),column_name) FROM information_schema.columns WHERE table_schema=$1 AND table_name=$2 AND (column_default LIKE 'nextval(%' OR is_identity='YES')").bind(&table.schema).bind(&table.table).fetch_all(&mut *tx).await?;
                    for sequence in sequences {
                        sqlx::query(&format!(
                            "GRANT USAGE, SELECT ON SEQUENCE {sequence} TO {role}"
                        ))
                        .execute(&mut *tx)
                        .await?;
                    }
                }
            }
            tx.commit().await?;
        }
        DatabaseConnection::Mysql(c) => {
            let role = format!("'{}'@'%'", binding.role);
            let existing:Option<String>=sqlx::query_scalar("SELECT CAST(ATTRIBUTE AS CHAR CHARACTER SET utf8mb4) FROM information_schema.user_attributes WHERE USER=? AND HOST='%'").bind(&binding.role).fetch_optional(&mut *c).await?;
            if let Some(attributes) = existing {
                let owned = serde_json::from_str::<serde_json::Value>(&attributes)
                    .ok()
                    .is_some_and(|v| v["firecrab"] == binding.id.to_string());
                if !owned {
                    return Err(Error::Invalid(
                        "The service account already exists and is not owned by this binding",
                    ));
                }
            } else {
                let tls = if settings.summary.tls_mode == TlsMode::Disable {
                    ""
                } else {
                    " REQUIRE SSL"
                };
                write::mysql_command(&mut *c,&format!("CREATE USER {role} IDENTIFIED BY '{password}'{tls} WITH MAX_USER_CONNECTIONS 20 ATTRIBUTE '{{\"firecrab\":\"{}\"}}'",binding.id)).await?;
            }
            for table in &binding.tables {
                table.validate()?;
                write::mysql_command(
                    &mut *c,
                    &format!(
                        "GRANT {privileges} ON {} TO {role}",
                        table.qualified(Engine::Mysql)
                    ),
                )
                .await?;
            }
            if binding.input.access == Access::Write || binding.event_inbox {
                let inbox = format!(
                    "{}.{}",
                    quote(Engine::Mysql, &write::internal_schema(admin.id)),
                    quote(Engine::Mysql, &format!("inbox_{}", binding.id.simple()))
                );
                sqlx::query(&format!("CREATE TABLE IF NOT EXISTS {inbox}(event_id varchar(128) CHARACTER SET ascii COLLATE ascii_bin PRIMARY KEY,received_at timestamp(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6)) ENGINE=InnoDB")).execute(&mut *c).await?;
                write::mysql_command(
                    c,
                    &format!("GRANT SELECT,INSERT(event_id) ON {inbox} TO {role}"),
                )
                .await?;
            }
        }
    }
    // Successful role creation alone is not a usable binding. Verify its TLS login.
    DatabaseConnection::connect(&binding.connection, &private)
        .await?
        .ping()
        .await?;
    Ok(())
}

/// Revoke an owned account without removing source application data.
pub async fn revoke(admin: &DatabaseRecord, binding: &BindingRecord) -> Result<(), Error> {
    let mut privileged = admin.input.clone();
    privileged.write_enabled = true;
    let settings = privileged.settings(admin.id).map_err(Error::Invalid)?;
    let mut connection = DatabaseConnection::connect_write(&privileged, &settings).await?;
    match &mut connection {
        DatabaseConnection::Postgres(c) => {
            let comment: Option<Option<String>> = sqlx::query_scalar(
                "SELECT shobj_description(oid,'pg_authid') FROM pg_roles WHERE rolname=$1",
            )
            .bind(&binding.role)
            .fetch_optional(&mut *c)
            .await?;
            if comment.is_none() {
                return Ok(());
            }
            if comment.flatten().as_deref() != Some(&format!("firecrab-binding:{}", binding.id)) {
                return Err(Error::Invalid(
                    "Cannot remove an account owned by another binding",
                ));
            }
            let role = quote(Engine::Postgres, &binding.role);
            // These generated roles cannot own application objects. RESTRICT prevents data loss.
            let mut tx = write::pg_transaction(c).await?;
            for table in &binding.tables {
                let present: bool = sqlx::query_scalar(
                    "SELECT to_regclass(format('%I.%I',$1::text,$2::text)) IS NOT NULL",
                )
                .bind(&table.schema)
                .bind(&table.table)
                .fetch_one(&mut *tx)
                .await?;
                if present {
                    sqlx::query(&format!(
                        "REVOKE ALL ON {} FROM {role}",
                        table.qualified(Engine::Postgres)
                    ))
                    .execute(&mut *tx)
                    .await?;
                }
                let schema_present: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname=$1)",
                )
                .bind(&table.schema)
                .fetch_one(&mut *tx)
                .await?;
                if !schema_present {
                    continue;
                }
                sqlx::query(&format!(
                    "REVOKE USAGE ON SCHEMA {} FROM {role}",
                    quote(Engine::Postgres, &table.schema)
                ))
                .execute(&mut *tx)
                .await?;
            }
            // DROP OWNED is deliberately avoided; owned objects must never be deleted here.
            sqlx::query(&format!("ALTER ROLE {role} NOLOGIN"))
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
        DatabaseConnection::Mysql(c) => {
            let attrs:Option<String>=sqlx::query_scalar("SELECT CAST(ATTRIBUTE AS CHAR CHARACTER SET utf8mb4) FROM information_schema.user_attributes WHERE USER=? AND HOST='%'").bind(&binding.role).fetch_optional(&mut *c).await?;
            if attrs.is_none() {
                return Ok(());
            }
            if !attrs
                .and_then(|a| serde_json::from_str::<serde_json::Value>(&a).ok())
                .is_some_and(|v| v["firecrab"] == binding.id.to_string())
            {
                return Err(Error::Invalid(
                    "Cannot remove an account owned by another binding",
                ));
            }
            write::mysql_command(c, &format!("DROP USER '{}'@'%'", binding.role)).await?;
        }
    }
    Ok(())
}

/// Private guest environment and public certificate files.
type GuestInputs = (BTreeMap<String, String>, BTreeMap<String, String>);

/// The caller writes this only into the private guest file, never into VmRecord.env.
pub fn guest_inputs(store: &Store, vm: Uuid) -> Result<GuestInputs, PersistenceError> {
    let mut env = BTreeMap::new();
    let mut files = BTreeMap::new();
    if let Some(cluster) = store.managed_database_rabbitmq()?
        && let Some(broker) = cluster.brokers.into_iter().find(|b| b.id == vm)
    {
        return Ok((broker.env, broker.files));
    }
    let Some(service) = store.database_vm_service(vm)? else {
        return Ok((env, files));
    };
    env.insert(
        "FIRECRAB_EVENT_SECRET".into(),
        store.database_service_secret(service)?,
    );
    for binding in store
        .database_bindings()?
        .into_iter()
        .filter(|b| b.input.service_id == service && b.ready)
    {
        let ca_path = format!("/etc/firecrab/database-{}.pem", binding.id.simple());
        let mut uri = binding.connection.uri.clone();
        if let Some(ca) = &binding.connection.ca_certificate {
            files.insert(ca_path.clone(), ca.clone());
            if let Ok(mut url) = reqwest::Url::parse(&uri) {
                if url.scheme() != "mysql" {
                    url.query_pairs_mut().append_pair("sslrootcert", &ca_path);
                } else {
                    url.query_pairs_mut().append_pair("ssl-ca", &ca_path);
                }
                uri = url.into();
            }
            env.insert(
                format!("{}_CA_CERT", binding.input.env_name),
                ca_path.clone(),
            );
            if binding.input.env_name == "DATABASE_URL" {
                env.insert("PGSSLROOTCERT".into(), ca_path.clone());
                env.insert("MYSQL_SSL_CA".into(), ca_path);
            }
        }
        if let Some(mode) = binding.connection.tls_mode
            && binding.input.env_name == "DATABASE_URL"
        {
            env.insert(
                "PGSSLMODE".into(),
                match mode {
                    TlsMode::Disable => "disable",
                    TlsMode::Require => "require",
                    TlsMode::VerifyFull => "verify-full",
                }
                .into(),
            );
        }
        env.insert(binding.input.env_name.clone(), uri);
        if binding.input.access == Access::Write || binding.event_inbox {
            let engine = binding
                .connection
                .settings(binding.id)
                .map_err(|_| PersistenceError::CorruptRecord {
                    id: binding.id.to_string(),
                    reason: "invalid binding".into(),
                })?
                .summary
                .engine;
            let name = if binding.input.env_name == "DATABASE_URL" {
                "FIRECRAB_INBOX_TABLE".into()
            } else {
                format!("{}_INBOX_TABLE", binding.input.env_name)
            };
            env.insert(
                name,
                format!(
                    "{}.{}",
                    quote(engine, &write::internal_schema(binding.database_id)),
                    quote(engine, &format!("inbox_{}", binding.id.simple()))
                ),
            );
        }
    }
    Ok((env, files))
}
