//! Native logical backups and versioned schema jobs. Driver/CLI output stays private.
use super::{
    DatabaseConnection, DatabaseRecord, Engine, TlsMode, quote,
    write::{self, Error},
};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use uuid::Uuid;
#[cfg(test)]
#[path = "maintenance_tests.rs"]
mod tests;

/// Maximum archive size accepted by logical backup and restore jobs.
const MAX_BACKUP: u64 = 10 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Native database maintenance operation.
pub enum Kind {
    Backup,
    Migration,
    Restore,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
/// Private durable job state and verified backup metadata.
pub struct Job {
    /// Stable UUID for this owned resource.
    pub id: Uuid,
    /// Saved source connection that owns this resource.
    pub database_id: Uuid,
    /// Backup, migration or restore operation.
    pub kind: Kind,
    /// Durable queue, completion or recovery state.
    pub status: String,
    /// Creation time as UTC Unix seconds.
    pub created_at: i64,
    /// Successful snapshot selected for a restore.
    pub backup_id: Option<Uuid>,
    /// Immutable identifier of the schema change.
    pub version: Option<String>,
    /// SHA-256 digest of a completed backup archive.
    pub checksum: Option<String>,
    /// Verified archive size in bytes.
    pub bytes: Option<u64>,
    /// Redacted operation failure, never raw driver diagnostics.
    pub error: Option<String>,
    /// Connection target fingerprint excluding credentials.
    pub target_hash: String,
    #[serde(default)]
    /// Operator migration SQL, kept in private persistence only.
    pub sql: Option<String>,
    #[serde(default)]
    /// Application tables covered by this operation.
    pub tables: Vec<write::TableRequest>,
    #[serde(default)]
    /// True once dependency capture can have been detached.
    pub maintenance_started: bool,
    #[serde(default)]
    /// Fresh snapshot created before destructive maintenance.
    pub safety_backup_id: Option<Uuid>,
    #[serde(default)]
    /// PostgreSQL schema names and owners from the exported snapshot.
    pub schemas: Vec<(String, String)>,
    #[serde(default)]
    /// Snapshot OID used to identify the public schema TOC entry.
    pub public_schema_oid: Option<i64>,
}
impl Job {
    /// Create a queued job bound to the current database target.
    pub fn new(record: &DatabaseRecord, id: Uuid, kind: Kind) -> Result<Self, Error> {
        if id.is_nil() {
            return Err(Error::Invalid("A request ID is required"));
        }
        Ok(Self {
            id,
            database_id: record.id,
            kind,
            status: "queued".into(),
            created_at: chrono::Utc::now().timestamp(),
            backup_id: None,
            version: None,
            checksum: None,
            bytes: None,
            error: None,
            target_hash: target_hash(record)?,
            sql: None,
            tables: vec![],
            maintenance_started: false,
            safety_backup_id: None,
            schemas: vec![],
            public_schema_oid: None,
        })
    }
    /// Return public metadata without credentials or private SQL.
    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({"id":self.id,"databaseId":self.database_id,"kind":self.kind,"status":self.status,"createdAt":self.created_at,"backupId":self.backup_id,"safetyBackupId":self.safety_backup_id,"version":self.version,"checksum":self.checksum,"bytes":self.bytes,"error":self.error})
    }
}
/// Hash engine, endpoint and database so credentials can rotate independently.
pub(crate) fn target_hash(record: &DatabaseRecord) -> Result<String, Error> {
    let s = record
        .input
        .settings(record.id)
        .map_err(Error::Invalid)?
        .summary;
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(s.engine, s.host, s.port, s.database))
                .map_err(|_| Error::Invalid("Invalid database target"))?
        )
    ))
}
/// Save private job progress and recovery state.
fn persist(state: &AppState, job: &Job) -> Result<(), Error> {
    state
        .store
        .save_database_job(job)
        .map_err(|_| Error::Invalid("Cannot save database job"))
}
/// Create an owner-readable file without overwriting an existing artifact.
fn private_file(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| Error::Invalid("Cannot create the private job file"))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| Error::Invalid("Cannot save the private job file"))
}
/// Create or verify private host backup directories for one job.
fn directory(state: &AppState, id: Uuid) -> Result<PathBuf, Error> {
    let root = state
        .store
        .database_backup_directory()
        .map_err(|_| Error::Invalid("Cannot locate backup storage"))?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)
        .map_err(|_| Error::Invalid("Cannot create backup storage"))?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .map_err(|_| Error::Invalid("Cannot protect backup storage"))?;
    let dir = root.join(id.to_string());
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .map_err(|_| Error::Invalid("The job directory already exists or cannot be created"))?;
    Ok(dir)
}
/// Resolve an archive under the private host backup directory.
pub fn backup_path(state: &AppState, id: Uuid) -> Result<PathBuf, Error> {
    Ok(state
        .store
        .database_backup_directory()
        .map_err(|_| Error::Invalid("Cannot locate backup storage"))?
        .join(id.to_string())
        .join("database.dump"))
}

