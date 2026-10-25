//! Application messaging through one host-local, stateful RabbitMQ MicroVM.
//! Credentials never enter VM records, service specs, or ordinary read responses.
mod client;
mod engine;
pub(crate) use client::RabbitClient;
pub(crate) use engine::{create_engine, engine_status, recover, start_engine, stop_engine};

use crate::{error::AppError, persistence::Store, state::AppState};
use firecrab_api_types::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

/// Immutable official RabbitMQ management image used by the internal engine.
pub(crate) const IMAGE: &str = "docker.io/library/rabbitmq@sha256:79fb281ef7e0a15e3c5e0e12e66e944cebbf14a129493ef464369abd2c48d01a";
/// Canonical alias that the OCI importer derives from the pinned broker image.
pub(crate) const TEMPLATE: &str = "rabbitmq-sha256-79fb281ef7e0";

// Intentionally no Debug: these private records contain passwords.
/// Private durable broker identity, lifecycle checkpoint and administrator credentials.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct EngineRecord {
    /// Reserved VM identity that must never change during recovery.
    pub vm_id: Uuid,
    /// Ownership checkpoint that prevents replacement of a previously created VM.
    pub vm_created: bool,
    /// Original network, storage and guest resource allocation.
    pub spec: CreateQueueEngineRequest,
    /// Last persisted lifecycle state.
    pub phase: QueueEnginePhase,
    /// Publicly safe operation failure.
    pub error: Option<String>,
    /// Private RabbitMQ administrator account.
    pub username: String,
    /// Stable administrator password stored only in the owner-only database.
    pub password: String,
}
/// Private queue declaration and stable publisher/consumer credentials.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct QueueRecord {
    /// Stable identifier used to derive the private vhost and role accounts.
    pub id: Uuid,
    /// Validated RabbitMQ queue name.
    pub name: String,
    /// Failed-redelivery limit before moving work to the failure queue.
    pub delivery_limit: u16,
    /// Byte limit for each main and failure queue.
    pub max_bytes: u64,
    /// Persisted declaration checkpoint for explicit retries.
    pub phase: QueuePhase,
    /// Publicly safe provisioning failure.
    pub error: Option<String>,
    /// Stable secret for the write-only application role.
    pub publish_password: String,
    /// Stable secret for the read-only application role.
    pub consume_password: String,
}
impl QueueRecord {
    /// Derive a private vhost from the immutable queue identifier.
    pub(crate) fn vhost(&self) -> String {
        format!("firecrab_{}", self.id.simple())
    }
    /// Derive a stable role account within this queue vhost.
    pub(crate) fn username(&self, access: QueueAccess) -> String {
        format!(
            "{}_{}",
            self.vhost(),
            match access {
                QueueAccess::Publish => "publisher",
                QueueAccess::Consume => "consumer",
            }
        )
    }
    /// Return the selected role secret without cloning it.
    pub(crate) fn password(&self, access: QueueAccess) -> &str {
        match access {
            QueueAccess::Publish => &self.publish_password,
            QueueAccess::Consume => &self.consume_password,
        }
    }
    /// Expose queue settings and service references without credentials.
    pub(crate) fn response(&self, services: &[MicroServiceResponse]) -> QueueResponse {
        QueueResponse {
            id: self.id,
            name: self.name.clone(),
            phase: self.phase,
            delivery_limit: self.delivery_limit,
            max_bytes: self.max_bytes,
            error: self.error.clone(),
            services: services
                .iter()
                .filter_map(|s| {
                    let bindings: BTreeMap<_, _> = s
                        .spec
                        .queues
                        .iter()
                        .filter(|(_, b)| b.queue_id == self.id)
                        .map(|(alias, b)| (alias.clone(), b.access))
                        .collect();
                    (!bindings.is_empty()).then(|| QueueServiceRef {
                        id: s.id,
                        name: s.spec.name.clone(),
                        bindings,
                    })
                })
                .collect(),
        }
    }
}
/// Generate a fresh high-entropy credential for a new persisted identity.
pub(crate) fn password() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
/// Percent-encode one AMQP URI or RabbitMQ management path segment.
pub(crate) fn segment(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}
/// Check the restricted queue-name alphabet and byte-length limit.
pub(crate) fn valid_name(name: &str) -> bool {
    (1..=40).contains(&name.len())
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}
/// Check aliases used to form service environment variable names.
pub(crate) fn valid_alias(alias: &str) -> bool {
    (1..=24).contains(&alias.len())
        && alias.as_bytes()[0].is_ascii_uppercase()
        && alias
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
}

