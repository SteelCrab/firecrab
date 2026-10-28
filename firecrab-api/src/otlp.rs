//! Bounded, opt-in OTLP/HTTP export and guest SDK configuration.
//!
//! The queue is in memory. An API restart or disabling export discards queued
//! and in-flight records; local observation history is the durable source.
//! Credentials in collector URLs and collector authentication are unsupported.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use uuid::Uuid;

const QUEUE_LIMIT: usize = 128;
const BATCH_LIMIT: usize = 16;
// Local app history retains up to 4096 bytes plus a UTF-8 truncation marker.
const RECORD_LIMIT: usize = 8 * 1024;
const RESPONSE_LIMIT: usize = 16 * 1024;
const MAX_ATTEMPTS: usize = 3;

/// Invalid collector settings never include the supplied URL in errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConfigError;

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid OTLP endpoint")
    }
}

/// Validate the OTLP base URL. Credentials, query parameters and fragments
/// are forbidden, so they cannot be persisted or returned as configuration.
pub(crate) fn validate_endpoint(endpoint: &str) -> Result<Url, ConfigError> {
    if endpoint.is_empty()
        || endpoint.len() > 2048
        || endpoint
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
    {
        return Err(ConfigError);
    }
    let url = Url::parse(endpoint).map_err(|_| ConfigError)?;
    let authority = endpoint
        .split_once("://")
        .map(|(_, tail)| tail.split(['/', '?', '#']).next().unwrap_or_default())
        .ok_or(ConfigError)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || url.cannot_be_a_base()
        || authority.contains('@')
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ConfigError);
    }
    Ok(url)
}

/// The common OTLP base URL receives the standard logs suffix.
fn logs_url(endpoint: &str) -> Result<Url, ConfigError> {
    let mut url = validate_endpoint(endpoint)?;
    url.path_segments_mut()
        .map_err(|_| ConfigError)?
        .pop_if_empty()
        .extend(["v1", "logs"]);
    Ok(url)
}

/// Environment injection never changes a user-specified value or Secret binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnvError {
    /// The endpoint or service name is invalid.
    Invalid,
    /// A managed OTLP variable conflicts with a user variable or Secret.
    Conflict(&'static str),
    /// The VM's existing 64-variable limit would be exceeded.
    TooMany,
}

impl std::fmt::Display for EnvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid => f.write_str("invalid OTLP configuration"),
            Self::Conflict(key) => write!(f, "OTLP environment conflicts with {key}"),
            Self::TooMany => f.write_str("OTLP environment exceeds the VM variable limit"),
        }
    }
}

