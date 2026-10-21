//! Saved external databases. Only metadata and bounded SELECTs cross the API.

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::TryStreamExt;
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use sqlx::mysql::{MySqlConnectOptions, MySqlConnection, MySqlSslMode};
use sqlx::postgres::{PgConnectOptions, PgConnection, PgSslMode};
use sqlx::{ConnectOptions, Connection, Row};
use uuid::Uuid;

const MAX_TABLES: usize = 500;
const MAX_COLUMNS: usize = 128;
const MAX_CELL_CHARS: usize = 2048;
const MAX_PREVIEW_BYTES: usize = 2 * 1024 * 1024;
pub const OPERATION_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Local,
    Supabase,
    Rds,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    Postgres,
    Mysql,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    Disable,
    Require,
    VerifyFull,
}

/// Never derive Debug: the URI contains a password.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DatabaseInput {
    pub name: String,
    pub provider: Provider,
    pub uri: String,
    #[serde(default)]
    pub tls_mode: Option<TlsMode>,
    #[serde(default)]
    pub ca_certificate: Option<String>,
}

impl std::fmt::Debug for DatabaseInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DatabaseInput { credentials: [REDACTED] }")
    }
}

#[derive(Debug, Clone)]
pub struct DatabaseRecord {
    pub id: Uuid,
    pub input: DatabaseInput,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseSummary {
    pub id: Uuid,
    pub name: String,
    pub provider: Provider,
    pub engine: Engine,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub tls_mode: TlsMode,
}

pub struct ConnectionSettings {
    pub summary: DatabaseSummary,
    username: String,
    password: String,
}

fn decode(value: &str) -> Result<String, &'static str> {
    percent_decode_str(value)
        .decode_utf8()
        .map(|v| v.into_owned())
        .map_err(|_| "URI components must contain valid UTF-8")
}

