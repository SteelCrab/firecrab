//! A source transaction writes the outbox. A single host worker publishes and delivers it.
//! RabbitMQ acknowledgments never cause deletion: the source retains delivery receipts.
use super::rabbitmq;
pub use super::rabbitmq::RabbitMQInput;
use super::{
    DatabaseConnection, DatabaseRecord, Engine, quote,
    write::{self, Error, TableRequest},
};
use crate::state::AppState;
use chrono::Utc;
use hmac::{Hmac, Mac};
use lapin::{Connection, options::*};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{sync::Arc, time::Duration};
use uuid::Uuid;
#[cfg(test)]
#[path = "events_tests.rs"]
mod tests;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Source-table and handler selection with a stable retry ID.
pub struct TriggerInput {
    /// Client UUID reused for retries of the same operation.
    pub request_id: Uuid,
    /// Exact source schema, or MySQL database name.
    pub schema: String,
    /// Source table identifier, or a validated table selection.
    pub table: String,
    /// MicroService that receives the connection or events.
    pub service_id: Uuid,
    #[serde(default = "default_path")]
    /// Handler HTTP path; defaults to /events/database.
    pub path: String,
}
/// Default signed event handler endpoint.
fn default_path() -> String {
    "/events/database".into()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
/// Owned capture lifecycle and persisted delivery controls.
pub struct Trigger {
    /// Stable UUID for this owned resource.
    pub id: Uuid,
    /// Saved source connection that owns this resource.
    pub database_id: Uuid,
    #[serde(flatten)]
    /// Original setup intent used to detect conflicting retries.
    pub input: TriggerInput,
    /// Stop new delivery attempts after an in-flight request finishes.
    pub paused: bool,
    /// Capture and its outbox finished setup successfully.
    pub installed: bool,
    #[serde(default)]
    /// Source capture is removed; retained events can still drain.
    pub detached: bool,
}
impl Trigger {
    /// Derive the RabbitMQ queue owned by this trigger UUID.
    pub fn queue(&self) -> String {
        format!("firecrab.database.{}", self.id.simple())
    }
    fn table(&self) -> TableRequest {
        TableRequest {
            schema: self.input.schema.clone(),
            table: self.input.table.clone(),
        }
    }
    /// Quote the owned outbox table for the selected SQL dialect.
    fn outbox(&self, engine: Engine) -> String {
        format!(
            "{}.{}",
            quote(engine, &write::internal_schema(self.database_id)),
            quote(engine, &format!("outbox_{}", self.id.simple()))
        )
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
/// Redacted capture status and source delivery counts.
pub struct TriggerStatus {
    #[serde(flatten)]
    /// Capture definition without database credentials.
    pub trigger: Trigger,
    /// Committed events awaiting successful HTTP delivery.
    pub pending: i64,
    /// Events held after a permanent error or exhausted attempts.
    pub failed: i64,
    /// Retained receipts for successful HTTP delivery.
    pub delivered: i64,
    /// Failed payloads retained after an explicit operator acknowledgement.
    pub skipped: i64,
    /// Redacted worker error suitable for the dashboard.
    pub last_error: Option<String>,
}

/// Install transactional row capture after checking supported table semantics.
pub async fn install(connection: &mut DatabaseConnection, trigger: &Trigger) -> Result<(), Error> {
    if trigger.input.service_id.is_nil()
        || !trigger.input.path.starts_with('/')
        || trigger.input.path.starts_with("//")
        || trigger.input.path.len() > 256
        || trigger.input.path.chars().any(char::is_control)
        || trigger.input.path.contains(['?', '#', '\\'])
    {
        return Err(Error::Invalid(
            "Choose a service and a valid event endpoint path",
        ));
    }
    let table = trigger.table();
    let definition = connection.table_info(&table).await?;
    if !definition.transactional || definition.primary_key.is_empty() {
        return Err(Error::Invalid(
            "Reliable events require a transactional table with a primary key",
        ));
    }
    write::verify_owner(connection, trigger.database_id).await?;
    let engine = connection.engine();
    let source = table.qualified(engine);
    let outbox = trigger.outbox(engine);
    let function = quote(engine, &format!("capture_{}", trigger.id.simple()));
    let schema = quote(engine, &write::internal_schema(trigger.database_id));
    let index_name = trigger.id.simple();
    match connection {
        DatabaseConnection::Postgres(c) => {
            let mut tx = write::pg_transaction(c).await?;
            write::pg_batch(&mut tx,&format!("CREATE TABLE IF NOT EXISTS {outbox} (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,payload text NOT NULL,status text NOT NULL DEFAULT 'pending',attempts integer NOT NULL DEFAULT 0,next_attempt bigint NOT NULL DEFAULT 0,last_error text,created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,delivered_at timestamptz); CREATE INDEX IF NOT EXISTS pending_{index_name} ON {outbox}(status,next_attempt,id); CREATE OR REPLACE FUNCTION {schema}.{function}() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path=pg_catalog SET timezone='UTC' AS $fc$ BEGIN INSERT INTO {outbox}(payload) VALUES (json_build_object('operation',lower(TG_OP),'schema',TG_TABLE_SCHEMA,'table',TG_TABLE_NAME,'before',CASE WHEN TG_OP='INSERT' THEN NULL ELSE (SELECT jsonb_object_agg(key,CASE WHEN jsonb_typeof(value)='number' THEN to_jsonb(value::text) ELSE value END) FROM jsonb_each(to_jsonb(OLD))) END,'after',CASE WHEN TG_OP='DELETE' THEN NULL ELSE (SELECT jsonb_object_agg(key,CASE WHEN jsonb_typeof(value)='number' THEN to_jsonb(value::text) ELSE value END) FROM jsonb_each(to_jsonb(NEW))) END,'occurredAt',clock_timestamp())::text); RETURN NULL; END $fc$; REVOKE ALL ON FUNCTION {schema}.{function}() FROM PUBLIC; CREATE OR REPLACE TRIGGER {function} AFTER INSERT OR UPDATE OR DELETE ON {source} FOR EACH ROW EXECUTE FUNCTION {schema}.{function}();")).await?;
            tx.commit().await?;
        }
        DatabaseConnection::Mysql(c) => {
            let cascades:i64=sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.referential_constraints WHERE CONSTRAINT_SCHEMA=? AND TABLE_NAME=? AND (DELETE_RULE IN ('CASCADE','SET NULL') OR UPDATE_RULE IN ('CASCADE','SET NULL'))").bind(&table.schema).bind(&table.table).fetch_one(&mut *c).await?;
            if cascades != 0 {
                return Err(Error::Invalid(
                    "MySQL row triggers cannot capture foreign-key CASCADE or SET NULL; use RESTRICT before enabling events",
                ));
            }
            if definition
                .columns
                .iter()
                .any(|column| column.binary || column.data_type == "bytea")
            {
                return Err(Error::Invalid(
                    "Binary columns require a CDC connector; row-trigger capture is unavailable for this table",
                ));
            }
            let generated:i64=sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.columns WHERE TABLE_SCHEMA=? AND TABLE_NAME=? AND GENERATION_EXPRESSION<>''").bind(&table.schema).bind(&table.table).fetch_one(&mut *c).await?;
            if generated != 0 {
                return Err(Error::Invalid(
                    "MySQL generated columns require a CDC connector",
                ));
            }
            sqlx::query(&format!("CREATE TABLE IF NOT EXISTS {outbox} (id bigint NOT NULL AUTO_INCREMENT PRIMARY KEY,payload longtext NOT NULL,status varchar(16) NOT NULL DEFAULT 'pending',attempts int NOT NULL DEFAULT 0,next_attempt bigint NOT NULL DEFAULT 0,last_error text,created_at timestamp(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6),delivered_at timestamp(6) NULL,KEY pending_events(status,next_attempt,id)) ENGINE=InnoDB")).execute(&mut *c).await?;
            // MySQL DDL commits implicitly. Install all three before returning success.
            // A failed install remains paused in the host store and is never advertised active.
            for (operation, timing) in [
                ("insert", "INSERT"),
                ("update", "UPDATE"),
                ("delete", "DELETE"),
            ] {
                let image = |prefix: &str| {
                    definition
                        .columns
                        .iter()
                        .map(|column| {
                            let name = quote(engine, &column.name);
                            let value = if matches!(
                                column.data_type.as_str(),
                                "tinyint"
                                    | "smallint"
                                    | "mediumint"
                                    | "int"
                                    | "integer"
                                    | "bigint"
                                    | "decimal"
                                    | "numeric"
                                    | "float"
                                    | "double"
                                    | "real"
                            ) {
                                format!("CAST({prefix}.{name} AS CHAR CHARACTER SET utf8mb4)")
                            } else {
                                format!("{prefix}.{name}")
                            };
                            format!("{}, {value}", write::mysql_literal(&column.name))
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                };
                let before = if operation == "insert" {
                    "NULL".into()
                } else {
                    format!("JSON_OBJECT({})", image("OLD"))
                };
                let after = if operation == "delete" {
                    "NULL".into()
                } else {
                    format!("JSON_OBJECT({})", image("NEW"))
                };
                let name = quote(engine, &format!("fc_{}_{operation}", trigger.id.simple()));
                let body = format!(
                    "INSERT INTO {outbox}(payload) VALUES (JSON_OBJECT('operation','{operation}','schema',{},'table',{},'before',{before},'after',{after},'occurredAt',UTC_TIMESTAMP(6)))",
                    write::mysql_literal(&table.schema),
                    write::mysql_literal(&table.table)
                );
                let existing:Option<String>=sqlx::query_scalar("SELECT CAST(ACTION_STATEMENT AS CHAR CHARACTER SET utf8mb4) FROM information_schema.triggers WHERE TRIGGER_SCHEMA=? AND TRIGGER_NAME=?").bind(&table.schema).bind(format!("fc_{}_{operation}",trigger.id.simple())).fetch_optional(&mut *c).await?;
                if let Some(existing) = existing {
                    if existing != body {
                        return Err(Error::Invalid(
                            "An existing capture trigger differs from the saved setup; inspect it before retrying",
                        ));
                    }
                    continue;
                }
                write::mysql_command(
                    &mut *c,
                    &format!(
                        "CREATE TRIGGER {}.{name} AFTER {timing} ON {source} FOR EACH ROW {body}",
                        quote(engine, &table.schema)
                    ),
                )
                .await?;
            }
        }
    }
    Ok(())
}

/// Detect retained source events after an interrupted setup or detach.
pub async fn outbox_exists(
    connection: &mut DatabaseConnection,
    trigger: &Trigger,
) -> Result<bool, Error> {
    let schema = write::internal_schema(trigger.database_id);
    let table = format!("outbox_{}", trigger.id.simple());
    let count:i64=match connection{
        DatabaseConnection::Postgres(c)=>sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.tables WHERE table_schema=$1 AND table_name=$2").bind(schema).bind(table).fetch_one(c).await?,
        DatabaseConnection::Mysql(c)=>sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.tables WHERE table_schema=? AND table_name=?").bind(schema).bind(table).fetch_one(c).await?,
    };
    Ok(count == 1)
}

/// Remove owned capture while preserving its source delivery receipts.
pub async fn detach(connection: &mut DatabaseConnection, trigger: &Trigger) -> Result<(), Error> {
    write::verify_owner(connection, trigger.database_id).await?;
    let table = trigger.table();
    match connection {
        DatabaseConnection::Postgres(c) => {
            let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_schema=$1 AND table_name=$2)").bind(&table.schema).bind(&table.table).fetch_one(&mut *c).await?;
            let mut tx = write::pg_transaction(c).await?;
            if exists {
                sqlx::query(&format!(
                    "DROP TRIGGER IF EXISTS {} ON {}",
                    quote(
                        Engine::Postgres,
                        &format!("capture_{}", trigger.id.simple())
                    ),
                    table.qualified(Engine::Postgres)
                ))
                .execute(&mut *tx)
                .await?;
            }
            sqlx::query(&format!(
                "DROP FUNCTION IF EXISTS {}.{}()",
                quote(
                    Engine::Postgres,
                    &write::internal_schema(trigger.database_id)
                ),
                quote(
                    Engine::Postgres,
                    &format!("capture_{}", trigger.id.simple())
                )
            ))
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        }
        DatabaseConnection::Mysql(c) => {
            for operation in ["insert", "update", "delete"] {
                write::mysql_command(
                    &mut *c,
                    &format!(
                        "DROP TRIGGER IF EXISTS {}.{}",
                        quote(Engine::Mysql, &table.schema),
                        quote(
                            Engine::Mysql,
                            &format!("fc_{}_{operation}", trigger.id.simple())
                        )
                    ),
                )
                .await?;
            }
        }
    }
    Ok(())
}

/// Count retained delivery states without exposing event payloads.
pub async fn status(
    connection: &mut DatabaseConnection,
    trigger: Trigger,
) -> Result<TriggerStatus, Error> {
    let outbox = trigger.outbox(connection.engine());
    let sql = format!(
        "SELECT CAST(COALESCE(SUM(CASE WHEN status='pending' THEN 1 ELSE 0 END),0) AS {}), CAST(COALESCE(SUM(CASE WHEN status='failed' THEN 1 ELSE 0 END),0) AS {}),CAST(COALESCE(SUM(CASE WHEN status='delivered' THEN 1 ELSE 0 END),0) AS {}),CAST(COALESCE(SUM(CASE WHEN status='skipped' THEN 1 ELSE 0 END),0) AS {}) FROM {outbox}",
        if connection.engine() == Engine::Postgres {
            "bigint"
        } else {
            "SIGNED"
        },
        if connection.engine() == Engine::Postgres {
            "bigint"
        } else {
            "SIGNED"
        },
        if connection.engine() == Engine::Postgres {
            "bigint"
        } else {
            "SIGNED"
        },
        if connection.engine() == Engine::Postgres {
            "bigint"
        } else {
            "SIGNED"
        }
    );
    let (pending, failed, delivered, skipped): (i64, i64, i64, i64) = match connection {
        DatabaseConnection::Postgres(c) => sqlx::query_as(&sql).fetch_one(c).await?,
        DatabaseConnection::Mysql(c) => sqlx::query_as(&sql).fetch_one(c).await?,
    };
    Ok(TriggerStatus {
        trigger,
        pending,
        failed,
        delivered,
        skipped,
        last_error: None,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
/// Retained event metadata available for operator replay.
pub struct FailedEvent {
    /// Stable trigger UUID and source outbox ID.
    pub event_id: String,
    /// Persisted number of failed HTTP delivery attempts.
    pub attempts: i32,
    /// Redacted operation failure, never raw driver diagnostics.
    pub error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
/// Signed delivery envelope that preserves the raw source JSON.
struct Envelope<'a> {
    /// Stable event ID combining the trigger UUID and outbox ID.
    id: &'a str,
    /// Capture UUID used to identify the delivery stream.
    trigger_id: Uuid,
    /// Saved source connection that owns this resource.
    database_id: Uuid,
    /// Raw JSON row change preserved without numeric conversion.
    change: &'a serde_json::value::RawValue,
}

/// Serialize an envelope without parsing or rounding its source JSON.
fn event_body(
    id: &str,
    trigger: &Trigger,
    change: &serde_json::value::RawValue,
) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(&Envelope {
        id,
        trigger_id: trigger.id,
        database_id: trigger.database_id,
        change,
    })
    .map_err(|_| Error::Invalid("Cannot encode event"))
}

/// Return bounded failure metadata for replay without row contents.
pub async fn failed(
    connection: &mut DatabaseConnection,
    trigger: &Trigger,
) -> Result<Vec<FailedEvent>, Error> {
    let sql = format!(
        "SELECT id,attempts,last_error FROM {} WHERE status='failed' ORDER BY id LIMIT 100",
        trigger.outbox(connection.engine())
    );
    let rows: Vec<(i64, i32, Option<String>)> = match connection {
        DatabaseConnection::Postgres(c) => sqlx::query_as(&sql).fetch_all(c).await?,
        DatabaseConnection::Mysql(c) => sqlx::query_as(&sql).fetch_all(c).await?,
    };
    Ok(rows
        .into_iter()
        .map(|(id, attempts, error)| FailedEvent {
            event_id: format!("{}:{id}", trigger.id),
            attempts,
            error,
        })
        .collect())
}

/// Requeue one failed source event without changing its stable ID.
pub async fn replay(
    connection: &mut DatabaseConnection,
    trigger: &Trigger,
    id: i64,
) -> Result<(), Error> {
    if id <= 0 {
        return Err(Error::Invalid("Invalid event ID"));
    }
    let sql = format!(
        "UPDATE {} SET status='pending',attempts=0,next_attempt=0,last_error=NULL WHERE id={} AND status='failed'",
        trigger.outbox(connection.engine()),
        if connection.engine() == Engine::Postgres {
            "$1"
        } else {
            "?"
        }
    );
    let changed = match connection {
        DatabaseConnection::Postgres(c) => {
            sqlx::query(&sql).bind(id).execute(c).await?.rows_affected()
        }
        DatabaseConnection::Mysql(c) => {
            sqlx::query(&sql).bind(id).execute(c).await?.rows_affected()
        }
    };
    if changed != 1 {
        return Err(Error::NotFound);
    }
    Ok(())
}

/// Retain a failed payload and record the operator's decision to skip delivery.
pub async fn acknowledge(
    connection: &mut DatabaseConnection,
    trigger: &Trigger,
    id: i64,
) -> Result<(), Error> {
    if id <= 0 {
        return Err(Error::Invalid("Invalid event ID"));
    }
    let engine = connection.engine();
    let outbox = trigger.outbox(engine);
    let placeholder = if engine == Engine::Postgres {
        "$1"
    } else {
        "?"
    };
    let sql = format!(
        "UPDATE {outbox} SET status='skipped',last_error='operator_acknowledged' WHERE id={placeholder} AND status='failed'"
    );
    match &mut *connection {
        DatabaseConnection::Postgres(c) => {
            sqlx::query(&sql).bind(id).execute(c).await?;
        }
        DatabaseConnection::Mysql(c) => {
            sqlx::query(&sql).bind(id).execute(c).await?;
        }
    }
    let sql = format!("SELECT status FROM {outbox} WHERE id={placeholder}");
    let status: Option<String> = match connection {
        DatabaseConnection::Postgres(c) => {
            sqlx::query_scalar(&sql).bind(id).fetch_optional(c).await?
        }
        DatabaseConnection::Mysql(c) => sqlx::query_scalar(&sql).bind(id).fetch_optional(c).await?,
    };
    if status.as_deref() != Some("skipped") {
        return Err(Error::Invalid("Only a failed event can be acknowledged"));
    }
    Ok(())
}

/// Backoff and terminal failure are durable in the source DB, not a memory queue.
fn retry_delay(attempt: i32) -> i64 {
    (1_i64 << attempt.clamp(1, 9)).min(300)
}

/// One canonical source event. Publication is repaired when its broker copy is absent.
type DeliveryRow = (i64, String, i32, String, i64, i64);

struct RelayChannel(lapin::Channel);
impl Drop for RelayChannel {
    fn drop(&mut self) {
        if !self.0.status().connected() {
            return;
        }
        let channel = self.0.clone();
        tokio::spawn(async move {
            let _ = tokio::time::timeout(
                Duration::from_secs(3),
                channel.close(200, "relay stopped".into()),
            )
            .await;
        });
    }
}

/// Share source ownership between delivery and trigger cleanup. Held until disconnect.
pub(crate) async fn claim(
    connection: &mut DatabaseConnection,
    trigger: &Trigger,
) -> Result<bool, Error> {
    Ok(match connection {
        DatabaseConnection::Postgres(c) => {
            sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_lock(hashtextextended($1,0))")
                .bind(trigger.id.to_string())
                .fetch_one(c)
                .await?
        }
        DatabaseConnection::Mysql(c) => {
            sqlx::query_scalar::<_, i32>("SELECT GET_LOCK(?,0)")
                .bind(format!("fc:{}", trigger.id))
                .fetch_one(c)
                .await?
                == 1
        }
    })
}

/// Keep one unacknowledged delivery per trigger under the source advisory lock.
async fn relay(
    state: &AppState,
    record: &DatabaseRecord,
    trigger: &Trigger,
    client: &Connection,
    development: bool,
) -> Result<(), Error> {
    let settings = record.input.settings(record.id).map_err(Error::Invalid)?;
    let mut connection = DatabaseConnection::connect_write(&record.input, &settings).await?;
    write::verify_owner(&mut connection, record.id).await?;
    if !claim(&mut connection, trigger).await? {
        return Ok(());
    }
    if state
        .store
        .database_trigger(trigger.id)
        .map_err(|_| Error::Invalid("Cannot inspect trigger ownership"))?
        .is_none_or(|current| current.paused || !current.installed)
    {
        return Ok(());
    }
    let channel = rabbitmq::channel(client, &trigger.queue(), development).await?;
    let guard = RelayChannel(channel.clone());
    // Always close the channel, including failed/cancelled attempts. Dropping an
    // unacknowledged delivery alone does not requeue it while a channel is open.
    let result = drain(state, trigger, &mut connection, &channel).await;
    let _ = channel.close(200, "relay complete".into()).await;
    drop(guard);
    result
}

async fn drain(
    state: &AppState,
    trigger: &Trigger,
    connection: &mut DatabaseConnection,
    channel: &lapin::Channel,
) -> Result<(), Error> {
    let engine = connection.engine();
    let outbox = trigger.outbox(engine);
    let queue = trigger.queue();
    let secret = state
        .store
        .database_service_secret(trigger.input.service_id)
        .map_err(|_| Error::Invalid("Cannot load the event signing key"))?;
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| Error::Invalid("Cannot create event client"))?;
    let length_type = if engine == Engine::Postgres {
        "bigint"
    } else {
        "SIGNED"
    };
    for _ in 0..100 {
        if state
            .store
            .database_trigger(trigger.id)
            .map_err(|_| Error::Invalid("Cannot inspect trigger state"))?
            .is_some_and(|t| t.paused)
        {
            break;
        }
        let sql = format!(
            "SELECT id,LEFT(payload,1048577),attempts,status,next_attempt,CAST(OCTET_LENGTH(payload) AS {length_type}) FROM {outbox} WHERE status IN ('pending','failed') ORDER BY id LIMIT 1"
        );
        let row: Option<DeliveryRow> = match connection {
            DatabaseConnection::Postgres(c) => sqlx::query_as(&sql).fetch_optional(c).await?,
            DatabaseConnection::Mysql(c) => sqlx::query_as(&sql).fetch_optional(c).await?,
        };
        if row
            .as_ref()
            .is_some_and(|r| r.3 == "pending" && r.4 > Utc::now().timestamp())
        {
            break;
        }
        let mut message = channel
            .basic_get(queue.as_str().into(), BasicGetOptions::default())
            .await
            .map_err(|_| Error::Invalid("RabbitMQ delivery is unavailable"))?;
        // Discard broker duplicates only after consulting durable source state.
        // Source rows are retained, including explicit skips and failed deliveries.
        if let Some(delivery) = &message {
            let expected = row.as_ref().map(|r| format!("{}:{}", trigger.id, r.0));
            if delivery
                .properties
                .message_id()
                .as_ref()
                .map(|s| s.as_str())
                != expected.as_deref()
            {
                let id = delivery
                    .properties
                    .message_id()
                    .as_ref()
                    .and_then(|s| s.as_str().strip_prefix(&format!("{}:", trigger.id)))
                    .and_then(|s| s.parse::<i64>().ok())
                    .ok_or(Error::Invalid("Unexpected RabbitMQ event identity"))?;
                let sql = format!("SELECT status FROM {outbox} WHERE id={id}");
                let stored: Option<String> = match connection {
                    DatabaseConnection::Postgres(c) => {
                        sqlx::query_scalar(&sql).fetch_optional(c).await?
                    }
                    DatabaseConnection::Mysql(c) => {
                        sqlx::query_scalar(&sql).fetch_optional(c).await?
                    }
                };
                if stored.is_none() {
                    return Err(Error::Invalid(
                        "RabbitMQ event has no retained source record",
                    ));
                }
                delivery
                    .ack(BasicAckOptions::default())
                    .await
                    .map_err(|_| {
                        Error::Invalid("Cannot acknowledge a retained broker duplicate")
                    })?;
                continue;
            }
        }
        let Some((id, payload, attempts, status, next, payload_bytes)) = row else {
            break;
        };
        if status == "failed" {
            if let Some(delivery) = message {
                delivery
                    .reject(BasicRejectOptions { requeue: false })
                    .await
                    .map_err(|_| Error::Invalid("Cannot retain failed delivery in RabbitMQ"))?;
            }
            return Err(Error::Invalid(
                "Replay the failed event before later events can be delivered",
            ));
        }
        if next > Utc::now().timestamp() {
            break;
        }
        let event_id = format!("{}:{id}", trigger.id);
        if payload_bytes > 512 * 1024 {
            update_delivery(
                connection,
                &outbox,
                id,
                12,
                false,
                Some("payload_too_large"),
            )
            .await?;
            break;
        }
        let change: Box<serde_json::value::RawValue> =
            serde_json::from_str(&payload).map_err(|_| Error::Invalid("Invalid stored event"))?;
        let body = event_body(&event_id, trigger, &change)?;
        if body.len() > 512 * 1024 {
            update_delivery(
                connection,
                &outbox,
                id,
                12,
                false,
                Some("payload_too_large"),
            )
            .await?;
            break;
        }
        if message.is_none() {
            rabbitmq::publish(channel, &queue, &event_id, &body).await?;
            message = channel
                .basic_get(queue.as_str().into(), BasicGetOptions::default())
                .await
                .map_err(|_| Error::Invalid("RabbitMQ delivery is unavailable"))?;
        }
        let delivery = message.ok_or(Error::Invalid(
            "Confirmed RabbitMQ delivery is not yet available",
        ))?;
        if delivery
            .properties
            .message_id()
            .as_ref()
            .map(|s| s.as_str())
            != Some(event_id.as_str())
            || delivery.data != body
        {
            delivery
                .reject(BasicRejectOptions { requeue: false })
                .await
                .map_err(|_| Error::Invalid("Cannot isolate an invalid RabbitMQ delivery"))?;
            return Err(Error::Invalid(
                "RabbitMQ payload differs from its retained source",
            ));
        }
        let deployment = state
            .store
            .service_deployment(trigger.input.service_id)
            .map_err(|_| Error::Invalid("Cannot load the target service"))?;
        let endpoint = deployment.as_ref().and_then(|d| {
            d.instances
                .iter()
                .find(|v| v.healthy && v.state == firecrab_api_types::VmState::Running)
                .map(|v| format!("http://{}:{}{}", v.ipv4, d.spec.port, trigger.input.path))
        });
        let timestamp = Utc::now().timestamp().to_string();
        let mut signature = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
            .map_err(|_| Error::Invalid("Invalid signing key"))?;
        signature.update(timestamp.as_bytes());
        signature.update(b".");
        signature.update(&delivery.data);
        let delivered = if let Some(endpoint) = endpoint {
            http.post(endpoint)
                .header("content-type", "application/json")
                .header("x-firecrab-event-id", &event_id)
                .header("x-firecrab-timestamp", timestamp)
                .header(
                    "x-firecrab-signature",
                    format!("sha256={:x}", signature.finalize().into_bytes()),
                )
                .body(delivery.data.clone())
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
        } else {
            false
        };
        // A handler returns success only after committing its inbox and business
        // effect. Persist completion before ACK; an uncertain HTTP result retries
        // the same event ID and the transactional inbox suppresses duplicates.
        update_delivery(
            connection,
            &outbox,
            id,
            attempts + 1,
            delivered,
            if delivered {
                None
            } else {
                Some("handler_unavailable")
            },
        )
        .await?;
        if delivered {
            delivery
                .ack(BasicAckOptions::default())
                .await
                .map_err(|_| Error::Invalid("RabbitMQ completion acknowledgement failed"))?;
        } else {
            delivery
                .reject(BasicRejectOptions {
                    requeue: attempts + 1 < 12,
                })
                .await
                .map_err(|_| Error::Invalid("RabbitMQ retry acknowledgement failed"))?;
            break;
        }
    }
    Ok(())
}

/// Persist retry scheduling and completion before changing broker delivery state.
async fn update_delivery(
    connection: &mut DatabaseConnection,
    outbox: &str,
    id: i64,
    attempts: i32,
    delivered: bool,
    error: Option<&str>,
) -> Result<(), Error> {
    let status = if delivered {
        "delivered"
    } else if attempts >= 12 {
        "failed"
    } else {
        "pending"
    };
    let next = Utc::now().timestamp() + retry_delay(attempts);
    let stamp = if delivered {
        "CURRENT_TIMESTAMP"
    } else {
        "NULL"
    };
    match connection {
        DatabaseConnection::Postgres(c) => {
            sqlx::query(&format!("UPDATE {outbox} SET status=$2,attempts=$3,next_attempt=$4,last_error=$5,delivered_at={stamp} WHERE id=$1")).bind(id).bind(status).bind(attempts).bind(next).bind(error).execute(c).await?;
        }
        DatabaseConnection::Mysql(c) => {
            sqlx::query(&format!("UPDATE {outbox} SET status=?,attempts=?,next_attempt=?,last_error=?,delivered_at={stamp} WHERE id=?")).bind(status).bind(attempts).bind(next).bind(error).bind(id).execute(c).await?;
        }
    }
    Ok(())
}

/// Poll retained triggers with a bounded number of concurrent source relays.
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        use futures_util::{StreamExt, stream};
        use sha2::Digest;
        let mut cached: Option<(String, Arc<Connection>)> = None;
        loop {
            let configs = (
                state.store.database_rabbitmq(),
                state.store.database_triggers(),
            );
            if let (Ok(Some(rabbitmq)), Ok(triggers)) = configs {
                let hash = format!(
                    "{:x}",
                    Sha256::digest(serde_json::to_vec(&rabbitmq).unwrap_or_default())
                );
                if cached
                    .as_ref()
                    .is_none_or(|(key, client)| *key != hash || !client.status().connected())
                {
                    cached = match tokio::time::timeout(Duration::from_secs(8), rabbitmq.connect())
                        .await
                    {
                        Ok(Ok(client)) => Some((hash, Arc::new(client))),
                        _ => None,
                    };
                }
                if let Some((_, client)) = &cached {
                    let development = rabbitmq.development;
                    let results =
                        stream::iter(triggers.into_iter().filter(|t| t.installed && !t.paused))
                            .map(|trigger| {
                                let state = &state;
                                let client = Arc::clone(client);
                                async move {
                                    let Ok(Some(record)) =
                                        state.store.pipe_database(trigger.database_id)
                                    else {
                                        return (trigger.id, Some("source_unavailable"));
                                    };
                                    let result = tokio::time::timeout(
                                        Duration::from_secs(45),
                                        relay(state, &record, &trigger, &client, development),
                                    )
                                    .await;
                                    (
                                        trigger.id,
                                        match result {
                                            Ok(Ok(())) => None,
                                            Ok(Err(_)) => Some("delivery_unavailable"),
                                            Err(_) => Some("delivery_timeout"),
                                        },
                                    )
                                }
                            })
                            .buffer_unordered(4)
                            .collect::<Vec<_>>()
                            .await;
                    for (id, error) in results {
                        let _ = state.store.set_database_trigger_health(id, error);
                    }
                } else {
                    for trigger in triggers.into_iter().filter(|t| t.installed && !t.paused) {
                        let _ = state
                            .store
                            .set_database_trigger_health(trigger.id, Some("rabbitmq_unavailable"));
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
}