/// Inject SDK settings into the environment for a new guest boot. Apps must
/// already use an OpenTelemetry SDK or agent to produce telemetry.
pub(crate) fn application_env(
    endpoint: &str,
    service_name: &str,
    existing: &BTreeMap<String, String>,
    secret_names: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>, EnvError> {
    let endpoint = validate_endpoint(endpoint).map_err(|_| EnvError::Invalid)?;
    if service_name.is_empty()
        || service_name.len() > 64
        || service_name.bytes().any(|b| b.is_ascii_control())
    {
        return Err(EnvError::Invalid);
    }
    // Signal-specific settings override the common SDK settings and would
    // silently send a signal elsewhere. Require the operator to remove them.
    for key in [
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
        "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
        "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
        "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL",
        "OTEL_EXPORTER_OTLP_METRICS_PROTOCOL",
        "OTEL_EXPORTER_OTLP_LOGS_PROTOCOL",
    ] {
        if existing.contains_key(key) || secret_names.contains(key) {
            return Err(EnvError::Conflict(key));
        }
    }
    let variables = [
        ("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint.to_string()),
        ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf".to_owned()),
        ("OTEL_SERVICE_NAME", service_name.to_owned()),
    ];
    let mut result = existing.clone();
    for (key, value) in variables {
        if secret_names.contains(key) || existing.get(key).is_some_and(|current| current != &value)
        {
            return Err(EnvError::Conflict(key));
        }
        result.entry(key.into()).or_insert(value);
    }
    if result.len() + secret_names.len() > 64 {
        return Err(EnvError::TooMany);
    }
    Ok(result)
}

/// Reject a service configuration that cannot receive the managed OTLP settings.
pub(crate) fn preflight_spec(
    endpoint: &str,
    spec: &firecrab_api_types::MicroServiceSpec,
    request: Uuid,
) -> Result<(), crate::error::AppError> {
    let secrets = spec.secrets.keys().cloned().collect();
    application_env(endpoint, &spec.name, &spec.env, &secrets).map_err(|_| {
        crate::error::AppError::validation(
            BTreeMap::from([(
                "otlp".into(),
                "OTLP settings conflict with the service environment or exceed its limit".into(),
            )]),
            request,
        )
    })?;
    Ok(())
}

/// Check the current operator setting before admitting a service change.
pub(crate) async fn preflight_service(
    state: &crate::state::AppState,
    service: Uuid,
    spec: &firecrab_api_types::MicroServiceSpec,
    request: Uuid,
) -> Result<(), crate::error::AppError> {
    let store = state.store.clone();
    let endpoint =
        crate::handlers::micro_services::database(request, move || store.otlp_endpoint(service))
            .await?;
    if let Some(endpoint) = endpoint {
        preflight_spec(&endpoint, spec, request)?;
    }
    Ok(())
}

/// The source of an exported service observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordKind {
    /// Isolated application stdout/stderr.
    Log,
    /// A platform lifecycle or health event.
    Event,
}

/// Log severity uses the OpenTelemetry numeric severity ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Severity {
    /// Informational observation.
    Info,
    /// Recoverable failure or warning.
    Warn,
    /// Failed operation or unhealthy condition.
    Error,
}

/// Guest process stream, kept separate from the interactive console.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stream {
    /// Application standard output.
    Stdout,
    /// Application standard error.
    Stderr,
}

/// One accepted item. Timestamps are Unix nanoseconds; body size is capped.
#[derive(Clone)]
pub(crate) struct ExportRecord {
    /// Event time as Unix nanoseconds, encoded as a string for OTLP JSON.
    pub timestamp_unix_nano: u64,
    /// Application output or an operator-visible lifecycle event.
    pub kind: RecordKind,
    /// The severity attached to the record.
    pub severity: Severity,
    /// Replica that produced this record, when known.
    pub instance_id: Option<Uuid>,
    /// Guest stdout or stderr, when applicable.
    pub stream: Option<Stream>,
    /// Text sent only to the explicitly configured collector.
    pub body: String,
}

/// The most recent delivery outcome. No collector response body is exposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DeliveryState {
    /// No export has completed yet.
    Pending,
    /// All records in the latest batch were accepted.
    Delivered,
    /// A transient failure is being retried.
    Retrying,
    /// The collector accepted only part of a batch, or reported a warning.
    Partial,
    /// A batch could not be delivered or was rejected.
    Failed,
    /// The exporter was disabled.
    Disabled,
}

/// Redacted, in-memory counters. Restarting the API resets these counters and
/// can discard unsent records; export does not block local observation storage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExportStatus {
    /// Whether this worker accepts new records.
    pub enabled: bool,
    /// The queue is intentionally volatile and is not replayed after restart.
    pub queue_persistent: bool,
    /// Most recent worker outcome.
    pub state: DeliveryState,
    /// Number of accepted records still waiting to be sent.
    pub queued: u64,
    /// Number of records in the current request or retry.
    pub in_flight: u64,
    /// Records accepted into the bounded queue.
    pub accepted: u64,
    /// Records the collector acknowledged successfully.
    pub delivered: u64,
    /// Records abandoned on overflow, failure, or shutdown.
    pub dropped: u64,
    /// Records rejected by partial-success responses.
    pub rejected: u64,
    /// Batches that failed permanently or exhausted retries.
    pub failed_batches: u64,
}

