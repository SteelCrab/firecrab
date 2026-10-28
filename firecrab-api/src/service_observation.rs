//! Operator-scoped service observations and bounded, opt-in export.
use crate::{
    ai_sandbox::access::Caller,
    error::AppError,
    extract::ValidatedJson,
    handlers::{micro_services::database, vms::parse_id},
    otlp::{self, DeliveryState, ExportRecord, Exporter, RecordKind, Severity, Stream},
    persistence::{
        service_logs::ServiceLog,
        service_observation::{Alert, Event},
    },
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::Duration,
};
use uuid::Uuid;

/// One service's exporter and in-memory history cursors.
struct Session {
    /// Validated collector base URL.
    endpoint: String,
    /// Service name attached to exported records.
    name: String,
    /// Bounded asynchronous OTLP worker.
    exporter: Exporter,
    /// Last submitted event identifier.
    events: i64,
    /// Last submitted app output identifier.
    logs: i64,
    /// Do not export buffered app output created before this opt-in.
    since_ms: Option<i64>,
}

/// Live exporter state. Observation history remains in SQLite.
#[derive(Default)]
pub(crate) struct Runtime {
    /// Active exporters indexed by service identifier.
    sessions: Mutex<HashMap<Uuid, Session>>,
    /// Services whose exporter could not be started.
    failed: Mutex<HashSet<Uuid>>,
}
impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObservationRuntime").finish_non_exhaustive()
    }
}
impl Runtime {
    /// Stop and discard the active exporter for a service.
    fn remove(&self, service: Uuid) {
        if let Some(old) = self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&service)
        {
            old.exporter.shutdown();
        }
        self.failed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&service);
    }
    /// Replace the exporter after a new setting has been persisted.
    fn install(&self, service: Uuid, session: Session) {
        let old = self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(service, session);
        if let Some(old) = old {
            old.exporter.shutdown();
        }
        self.failed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&service);
    }
    /// Return redacted delivery status for the currently stored endpoint.
    fn view(&self, service: Uuid, endpoint: Option<String>) -> OtlpView {
        let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let status = sessions
            .get(&service)
            .filter(|s| endpoint.as_deref() == Some(s.endpoint.as_str()))
            .map(|s| s.exporter.status());
        let (state, queued, in_flight, delivered, dropped, rejected, failed_batches) = match status
        {
            Some(s) => (
                s.state,
                s.queued,
                s.in_flight,
                s.delivered,
                s.dropped,
                s.rejected,
                s.failed_batches,
            ),
            None if endpoint.is_some()
                && self
                    .failed
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .contains(&service) =>
            {
                (DeliveryState::Failed, 0, 0, 0, 0, 0, 0)
            }
            None if endpoint.is_some() => (DeliveryState::Pending, 0, 0, 0, 0, 0, 0),
            None => (DeliveryState::Disabled, 0, 0, 0, 0, 0, 0),
        };
        OtlpView {
            endpoint,
            status: state,
            queue_persistent: false,
            queued,
            in_flight,
            delivered,
            dropped,
            rejected,
            failed_batches,
        }
    }
}

/// Redacted endpoint and delivery counters returned to an operator.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OtlpView {
    /// Current collector base URL, or none when disabled.
    endpoint: Option<String>,
    /// Most recent exporter state.
    status: DeliveryState,
    /// Whether queued records survive an API restart.
    queue_persistent: bool,
    /// Records waiting for delivery.
    queued: u64,
    /// Records in a delivery attempt.
    in_flight: u64,
    /// Records acknowledged by the collector.
    delivered: u64,
    /// Records dropped on overflow, shutdown or failure.
    dropped: u64,
    /// Records rejected by the collector.
    rejected: u64,
    /// Batches that exhausted retries or failed permanently.
    failed_batches: u64,
}
/// Operator view of one service's observations.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct View {
    /// Recent isolated application output.
    logs: Vec<ServiceLog>,
    /// App output collection status and any known gap.
    logs_status: String,
    /// Recent lifecycle and health events.
    events: Vec<Event>,
    /// Current state of actionable alerts.
    alerts: Vec<Alert>,
    /// Redacted collector and delivery status.
    otlp: OtlpView,
}
/// Explicit collector setting supplied by an operator.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Settings {
    /// Collector base URL; explicit null disables export.
    #[serde(deserialize_with = "required_endpoint")]
    endpoint: Option<String>,
}

