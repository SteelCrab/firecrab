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

/// Maximum number of base tables returned by one metadata request.
const MAX_TABLES: usize = 500;
/// Maximum number of columns accepted for a row preview.
const MAX_COLUMNS: usize = 128;
/// Maximum text length returned for each preview cell.
const MAX_CELL_CHARS: usize = 2048;
/// Maximum serialized row data collected for one preview response.
const MAX_PREVIEW_BYTES: usize = 2 * 1024 * 1024;
/// Deadline for connecting and completing one database operation.
pub const OPERATION_TIMEOUT: Duration = Duration::from_secs(8);

/// Connection source and its engine and TLS requirements.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// An operator-supplied MySQL or PostgreSQL TCP URI.
    Local,
    /// A Supabase PostgreSQL endpoint or session pooler.
    Supabase,
    /// An Amazon RDS MySQL or PostgreSQL endpoint.
    Rds,
}

/// SQL dialect and connection driver selected from the URI scheme.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    /// PostgreSQL with native text conversion for preview values.
    Postgres,
    /// MySQL, including compatible servers that support read-only sessions.
    Mysql,
}

/// Encryption and server identity policy for a database connection.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    /// Plain TCP, accepted only for the local URI source.
    Disable,
    /// Require encryption without checking the server certificate identity.
    Require,
    /// Verify the certificate chain and the requested hostname.
    VerifyFull,
}

/// Connection form input containing credentials; Debug output is redacted.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DatabaseInput {
    /// Display name, trimmed before it appears in a summary.
    pub name: String,
    /// Source used to validate the engine and default TLS policy.
    pub provider: Provider,
    /// TCP database URI, including percent-encoded credentials.
    pub uri: String,
    /// Explicit TLS choice, overriding the URI and provider default.
    #[serde(default)]
    pub tls_mode: Option<TlsMode>,
    /// Optional PEM certificate bundle supplied directly, never a file path.
    #[serde(default)]
    pub ca_certificate: Option<String>,
}

impl std::fmt::Debug for DatabaseInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DatabaseInput { credentials: [REDACTED] }")
    }
}

/// Persisted connection input and the identifier used by REST routes.
#[derive(Debug, Clone)]
pub struct DatabaseRecord {
    /// Stable identifier assigned when the connection is saved.
    pub id: Uuid,
    /// Saved configuration with credentials, retained only on the API host.
    pub input: DatabaseInput,
}

/// Credential-free connection metadata returned to dashboard clients.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseSummary {
    /// Identifier used to browse or remove this saved connection.
    pub id: Uuid,
    /// Human-readable connection name.
    pub name: String,
    /// Selected connection source.
    pub provider: Provider,
    /// Driver selected from the validated URI scheme.
    pub engine: Engine,
    /// TCP hostname or IP address without IPv6 brackets.
    pub host: String,
    /// Explicit port, or the engine's default TCP port.
    pub port: u16,
    /// Decoded database name from the URI path.
    pub database: String,
    /// Effective encryption and server verification policy.
    pub tls_mode: TlsMode,
}

/// Validated connection fields kept separate from serializable metadata.
pub struct ConnectionSettings {
    /// Safe metadata that can be returned after authentication succeeds.
    pub summary: DatabaseSummary,
    /// Decoded account name, excluded from dashboard summaries.
    username: String,
    /// Decoded password, excluded from Debug output and API responses.
    password: String,
}

/// Decode one URI component and reject invalid UTF-8.
fn decode(value: &str) -> Result<String, &'static str> {
    percent_decode_str(value)
        .decode_utf8()
        .map(|v| v.into_owned())
        .map_err(|_| "URI components must contain valid UTF-8")
}

impl DatabaseInput {
    /// Validate input and resolve engine, credentials, port and TLS policy.
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
        if let Some(ca) = &self.ca_certificate
            && (ca.len() > 32 * 1024
                || !ca.trim().starts_with("-----BEGIN CERTIFICATE-----")
                || !ca.trim().ends_with("-----END CERTIFICATE-----"))
        {
            return Err("CA certificate must be PEM text of at most 32 KiB");
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

/// Accessible base table identified by its schema and exact name.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// Schema name, or database name for MySQL.
    pub schema: String,
    /// Table name preserved for dialect-specific identifier quoting.
    pub name: String,
}

/// Bounded table listing with an explicit truncation indicator.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tables {
    /// Accessible base tables in metadata sort order.
    pub tables: Vec<Table>,
    /// Whether additional tables were omitted by the listing limit.
    pub truncated: bool,
}