impl Default for ExportStatus {
    fn default() -> Self {
        Self {
            enabled: true,
            queue_persistent: false,
            state: DeliveryState::Pending,
            queued: 0,
            in_flight: 0,
            accepted: 0,
            delivered: 0,
            dropped: 0,
            rejected: 0,
            failed_batches: 0,
        }
    }
}

/// Enqueue errors contain no observation body or endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubmitError {
    /// One record exceeded the maximum UTF-8 byte length.
    TooLarge,
    /// The bounded queue is full; the record was not accepted.
    Full,
    /// Export is disabled or the worker has stopped.
    Disabled,
}

struct Inner {
    /// Bounded per-service submission queue.
    sender: mpsc::Sender<ExportRecord>,
    /// Redacted worker state shared with the operator view.
    status: Arc<Mutex<ExportStatus>>,
    /// Cancels an old worker on disable or replacement.
    worker: tokio::task::AbortHandle,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

/// One service's exporter. Construct it only for an explicitly enabled setting.
#[derive(Clone)]
pub(crate) struct Exporter {
    inner: Arc<Inner>,
}

impl Exporter {
    /// Create one bounded service-specific worker, without making a request yet.
    pub(crate) fn new(
        endpoint: &str,
        service_id: Uuid,
        service_name: &str,
    ) -> Result<Self, ConfigError> {
        let destination = logs_url(endpoint)?;
        if service_name.is_empty()
            || service_name.len() > 64
            || service_name.bytes().any(|b| b.is_ascii_control())
            || tokio::runtime::Handle::try_current().is_err()
        {
            return Err(ConfigError);
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| ConfigError)?;
        let (sender, receiver) = mpsc::channel(QUEUE_LIMIT);
        let status = Arc::new(Mutex::new(ExportStatus::default()));
        let worker = tokio::spawn(run(
            receiver,
            client,
            destination,
            service_id,
            service_name.to_owned(),
            Arc::clone(&status),
        ))
        .abort_handle();
        Ok(Self {
            inner: Arc::new(Inner {
                sender,
                status,
                worker,
            }),
        })
    }

    /// Submit without blocking service operation. Full queues drop the new item.
    pub(crate) fn try_send(&self, record: ExportRecord) -> Result<(), SubmitError> {
        let mut status = self.inner.status.lock().unwrap_or_else(|p| p.into_inner());
        if !status.enabled {
            return Err(SubmitError::Disabled);
        }
        if record.body.len() > RECORD_LIMIT {
            status.dropped = status.dropped.saturating_add(1);
            status.state = DeliveryState::Failed;
            return Err(SubmitError::TooLarge);
        }
        match self.inner.sender.try_send(record) {
            Ok(()) => {
                status.accepted = status.accepted.saturating_add(1);
                status.queued = status.queued.saturating_add(1);
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                status.dropped = status.dropped.saturating_add(1);
                status.state = DeliveryState::Failed;
                Err(SubmitError::Full)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                status.dropped = status.dropped.saturating_add(1);
                status.state = DeliveryState::Failed;
                Err(SubmitError::Disabled)
            }
        }
    }