/// Require the endpoint field even when the supplied value is null.
fn required_endpoint<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)
}

/// Reject callers without verified operator privileges.
fn operator(caller: Caller, request: Uuid) -> Result<(), AppError> {
    caller.operator().map_err(|_| {
        AppError::new(
            StatusCode::FORBIDDEN,
            "service_operator_required",
            "Only the operator can access service observations",
            request,
        )
    })
}

/// Reject services owned by a controller or managed across hosts.
async fn ensure_local_service(
    state: &AppState,
    service: Uuid,
    request: Uuid,
) -> Result<(), AppError> {
    let store = state.store.clone();
    let cluster_service = crate::cluster::stored(move || store.cluster_service(service)).await;
    let cluster_owned = match cluster_service {
        Ok(_) => true,
        Err(crate::persistence::ClusterError::Missing) => false,
        Err(_) => return Err(AppError::internal(request)),
    };
    let store = state.store.clone();
    let worker_owned =
        database(request, move || store.service_is_controller_owned(service)).await?;
    if cluster_owned || worker_owned {
        return Err(AppError::conflict(
            "cluster_observability_unsupported",
            "Observations are available only for locally managed services",
            request,
        ));
    }
    Ok(())
}

/// Return stored observations only; reading does not collect or export data.
pub(crate) async fn get(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Json<View>, AppError> {
    operator(caller, request.0)?;
    let service = parse_id(&id, request.0)?;
    ensure_local_service(&state, service, request.0).await?;
    let store = state.store.clone();
    let (logs, events, alerts, endpoint, deployment) = database(request.0, move || {
        if store.micro_service(service)?.is_none() {
            return Ok(None);
        }
        Ok(Some((
            store.service_logs(service)?,
            store.service_events(service)?,
            store.service_alerts(service)?,
            store.otlp_endpoint(service)?,
            store.service_deployment(service)?,
        )))
    })
    .await?
    .ok_or_else(|| AppError::not_found(request.0))?;
    let mut states = Vec::new();
    if let Some(deployment) = deployment {
        let vms = state.vms.lock().unwrap_or_else(|p| p.into_inner());
        for instance in &deployment.instances {
            if let Some(runtime) = vms.get(&instance.vm_id).and_then(|vm| vm.last_runtime_id) {
                states.push((instance.vm_id, runtime));
            }
        }
    }
    let store = state.store.clone();
    let logs_status = database(request.0, move || {
        let mut result = if store.service_has_log_gap(service)? {
            "truncated"
        } else {
            "waiting"
        };
        for (vm, runtime) in states {
            let (status, lost) = store.service_log_state(service, vm, runtime)?;
            if lost && result != "error" {
                result = "truncated";
            }
            if status == "error" {
                result = "error";
            } else if status == "unavailable" && !matches!(result, "error" | "truncated") {
                result = "unavailable";
            } else if status == "active" && result == "waiting" {
                result = "active";
            }
        }
        Ok(result.to_owned())
    })
    .await?;
    let otlp = state.observations.view(service, endpoint);
    Ok(Json(View {
        logs,
        logs_status,
        events,
        alerts,
        otlp,
    }))
}

/// Configure the current operator endpoint, independently of release snapshots.
pub(crate) async fn configure(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: Caller,
    Path(id): Path<String>,
    ValidatedJson(settings): ValidatedJson<Settings>,
) -> Result<Json<OtlpView>, AppError> {
    operator(caller, request.0)?;
    let service = parse_id(&id, request.0)?;
    let endpoint = settings
        .endpoint
        .map(|raw| otlp::validate_endpoint(&raw).map(|url| url.to_string()))
        .transpose()
        .map_err(|_| {
            AppError::new(
                StatusCode::BAD_REQUEST,
                "invalid_otlp_endpoint",
                "Use an HTTP or HTTPS OTLP base URL without credentials, query, or fragment",
                request.0,
            )
        })?;
    let claim = state.service_mutations.clone().lock_owned().await;
    ensure_local_service(&state, service, request.0).await?;
    let store = state.store.clone();
    let (saved, active, current_endpoint) = database(request.0, move || {
        Ok((
            store.micro_service(service)?,
            store.service_deployment(service)?,
            store.otlp_endpoint(service)?,
        ))
    })
    .await?;
    let saved = saved.ok_or_else(|| AppError::not_found(request.0))?;
    if endpoint == current_endpoint {
        return Ok(Json(state.observations.view(service, current_endpoint)));
    }
    if let Some(active) = &active
        && (active.phase.is_busy()
            || active.update.as_ref().is_some_and(|u| !u.completed)
            || active.recovery.as_ref().is_some_and(|r| r.repair.is_some()))
    {
        return Err(AppError::conflict(
            "service_busy",
            "Wait for the service operation to finish",
            request.0,
        ));
    }
    if let Some(endpoint) = &endpoint {
        otlp::preflight_spec(endpoint, &saved.spec, request.0)?;
        if let Some(active) = &active {
            otlp::preflight_spec(endpoint, &active.spec, request.0)?;
        }
    }
    let store = state.store.clone();
    let new_endpoint = endpoint.clone();
    let saved_name = saved.spec.name.clone();
    let response = tokio::spawn(async move {
        let _claim = claim;
        let store2 = store.clone();
        let cursors = database(request.0, move || {
            store2.service_observation_highwater(service)
        })
        .await?;
        // Prepare before the database write, so allocation failure leaves the
        // old endpoint and its active exporter unchanged.
        let exporter = new_endpoint
            .as_ref()
            .map(|endpoint| Exporter::new(endpoint, service, &saved_name))
            .transpose()
            .map_err(|_| AppError::internal(request.0))?;
        let value = new_endpoint.clone();
        let enabled_at_ms = database(request.0, move || {
            store.set_otlp_endpoint(service, value.as_deref())
        })
        .await?;
        if let (Some(endpoint), Some(exporter)) = (&new_endpoint, exporter) {
            state.observations.install(
                service,
                Session {
                    endpoint: endpoint.clone(),
                    name: saved_name,
                    exporter,
                    events: cursors.0,
                    logs: cursors.1,
                    since_ms: Some(enabled_at_ms),
                },
            );
        } else {
            state.observations.remove(service);
        }
        Ok::<_, AppError>(state.observations.view(service, new_endpoint))
    })
    .await
    .map_err(|_| AppError::internal(request.0))??;
    Ok(Json(response))
}

/// Parse an API timestamp into OTLP Unix nanoseconds.
fn nanos(timestamp: &str) -> u64 {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .and_then(|v| v.timestamp_nanos_opt())
        .and_then(|v| u64::try_from(v).ok())
        .unwrap_or(0)
}

/// Start service-specific exporters before collecting buffered VM logs.
pub(crate) fn start(state: AppState) {
    crate::service_log_collection::start_retention(state.store.clone());
    crate::service_log_retention::start(state.clone());
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if export_pass(&state).await.is_err() {
                tracing::warn!("service observation export failed");
            }
            crate::service_log_collection::collect(&state).await;
            if export_pass(&state).await.is_err() {
                tracing::warn!("service observation export failed");
            }
        }
    });
}

