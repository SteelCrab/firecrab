//! Bounded management calls to the private, Firecrab-owned RabbitMQ guest.
use super::{EngineRecord, QueueRecord, segment};
use crate::{error::AppError, persistence::Store};
use firecrab_api_types::{QueueAccess, QueueMetrics};
use futures_util::StreamExt;
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

/// Bounded management client for the owned broker with sanitized failures.
pub(crate) struct RabbitClient {
    client: Client,
    origin: String,
    username: String,
    password: String,
}
impl RabbitClient {
    /// Resolve the owned broker address and its private management credentials.
    pub(crate) fn new(store: &Store) -> Result<Self, &'static str> {
        let record = store
            .queue_engine()
            .map_err(|_| "Could not read Queue engine")?
            .ok_or("Queue engine is not configured")?;
        let lease = store
            .active_lease(record.vm_id)
            .map_err(|_| "Could not read Queue engine address")?
            .ok_or("Queue engine has no network address")?;
        Self::at(format!("http://{}:15672", lease.ipv4), record)
    }
    /// Build transport for an explicit broker origin; disposable tests use a loopback origin.
    pub(super) fn at(origin: String, record: EngineRecord) -> Result<Self, &'static str> {
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(4))
            .build()
            .map_err(|_| "Could not configure Queue client")?;
        Ok(Self {
            client,
            origin,
            username: record.username,
            password: record.password,
        })
    }
    /// Send a bounded management request and sanitize upstream failures.
    pub(crate) async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        id: Uuid,
    ) -> Result<Value, AppError> {
        self.send(method, path, body, id, false).await
    }
    /// Implement bounded response decoding and idempotent missing-resource deletion.
    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        id: Uuid,
        missing_ok: bool,
    ) -> Result<Value, AppError> {
        let deleting = method == Method::DELETE;
        let mut request = self
            .client
            .request(method, format!("{}/api/{path}", self.origin))
            .basic_auth(&self.username, Some(&self.password));
        if let Some(body) = body {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.to_string())
        }
        let response = request.send().await.map_err(|_| {
            AppError::unavailable(
                "RabbitMQ is unreachable; reconnect after the engine recovers",
                id,
            )
        })?;
        if missing_ok && response.status() == StatusCode::NOT_FOUND {
            return Ok(Value::Null);
        }
        if !response.status().is_success() {
            return Err(match response.status() {
                StatusCode::NOT_FOUND => AppError::conflict(
                    "queue_missing",
                    "RabbitMQ does not contain the expected Queue resource",
                    id,
                ),
                StatusCode::BAD_REQUEST if deleting => AppError::conflict(
                    "queue_not_empty",
                    "RabbitMQ rejected the operation; verify that the queue is empty and unused",
                    id,
                ),
                _ => AppError::unavailable("RabbitMQ rejected the management operation", id),
            });
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|_| AppError::unavailable("Could not read RabbitMQ response", id))?;
            if bytes.len() + chunk.len() > 128 * 1024 {
                return Err(AppError::unavailable(
                    "RabbitMQ response exceeded its size limit",
                    id,
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| AppError::unavailable("RabbitMQ returned invalid metadata", id))
    }
    /// Require the RabbitMQ overview endpoint to respond successfully.
    pub(crate) async fn verify(&self, id: Uuid) -> Result<(), AppError> {
        self.request(Method::GET, "overview?columns=rabbitmq_version", None, id)
            .await?;
        Ok(())
    }
    /// Idempotently declare durable queues, failure routing and separate role permissions.
    pub(crate) async fn declare(&self, q: &QueueRecord, id: Uuid) -> Result<(), AppError> {
        let v = segment(&q.vhost());
        let main = segment(&q.name);
        let dead = segment(&format!("{}.failed", q.name));
        self.request(Method::PUT, &format!("vhosts/{v}"), Some(json!({})), id)
            .await?;
        self.request(Method::PUT,&format!("exchanges/{v}/failed"),Some(json!({"type":"fanout","durable":true,"auto_delete":false,"internal":false,"arguments":{}})),id).await?;
        self.request(Method::PUT,&format!("queues/{v}/{dead}"),Some(json!({"durable":true,"auto_delete":false,"arguments":{"x-queue-type":"quorum","x-quorum-initial-group-size":1,"x-max-length-bytes":q.max_bytes,"x-overflow":"reject-publish"}})),id).await?;
        self.request(
            Method::POST,
            &format!("bindings/{v}/e/failed/q/{dead}"),
            Some(json!({"routing_key":"","arguments":{}})),
            id,
        )
        .await?;
        self.request(Method::PUT,&format!("policies/{v}/firecrab"),Some(json!({"pattern":format!("^{}$",q.name),"apply-to":"quorum_queues","priority":100,"definition":{"dead-letter-exchange":"failed","dead-letter-strategy":"at-least-once","overflow":"reject-publish","delivery-limit":q.delivery_limit}})),id).await?;
        self.request(Method::PUT,&format!("queues/{v}/{main}"),Some(json!({"durable":true,"auto_delete":false,"arguments":{"x-queue-type":"quorum","x-quorum-initial-group-size":1,"x-max-length-bytes":q.max_bytes,"x-overflow":"reject-publish"}})),id).await?;
        for access in [QueueAccess::Publish, QueueAccess::Consume] {
            let user = segment(&q.username(access));
            self.request(
                Method::PUT,
                &format!("users/{user}"),
                Some(json!({"password":q.password(access),"tags":""})),
                id,
            )
            .await?;
            let (write, read) = match access {
                QueueAccess::Publish => (format!("^(amq[.]default|{})$", q.name), "^$".into()),
                QueueAccess::Consume => {
                    ("^$".into(), format!("^({}|{}[.]failed)$", q.name, q.name))
                }
            };
            self.request(
                Method::PUT,
                &format!("permissions/{v}/{user}"),
                Some(json!({"configure":"^$","write":write,"read":read})),
                id,
            )
            .await?;
        }
        Ok(())
    }
    /// Read main and failure queue counts without returning upstream metadata.
    pub(crate) async fn metrics(
        &self,
        q: &QueueRecord,
        id: Uuid,
    ) -> Result<QueueMetrics, AppError> {
        self.read_metrics(q, id, false).await
    }
    async fn read_metrics(
        &self,
        q: &QueueRecord,
        id: Uuid,
        missing_ok: bool,
    ) -> Result<QueueMetrics, AppError> {
        let v = segment(&q.vhost());
        let main = segment(&q.name);
        let dead = segment(&format!("{}.failed", q.name));
        let value = self
            .send(
                Method::GET,
                &format!(
                    "queues/{v}/{main}?columns=messages_ready,messages_unacknowledged,consumers"
                ),
                None,
                id,
                missing_ok,
            )
            .await?;
        let failed = self
            .send(
                Method::GET,
                &format!("queues/{v}/{dead}?columns=messages"),
                None,
                id,
                missing_ok,
            )
            .await?;
        Ok(QueueMetrics {
            ready: value["messages_ready"].as_u64().unwrap_or(0),
            unacknowledged: value["messages_unacknowledged"].as_u64().unwrap_or(0),
            consumers: value["consumers"].as_u64().unwrap_or(0),
            failed: failed["messages"].as_u64().unwrap_or(0),
        })
    }
    /// Delete only empty, unused resources and tolerate an interrupted prior deletion.
    pub(crate) async fn delete(&self, q: &QueueRecord, id: Uuid) -> Result<(), AppError> {
        // Check both queues before any deletion. Conditional deletes still guard races.
        let metrics = self.read_metrics(q, id, true).await?;
        if metrics.ready != 0
            || metrics.unacknowledged != 0
            || metrics.consumers != 0
            || metrics.failed != 0
        {
            return Err(AppError::conflict(
                "queue_in_use",
                "Queue deletion requires no pending messages, failed messages, or consumers",
                id,
            ));
        }
        let v = segment(&q.vhost());
        for name in [&q.name, &format!("{}.failed", q.name)] {
            self.send(
                Method::DELETE,
                &format!("queues/{v}/{}?if-empty=true&if-unused=true", segment(name)),
                None,
                id,
                true,
            )
            .await?;
        }
        self.send(Method::DELETE, &format!("vhosts/{v}"), None, id, true)
            .await?;
        for access in [QueueAccess::Publish, QueueAccess::Consume] {
            self.send(
                Method::DELETE,
                &format!("users/{}", segment(&q.username(access))),
                None,
                id,
                true,
            )
            .await?;
        }
        Ok(())
    }
}