/// Resolve a role-scoped AMQP URL using the existing broker lease.
pub(crate) fn connection(
    store: &Store,
    queue: &QueueRecord,
    access: QueueAccess,
) -> Result<QueueConnection, String> {
    let engine = store
        .queue_engine()
        .map_err(|_| "Could not read Queue engine")?
        .ok_or("Queue engine is not configured")?;
    let lease = store
        .active_lease(engine.vm_id)
        .map_err(|_| "Could not read Queue engine address")?
        .ok_or("Queue engine has no network address")?;
    Ok(QueueConnection {
        queue: queue.name.clone(),
        url: format!(
            "amqp://{}:{}@{}:5672/{}?heartbeat=30",
            segment(&queue.username(access)),
            segment(queue.password(access)),
            lease.ipv4,
            segment(&queue.vhost())
        ),
    })
}

/// Binding credentials are resolved at boot, never serialized in public VM metadata.
pub(crate) fn environment_for_vm(
    store: &Store,
    id: Uuid,
) -> Result<BTreeMap<String, String>, String> {
    let mut env = BTreeMap::new();
    if let Some(engine) = store
        .queue_engine()
        .map_err(|_| "Could not read Queue engine")?
        && engine.vm_id == id
    {
        env.insert("RABBITMQ_DEFAULT_USER".into(), engine.username);
        env.insert("RABBITMQ_DEFAULT_PASS".into(), engine.password);
        // A stable node name is required to reuse the same database on every boot.
        env.insert("RABBITMQ_NODENAME".into(), "rabbit@localhost".into());
        return Ok(env);
    }
    let Some(deployment) = store
        .service_deployment_for_vm(id)
        .map_err(|_| "Could not read Queue bindings")?
    else {
        return Ok(env);
    };
    for (alias, binding) in &deployment.spec.queues {
        let queue = store
            .queue(binding.queue_id)
            .map_err(|_| "Could not read Queue binding")?
            .filter(|q| q.phase == QueuePhase::Ready)
            .ok_or("The service's Queue is unavailable")?;
        let connection = connection(store, &queue, binding.access)?;
        env.insert(format!("FIRECRAB_QUEUE_{alias}_URL"), connection.url);
        env.insert(format!("FIRECRAB_QUEUE_{alias}_NAME"), connection.queue);
    }
    Ok(env)
}

/// Reject unknown queues, invalid aliases and environment-name conflicts.
pub(crate) async fn validate_bindings(
    state: &AppState,
    spec: &MicroServiceSpec,
    id: Uuid,
) -> Result<(), AppError> {
    let mut fields = BTreeMap::new();
    if spec.queues.len() > 8 {
        fields.insert("queues".into(), "at most eight Queue bindings".into());
    }
    for (alias, binding) in &spec.queues {
        if !valid_alias(alias) {
            fields.insert("queues".into(),"binding aliases must be 1-24 uppercase letters, digits or '_', starting with a letter".into());
            continue;
        }
        for suffix in ["URL", "NAME"] {
            let key = format!("FIRECRAB_QUEUE_{alias}_{suffix}");
            if spec.env.contains_key(&key) || spec.secrets.contains_key(&key) {
                fields.insert(
                    "queues".into(),
                    format!("{key} is reserved by the Queue binding"),
                );
            }
        }
        if !state
            .store
            .queue(binding.queue_id)
            .map_err(|_| AppError::internal(id))?
            .is_some_and(|q| q.phase == QueuePhase::Ready)
        {
            fields.insert(
                "queues".into(),
                "every Queue binding must refer to a ready Queue".into(),
            );
        }
    }
    if fields.is_empty() {
        Ok(())
    } else {
        Err(AppError::validation(fields, id))
    }
}

/// Place bound services on the broker network and reject conflicting selection.
pub(crate) fn placement(
    state: &AppState,
    spec: &MicroServiceSpec,
    request: &mut DeployMicroServiceRequest,
    id: Uuid,
) -> Result<(), AppError> {
    if spec.queues.is_empty() {
        return Ok(());
    }
    let engine = state
        .store
        .queue_engine()
        .map_err(|_| AppError::internal(id))?
        .ok_or_else(|| AppError::unavailable("Queue engine is not configured", id))?;
    if request
        .micro_network_id
        .is_some_and(|network| network != engine.spec.micro_network_id)
    {
        return Err(AppError::validation(
            BTreeMap::from([(
                "microNetworkId".into(),
                "Queue-bound services must use the Queue engine's MicroNetwork".into(),
            )]),
            id,
        ));
    }
    request.micro_network_id = Some(engine.spec.micro_network_id);
    Ok(())
}

#[cfg(test)]
mod tests;