/// Reconcile exporters without blocking the async runtime on SQLite or mutexes.
async fn export_pass(state: &AppState) -> Result<(), ()> {
    let claim = state.service_mutations.clone().lock_owned().await;
    let state = state.clone();
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        // Keep mutation ownership even if the caller is cancelled while the
        // blocking pass is still using its configuration snapshot.
        let _claim = claim;
        let _runtime = runtime.enter();
        export_pass_blocking(&state)
    })
    .await
    .map_err(|_| ())?
    .map_err(|_| ())
}

/// Submit bounded pages while configuration mutations are excluded.
fn export_pass_blocking(state: &AppState) -> Result<(), crate::persistence::PersistenceError> {
    let services = state.store.list_micro_services()?;
    let mut present = HashSet::new();
    for service in services {
        let id = service.id;
        present.insert(id);
        if state.store.service_is_controller_owned(id)? {
            state.observations.remove(id);
            continue;
        }
        let config = state.store.otlp_configuration(id)?;
        let Some((endpoint, enabled_at_ms)) = config else {
            state.observations.remove(id);
            continue;
        };
        let previous = {
            let sessions = state
                .observations
                .sessions
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            sessions.get(&id).map(|s| {
                (
                    s.endpoint == endpoint,
                    s.name == service.spec.name,
                    (s.events, s.logs),
                    s.since_ms,
                )
            })
        };
        if previous.is_none_or(|(same_endpoint, same_name, _, _)| !same_endpoint || !same_name) {
            let (cursors, since_ms) = match previous {
                Some((true, _, cursors, since_ms)) => (cursors, since_ms),
                _ => (
                    state.store.service_observation_highwater(id)?,
                    Some(enabled_at_ms),
                ),
            };
            let Ok(exporter) = Exporter::new(&endpoint, id, &service.spec.name) else {
                state.observations.remove(id);
                state
                    .observations
                    .failed
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(id);
                continue;
            };
            state.observations.install(
                id,
                Session {
                    endpoint: endpoint.clone(),
                    name: service.spec.name.clone(),
                    exporter,
                    events: cursors.0,
                    logs: cursors.1,
                    since_ms,
                },
            );
        }
        let (event_cursor, log_cursor) = {
            let sessions = state
                .observations
                .sessions
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let Some(session) = sessions.get(&id) else {
                continue;
            };
            (session.events, session.logs)
        };
        // Do not hold the status mutex during database work: operator reads
        // use it to display delivery state on the asynchronous runtime.
        let events = state.store.service_events_after(id, event_cursor)?;
        let logs = state.store.service_logs_after(id, log_cursor)?;
        let mut sessions = state
            .observations
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(session) = sessions.get_mut(&id) else {
            continue;
        };
        for event in events {
            session.events = event.id;
            let time = nanos(&event.timestamp);
            let severity = match event.severity.as_str() {
                "error" => Severity::Error,
                "warning" => Severity::Warn,
                _ => Severity::Info,
            };
            let _ = session.exporter.try_send(ExportRecord {
                timestamp_unix_nano: time,
                kind: RecordKind::Event,
                severity,
                body: event.message,
                instance_id: None,
                stream: None,
            });
        }
        for log in logs {
            session.logs = log.id;
            let time = nanos(&log.timestamp);
            if session
                .since_ms
                .is_some_and(|since| time / 1_000_000 < since.max(0) as u64)
            {
                continue;
            }
            let stream = match log.stream.as_str() {
                "stdout" => Some(Stream::Stdout),
                "stderr" => Some(Stream::Stderr),
                _ => None,
            };
            let _ = session.exporter.try_send(ExportRecord {
                timestamp_unix_nano: time,
                kind: RecordKind::Log,
                severity: Severity::Info,
                body: log.message,
                instance_id: Uuid::parse_str(&log.instance_id).ok(),
                stream,
            });
        }
    }
    let stale: Vec<_> = state
        .observations
        .sessions
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .keys()
        .filter(|id| !present.contains(id))
        .copied()
        .collect();
    for id in stale {
        state.observations.remove(id);
    }
    state
        .observations
        .failed
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .retain(|id| present.contains(id));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{persistence::service_logs::InputLog, templates::TemplateRegistry};
    use firecrab_api_types::{MicroServiceResponse, MicroServiceSpec};

    #[tokio::test(flavor = "current_thread")]
    async fn a_blocked_export_pass_does_not_block_the_async_executor() {
        let root = tempfile::tempdir().unwrap();
        let state = AppState::with_db_file(
            TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap(),
            root.path().join("state.db"),
        )
        .await
        .unwrap();
        let id = Uuid::new_v4();
        let spec = serde_json::from_value(serde_json::json!({"name":"lock-test", "image":"ghcr.io/team/app:v1", "port":8080, "ram":128, "cpu":1})).unwrap();
        state
            .store
            .insert_micro_service(&MicroServiceResponse { id, spec })
            .unwrap();
        let blocker = state.clone();
        let (ready, wait) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let _guard = blocker.observations.sessions.lock().unwrap();
            ready.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(1200));
        });
        wait.recv().unwrap();
        let exporting = state.clone();
        let started = std::time::Instant::now();
        let pass = tokio::spawn(async move { export_pass(&exporting).await });
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            started.elapsed() < Duration::from_millis(600),
            "export blocked the async executor"
        );
        assert!(state.service_mutations.try_lock().is_err());
        let configuring = state.clone();
        let update = tokio::spawn(async move {
            configure(
                State(configuring),
                Extension(RequestId(Uuid::new_v4())),
                Caller::default(),
                Path(id.to_string()),
                ValidatedJson(Settings {
                    endpoint: Some("http://collector.test".into()),
                }),
            )
            .await
        });
        pass.abort();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            state.service_mutations.try_lock().is_err(),
            "cancelled caller released the active pass's lock"
        );
        assert!(
            !update.is_finished(),
            "configuration changed during the active pass"
        );
        tokio::task::spawn_blocking(move || thread.join().unwrap())
            .await
            .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(1), update)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_ok());
        assert_eq!(
            state.store.otlp_endpoint(id).unwrap().as_deref(),
            Some("http://collector.test/")
        );
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if state.service_mutations.try_lock().is_ok() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn exporter_only_submits_new_service_history_and_same_settings_preserve_session() {
        let root = tempfile::tempdir().unwrap();
        let state = AppState::with_db_file(
            TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap(),
            root.path().join("state.db"),
        )
        .await
        .unwrap();
        let id = Uuid::new_v4();
        let spec: MicroServiceSpec = serde_json::from_value(serde_json::json!({"name":"orders", "image":"ghcr.io/team/app:v1", "port":8080, "ram":128, "cpu":1})).unwrap();
        state
            .store
            .insert_micro_service(&MicroServiceResponse { id, spec })
            .unwrap();
        let vm = Uuid::new_v4();
        let runtime = Uuid::new_v4();
        let log = |sequence, timestamp, text: &str| InputLog {
            sequence,
            timestamp,
            stream: "stdout".into(),
            message: text.into(),
        };
        state
            .store
            .append_service_logs(
                id,
                (vm, runtime),
                1,
                "active",
                false,
                &[log(1, chrono::Utc::now().timestamp_millis(), "before")],
            )
            .unwrap();
        let boundary = state
            .store
            .set_otlp_endpoint(id, Some("http://127.0.0.1:9/"))
            .unwrap();
        export_pass(&state).await.unwrap();
        assert_eq!(
            state.observations.sessions.lock().unwrap()[&id]
                .exporter
                .status()
                .accepted,
            0
        );
        state
            .store
            .append_service_logs(
                id,
                (vm, runtime),
                2,
                "active",
                false,
                &[log(2, boundary + 1, "after")],
            )
            .unwrap();
        export_pass(&state).await.unwrap();
        assert_eq!(
            state.observations.sessions.lock().unwrap()[&id]
                .exporter
                .status()
                .accepted,
            1
        );
        let response = configure(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Caller::default(),
            Path(id.to_string()),
            ValidatedJson(Settings {
                endpoint: Some("http://127.0.0.1:9".into()),
            }),
        )
        .await
        .unwrap();
        assert!(response.0.endpoint.is_some());
        assert_eq!(
            state.observations.sessions.lock().unwrap()[&id]
                .exporter
                .status()
                .accepted,
            1
        );
        let _ = configure(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Caller::default(),
            Path(id.to_string()),
            ValidatedJson(Settings { endpoint: None }),
        )
        .await
        .unwrap();
        assert!(
            !state
                .observations
                .sessions
                .lock()
                .unwrap()
                .contains_key(&id)
        );
        assert!(state.store.otlp_endpoint(id).unwrap().is_none());
    }

    #[tokio::test]
    async fn access_is_operator_only_and_observations_are_isolated() {
        use axum::response::IntoResponse;
        let root = tempfile::tempdir().unwrap();
        let state = AppState::with_db_file(
            TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap(),
            root.path().join("state.db"),
        )
        .await
        .unwrap();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        for (id, name) in [(first, "first"), (second, "second")] {
            let spec = serde_json::from_value(serde_json::json!({"name":name, "image":"ghcr.io/team/app:v1", "port":8080, "ram":128, "cpu":1})).unwrap();
            state
                .store
                .insert_micro_service(&MicroServiceResponse { id, spec })
                .unwrap();
        }
        state
            .store
            .append_service_logs(
                first,
                (Uuid::new_v4(), Uuid::new_v4()),
                1,
                "active",
                false,
                &[InputLog {
                    sequence: 1,
                    timestamp: chrono::Utc::now().timestamp_millis(),
                    stream: "stdout".into(),
                    message: "private application text".into(),
                }],
            )
            .unwrap();
        let request = || Extension(RequestId(Uuid::new_v4()));
        let delegate: Caller =
            serde_json::from_value(serde_json::json!({"principal":"local", "operator":false}))
                .unwrap();
        let denied = match get(
            State(state.clone()),
            request(),
            delegate,
            Path(first.to_string()),
        )
        .await
        {
            Err(e) => e,
            Ok(_) => panic!("delegate must not read logs"),
        };
        assert_eq!(denied.into_response().status(), StatusCode::FORBIDDEN);
        let other = get(
            State(state.clone()),
            request(),
            Caller::default(),
            Path(second.to_string()),
        )
        .await
        .unwrap();
        assert!(other.0.logs.is_empty());
        let own = get(
            State(state),
            request(),
            Caller::default(),
            Path(first.to_string()),
        )
        .await
        .unwrap();
        assert_eq!(own.0.logs[0].message, "private application text");
    }

    #[tokio::test]
    async fn invalid_endpoints_and_environment_conflicts_are_rejected_without_sensitive_values() {
        use axum::response::IntoResponse;
        let root = tempfile::tempdir().unwrap();
        let state = AppState::with_db_file(
            TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap(),
            root.path().join("state.db"),
        )
        .await
        .unwrap();
        let id = Uuid::new_v4();
        let spec = serde_json::from_value(serde_json::json!({"name":"orders", "image":"ghcr.io/team/app:v1", "port":8080, "ram":128, "cpu":1, "env":{"OTEL_EXPORTER_OTLP_ENDPOINT":"private-env-value"}})).unwrap();
        state
            .store
            .insert_micro_service(&MicroServiceResponse { id, spec })
            .unwrap();
        let request = || Extension(RequestId(Uuid::new_v4()));
        for endpoint in [
            "http://user:password@collector.test",
            "http://collector.test/?token=secret",
            "http://collector.test",
        ] {
            let error = match configure(
                State(state.clone()),
                request(),
                Caller::default(),
                Path(id.to_string()),
                ValidatedJson(Settings {
                    endpoint: Some(endpoint.into()),
                }),
            )
            .await
            {
                Err(e) => e,
                Ok(_) => panic!("invalid setting accepted"),
            };
            assert!(!error.description().contains("password"));
            assert!(!error.description().contains("private-env-value"));
            assert!(!error.description().contains("collector.test"));
            assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
            assert!(state.store.otlp_endpoint(id).unwrap().is_none());
        }
    }
    #[test]
    fn endpoint_must_be_explicit_and_null_disables_export() {
        assert!(serde_json::from_str::<Settings>("{}").is_err());
        assert!(
            serde_json::from_str::<Settings>(r#"{"endpoint":null}"#)
                .unwrap()
                .endpoint
                .is_none()
        );
        assert!(serde_json::from_str::<Settings>(r#"{"endpoint":null,"extra":true}"#).is_err());
    }
    #[tokio::test]
    async fn restart_keeps_the_opt_in_boundary_for_late_vm_buffers() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("state.db");
        let templates = || TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap();
        let original = AppState::with_db_file(templates(), db.clone())
            .await
            .unwrap();
        let id = Uuid::new_v4();
        let spec = serde_json::from_value(serde_json::json!({"name":"restart", "image":"ghcr.io/team/app:v1", "port":8080, "ram":128, "cpu":1})).unwrap();
        original
            .store
            .insert_micro_service(&MicroServiceResponse { id, spec })
            .unwrap();
        let boundary = original
            .store
            .set_otlp_endpoint(id, Some("http://127.0.0.1:9"))
            .unwrap();
        let restarted = AppState::with_db_file(templates(), db).await.unwrap();
        export_pass(&restarted).await.unwrap();
        let vm = Uuid::new_v4();
        let runtime = Uuid::new_v4();
        restarted
            .store
            .append_service_logs(
                id,
                (vm, runtime),
                2,
                "active",
                false,
                &[
                    InputLog {
                        sequence: 1,
                        timestamp: boundary - 1,
                        stream: "stdout".into(),
                        message: "old buffered data".into(),
                    },
                    InputLog {
                        sequence: 2,
                        timestamp: boundary + 1,
                        stream: "stdout".into(),
                        message: "new buffered data".into(),
                    },
                ],
            )
            .unwrap();
        export_pass(&restarted).await.unwrap();
        assert_eq!(
            restarted.observations.sessions.lock().unwrap()[&id]
                .exporter
                .status()
                .accepted,
            1
        );
    }
    #[tokio::test]
    async fn controller_managed_service_returns_explicit_unsupported_error() {
        use axum::response::IntoResponse;
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("state.db");
        let state = AppState::with_db_file(
            TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap(),
            db.clone(),
        )
        .await
        .unwrap();
        let id = Uuid::new_v4();
        let record = serde_json::json!({
            "id": id, "spec": {"service": {"name":"orders", "image":"ghcr.io/team/app:v1", "port":8080, "ram":128, "cpu":1}, "targets": []},
            "generation": 1, "releaseGeneration": 1, "operationId": Uuid::new_v4(), "desired": "running", "observed": "pending", "pending":true, "degraded":false, "hosts":[]
        });
        rusqlite::Connection::open(db)
            .unwrap()
            .execute(
                "INSERT INTO cluster_services(id,data) VALUES (?1,?2)",
                rusqlite::params![id.to_string(), record.to_string()],
            )
            .unwrap();
        let request = || Extension(RequestId(Uuid::new_v4()));
        let read = match get(
            State(state.clone()),
            request(),
            Caller::default(),
            Path(id.to_string()),
        )
        .await
        {
            Err(e) => e,
            Ok(_) => panic!("remote observations are unsupported"),
        };
        assert_eq!(read.code(), "cluster_observability_unsupported");
        assert_eq!(read.into_response().status(), StatusCode::CONFLICT);
        let write = match configure(
            State(state),
            request(),
            Caller::default(),
            Path(id.to_string()),
            ValidatedJson(Settings {
                endpoint: Some("http://collector.test".into()),
            }),
        )
        .await
        {
            Err(e) => e,
            Ok(_) => panic!("remote settings are unsupported"),
        };
        assert_eq!(write.code(), "cluster_observability_unsupported");
        assert_eq!(write.into_response().status(), StatusCode::CONFLICT);
    }
}
