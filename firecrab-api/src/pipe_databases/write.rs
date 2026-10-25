//! Typed row operations. Primary keys identify rows; fingerprints reject stale edits.

use super::{
    DatabaseConnection, Engine, MAX_CELL_CHARS, MAX_COLUMNS, MAX_PREVIEW_BYTES, PreviewRequest,
    quote,
};
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{Connection, Row};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
/// Typed write failures whose public messages do not expose row values.
pub enum Error {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("The row changed; refresh it before saving")]
    Conflict,
    #[error("The request ID was already used for another operation")]
    RequestConflict,
    #[error("The table or row no longer exists")]
    NotFound,
    #[error("Database operation failed")]
    Database(#[from] sqlx::Error),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
/// Validated schema and table selection without arbitrary SQL.
pub struct TableRequest {
    /// Exact source schema, or MySQL database name.
    pub schema: String,
    /// Exact source table name.
    pub table: String,
}

impl TableRequest {
    /// Reject unsafe identifiers or an inconsistent mutation before SQL runs.
    pub fn validate(&self) -> Result<(), Error> {
        for identifier in [&self.schema, &self.table] {
            if identifier.is_empty() || identifier.len() > 256 || identifier.contains('\0') {
                return Err(Error::Invalid("Select a valid schema and table"));
            }
        }
        if matches!(
            self.schema.to_ascii_lowercase().as_str(),
            "pg_catalog" | "information_schema" | "mysql" | "sys" | "performance_schema"
        ) || (self.schema.starts_with("firecrab_") && self.schema.len() == 41)
        {
            return Err(Error::Invalid("System tables cannot be edited"));
        }
        Ok(())
    }

    /// Quote schema and table names for the selected SQL dialect.
    pub fn qualified(&self, engine: Engine) -> String {
        format!(
            "{}.{}",
            quote(engine, &self.schema),
            quote(engine, &self.table)
        )
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
/// Column capabilities needed for safe text-based editing.
pub struct ColumnInfo {
    /// Exact column name returned by server metadata.
    pub name: String,
    /// Server-rendered SQL type used for display.
    pub data_type: String,
    /// Base type without modifiers; the target column enforces length limits.
    #[serde(skip)]
    cast_type: String,
    /// Whether SQL NULL is accepted by this column.
    pub nullable: bool,
    /// Server default expression; omitted input keeps that default.
    pub default_value: Option<String>,
    /// Identity, generated or automatic fields cannot be edited.
    pub generated: bool,
    /// Whether this column belongs to the primary key.
    pub primary_key: bool,
    /// Whether the value uses binary input and preview handling.
    pub binary: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
/// Editable table metadata and transactional capability.
pub struct TableInfo {
    /// Inspected columns in their physical table order.
    pub columns: Vec<ColumnInfo>,
    /// Primary-key column names in physical table order.
    pub primary_key: Vec<String>,
    /// Whether row changes can safely share an audit transaction.
    pub transactional: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
/// A bounded editable page selected by table and offset.
pub struct PageRequest {
    #[serde(flatten)]
    /// Validated table whose rows are requested.
    pub table: TableRequest,
    #[serde(default)]
    /// Bounded row offset for this page.
    pub offset: u32,
    #[serde(default = "page_limit")]
    /// Maximum rows requested, capped by the preview policy.
    pub limit: u32,
}

/// Default editable page size.
fn page_limit() -> u32 {
    50
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
/// Display values plus original key and conflict fingerprint.
pub struct EditableRow {
    /// Text field values or SQL NULL; JSON cells contain raw JSON text.
    pub values: BTreeMap<String, Option<String>>,
    /// Original primary-key text values used to identify one row.
    pub key: BTreeMap<String, Value>,
    /// Original server-side row digest used for conflict detection.
    pub version: String,
    /// Display fields too large for safe console editing.
    pub truncated_columns: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
/// Bounded editable rows and table capabilities.
pub struct Page {
    /// Inspected metadata required to validate row changes.
    pub definition: TableInfo,
    /// Bounded result rows with their original keys and fingerprints.
    pub rows: Vec<EditableRow>,
    /// Bounded row offset for this page.
    pub offset: u32,
    /// Another row was observed beyond this page.
    pub has_more: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// One audited row mutation.
pub enum Operation {
    Insert,
    Update,
    Delete,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
/// Retryable change identified by UUID, primary key and row version.
pub struct Mutation {
    /// Client UUID reused for retries of the same operation.
    pub request_id: Uuid,
    #[serde(flatten)]
    /// Validated source schema and table selection.
    pub table: TableRequest,
    /// Insert, update or delete operation.
    pub operation: Operation,
    #[serde(default)]
    /// Text field values or SQL NULL; JSON cells contain raw JSON text.
    pub values: BTreeMap<String, Value>,
    #[serde(default)]
    /// Original primary-key text values used to identify one row.
    pub key: BTreeMap<String, Value>,
    /// Original row digest required for updates and deletes.
    pub version: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
/// Committed mutation result without sensitive field values.
pub struct Receipt {
    /// Client UUID reused for retries of the same operation.
    pub request_id: Uuid,
    /// Insert, update or delete operation.
    pub operation: Operation,
    /// Exact source schema, or MySQL database name.
    pub schema: String,
    /// Source table identifier, or a validated table selection.
    pub table: String,
    /// Exactly one committed application row.
    pub affected_rows: u64,
}

impl DatabaseConnection {
    pub(super) fn engine(&self) -> Engine {
        match self {
            Self::Postgres(_) => Engine::Postgres,
            Self::Mysql(_) => Engine::Mysql,
        }
    }

    pub async fn table_info(&mut self, table: &TableRequest) -> Result<TableInfo, Error> {
        table.validate()?;
        let mut columns = Vec::new();
        let transactional;
        match self {
            Self::Postgres(connection) => {
                let rows = sqlx::query("SELECT a.attname::text AS name, format_type(a.atttypid, a.atttypmod)::text AS kind, format('%I.%I',tn.nspname,t.typname)::text AS cast_kind, NOT a.attnotnull AS nullable, pg_get_expr(d.adbin,d.adrelid)::text AS default_value, (a.attgenerated <> '' OR a.attidentity <> '') AS generated, EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid=c.oid AND i.indisprimary AND a.attnum=ANY(i.indkey)) AS primary_key FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid JOIN pg_namespace n ON n.oid=c.relnamespace JOIN pg_type t ON t.oid=a.atttypid JOIN pg_namespace tn ON tn.oid=t.typnamespace LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum WHERE n.nspname=$1 AND c.relname=$2 AND c.relkind IN ('r','p') AND a.attnum>0 AND NOT a.attisdropped ORDER BY a.attnum LIMIT 129")
                    .bind(&table.schema).bind(&table.table).persistent(false).fetch_all(connection).await?;
                for row in rows {
                    let data_type: String = row.try_get("kind")?;
                    columns.push(ColumnInfo {
                        name: row.try_get("name")?,
                        binary: data_type == "bytea",
                        cast_type: row.try_get("cast_kind")?,
                        data_type,
                        nullable: row.try_get("nullable")?,
                        default_value: row.try_get("default_value")?,
                        generated: row.try_get("generated")?,
                        primary_key: row.try_get("primary_key")?,
                    });
                }
                transactional = true;
            }
            Self::Mysql(connection) => {
                let engine: Option<String> = sqlx::query_scalar("SELECT CAST(ENGINE AS CHAR CHARACTER SET utf8mb4) FROM information_schema.tables WHERE TABLE_SCHEMA=DATABASE() AND TABLE_SCHEMA=? AND TABLE_NAME=? AND TABLE_TYPE='BASE TABLE'")
                    .bind(&table.schema).bind(&table.table).persistent(false).fetch_optional(&mut *connection).await?;
                transactional = engine
                    .as_deref()
                    .is_some_and(|value| value.eq_ignore_ascii_case("innodb"));
                let rows = sqlx::query("SELECT CAST(COLUMN_NAME AS CHAR CHARACTER SET utf8mb4) AS name, CAST(DATA_TYPE AS CHAR CHARACTER SET utf8mb4) AS kind, CAST(IS_NULLABLE AS CHAR CHARACTER SET utf8mb4) AS nullable, CAST(COLUMN_DEFAULT AS CHAR CHARACTER SET utf8mb4) AS default_value, CAST(EXTRA AS CHAR CHARACTER SET utf8mb4) AS extra, CAST(COLUMN_KEY AS CHAR CHARACTER SET utf8mb4) AS column_key FROM information_schema.columns WHERE TABLE_SCHEMA=DATABASE() AND TABLE_SCHEMA=? AND TABLE_NAME=? ORDER BY ORDINAL_POSITION LIMIT 129")
                    .bind(&table.schema).bind(&table.table).persistent(false).fetch_all(connection).await?;
                for row in rows {
                    let data_type: String = row.try_get("kind")?;
                    let extra: String = row.try_get("extra")?;
                    columns.push(ColumnInfo {
                        name: row.try_get("name")?,
                        binary: mysql_binary(&data_type),
                        cast_type: data_type.clone(),
                        data_type,
                        nullable: row.try_get::<String, _>("nullable")? == "YES",
                        default_value: row.try_get("default_value")?,
                        generated: (extra.contains("VIRTUAL GENERATED")
                            || extra.contains("STORED GENERATED"))
                            || extra.contains("auto_increment"),
                        primary_key: row.try_get::<String, _>("column_key")? == "PRI",
                    });
                }
            }
        }
        if columns.is_empty() {
            return Err(Error::NotFound);
        }
        if columns.len() > MAX_COLUMNS {
            return Err(Error::Invalid(
                "Tables with more than 128 columns cannot be edited",
            ));
        }
        let primary_key = columns
            .iter()
            .filter(|column| column.primary_key)
            .map(|column| column.name.clone())
            .collect();
        Ok(TableInfo {
            columns,
            primary_key,
            transactional,
        })
    }

    pub async fn editable_page(&mut self, request: &PageRequest) -> Result<Page, Error> {
        PreviewRequest {
            schema: request.table.schema.clone(),
            table: request.table.table.clone(),
            limit: request.limit,
            offset: request.offset,
        }
        .validate()
        .map_err(Error::Invalid)?;
        let definition = self.table_info(&request.table).await?;
        let engine = self.engine();
        let fields = definition
            .columns
            .iter()
            .map(|column| {
                format!(
                    "LEFT({}, {})",
                    text_value(engine, column),
                    MAX_CELL_CHARS + 1
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let keys = definition
            .columns
            .iter()
            .filter(|column| column.primary_key)
            .map(|column| {
                format!(
                    "LEFT({}, {})",
                    text_value(engine, column),
                    MAX_CELL_CHARS + 1
                )
            })
            .collect::<Vec<_>>();
        let key_projection = if keys.is_empty() {
            String::new()
        } else {
            format!(", {}", keys.join(", "))
        };
        let order = if keys.is_empty() {
            String::new()
        } else {
            format!(
                " ORDER BY {}",
                definition
                    .primary_key
                    .iter()
                    .map(|name| quote(engine, name))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let placeholders = if engine == Engine::Postgres {
            "$1 OFFSET $2"
        } else {
            "? OFFSET ?"
        };
        let sql = format!(
            "SELECT {fields}{key_projection}, {} FROM {} AS __fc_row{order} LIMIT {placeholders}",
            fingerprint(engine, &definition.columns),
            request.table.qualified(engine)
        );
        let projected = definition.columns.len() + keys.len() + 1;
        let mut page = Page {
            definition,
            rows: vec![],
            offset: request.offset,
            has_more: false,
        };
        let mut bytes = 0;
        match self {
            Self::Postgres(c) => {
                let mut stream = sqlx::query(&sql)
                    .bind(i64::from(request.limit) + 1)
                    .bind(i64::from(request.offset))
                    .persistent(false)
                    .fetch(c);
                while let Some(row) = stream.try_next().await? {
                    let values = (0..projected)
                        .map(|index| row.try_get(index))
                        .collect::<Result<Vec<Option<String>>, _>>()?;
                    if !collect_row(&mut page, values, request.limit, &mut bytes)? {
                        break;
                    }
                }
            }
            Self::Mysql(c) => {
                let mut stream = sqlx::query(&sql)
                    .bind(request.limit + 1)
                    .bind(request.offset)
                    .persistent(false)
                    .fetch(c);
                while let Some(row) = stream.try_next().await? {
                    let values = (0..projected)
                        .map(|index| row.try_get(index))
                        .collect::<Result<Vec<Option<String>>, _>>()?;
                    if !collect_row(&mut page, values, request.limit, &mut bytes)? {
                        break;
                    }
                }
            }
        }
        Ok(page)
    }
}

/// Append one display row while enforcing cell, key and response limits.
fn collect_row(
    page: &mut Page,
    row: Vec<Option<String>>,
    limit: u32,
    bytes: &mut usize,
) -> Result<bool, Error> {
    if page.rows.len() >= limit as usize {
        page.has_more = true;
        return Ok(false);
    }
    let mut values = BTreeMap::new();
    let mut truncated_columns = Vec::new();
    for (column, value) in page.definition.columns.iter().zip(&row) {
        let value = value.as_ref().map(|value| {
            if value.chars().count() > MAX_CELL_CHARS {
                truncated_columns.push(column.name.clone());
            }
            value.chars().take(MAX_CELL_CHARS).collect::<String>()
        });
        values.insert(column.name.clone(), value);
    }
    let mut key: BTreeMap<_, _> = page
        .definition
        .primary_key
        .iter()
        .enumerate()
        .map(|(index, name)| {
            (
                name.clone(),
                row[page.definition.columns.len() + index]
                    .clone()
                    .map_or(Value::Null, Value::String),
            )
        })
        .collect();
    let mut version = row.last().and_then(Clone::clone).unwrap_or_default();
    if key.values().any(|value| {
        value
            .as_str()
            .is_some_and(|text| text.chars().count() > MAX_CELL_CHARS)
    }) {
        key.clear();
        version.clear();
    }
    let item = EditableRow {
        values,
        key,
        version,
        truncated_columns,
    };
    let size = serde_json::to_vec(&item)
        .map_err(|_| Error::Invalid("Cannot encode this row"))?
        .len();
    if *bytes + size > MAX_PREVIEW_BYTES {
        page.has_more = true;
        return Ok(false);
    }
    *bytes += size;
    page.rows.push(item);
    Ok(true)
}

/// Identify MySQL types that require hexadecimal conversion.
fn mysql_binary(kind: &str) -> bool {
    matches!(
        kind,
        "binary"
            | "varbinary"
            | "tinyblob"
            | "blob"
            | "mediumblob"
            | "longblob"
            | "bit"
            | "geometry"
            | "point"
            | "linestring"
            | "polygon"
            | "multipoint"
            | "multilinestring"
            | "multipolygon"
            | "geomcollection"
            | "geometrycollection"
    )
}

/// Identify spatial types unsupported by console field editing.
fn spatial(kind: &str) -> bool {
    matches!(
        kind,
        "geometry"
            | "point"
            | "linestring"
            | "polygon"
            | "multipoint"
            | "multilinestring"
            | "multipolygon"
            | "geomcollection"
            | "geometrycollection"
    )
}

/// Render one inspected column as lossless display text where supported.
fn text_value(engine: Engine, column: &ColumnInfo) -> String {
    let name = quote(engine, &column.name);
    match engine {
        Engine::Postgres => format!("{name}::text"),
        Engine::Mysql if column.binary => format!("HEX({name})"),
        Engine::Mysql => format!("CAST({name} AS CHAR CHARACTER SET utf8mb4)"),
    }
}

/// Build the server-side row digest used to reject stale edits.
fn fingerprint(engine: Engine, columns: &[ColumnInfo]) -> String {
    match engine {
        Engine::Postgres => {
            "encode(sha256(convert_to(to_jsonb(__fc_row)::text,'UTF8')),'hex')".into()
        }
        Engine::Mysql => {
            let values = columns
                .iter()
                .map(|column| {
                    format!(
                        "{}, {}",
                        mysql_literal(&column.name),
                        text_value(engine, column)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("SHA2(CAST(JSON_OBJECT({values}) AS CHAR CHARACTER SET utf8mb4),256)")
        }
    }
}

// Hex literals are independent of the server's NO_BACKSLASH_ESCAPES mode.
/// Encode internal MySQL string literals independently of SQL modes.
pub(super) fn mysql_literal(value: &str) -> String {
    let hex = value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("CONVERT(X'{hex}' USING utf8mb4)")
}

/// The internal namespace is owned by one saved connection, not by user input.
pub(super) fn internal_schema(id: Uuid) -> String {
    format!("firecrab_{}", id.simple())
}

/// Create an owned audit namespace or verify and repair its completed setup.
pub async fn initialize(connection: &mut DatabaseConnection, id: Uuid) -> Result<(), Error> {
    initialize_claimed(connection, id, false).await
}

/// Repair cancelled MySQL creation only when this host recorded prior absence.
pub async fn initialize_saved(
    connection: &mut DatabaseConnection,
    record: &super::DatabaseRecord,
    store: &crate::persistence::Store,
) -> Result<(), Error> {
    if let DatabaseConnection::Mysql(c) = connection {
        let target = super::maintenance::target_hash(record)?;
        let exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.schemata WHERE SCHEMA_NAME=?",
        )
        .bind(internal_schema(record.id))
        .fetch_one(c)
        .await?;
        if exists == 0 {
            store
                .set_database_setup_claim(record.id, &target)
                .map_err(|_| Error::Invalid("Cannot save database setup ownership"))?;
        }
        let claimed = store
            .database_setup_claim(record.id, &target)
            .map_err(|_| Error::Invalid("Cannot load database setup ownership"))?;
        initialize_claimed(connection, record.id, claimed).await?;
        store
            .clear_database_setup_claim(record.id, &target)
            .map_err(|_| Error::Invalid("Cannot finish database setup ownership"))?;
        Ok(())
    } else {
        initialize(connection, record.id).await
    }
}

/// Never repair an unmarked namespace without a durable creation claim.
async fn initialize_claimed(
    connection: &mut DatabaseConnection,
    id: Uuid,
    claimed: bool,
) -> Result<(), Error> {
    let engine = connection.engine();
    let schema = quote(engine, &internal_schema(id));
    match connection {
        DatabaseConnection::Postgres(c) => {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname=$1)")
                    .bind(internal_schema(id))
                    .fetch_one(&mut *c)
                    .await?;
            if exists {
                return verify_owner(connection, id).await;
            }
            let mut tx = pg_transaction(c).await?;
            pg_batch(&mut tx,&format!("CREATE SCHEMA {schema}; REVOKE ALL ON SCHEMA {schema} FROM PUBLIC; CREATE TABLE {schema}.owner (id text PRIMARY KEY); INSERT INTO {schema}.owner VALUES ('{id}'); CREATE TABLE {schema}.audit (request_id text PRIMARY KEY, request_hash text NOT NULL, receipt text NOT NULL, created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP)")).await?;
            tx.commit().await?;
        }
        DatabaseConnection::Mysql(c) => {
            let exists: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM information_schema.schemata WHERE SCHEMA_NAME=?",
            )
            .bind(internal_schema(id))
            .fetch_one(&mut *c)
            .await?;
            if exists != 0 {
                if claimed {
                    let other_tables: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.tables WHERE TABLE_SCHEMA=? AND TABLE_NAME<>'owner'")
                        .bind(internal_schema(id)).fetch_one(&mut *c).await?;
                    if other_tables == 0 {
                        sqlx::query(&format!("CREATE TABLE IF NOT EXISTS {schema}.owner (id char(36) PRIMARY KEY) ENGINE=InnoDB"))
                            .execute(&mut *c).await?;
                        sqlx::query(&format!("INSERT INTO {schema}.owner (id) SELECT ? FROM DUAL WHERE NOT EXISTS (SELECT 1 FROM {schema}.owner)"))
                            .bind(id.to_string()).execute(&mut *c).await?;
                    }
                }
                verify_owner(connection, id).await?;
                let DatabaseConnection::Mysql(c) = connection else {
                    unreachable!()
                };
                sqlx::query(&format!("CREATE TABLE IF NOT EXISTS {schema}.audit (request_id char(36) PRIMARY KEY, request_hash char(64) NOT NULL, receipt longtext NOT NULL, created_at timestamp(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6)) ENGINE=InnoDB")).execute(c).await?;
                return Ok(());
            }
            sqlx::query(&format!("CREATE DATABASE {schema}"))
                .execute(&mut *c)
                .await?;
            sqlx::query(&format!(
                "CREATE TABLE {schema}.owner (id char(36) PRIMARY KEY) ENGINE=InnoDB"
            ))
            .execute(&mut *c)
            .await?;
            sqlx::query(&format!("INSERT INTO {schema}.owner VALUES (?)"))
                .bind(id.to_string())
                .execute(&mut *c)
                .await?;
            sqlx::query(&format!("CREATE TABLE {schema}.audit (request_id char(36) PRIMARY KEY, request_hash char(64) NOT NULL, receipt longtext NOT NULL, created_at timestamp(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6)) ENGINE=InnoDB")).persistent(false).execute(c).await?;
        }
    }
    Ok(())
}

/// Pin a writable PostgreSQL transaction and enforce local execution bounds.
pub(super) async fn pg_transaction(
    connection: &mut sqlx::PgConnection,
) -> Result<sqlx::Transaction<'_, sqlx::Postgres>, sqlx::Error> {
    let mut transaction = connection.begin_with("BEGIN READ WRITE").await?;
    pg_batch(
        &mut transaction,
        "SET LOCAL statement_timeout=5000; SET LOCAL lock_timeout=5000; SET LOCAL timezone='UTC'",
    )
    .await?;
    Ok(transaction)
}

/// Execute internally rendered PostgreSQL DDL on the caller transaction.
pub(super) async fn pg_batch(
    connection: &mut sqlx::PgConnection,
    sql: &str,
) -> Result<(), sqlx::Error> {
    // Execute simple-protocol SQL on one concrete connection. The caller owns
    // the transaction; PostgreSQL functions can contain statement separators.
    sqlx::Executor::execute(connection, sql).await?;
    Ok(())
}

/// Use simple protocol for internally rendered MySQL account and trigger DDL.
pub(super) async fn mysql_command(
    connection: &mut sqlx::MySqlConnection,
    sql: &str,
) -> Result<(), sqlx::Error> {
    // CREATE TRIGGER and account statements are not supported by COM_STMT_PREPARE.
    // Only internally rendered, quoted DDL reaches this simple-protocol path.
    sqlx::Executor::execute(connection, sql).await?;
    Ok(())
}

/// Refuse an internal namespace that does not contain this connection UUID.
pub(super) async fn verify_owner(
    connection: &mut DatabaseConnection,
    id: Uuid,
) -> Result<(), Error> {
    let sql = format!(
        "SELECT id FROM {}.owner LIMIT 2",
        quote(connection.engine(), &internal_schema(id))
    );
    let ids: Vec<String> = match connection {
        DatabaseConnection::Postgres(c) => sqlx::query_scalar(&sql).fetch_all(c).await?,
        DatabaseConnection::Mysql(c) => sqlx::query_scalar(&sql).fetch_all(c).await?,
    };
    if ids != vec![id.to_string()] {
        return Err(Error::Invalid(
            "The internal database namespace is not owned by this connection",
        ));
    }
    Ok(())
}

impl Mutation {
    /// Reject unsafe identifiers or an inconsistent mutation before SQL runs.
    fn validate(&self, definition: &TableInfo) -> Result<(), Error> {
        self.table.validate()?;
        if self.request_id.is_nil() {
            return Err(Error::Invalid("A nonempty request ID is required"));
        }
        if !definition.transactional {
            return Err(Error::Invalid("Writes require a transactional table"));
        }
        if self.values.len() > MAX_COLUMNS {
            return Err(Error::Invalid("Too many fields"));
        }
        for (name, value) in &self.values {
            let column = definition
                .columns
                .iter()
                .find(|column| column.name == *name)
                .ok_or(Error::Invalid(
                    "A column no longer exists; refresh the table",
                ))?;
            if column.generated
                || spatial(&column.data_type)
                || (self.operation == Operation::Update && column.primary_key)
            {
                return Err(Error::Invalid(
                    "Generated, primary-key and spatial fields cannot be changed",
                ));
            }
            if !value.is_null() && !value.is_string() {
                return Err(Error::Invalid("Field values must be text or SQL NULL"));
            }
            if matches!(column.data_type.as_str(), "json" | "jsonb") && !value.is_null() {
                serde_json::from_str::<Box<serde_json::value::RawValue>>(value.as_str().unwrap())
                    .map_err(|_| Error::Invalid("JSON fields require valid JSON text"))?;
            }
            if value.is_null() && !column.nullable {
                return Err(Error::Invalid("A required field cannot be NULL"));
            }
            if column.binary && column.data_type != "bytea" && !value.is_null() {
                let hex = value
                    .as_str()
                    .ok_or(Error::Invalid("Binary fields require hexadecimal text"))?;
                if hex.len() % 2 != 0 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(Error::Invalid("Binary fields require hexadecimal text"));
                }
            }
        }
        if self.operation == Operation::Insert {
            if !self.key.is_empty() || self.version.is_some() {
                return Err(Error::Invalid(
                    "New rows must not include an existing row key",
                ));
            }
            return Ok(());
        }
        if definition.primary_key.is_empty() {
            return Err(Error::Invalid(
                "This table has no primary key; writes are disabled",
            ));
        }
        if self.key.len() != definition.primary_key.len()
            || definition
                .primary_key
                .iter()
                .any(|key| self.key.get(key).is_none_or(Value::is_null))
        {
            return Err(Error::Invalid("Every primary-key field is required"));
        }
        for (name, value) in &self.key {
            let text = value.as_str().ok_or(Error::Invalid(
                "Row keys must use their original text values",
            ))?;
            if text.chars().count() > MAX_CELL_CHARS {
                return Err(Error::Invalid(
                    "This row key is too large for console editing",
                ));
            }
            if definition
                .columns
                .iter()
                .any(|c| c.name == *name && c.binary && c.data_type != "bytea")
                && (text.len() % 2 != 0 || !text.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                return Err(Error::Invalid("Invalid binary row key"));
            }
        }
        if !self.version.as_ref().is_some_and(|value| {
            !value.is_empty() && value.len() <= 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
        }) {
            return Err(Error::Invalid("Refresh the row before editing it"));
        }
        if self.operation == Operation::Update && self.values.is_empty() {
            return Err(Error::Invalid("Select at least one changed field"));
        }
        if self.operation == Operation::Delete && !self.values.is_empty() {
            return Err(Error::Invalid("Delete requests cannot change fields"));
        }
        Ok(())
    }
}

/// Convert a validated cell to bound text while retaining SQL NULL.
fn json_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(if *value { "1" } else { "0" }.into()),
        value => Some(value.to_string()),
    }
}

/// Decode hexadecimal binary input or bind ordinary text.
fn mysql_placeholder(column: &ColumnInfo) -> &'static str {
    if column.binary { "UNHEX(?)" } else { "?" }
}

/// Build equality predicates for every inspected MySQL primary-key field.
fn mysql_key(definition: &TableInfo) -> String {
    definition
        .columns
        .iter()
        .filter(|column| column.primary_key)
        .map(|column| {
            format!(
                "{} <=> {}",
                quote(Engine::Mysql, &column.name),
                mysql_placeholder(column)
            )
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// Cast a text parameter using PostgreSQL-rendered type metadata.
fn postgres_cast(column: &ColumnInfo, position: usize) -> String {
    // Qualified catalog names preserve domains without silently truncating fields.
    format!("CAST(${position}::text AS {})", column.cast_type)
}

/// Build parameterized predicates in physical primary-key order.
fn postgres_key(definition: &TableInfo, offset: usize) -> String {
    definition
        .columns
        .iter()
        .filter(|column| column.primary_key)
        .enumerate()
        .map(|(index, column)| {
            format!(
                "__fc_row.{} IS NOT DISTINCT FROM {}",
                quote(Engine::Postgres, &column.name),
                postgres_cast(column, offset + index + 1)
            )
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// Commit one validated row change and retry receipt in the same transaction.
pub async fn mutate(
    connection: &mut DatabaseConnection,
    id: Uuid,
    request: &Mutation,
) -> Result<Receipt, Error> {
    verify_owner(connection, id).await?;
    let definition = connection.table_info(&request.table).await?;
    request.validate(&definition)?;
    let engine = connection.engine();
    let table = request.table.qualified(engine);
    let audit = format!("{}.audit", quote(engine, &internal_schema(id)));
    let encoded =
        serde_json::to_vec(request).map_err(|_| Error::Invalid("Cannot encode the change"))?;
    let hash = format!("{:x}", Sha256::digest(encoded));
    let request_id = request.request_id.to_string();
    let receipt = Receipt {
        request_id: request.request_id,
        operation: request.operation,
        schema: request.table.schema.clone(),
        table: request.table.table.clone(),
        affected_rows: 1,
    };
    let receipt_json =
        serde_json::to_string(&receipt).map_err(|_| Error::Invalid("Cannot encode the receipt"))?;
    let names = request
        .values
        .keys()
        .map(|name| quote(engine, name))
        .collect::<Vec<_>>();
    match connection {
        DatabaseConnection::Postgres(connection) => {
            let mut transaction = pg_transaction(connection).await?;
            sqlx::query(&format!("INSERT INTO {audit} (request_id,request_hash,receipt) VALUES ($1,$2,'{{}}') ON CONFLICT DO NOTHING")).bind(&request_id).bind(&hash).persistent(false).execute(&mut *transaction).await?;
            let (saved_hash, saved_receipt): (String, String) = sqlx::query_as(&format!(
                "SELECT request_hash,LEFT(receipt,4097) FROM {audit} WHERE request_id=$1 FOR UPDATE"
            ))
            .bind(&request_id)
            .persistent(false)
            .fetch_one(&mut *transaction)
            .await?;
            if saved_hash != hash {
                return Err(Error::RequestConflict);
            }
            if saved_receipt != "{}" {
                return serde_json::from_str(&saved_receipt)
                    .map_err(|_| Error::Invalid("Stored receipt is invalid"));
            }
            let key_columns = definition
                .columns
                .iter()
                .filter(|column| column.primary_key)
                .collect::<Vec<_>>();
            let key = postgres_key(&definition, 0);
            if request.operation != Operation::Insert {
                let sql = format!(
                    "SELECT {} FROM {table} AS __fc_row WHERE {key} FOR UPDATE",
                    fingerprint(engine, &definition.columns)
                );
                let mut query = sqlx::query_scalar::<_, String>(&sql);
                for column in &key_columns {
                    query = query.bind(json_text(&request.key[&column.name]));
                }
                let version = query
                    .persistent(false)
                    .fetch_optional(&mut *transaction)
                    .await?
                    .ok_or(Error::NotFound)?;
                if Some(&version) != request.version.as_ref() {
                    return Err(Error::Conflict);
                }
            }
            let columns = request
                .values
                .keys()
                .map(|name| {
                    definition
                        .columns
                        .iter()
                        .find(|column| column.name == *name)
                        .expect("validated column")
                })
                .collect::<Vec<_>>();
            let sql = match request.operation {
                Operation::Insert if names.is_empty() => {
                    format!("INSERT INTO {table} DEFAULT VALUES")
                }
                Operation::Insert => format!(
                    "INSERT INTO {table} ({}) VALUES ({})",
                    names.join(", "),
                    columns
                        .iter()
                        .enumerate()
                        .map(|(i, c)| postgres_cast(c, i + 1))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                Operation::Update => format!(
                    "UPDATE {table} AS __fc_row SET {} WHERE {}",
                    columns
                        .iter()
                        .enumerate()
                        .map(|(i, c)| format!(
                            "{}={}",
                            quote(engine, &c.name),
                            postgres_cast(c, i + 1)
                        ))
                        .collect::<Vec<_>>()
                        .join(", "),
                    postgres_key(&definition, columns.len())
                ),
                Operation::Delete => format!("DELETE FROM {table} AS __fc_row WHERE {key}"),
            };
            let mut query = sqlx::query(&sql);
            for column in columns {
                query = query.bind(json_text(&request.values[&column.name]));
            }
            if request.operation != Operation::Insert {
                for column in key_columns {
                    query = query.bind(json_text(&request.key[&column.name]));
                }
            }
            let affected = query
                .persistent(false)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if affected != 1 {
                return Err(Error::Conflict);
            }
            sqlx::query(&format!(
                "UPDATE {audit} SET receipt=$2 WHERE request_id=$1"
            ))
            .bind(&request_id)
            .bind(&receipt_json)
            .persistent(false)
            .execute(&mut *transaction)
            .await?;
            transaction.commit().await?;
        }
        DatabaseConnection::Mysql(connection) => {
            let mut transaction = connection.begin().await?;
            sqlx::query(&format!("INSERT INTO {audit} (request_id,request_hash,receipt) VALUES (?,?,'{{}}') ON DUPLICATE KEY UPDATE request_id=request_id")).bind(&request_id).bind(&hash).persistent(false).execute(&mut *transaction).await?;
            let (saved_hash, saved_receipt): (String, String) = sqlx::query_as(&format!(
                "SELECT request_hash,LEFT(receipt,4097) FROM {audit} WHERE request_id=? FOR UPDATE"
            ))
            .bind(&request_id)
            .persistent(false)
            .fetch_one(&mut *transaction)
            .await?;
            if saved_hash != hash {
                return Err(Error::RequestConflict);
            }
            if saved_receipt != "{}" {
                return serde_json::from_str(&saved_receipt)
                    .map_err(|_| Error::Invalid("Stored receipt is invalid"));
            }
            let key_columns = definition
                .columns
                .iter()
                .filter(|column| column.primary_key)
                .collect::<Vec<_>>();
            let key = mysql_key(&definition);
            if request.operation != Operation::Insert {
                let sql = format!(
                    "SELECT {} FROM {table} AS __fc_row WHERE {key} FOR UPDATE",
                    fingerprint(engine, &definition.columns)
                );
                let mut query = sqlx::query_scalar::<_, String>(&sql);
                for column in &key_columns {
                    query = query.bind(json_text(&request.key[&column.name]));
                }
                let version = query
                    .persistent(false)
                    .fetch_optional(&mut *transaction)
                    .await?
                    .ok_or(Error::NotFound)?;
                if Some(&version) != request.version.as_ref() {
                    return Err(Error::Conflict);
                }
            }
            let columns = request
                .values
                .keys()
                .map(|name| {
                    definition
                        .columns
                        .iter()
                        .find(|column| column.name == *name)
                        .expect("validated column")
                })
                .collect::<Vec<_>>();
            let sql = match request.operation {
                Operation::Insert => format!(
                    "INSERT INTO {table} ({}) VALUES ({})",
                    names.join(", "),
                    columns
                        .iter()
                        .map(|column| mysql_placeholder(column))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                Operation::Update => format!(
                    "UPDATE {table} SET {} WHERE {key}",
                    columns
                        .iter()
                        .map(|column| format!(
                            "{}={}",
                            quote(engine, &column.name),
                            mysql_placeholder(column)
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                Operation::Delete => format!("DELETE FROM {table} WHERE {key}"),
            };
            let mut query = sqlx::query(&sql);
            for column in &columns {
                let value = &request.values[&column.name];
                query = query.bind(json_text(value));
            }
            if request.operation != Operation::Insert {
                for column in key_columns {
                    query = query.bind(json_text(&request.key[&column.name]));
                }
            }
            let affected = query
                .persistent(false)
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            // MySQL reports changed rows, so an already-equal update can return zero.
            // The primary-key row was locked and its version checked above.
            if affected != 1 && !(request.operation == Operation::Update && affected == 0) {
                return Err(Error::Conflict);
            }
            sqlx::query(&format!("UPDATE {audit} SET receipt=? WHERE request_id=?"))
                .bind(&receipt_json)
                .bind(&request_id)
                .persistent(false)
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
        }
    }
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn row_requests_preserve_keys_and_reject_unknown_fields() {
        let request = serde_json::json!({"schema":"app","table":"orders","operation":"update","requestId":Uuid::new_v4(),"values":{"note":null},"key":{"id":"42"},"version":"abc123"});
        let decoded: Mutation = serde_json::from_value(request.clone()).unwrap();
        assert_eq!(decoded.key["id"], "42");
        let mut bad = request;
        bad["sql"] = Value::String("DROP TABLE app.orders".into());
        assert!(serde_json::from_value::<Mutation>(bad).is_err());
        let page: PageRequest = serde_json::from_value(
            serde_json::json!({"schema":"app","table":"orders","offset":0,"limit":50}),
        )
        .unwrap();
        assert_eq!(page.table.table, "orders");
    }
    #[test]
    fn mysql_field_literals_do_not_depend_on_sql_mode() {
        let value = mysql_literal("quote'\\backtick`한글");
        assert!(!value.contains('\\'));
        assert!(!value.contains("quote"));
        assert_eq!(mysql_literal("a"), "CONVERT(X'61' USING utf8mb4)");
    }
}