    /// Copy the redacted current status.
    pub(crate) fn status(&self) -> ExportStatus {
        self.inner
            .status
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Stop sending immediately when configuration is disabled or replaced.
    /// Pending and in-flight records are counted as dropped; delivery of an
    /// already transmitted HTTP request cannot be undone.
    pub(crate) fn shutdown(&self) {
        let mut status = self.inner.status.lock().unwrap_or_else(|p| p.into_inner());
        if status.enabled {
            status.enabled = false;
            status.state = DeliveryState::Disabled;
            status.dropped = status
                .dropped
                .saturating_add(status.queued)
                .saturating_add(status.in_flight);
            status.queued = 0;
            status.in_flight = 0;
            self.inner.worker.abort();
        }
    }
}

/// Encode one service's records as OTLP JSON, grouped by VM identity.
fn payload(service_id: Uuid, service_name: &str, records: &[ExportRecord]) -> Result<Vec<u8>, ()> {
    let mut groups: BTreeMap<Option<Uuid>, Vec<Value>> = BTreeMap::new();
    for record in records {
        let (number, text) = match record.severity {
            Severity::Info => (9, "INFO"),
            Severity::Warn => (13, "WARN"),
            Severity::Error => (17, "ERROR"),
        };
        let kind = match record.kind {
            RecordKind::Log => "log",
            RecordKind::Event => "event",
        };
        let mut attributes =
            vec![json!({"key": "firecrab.observation.kind", "value": {"stringValue": kind}})];
        if let Some(stream) = record.stream {
            let stream = match stream {
                Stream::Stdout => "stdout",
                Stream::Stderr => "stderr",
            };
            attributes.push(json!({"key":"log.iostream","value":{"stringValue":stream}}));
        }
        groups.entry(record.instance_id).or_default().push(json!({
            "timeUnixNano": record.timestamp_unix_nano.to_string(),
            "severityNumber": number, "severityText": text,
            "body": {"stringValue": record.body},
            "attributes": attributes
        }));
    }
    let resources: Vec<Value> = groups.into_iter().map(|(instance_id, records)| {
        let mut attributes = vec![
            json!({"key":"service.name", "value":{"stringValue":service_name}}),
            json!({"key":"firecrab.service.id", "value":{"stringValue":service_id.to_string()}}),
        ];
        if let Some(id) = instance_id {
            attributes.push(json!({"key":"service.instance.id", "value":{"stringValue":id.to_string()}}));
        }
        json!({
        "resource": {"attributes": attributes},
        "scopeLogs": [{"scope":{"name":"firecrab.observability"}, "logRecords":records}]
        })
    }).collect();
    serde_json::to_vec(&json!({"resourceLogs": resources})).map_err(|_| ())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempt {
    /// Acknowledged records and a redacted optional collector warning.
    Accepted { rejected: usize, warning: bool },
    /// Retryable transport or HTTP status.
    Retry,
    /// Nonretryable response, protocol error, or exhausted attempts.
    Failed,
}

/// Send one request and interpret the bounded, redacted OTLP response.
async fn attempt(
    client: &Client,
    destination: &Url,
    body: &[u8],
    count: usize,
) -> (Attempt, Option<Duration>) {
    let response = client
        .post(destination.clone())
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_vec())
        .send()
        .await;
    let Ok(mut response) = response else {
        return (Attempt::Retry, None);
    };
    let status = response.status();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .map(|secs| Duration::from_secs(secs.min(5)));
    if matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    ) {
        return (Attempt::Retry, retry_after);
    }
    if status != StatusCode::OK {
        return (Attempt::Failed, None);
    }
    let mut bytes = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if bytes.len().saturating_add(chunk.len()) <= RESPONSE_LIMIT => {
                bytes.extend_from_slice(&chunk)
            }
            Ok(Some(_)) | Err(_) => return (Attempt::Failed, None),
            Ok(None) => break,
        }
    }
    let Ok(response) = serde_json::from_slice::<Value>(&bytes) else {
        return (Attempt::Failed, None);
    };
    if !response.is_object() {
        return (Attempt::Failed, None);
    }
    let partial = response
        .get("partialSuccess")
        .or_else(|| response.get("partial_success"));
    let Some(partial) = partial else {
        return (
            Attempt::Accepted {
                rejected: 0,
                warning: false,
            },
            None,
        );
    };
    // Protobuf JSON accepts null as the default value for any field.
    if partial.is_null() {
        return (
            Attempt::Accepted {
                rejected: 0,
                warning: false,
            },
            None,
        );
    }
    if !partial.is_object() {
        return (Attempt::Failed, None);
    }
    let rejected = partial
        .get("rejectedLogRecords")
        .or_else(|| partial.get("rejected_log_records"));
    let rejected = match rejected {
        None | Some(Value::Null) => 0,
        Some(Value::String(value)) => match value.parse::<usize>() {
            Ok(value) => value,
            Err(_) => return (Attempt::Failed, None),
        },
        Some(Value::Number(value)) => match value.as_u64().and_then(|n| usize::try_from(n).ok()) {
            Some(value) => value,
            None => return (Attempt::Failed, None),
        },
        Some(_) => return (Attempt::Failed, None),
    };
    if rejected > count {
        return (Attempt::Failed, None);
    }
    let warning = match partial
        .get("errorMessage")
        .or_else(|| partial.get("error_message"))
    {
        None | Some(Value::Null) => false,
        Some(Value::String(message)) => !message.is_empty(),
        Some(_) => return (Attempt::Failed, None),
    };
    (Attempt::Accepted { rejected, warning }, None)
}