/// Probe an installed native client with bounded output and execution time.
async fn version(program: &str) -> Option<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(3),
        Command::new(program)
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .chars()
            .take(160)
            .collect(),
    )
}
/// Report installed backup clients without returning private connection data.
pub async fn capabilities() -> serde_json::Value {
    let (pg_dump, pg_restore, psql, mysql, mysqldump) = tokio::join!(
        version("pg_dump"),
        version("pg_restore"),
        version("psql"),
        version("mysql"),
        version("mysqldump")
    );
    serde_json::json!({"postgres":pg_dump.is_some()&&pg_restore.is_some()&&psql.is_some(),"mysql":mysql.is_some()&&mysqldump.is_some(),"pgDump":pg_dump,"mysqlDump":mysqldump})
}
/// Escape a PostgreSQL database name as a conninfo value, not URI options.
fn conninfo(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// Build a native client command with private credentials outside its arguments.
async fn native(record: &DatabaseRecord, program: &str, dir: &Path) -> Result<Command, Error> {
    let settings = record.input.settings(record.id).map_err(Error::Invalid)?;
    let s = &settings.summary;
    let mut command = Command::new(program);
    command
        .kill_on_drop(true)
        .env_clear()
        .env(
            "PATH",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
        )
        .env("HOME", dir)
        .env("LANG", "C.UTF-8")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    let ca = if let Some(pem) = &record.input.ca_certificate {
        let path = dir.join("ca.pem");
        if !path.exists() {
            private_file(&path, pem.as_bytes())?;
        }
        Some(path)
    } else {
        [
            "/etc/ssl/certs/ca-certificates.crt",
            "/etc/pki/tls/certs/ca-bundle.crt",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
    };
    if s.engine == Engine::Postgres {
        command
            .env("PGHOST", &s.host)
            .env("PGPORT", s.port.to_string())
            .env("PGUSER", &settings.username)
            .env("PGPASSWORD", &settings.password)
            .env("PGCONNECT_TIMEOUT", "5")
            .env("PGPASSFILE", "/dev/null")
            .env("PGOPTIONS", "-c lock_timeout=5000")
            .env(
                "PGSSLMODE",
                match s.tls_mode {
                    TlsMode::Disable => "disable",
                    TlsMode::Require => "require",
                    TlsMode::VerifyFull => "verify-full",
                },
            );
        if let Some(ca) = ca {
            command.env("PGSSLROOTCERT", ca);
        }
        command
            .arg("--no-password")
            .arg("--dbname")
            .arg(format!("dbname={}", conninfo(&s.database)));
    } else {
        let flavor = version(program).await.ok_or(Error::Invalid(
            "Install current PostgreSQL and MySQL client tools on the API host",
        ))?;
        command
            .arg("--no-defaults")
            .arg("--protocol=TCP")
            .arg(format!("--host={}", s.host))
            .arg(format!("--port={}", s.port))
            .arg(format!("--user={}", settings.username))
            .arg("--default-character-set=utf8mb4")
            .env("MYSQL_PWD", &settings.password);
        if program == "mysql" {
            command.arg("--connect-timeout=5");
        }
        if flavor.to_ascii_lowercase().contains("mariadb") {
            if s.tls_mode == TlsMode::Require {
                return Err(Error::Invalid(
                    "MariaDB backup clients require verified TLS; select Verify identity with a trusted CA, or install Oracle MySQL clients",
                ));
            }
            command.arg(if s.tls_mode == TlsMode::Disable {
                "--skip-ssl"
            } else {
                "--ssl"
            });
            if s.tls_mode == TlsMode::VerifyFull {
                command.arg("--ssl-verify-server-cert");
            }
        } else {
            command.arg(format!(
                "--ssl-mode={}",
                match s.tls_mode {
                    TlsMode::Disable => "DISABLED",
                    TlsMode::Require => "REQUIRED",
                    TlsMode::VerifyFull => "VERIFY_IDENTITY",
                }
            ));
        }
        if s.tls_mode != TlsMode::Disable
            && let Some(ca) = ca
        {
            command.arg(format!("--ssl-ca={}", ca.display()));
        }
        if program == "mysqldump" && !flavor.to_ascii_lowercase().contains("mariadb") {
            command.arg("--set-gtid-purged=OFF");
        }
    }
    Ok(command)
}

/// Verify archive size and compute a streaming SHA-256 checksum.
async fn digest(path: &Path) -> Result<(String, u64), Error> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| Error::Invalid("Backup file is unavailable"))?;
    let mut hash = Sha256::new();
    let mut bytes = 0;
    let mut buffer = vec![0; 65536];
    loop {
        let n = file
            .read(&mut buffer)
            .await
            .map_err(|_| Error::Invalid("Cannot verify backup"))?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        if bytes > MAX_BACKUP {
            return Err(Error::Invalid(
                "The backup exceeds the 10 GiB logical backup limit",
            ));
        }
        hash.update(&buffer[..n]);
    }
    if bytes == 0 {
        return Err(Error::Invalid("The backup is empty"));
    }
    Ok((format!("{:x}", hash.finalize()), bytes))
}