impl DatabaseInput {
    pub fn settings(&self, id: Uuid) -> Result<ConnectionSettings, &'static str> {
        if self.name.trim().is_empty()
            || self.name.chars().count() > 100
            || self.name.chars().any(char::is_control)
        {
            return Err("Name must contain 1-100 characters without control characters");
        }
        if self.uri.len() > 8192 || self.uri.chars().any(char::is_whitespace) {
            return Err(
                "URI must be at most 8192 characters; percent-encode spaces and reserved characters",
            );
        }
        let url =
            reqwest::Url::parse(&self.uri).map_err(|_| "Enter a valid MySQL or PostgreSQL URI")?;
        let engine = match url.scheme() {
            "postgres" | "postgresql" => Engine::Postgres,
            "mysql" => Engine::Mysql,
            _ => return Err("URI must start with mysql://, postgres:// or postgresql://"),
        };
        if self.provider == Provider::Supabase && engine != Engine::Postgres {
            return Err("Supabase requires a PostgreSQL URI");
        }
        let host = url
            .host_str()
            .filter(|host| !host.is_empty())
            .ok_or("URI must include a TCP hostname or IP address")?;
        if host.contains('%') || url.fragment().is_some() || url.port() == Some(0) {
            return Err("URI must contain a TCP host, a valid port and no fragment");
        }
        let username = decode(url.username())?;
        let password = decode(url.password().unwrap_or(""))?;
        let database = decode(url.path().strip_prefix('/').unwrap_or(url.path()))?;
        if username.is_empty() || database.is_empty() || database.contains('/') {
            return Err("URI must include a username and one database name");
        }
        if [&username, &password, &database]
            .iter()
            .any(|value| value.contains('\0'))
        {
            return Err("URI components cannot contain null characters");
        }
        let mut uri_tls = None;
        for (key, value) in url.query_pairs() {
            if !matches!(key.as_ref(), "sslmode" | "ssl-mode") || uri_tls.is_some() {
                return Err(
                    "Only one sslmode or ssl-mode URI parameter is supported; paste CA certificates in the CA field",
                );
            }
            uri_tls = Some(match value.to_ascii_lowercase().as_str() {
                "disable" | "disabled" => TlsMode::Disable,
                "require" | "required" => TlsMode::Require,
                "verify-full" | "verify_identity" => TlsMode::VerifyFull,
                _ => return Err("URI TLS mode must be disable, require or verify-full"),
            });
        }
        let tls_mode = self
            .tls_mode
            .or(uri_tls)
            .unwrap_or(if self.provider == Provider::Local {
                TlsMode::Disable
            } else {
                TlsMode::VerifyFull
            });
        if self.provider != Provider::Local && tls_mode == TlsMode::Disable {
            return Err("Supabase and Amazon RDS connections require TLS");
        }
        if let Some(ca) = &self.ca_certificate {
            if ca.len() > 32 * 1024
                || !ca.trim().starts_with("-----BEGIN CERTIFICATE-----")
                || !ca.trim().ends_with("-----END CERTIFICATE-----")
            {
                return Err("CA certificate must be PEM text of at most 32 KiB");
            }
        }
        Ok(ConnectionSettings {
            summary: DatabaseSummary {
                id,
                name: self.name.trim().to_owned(),
                provider: self.provider,
                engine,
                host: host.trim_matches(['[', ']']).to_owned(),
                port: url.port().unwrap_or(if engine == Engine::Postgres {
                    5432
                } else {
                    3306
                }),
                database,
                tls_mode,
            },
            username,
            password,
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    pub schema: String,
    pub name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tables {
    pub tables: Vec<Table>,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    pub name: String,
    pub data_type: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewRequest {
    pub schema: String,
    pub table: String,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}
fn default_limit() -> u32 {
    50
}

impl PreviewRequest {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema.is_empty()
            || self.table.is_empty()
            || self.schema.len() > 256
            || self.table.len() > 256
            || self.schema.contains('\0')
            || self.table.contains('\0')
        {
            return Err("Select a valid schema and table");
        }
        if !(1..=100).contains(&self.limit) || self.offset > 100_000 {
            return Err("Preview limit must be 1-100 and offset must be at most 100000");
        }
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Option<String>>>,
    pub has_more: bool,
    pub offset: u32,
}

pub enum DatabaseConnection {
    Postgres(PgConnection),
    Mysql(MySqlConnection),
}

impl DatabaseConnection {
    pub async fn connect(
        input: &DatabaseInput,
        settings: &ConnectionSettings,
    ) -> Result<Self, sqlx::Error> {
        let summary = &settings.summary;
        match summary.engine {
            Engine::Postgres => {
                // Construct from explicit fields. Never read a URI-supplied file or .pgpass.
                let mut options = PgConnectOptions::new_without_pgpass()
                    .host(&summary.host)
                    .port(summary.port)
                    .database(&summary.database)
                    .username(&settings.username)
                    .password(&settings.password)
                    .ssl_mode(match summary.tls_mode {
                        TlsMode::Disable => PgSslMode::Disable,
                        TlsMode::Require => PgSslMode::Require,
                        TlsMode::VerifyFull => PgSslMode::VerifyFull,
                    })
                    .statement_cache_capacity(0)
                    .options([
                        ("statement_timeout", "5000"),
                        ("default_transaction_read_only", "on"),
                    ])
                    .disable_statement_logging();
                if let Some(ca) = &input.ca_certificate {
                    options = options.ssl_root_cert_from_pem(ca.as_bytes().to_vec());
                }
                PgConnection::connect_with(&options)
                    .await
                    .map(Self::Postgres)
            }
            Engine::Mysql => {
                let mut options = MySqlConnectOptions::new()
                    .host(&summary.host)
                    .port(summary.port)
                    .database(&summary.database)
                    .username(&settings.username)
                    .password(&settings.password)
                    .ssl_mode(match summary.tls_mode {
                        TlsMode::Disable => MySqlSslMode::Disabled,
                        TlsMode::Require => MySqlSslMode::Required,
                        TlsMode::VerifyFull => MySqlSslMode::VerifyIdentity,
                    })
                    .statement_cache_capacity(0)
                    .disable_statement_logging();
                if let Some(ca) = &input.ca_certificate {
                    options = options.ssl_ca_from_pem(ca.as_bytes().to_vec());
                }
                let mut connection = MySqlConnection::connect_with(&options).await?;
                sqlx::query("SET SESSION TRANSACTION READ ONLY")
                    .execute(&mut connection)
                    .await?;
                Ok(Self::Mysql(connection))
            }
        }
    }

    pub async fn ping(&mut self) -> Result<(), sqlx::Error> {
        match self {
            Self::Postgres(c) => c.ping().await,
            Self::Mysql(c) => c.ping().await,
        }
    }

    pub async fn tables(&mut self) -> Result<Tables, sqlx::Error> {
        let pairs: Vec<(String, String)> = match self {
            Self::Postgres(c) => sqlx::query_as("SELECT table_schema::text, table_name::text FROM information_schema.tables WHERE table_type = 'BASE TABLE' AND table_schema NOT IN ('pg_catalog', 'information_schema') ORDER BY table_schema, table_name LIMIT 501")
                .persistent(false).fetch_all(c).await?,
            Self::Mysql(c) => sqlx::query_as("SELECT TABLE_SCHEMA, TABLE_NAME FROM information_schema.tables WHERE TABLE_SCHEMA = DATABASE() AND TABLE_TYPE = 'BASE TABLE' ORDER BY TABLE_NAME LIMIT 501")
                .persistent(false).fetch_all(c).await?,
        };
        let truncated = pairs.len() > MAX_TABLES;
        Ok(Tables {
            tables: pairs
                .into_iter()
                .take(MAX_TABLES)
                .map(|(schema, name)| Table { schema, name })
                .collect(),
            truncated,
        })
    }

    pub async fn preview(&mut self, request: &PreviewRequest) -> Result<Preview, sqlx::Error> {
        let fields: Vec<(String, String)> = match self {
            Self::Postgres(c) => sqlx::query_as("SELECT column_name::text, data_type::text FROM information_schema.columns WHERE table_schema = $1 AND table_name = $2 ORDER BY ordinal_position LIMIT 129")
                .bind(&request.schema).bind(&request.table).persistent(false).fetch_all(c).await?,
            Self::Mysql(c) => sqlx::query_as("SELECT COLUMN_NAME, DATA_TYPE FROM information_schema.columns WHERE TABLE_SCHEMA = DATABASE() AND TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION LIMIT 129")
                .bind(&request.schema).bind(&request.table).persistent(false).fetch_all(c).await?,
        };
        if fields.is_empty() {
            return Err(sqlx::Error::RowNotFound);
        }
        if fields.len() > MAX_COLUMNS {
            return Err(sqlx::Error::Protocol("too_many_columns".into()));
        }
        let columns: Vec<Column> = fields
            .into_iter()
            .map(|(name, data_type)| Column { name, data_type })
            .collect();
        let engine = match self {
            Self::Postgres(_) => Engine::Postgres,
            Self::Mysql(_) => Engine::Mysql,
        };
        let query = preview_sql(engine, request, &columns);
        let mut preview = Preview {
            columns,
            rows: Vec::new(),
            has_more: false,
            offset: request.offset,
        };
        let mut bytes = 0;
        match self {
            Self::Postgres(c) => {
                let mut stream = sqlx::query(&query)
                    .bind(i64::from(request.limit) + 1)
                    .bind(i64::from(request.offset))
                    .persistent(false)
                    .fetch(c);
                while let Some(row) = stream.try_next().await? {
                    let cells = (0..preview.columns.len())
                        .map(|i| row.try_get(i))
                        .collect::<Result<Vec<Option<String>>, _>>()?;
                    if !append_row(&mut preview, cells, request.limit, &mut bytes) {
                        break;
                    }
                }
            }
            Self::Mysql(c) => {
                let mut stream = sqlx::query(&query)
                    .bind(request.limit + 1)
                    .bind(request.offset)
                    .persistent(false)
                    .fetch(c);
                while let Some(row) = stream.try_next().await? {
                    let cells = (0..preview.columns.len())
                        .map(|i| row.try_get(i))
                        .collect::<Result<Vec<Option<String>>, _>>()?;
                    if !append_row(&mut preview, cells, request.limit, &mut bytes) {
                        break;
                    }
                }
            }
        }
        Ok(preview)
    }
}

fn append_row(
    preview: &mut Preview,
    row: Vec<Option<String>>,
    limit: u32,
    bytes: &mut usize,
) -> bool {
    // Count escaped JSON bytes, not just input bytes.
    let size = serde_json::to_vec(&row).map_or(MAX_PREVIEW_BYTES, |value| value.len());
    if preview.rows.len() >= limit as usize || *bytes + size > MAX_PREVIEW_BYTES {
        preview.has_more = true;
        return false;
    }
    *bytes += size;
    preview.rows.push(row);
    true
}

fn quote(engine: Engine, name: &str) -> String {
    let delimiter = if engine == Engine::Postgres { '"' } else { '`' };
    format!(
        "{delimiter}{}{delimiter}",
        name.replace(delimiter, &format!("{delimiter}{delimiter}"))
    )
}

fn preview_sql(engine: Engine, request: &PreviewRequest, columns: &[Column]) -> String {
    let fields = columns
        .iter()
        .map(|column| {
            let name = quote(engine, &column.name);
            match engine {
                Engine::Postgres => format!("LEFT({name}::text, {MAX_CELL_CHARS})"),
                Engine::Mysql => {
                    let value = if matches!(
                        column.data_type.as_str(),
                        "binary"
                            | "varbinary"
                            | "tinyblob"
                            | "blob"
                            | "mediumblob"
                            | "longblob"
                            | "bit"
                            | "geometry"
                    ) {
                        format!("HEX({name})")
                    } else {
                        format!("CAST({name} AS CHAR CHARACTER SET utf8mb4)")
                    };
                    format!("LEFT({value}, {MAX_CELL_CHARS})")
                }
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let table = format!(
        "{}.{}",
        quote(engine, &request.schema),
        quote(engine, &request.table)
    );
    match engine {
        Engine::Postgres => format!("SELECT {fields} FROM {table} LIMIT $1 OFFSET $2"),
        Engine::Mysql => {
            format!("SELECT /*+ MAX_EXECUTION_TIME(5000) */ {fields} FROM {table} LIMIT ? OFFSET ?")
        }
    }
}

pub fn validation(message: &'static str, request_id: Uuid) -> crate::error::AppError {
    crate::error::AppError::validation(
        BTreeMap::from([("database".into(), message.into())]),
        request_id,
    )
}