#[cfg(test)]
mod tests;

/// Drain bounded batches; local observation persistence never waits for export.
async fn run(
    mut receiver: mpsc::Receiver<ExportRecord>,
    client: Client,
    destination: Url,
    service_id: Uuid,
    service_name: String,
    status: Arc<Mutex<ExportStatus>>,
) {
    while let Some(first) = receiver.recv().await {
        let mut records = Vec::with_capacity(BATCH_LIMIT);
        records.push(first);
        while records.len() < BATCH_LIMIT {
            match receiver.try_recv() {
                Ok(record) => records.push(record),
                Err(_) => break,
            }
        }
        let count = records.len() as u64;
        {
            let mut s = status.lock().unwrap_or_else(|p| p.into_inner());
            if !s.enabled {
                break;
            }
            s.queued = s.queued.saturating_sub(count);
            s.in_flight = count;
        }
        let body = match payload(service_id, &service_name, &records) {
            Ok(body) => body,
            Err(()) => {
                let mut s = status.lock().unwrap_or_else(|p| p.into_inner());
                if !s.enabled {
                    break;
                }
                s.in_flight = 0;
                s.dropped = s.dropped.saturating_add(count);
                s.failed_batches = s.failed_batches.saturating_add(1);
                s.state = DeliveryState::Failed;
                continue;
            }
        };
        let mut outcome = Attempt::Failed;
        for attempt_index in 0..MAX_ATTEMPTS {
            let (result, retry_after) = attempt(&client, &destination, &body, records.len()).await;
            if result != Attempt::Retry {
                outcome = result;
                break;
            }
            if attempt_index + 1 == MAX_ATTEMPTS {
                break;
            }
            {
                let mut s = status.lock().unwrap_or_else(|p| p.into_inner());
                if !s.enabled {
                    return;
                }
                s.state = DeliveryState::Retrying;
            }
            tokio::time::sleep(retry_after.unwrap_or(Duration::from_millis(250 << attempt_index)))
                .await;
        }
        let mut s = status.lock().unwrap_or_else(|p| p.into_inner());
        if !s.enabled {
            break;
        }
        s.in_flight = 0;
        match outcome {
            Attempt::Accepted { rejected, warning } => {
                s.delivered = s
                    .delivered
                    .saturating_add(count.saturating_sub(rejected as u64));
                s.rejected = s.rejected.saturating_add(rejected as u64);
                s.state = if rejected > 0 || warning {
                    DeliveryState::Partial
                } else {
                    DeliveryState::Delivered
                };
            }
            Attempt::Retry | Attempt::Failed => {
                s.dropped = s.dropped.saturating_add(count);
                s.failed_batches = s.failed_batches.saturating_add(1);
                s.state = DeliveryState::Failed;
            }
        }
    }
}