/// Column metadata used to render and convert preview values.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    /// Exact column identifier returned by the server.
    pub name: String,
    /// Server-reported SQL type used for text or binary conversion.
    pub data_type: String,
}

/// Table selection and bounded pagination input; arbitrary SQL is rejected.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewRequest {
    /// Exact schema identifier to look up before generating a query.
    pub schema: String,
    /// Exact table identifier to look up before generating a query.
    pub table: String,
    /// Requested row count, validated in the range 1 through 100.
    #[serde(default = "default_limit")]
    pub limit: u32,
    /// Number of rows to skip, capped at 100000.
    #[serde(default)]
    pub offset: u32,
}
/// Default page size used when a REST request omits its limit.
fn default_limit() -> u32 {
    50
}

impl PreviewRequest {
    /// Reject invalid identifiers and pagination before opening a connection.
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

/// Bounded row values and metadata returned by one read-only query.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    /// Columns in the same ordinal order as each row's values.
    pub columns: Vec<Column>,
    /// Text or hex values, with SQL NULL preserved as None.
    pub rows: Vec<Vec<Option<String>>>,
    /// Whether another row exists beyond the row or response-size limit.
    pub has_more: bool,
    /// Starting offset echoed from the validated request.
    pub offset: u32,
}

/// Single database session configured for read-only inspection.
pub enum DatabaseConnection {
    /// PostgreSQL session with read-only startup options and a statement timeout.
    Postgres(PgConnection),
    /// MySQL session switched to read-only transaction mode after connecting.
    Mysql(MySqlConnection),
}

impl DatabaseConnection {
    /// Open a TCP session from validated fields without reading URI file paths.
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

    /// Verify that the authenticated session is responsive.
    pub async fn ping(&mut self) -> Result<(), sqlx::Error> {
        match self {
            Self::Postgres(c) => c.ping().await,
            Self::Mysql(c) => c.ping().await,
        }
    }

    /// List accessible base tables and report truncation after 500 entries.
    pub async fn tables(&mut self) -> Result<Tables, sqlx::Error> {
        let pairs: Vec<(String, String)> = match self {
            Self::Postgres(c) => sqlx::query_as("SELECT table_schema::text, table_name::text FROM information_schema.tables WHERE table_type = 'BASE TABLE' AND table_schema NOT IN ('pg_catalog', 'information_schema') ORDER BY table_schema, table_name LIMIT 501")
                .persistent(false).fetch_all(c).await?,
            // Normalize metadata to nonbinary text before decoding it as String.
            Self::Mysql(c) => sqlx::query_as("SELECT CAST(TABLE_SCHEMA AS CHAR CHARACTER SET utf8mb4), CAST(TABLE_NAME AS CHAR CHARACTER SET utf8mb4) FROM information_schema.tables WHERE TABLE_SCHEMA = DATABASE() AND TABLE_TYPE = 'BASE TABLE' ORDER BY TABLE_NAME LIMIT 501")
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

    /// Inspect columns and stream a bounded page of converted row values.
    pub async fn preview(&mut self, request: &PreviewRequest) -> Result<Preview, sqlx::Error> {
        let fields: Vec<(String, String)> = match self {
            Self::Postgres(c) => sqlx::query_as("SELECT column_name::text, data_type::text FROM information_schema.columns WHERE table_schema = $1 AND table_name = $2 ORDER BY ordinal_position LIMIT 129")
                .bind(&request.schema).bind(&request.table).persistent(false).fetch_all(c).await?,
            Self::Mysql(c) => sqlx::query_as("SELECT CAST(COLUMN_NAME AS CHAR CHARACTER SET utf8mb4), CAST(DATA_TYPE AS CHAR CHARACTER SET utf8mb4) FROM information_schema.columns WHERE TABLE_SCHEMA = DATABASE() AND TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION LIMIT 129")
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

/// Append a row while enforcing the requested count and serialized byte budget.
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

/// Quote an exact SQL identifier using the selected engine's delimiter.
fn quote(engine: Engine, name: &str) -> String {
    let delimiter = if engine == Engine::Postgres { '"' } else { '`' };
    format!(
        "{delimiter}{}{delimiter}",
        name.replace(delimiter, &format!("{delimiter}{delimiter}"))
    )
}

/// Generate a SELECT with bounded cell conversion and bound pagination values.
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

/// Convert a safe validation message into the database form's API error field.
pub fn validation(message: &'static str, request_id: Uuid) -> crate::error::AppError {
    crate::error::AppError::validation(
        BTreeMap::from([("database".into(), message.into())]),
        request_id,
    )
}

#[cfg(test)]
#[path = "pipe_databases_tests.rs"]
mod tests;