/// Stream a bounded native backup into a private, synchronized file.
async fn capture(command: &mut Command, path: &Path) -> Result<(), Error> {
    let dir = path
        .parent()
        .ok_or(Error::Invalid("Invalid backup directory"))?;
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| Error::Invalid("Cannot create the private backup file"))?;
    let mut output = tokio::fs::File::from_std(output);
    let diagnostic = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path.with_extension("stderr.log"))
        .map_err(|_| Error::Invalid("Cannot create private tool diagnostics"))?;
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::from(diagnostic))
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| {
            Error::Invalid("Install current database backup client tools on the API host")
        })?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or(Error::Invalid("Cannot read native backup output"))?;
    tokio::time::timeout(Duration::from_secs(900), async {
        let mut written = 0_u64;
        let mut checkpoint = 0_u64;
        let mut buffer = vec![0; 65536];
        loop {
            let n = stdout
                .read(&mut buffer)
                .await
                .map_err(|_| Error::Invalid("Cannot read native database output"))?;
            if n == 0 {
                break;
            }
            written += n as u64;
            if written > MAX_BACKUP {
                return Err(Error::Invalid("The logical backup exceeds 10 GiB"));
            }
            if written >= checkpoint {
                if crate::storage::available_bytes(dir)
                    .map_err(|_| Error::Invalid("Cannot inspect backup space"))?
                    < 256 * 1024 * 1024
                {
                    return Err(Error::Invalid(
                        "Backup stopped to preserve 256 MiB of host disk headroom",
                    ));
                }
                checkpoint = written + 8 * 1024 * 1024;
            }
            output
                .write_all(&buffer[..n])
                .await
                .map_err(|_| Error::Invalid("Cannot save the backup file"))?;
        }
        output
            .sync_all()
            .await
            .map_err(|_| Error::Invalid("Cannot persist the backup file"))?;
        let status = child
            .wait()
            .await
            .map_err(|_| Error::Invalid("Native database client failed"))?;
        if !status.success() {
            return Err(Error::Invalid(
                "Native database client failed; check its version, DB grants and free space",
            ));
        }
        Ok(())
    })
    .await
    .map_err(|_| Error::Invalid("Native database operation timed out"))?
}

/// Capture a native consistent logical snapshot and record its verified digest.
pub async fn backup(state: &AppState, record: &DatabaseRecord, job: &mut Job) -> Result<(), Error> {
    let settings = record.input.settings(record.id).map_err(Error::Invalid)?;
    let mut connection = DatabaseConnection::connect(&record.input, &settings).await?;
    let mut exported_snapshot = None;
    if let DatabaseConnection::Postgres(c) = &mut connection {
        exported_snapshot = Some(
            sqlx::query_scalar::<_, String>("SELECT pg_export_snapshot()")
                .fetch_one(&mut *c)
                .await?,
        );
        job.schemas=sqlx::query_as("SELECT n.nspname::text,r.rolname::text FROM pg_namespace n JOIN pg_roles r ON r.oid=n.nspowner WHERE left(n.nspname::text,3)<>'pg_' AND n.nspname<>'information_schema' ORDER BY n.nspname LIMIT 10001").fetch_all(&mut *c).await?;
        job.public_schema_oid =
            sqlx::query_scalar("SELECT oid::bigint FROM pg_namespace WHERE nspname='public'")
                .fetch_optional(c)
                .await?;
        if job.schemas.len() > 10000 {
            return Err(Error::Invalid("The backup contains too many schemas"));
        }
    }
    let tables = connection.tables().await?;
    if tables.truncated {
        return Err(Error::Invalid(
            "Logical backup jobs support at most 500 application tables",
        ));
    }
    job.tables = tables
        .tables
        .into_iter()
        .map(|t| write::TableRequest {
            schema: t.schema,
            table: t.name,
        })
        .collect();
    if let DatabaseConnection::Mysql(c) = &mut connection {
        let unsafe_tables:i64=sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.tables WHERE TABLE_SCHEMA=DATABASE() AND TABLE_TYPE='BASE TABLE' AND ENGINE<>'InnoDB'").fetch_one(c).await?;
        if unsafe_tables != 0 {
            return Err(Error::Invalid(
                "Consistent MySQL backups require InnoDB tables",
            ));
        }
    }
    let dir = directory(state, job.id)?;
    let path = dir.join("database.dump");
    let program = if settings.summary.engine == Engine::Postgres {
        "pg_dump"
    } else {
        "mysqldump"
    };
    let mut command = native(record, program, &dir).await?;
    if settings.summary.engine == Engine::Postgres {
        command.arg("--format=custom");
        if let Some(snapshot) = exported_snapshot {
            command.arg("--snapshot").arg(snapshot);
        }
    } else {
        command
            .args([
                "--single-transaction",
                "--routines",
                "--events",
                "--triggers",
                "--hex-blob",
                "--no-tablespaces",
                "--add-drop-database",
                "--databases",
                "--",
            ])
            .arg(&settings.summary.database);
        // Include every saved Firecrab namespace sharing this source database.
        for saved in state
            .store
            .list_pipe_databases()
            .map_err(|_| Error::Invalid("Cannot inspect database ownership"))?
        {
            if saved.input.write_enabled && target_hash(&saved)? == target_hash(record)? {
                command.arg(write::internal_schema(saved.id));
            }
        }
    }
    capture(&mut command, &path).await?;
    if let DatabaseConnection::Postgres(c) = &mut connection {
        write::pg_batch(c, "COMMIT").await?;
    }
    if settings.summary.engine == Engine::Postgres {
        let status = Command::new("pg_restore")
            .arg("--list")
            .arg(&path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status()
            .await
            .map_err(|_| Error::Invalid("PostgreSQL restore client is unavailable"))?;
        if !status.success() {
            return Err(Error::Invalid("The PostgreSQL archive cannot be read"));
        }
    }
    let (hash, bytes) = digest(&path).await?;
    job.checksum = Some(hash);
    job.bytes = Some(bytes);
    Ok(())
}

/// Validate before a backup or DDL starts. Transaction-control and host/file SQL
/// are excluded rather than searched with a bypassable regular expression.
pub fn migration_statements(
    engine: Engine,
    database: &str,
    sql: &str,
) -> Result<Vec<String>, Error> {
    use sqlparser::{
        ast::{ObjectType, Statement},
        dialect::{Dialect, MySqlDialect, PostgreSqlDialect},
        parser::Parser,
    };
    if sql.trim().is_empty() || sql.len() > 65536 {
        return Err(Error::Invalid("Migration SQL must contain 1-65536 bytes"));
    }
    let dialect: &dyn Dialect = if engine == Engine::Postgres {
        &PostgreSqlDialect {}
    } else {
        &MySqlDialect {}
    };
    let statements = Parser::new(dialect)
        .with_recursion_limit(50)
        .try_with_sql(sql)
        .map_err(|_| Error::Invalid("Cannot parse the migration SQL"))?
        .parse_statements()
        .map_err(|_| Error::Invalid("Cannot parse the migration SQL"))?;
    if statements.is_empty() || statements.len() > 64 {
        return Err(Error::Invalid("A migration must contain 1-64 statements"));
    }
    for statement in &statements {
        let allowed = matches!(
            statement,
            Statement::CreateTable(_)
                | Statement::AlterTable(_)
                | Statement::CreateIndex(_)
                | Statement::AlterIndex { .. }
                | Statement::Insert(_)
                | Statement::Update(_)
                | Statement::Delete(_)
        ) || matches!(
            statement,
            Statement::Drop {
                object_type: ObjectType::Table | ObjectType::Index,
                ..
            }
        );
        if !allowed {
            return Err(Error::Invalid(
                "Migrations support table/index DDL and INSERT, UPDATE, DELETE; transaction, account and file commands are unavailable",
            ));
        }
    }
    use std::ops::ControlFlow;
    let allowed_name = |name: &sqlparser::ast::ObjectName| {
        if name.0.is_empty()
            || name.0.len() > 2
            || name.0.iter().any(|part| part.as_ident().is_none())
        {
            return false;
        }
        if name.0.len() == 2 {
            let schema = &name.0[0].as_ident().expect("checked identifier").value;
            if engine == Engine::Mysql && schema != database {
                return false;
            }
            if (write::TableRequest {
                schema: schema.clone(),
                table: name.0[1]
                    .as_ident()
                    .expect("checked identifier")
                    .value
                    .clone(),
            })
            .validate()
            .is_err()
            {
                return false;
            }
        }
        true
    };
    let scope = sqlparser::ast::visit_relations(&statements, |name| {
        if allowed_name(name) {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(())
        }
    });
    if scope.is_break() || statements.iter().any(|s|matches!(s,Statement::Drop{names,..} if names.iter().any(|name|!allowed_name(name)))){return Err(Error::Invalid("Migration objects must belong to this database and cannot use internal schemas"));}
    // Index and rename destinations are not relation nodes in the visitor.
    for statement in &statements {
        match statement {
            Statement::AlterIndex { name, .. } if !allowed_name(name) => {
                return Err(Error::Invalid("Internal indexes cannot be changed"));
            }
            Statement::CreateIndex(index)
                if index.concurrently
                    || index.name.as_ref().is_some_and(|name| !allowed_name(name)) =>
            {
                return Err(Error::Invalid(
                    "Use a local, nonconcurrent index in a transactional schema job",
                ));
            }
            Statement::AlterTable(table) => {
                for operation in &table.operations {
                    if let sqlparser::ast::AlterTableOperation::RenameTable {
                        table_name:
                            sqlparser::ast::RenameTableNameKind::To(name)
                            | sqlparser::ast::RenameTableNameKind::As(name),
                    } = operation
                        && !allowed_name(name)
                    {
                        return Err(Error::Invalid(
                            "A migration cannot move a table to another database or an internal schema",
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(statements.into_iter().map(|s| s.to_string()).collect())
}

/// Restrict immutable migration versions to a bounded ASCII identifier.
pub fn validate_version(version: &str) -> Result<(), Error> {
    if version.is_empty()
        || version.len() > 100
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return Err(Error::Invalid(
            "Enter a migration version with 1-100 letters, digits, dots, hyphens or underscores",
        ));
    }
    Ok(())
}

/// Check the version ledger and apply supported SQL without blind DDL retries.
pub async fn apply_migration(record: &DatabaseRecord, job: &Job) -> Result<(), Error> {
    let settings = record.input.settings(record.id).map_err(Error::Invalid)?;
    let sql = job
        .sql
        .as_deref()
        .ok_or(Error::Invalid("Migration SQL is missing"))?;
    let version = job
        .version
        .as_deref()
        .ok_or(Error::Invalid("Migration version is missing"))?;
    validate_version(version)?;
    let statements =
        migration_statements(settings.summary.engine, &settings.summary.database, sql)?;
    let hash = format!("{:x}", Sha256::digest(sql.as_bytes()));
    let mut c = DatabaseConnection::connect_write(&record.input, &settings).await?;
    write::initialize(&mut c, record.id).await?;
    let ledger = format!(
        "{}.migrations",
        quote(settings.summary.engine, &write::internal_schema(record.id))
    );
    match &mut c {
        DatabaseConnection::Postgres(c) => {
            sqlx::query(&format!("CREATE TABLE IF NOT EXISTS {ledger}(version text PRIMARY KEY,checksum text NOT NULL,status text NOT NULL,applied_at timestamptz)" )).execute(&mut *c).await?;
            let mut tx = write::pg_transaction(c).await?;
            sqlx::query("SET LOCAL search_path=public")
                .execute(&mut *tx)
                .await?;
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
                .bind(format!("firecrab-migration:{}", record.id))
                .execute(&mut *tx)
                .await?;
            let previous: Option<(String, String)> = sqlx::query_as(&format!(
                "SELECT checksum,status FROM {ledger} WHERE version=$1 FOR UPDATE"
            ))
            .bind(version)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some((checksum, status)) = previous {
                if checksum == hash && status == "applied" {
                    return Ok(());
                }
                return Err(Error::Invalid(
                    "This migration version already has different or incomplete content",
                ));
            }
            sqlx::query(&format!(
                "INSERT INTO {ledger} VALUES ($1,$2,'applying',NULL)"
            ))
            .bind(version)
            .bind(&hash)
            .execute(&mut *tx)
            .await?;
            for statement in statements {
                write::pg_batch(&mut tx, &statement).await?;
            }
            sqlx::query(&format!(
                "UPDATE {ledger} SET status='applied',applied_at=CURRENT_TIMESTAMP WHERE version=$1"
            ))
            .bind(version)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        }
        DatabaseConnection::Mysql(c) => {
            sqlx::query(&format!("CREATE TABLE IF NOT EXISTS {ledger}(version varchar(100) CHARACTER SET ascii COLLATE ascii_bin PRIMARY KEY,checksum char(64) NOT NULL,status varchar(16) NOT NULL,applied_at timestamp(6) NULL) ENGINE=InnoDB")).execute(&mut *c).await?;
            let locked: i32 = sqlx::query_scalar("SELECT GET_LOCK(?,0)")
                .bind(format!("fc-migration:{}", record.id))
                .fetch_one(&mut *c)
                .await?;
            if locked != 1 {
                return Err(Error::Invalid("Another schema migration is active"));
            }
            let previous: Option<(String, String)> = sqlx::query_as(&format!(
                "SELECT checksum,status FROM {ledger} WHERE version=?"
            ))
            .bind(version)
            .fetch_optional(&mut *c)
            .await?;
            if let Some((checksum, status)) = previous {
                if checksum == hash && status == "applied" {
                    return Ok(());
                }
                return Err(Error::Invalid(
                    "This migration version is different or incomplete; restore its safety backup before retrying",
                ));
            }
            sqlx::query(&format!(
                "INSERT INTO {ledger} VALUES (?,?,'applying',NULL)"
            ))
            .bind(version)
            .bind(&hash)
            .execute(&mut *c)
            .await?;
            for statement in statements {
                write::mysql_command(c, &statement).await?;
            }
            sqlx::query(&format!(
                "UPDATE {ledger} SET status='applied',applied_at=CURRENT_TIMESTAMP WHERE version=?"
            ))
            .bind(version)
            .execute(c)
            .await?;
        }
    }
    Ok(())
}

/// Run a native restore while retaining diagnostics in a private file.
async fn status_command(command: &mut Command, dir: &Path) -> Result<(), Error> {
    let diagnostic = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dir.join("client.stderr.log"))
        .map_err(|_| Error::Invalid("Cannot create private restore diagnostics"))?;
    let status = tokio::time::timeout(
        Duration::from_secs(900),
        command
            .kill_on_drop(true)
            .stdout(Stdio::null())
            .stderr(Stdio::from(diagnostic))
            .status(),
    )
    .await
    .map_err(|_| Error::Invalid("Native restore timed out; inspect the retained safety backup"))?
    .map_err(|_| Error::Invalid("Install current database restore clients on the API host"))?;
    if !status.success() {
        return Err(Error::Invalid(
            "Native restore failed; keep services stopped and retry from the retained safety backup",
        ));
    }
    Ok(())
}

/// Verify snapshot identity and checksum before replacing application state.
pub async fn restore(
    state: &AppState,
    record: &DatabaseRecord,
    job: &Job,
    snapshot: &Job,
) -> Result<(), Error> {
    if snapshot.kind != Kind::Backup
        || snapshot.status != "succeeded"
        || snapshot.target_hash != target_hash(record)?
    {
        return Err(Error::Invalid(
            "Select a successful backup of this database target",
        ));
    }
    let path = backup_path(state, snapshot.id)?;
    let (hash, bytes) = digest(&path).await?;
    if snapshot.checksum.as_deref() != Some(&hash) || snapshot.bytes != Some(bytes) {
        return Err(Error::Invalid(
            "The backup checksum does not match; restore was not started",
        ));
    }
    let dir = directory(state, job.id)?;
    let settings = record.input.settings(record.id).map_err(Error::Invalid)?;
    if settings.summary.engine == Engine::Postgres {
        let sql_file = dir.join("restore.sql");
        // Manage public explicitly: pg_dump omits its default CREATE SCHEMA,
        // while a changed public schema can have an archive definition.
        let toc_file = dir.join("archive.list");
        let mut list = Command::new("pg_restore");
        list.arg("--list").arg(&path);
        capture(&mut list, &toc_file).await?;
        let toc = tokio::fs::read_to_string(&toc_file)
            .await
            .map_err(|_| Error::Invalid("Cannot inspect the PostgreSQL archive"))?;
        let filtered = toc
            .lines()
            .map(|line| {
                let fields = line.split_whitespace().take(4).collect::<Vec<_>>();
                if fields.len() == 4
                    && fields[3] == "SCHEMA"
                    && fields[2].parse::<i64>().ok() == snapshot.public_schema_oid
                {
                    format!(";{line}")
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let selected = dir.join("restore.list");
        private_file(&selected, filtered.as_bytes())?;
        let mut render = Command::new("pg_restore");
        render
            .args(["--use-list"])
            .arg(&selected)
            .arg("--file=-")
            .arg(&path);
        capture(&mut render, &sql_file).await?;
        let _ = digest(&sql_file).await?;
        let mut c = DatabaseConnection::connect(&record.input, &settings).await?;
        let current:Vec<String>=match &mut c{DatabaseConnection::Postgres(c)=>sqlx::query_scalar("SELECT nspname::text FROM pg_namespace WHERE left(nspname::text,3)<>'pg_' AND nspname<>'information_schema' ORDER BY nspname LIMIT 10001").fetch_all(c).await?,_=>unreachable!()};
        if current.len() > 10000 {
            return Err(Error::Invalid("The restore contains too many schemas"));
        }
        let mut prelude = String::from(
            "SELECT pg_catalog.lo_unlink(oid) FROM pg_catalog.pg_largeobject_metadata;\n",
        );
        for schema in current {
            prelude.push_str(&format!(
                "DROP SCHEMA IF EXISTS {} CASCADE;\n",
                quote(Engine::Postgres, &schema)
            ));
        }
        if let Some((_, owner)) = snapshot.schemas.iter().find(|(name, _)| name == "public") {
            prelude.push_str(&format!(
                "CREATE SCHEMA public AUTHORIZATION {}; GRANT USAGE ON SCHEMA public TO PUBLIC;\n",
                quote(Engine::Postgres, owner)
            ));
        }
        let prelude_file = dir.join("prelude.sql");
        private_file(&prelude_file, prelude.as_bytes())?;
        let mut command = native(record, "psql", &dir).await?;
        command
            .args([
                "--no-psqlrc",
                "--single-transaction",
                "--set=ON_ERROR_STOP=1",
                "--file",
            ])
            .arg(prelude_file)
            .arg("--file")
            .arg(sql_file);
        status_command(&mut command, &dir).await?;
    } else {
        let mut command = native(record, "mysql", &dir).await?;
        command.args(["--binary-mode", "--batch"]);
        command.stdin(Stdio::from(
            fs::File::open(path).map_err(|_| Error::Invalid("Backup file is unavailable"))?,
        ));
        status_command(&mut command, &dir).await?;
    }
    Ok(())
}

/// Require stopped services and paused, drained captures on the same target.
pub async fn require_maintenance(state: &AppState, record: &DatabaseRecord) -> Result<(), Error> {
    if !record.input.write_enabled {
        return Err(Error::Invalid("Enable writes before database maintenance"));
    }
    let aliases = state
        .store
        .list_pipe_databases()
        .map_err(|_| Error::Invalid("Cannot inspect database connections"))?
        .into_iter()
        .filter_map(|r| {
            target_hash(&r)
                .ok()
                .filter(|h| *h == target_hash(record).unwrap_or_default())
                .map(|_| r.id)
        })
        .collect::<Vec<_>>();
    for binding in state
        .store
        .database_bindings()
        .map_err(|_| Error::Invalid("Cannot inspect service bindings"))?
        .into_iter()
        .filter(|b| aliases.contains(&b.database_id))
    {
        if state
            .store
            .service_deployment(binding.input.service_id)
            .map_err(|_| Error::Invalid("Cannot inspect service deployment"))?
            .is_some_and(|d| d.phase.is_busy() || !d.instances.is_empty())
        {
            return Err(Error::Invalid(
                "Stop every bound service before schema changes or restore",
            ));
        }
    }
    for trigger in state
        .store
        .database_triggers()
        .map_err(|_| Error::Invalid("Cannot inspect database triggers"))?
        .into_iter()
        .filter(|t| aliases.contains(&t.database_id) && t.installed)
    {
        if !trigger.paused {
            return Err(Error::Invalid(
                "Pause every database trigger before schema changes or restore",
            ));
        }
        let source = state
            .store
            .pipe_database(trigger.database_id)
            .map_err(|_| Error::Invalid("Cannot load the source database"))?
            .ok_or(Error::NotFound)?;
        let settings = source.input.settings(source.id).map_err(Error::Invalid)?;
        let mut c = DatabaseConnection::connect(&source.input, &settings).await?;
        let status = super::events::status(&mut c, trigger).await?;
        if status.pending != 0 || status.failed != 0 {
            return Err(Error::Invalid(
                "Deliver every retained event before schema changes or restore",
            ));
        }
    }
    Ok(())
}

/// Refresh application grants and reinstall capture after maintenance.
async fn reconcile_dependencies(state: &AppState, record: &DatabaseRecord) -> Result<(), Error> {
    let settings = record.input.settings(record.id).map_err(Error::Invalid)?;
    let mut c = DatabaseConnection::connect_write(&record.input, &settings).await?;
    write::initialize(&mut c, record.id).await?;
    let tables = c.tables().await?;
    if tables.truncated {
        return Err(Error::Invalid(
            "Cannot refresh more than 500 application table grants",
        ));
    }
    for mut binding in state
        .store
        .database_bindings()
        .map_err(|_| Error::Invalid("Cannot inspect service bindings"))?
        .into_iter()
        .filter(|b| b.database_id == record.id)
    {
        binding.tables = tables
            .tables
            .iter()
            .map(|t| write::TableRequest {
                schema: t.schema.clone(),
                table: t.name.clone(),
            })
            .collect();
        super::bindings::provision(record, &binding).await?;
        state
            .store
            .save_database_binding(&binding)
            .map_err(|_| Error::Invalid("Cannot save refreshed service grants"))?;
    }
    for mut trigger in state
        .store
        .database_triggers()
        .map_err(|_| Error::Invalid("Cannot inspect database triggers"))?
        .into_iter()
        .filter(|t| t.database_id == record.id && !t.detached)
    {
        let table = write::TableRequest {
            schema: trigger.input.schema.clone(),
            table: trigger.input.table.clone(),
        };
        if tables
            .tables
            .iter()
            .any(|t| t.schema == table.schema && t.name == table.table)
        {
            super::events::install(&mut c, &trigger).await?;
        } else {
            trigger.detached = true;
            state
                .store
                .save_database_trigger(&trigger)
                .map_err(|_| Error::Invalid("Cannot save detached table capture"))?;
        }
    }
    Ok(())
}

/// Create a safety snapshot, apply maintenance and reconcile dependencies.
async fn run(state: &AppState, record: &DatabaseRecord, job: &mut Job) -> Result<(), Error> {
    if target_hash(record)? != job.target_hash {
        return Err(Error::Invalid(
            "The saved database target changed; create a new job",
        ));
    }
    if job.kind == Kind::Backup {
        return backup(state, record, job).await;
    }
    require_maintenance(state, record).await?;
    let mut safety = Job::new(record, Uuid::new_v4(), Kind::Backup)?;
    safety.status = "running".into();
    persist(state, &safety)?;
    match backup(state, record, &mut safety).await {
        Ok(()) => safety.status = "succeeded".into(),
        Err(e) => {
            safety.status = "failed".into();
            safety.error = Some(e.to_string());
            persist(state, &safety)?;
            return Err(e);
        }
    }
    persist(state, &safety)?;
    job.safety_backup_id = Some(safety.id);
    job.maintenance_started = true;
    persist(state, job)?;
    let aliases = state
        .store
        .list_pipe_databases()
        .map_err(|_| Error::Invalid("Cannot inspect database aliases"))?
        .into_iter()
        .filter(|r| target_hash(r).ok().as_ref() == Some(&job.target_hash))
        .collect::<Vec<_>>();
    let settings = record.input.settings(record.id).map_err(Error::Invalid)?;
    let mut c = DatabaseConnection::connect_write(&record.input, &settings).await?;
    for trigger in state
        .store
        .database_triggers()
        .map_err(|_| Error::Invalid("Cannot inspect database triggers"))?
        .into_iter()
        .filter(|t| aliases.iter().any(|r| r.id == t.database_id) && t.installed && !t.detached)
    {
        super::events::detach(&mut c, &trigger).await?;
    }
    if job.kind == Kind::Migration {
        apply_migration(record, job).await?;
    } else {
        let snapshot = state
            .store
            .database_jobs(record.id)
            .map_err(|_| Error::Invalid("Cannot load backups"))?
            .into_iter()
            .find(|b| Some(b.id) == job.backup_id)
            .ok_or(Error::Invalid("The selected backup does not exist"))?;
        restore(state, record, job, &snapshot).await?;
    }
    for alias in aliases {
        let mut privileged = record.clone();
        privileged.id = alias.id;
        reconcile_dependencies(state, &privileged).await?;
    }
    for mut old in state
        .store
        .database_target_jobs(record.id)
        .map_err(|_| Error::Invalid("Cannot inspect recovery state"))?
        .into_iter()
        .filter(|j| j.status == "needs_recovery")
    {
        old.status = "recovered".into();
        persist(state, &old)?;
    }
    Ok(())
}

/// Expire old snapshots while preserving active recovery references.
async fn retain_backups(state: &AppState, record: &DatabaseRecord) {
    let _claim = state.service_mutations.lock().await;
    let Ok(jobs) = state.store.database_jobs(record.id) else {
        return;
    };
    for mut backup in jobs
        .iter()
        .filter(|j| j.kind == Kind::Backup && j.status == "succeeded")
        .skip(7)
        .cloned()
    {
        if jobs.iter().any(|j| {
            matches!(j.status.as_str(), "queued" | "running" | "needs_recovery")
                && (j.backup_id == Some(backup.id) || j.safety_backup_id == Some(backup.id))
        }) {
            continue;
        }
        let Ok(path) = backup_path(state, backup.id) else {
            continue;
        };
        let Some(dir) = path.parent() else { continue };
        match tokio::fs::remove_dir_all(dir).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => continue,
        }
        backup.status = "expired".into();
        let _ = persist(state, &backup);
    }
}

/// Recover interrupted jobs, execute queued maintenance and schedule daily snapshots.
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        // An interrupted MySQL DDL/restore cannot be assumed rolled back.
        if let Ok(records) = state.store.list_pipe_databases() {
            for record in records {
                if let Ok(jobs) = state.store.database_jobs(record.id) {
                    for mut job in jobs.into_iter().filter(|j| j.status == "running") {
                        job.status = if job.maintenance_started {
                            "needs_recovery"
                        } else {
                            "failed"
                        }
                        .into();
                        job.error=Some("The API restarted during this job; inspect its safety backup before retrying".into());
                        let _ = persist(&state, &job);
                    }
                }
            }
        }
        loop {
            let records = state.store.list_pipe_databases().unwrap_or_default();
            for record in records {
                let jobs = state.store.database_jobs(record.id).unwrap_or_default();
                if let Some(mut job) = jobs.iter().rev().find(|j| j.status == "queued").cloned() {
                    job.status = "running".into();
                    if persist(&state, &job).is_err() {
                        continue;
                    }
                    let outcome = tokio::time::timeout(
                        Duration::from_secs(1800),
                        run(&state, &record, &mut job),
                    )
                    .await
                    .unwrap_or(Err(Error::Invalid(
                        "Database maintenance timed out; inspect its safety backup",
                    )));
                    match outcome {
                        Ok(()) => job.status = "succeeded".into(),
                        Err(error) => {
                            job.status = if job.maintenance_started {
                                "needs_recovery"
                            } else {
                                "failed"
                            }
                            .into();
                            job.error=Some(match error{Error::Invalid(message)=>message.to_owned(),_=>"Database maintenance failed; check privileges, field types and connectivity".into()});
                        }
                    }
                    let _ = persist(&state, &job);
                    if (job.kind == Kind::Backup && job.status == "failed"
                        || job.kind == Kind::Restore)
                        && let Ok(path) = backup_path(&state, job.id)
                        && let Some(dir) = path.parent()
                    {
                        let _ = tokio::fs::remove_dir_all(dir).await;
                    }
                    retain_backups(&state, &record).await;
                } else if record.input.write_enabled
                    && !jobs.iter().any(|j| {
                        matches!(j.status.as_str(), "queued" | "running" | "needs_recovery")
                    })
                    && !jobs.iter().any(|j| {
                        j.kind == Kind::Backup
                            && chrono::Utc::now().timestamp() - j.created_at < 86400
                    })
                {
                    let _claim = state.service_mutations.lock().await;
                    if !state
                        .store
                        .pipe_database(record.id)
                        .ok()
                        .is_some_and(|r| r.is_some())
                    {
                        continue;
                    }
                    if state
                        .store
                        .database_target_jobs(record.id)
                        .ok()
                        .is_none_or(|jobs| {
                            jobs.iter().any(|j| {
                                matches!(j.status.as_str(), "queued" | "running" | "needs_recovery")
                            })
                        })
                    {
                        continue;
                    }
                    if let Ok(job) = Job::new(&record, Uuid::new_v4(), Kind::Backup) {
                        let _ = persist(&state, &job);
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
}
