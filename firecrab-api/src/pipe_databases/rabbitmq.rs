//! Verified AMQP transport. Source outboxes remain authoritative across broker loss.
use super::write::Error;
use lapin::{
    BasicProperties, Channel, Confirmation, Connection, ConnectionProperties,
    options::*,
    tcp::{OwnedIdentity, OwnedTLSConfig},
    types::{AMQPValue, FieldTable},
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Private RabbitMQ connection settings; ordinary users use the managed default.
pub struct RabbitMQInput {
    /// Failover endpoints in host:port form.
    pub brokers: Vec<String>,
    #[serde(default = "yes")]
    /// Verify the server certificate and hostname.
    pub tls: bool,
    #[serde(default)]
    /// PEM CA bundle, never a host file path.
    pub ca_certificate: Option<String>,
    /// Dedicated AMQP account.
    pub username: String,
    /// Private account password, never returned by status endpoints.
    pub password: String,
    #[serde(default = "default_vhost")]
    /// Dedicated virtual host.
    pub virtual_host: String,
    #[serde(default)]
    /// Optional PEM client certificate for mutual TLS.
    pub client_certificate: Option<String>,
    #[serde(default)]
    /// Private PEM key paired with the client certificate.
    pub client_key: Option<String>,
    #[serde(default)]
    /// Explicit single-node development profile.
    pub development: bool,
}
fn yes() -> bool {
    true
}
fn default_vhost() -> String {
    "firecrab".into()
}
impl std::fmt::Debug for RabbitMQInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RabbitMQInput { credentials: [REDACTED] }")
    }
}
impl RabbitMQInput {
    /// Reject unsafe production settings before opening a socket.
    pub fn validate(&self) -> Result<(), Error> {
        if self.brokers.is_empty()
            || self.brokers.len() > 20
            || self.brokers.iter().any(|b| {
                b.len() > 256
                    || b.contains(['/', '?', '#', '@'])
                    || b.chars().any(char::is_whitespace)
                    || !b.rsplit_once(':').is_some_and(|(host, port)| {
                        !host.is_empty() && port.parse::<u16>().is_ok_and(|p| p != 0)
                    })
            })
        {
            return Err(Error::Invalid("Enter valid RabbitMQ host:port addresses"));
        }
        if !self.development && (!self.tls || self.brokers.len() < 3) {
            return Err(Error::Invalid(
                "Production RabbitMQ requires TLS and three failover endpoints",
            ));
        }
        if self.username.is_empty()
            || self.username.len() > 256
            || self.password.is_empty()
            || self.password.len() > 4096
            || self.virtual_host.len() > 256
        {
            return Err(Error::Invalid(
                "Enter a RabbitMQ account, password and virtual host",
            ));
        }
        if !self.tls
            && (self.ca_certificate.is_some()
                || self.client_certificate.is_some()
                || self.client_key.is_some())
        {
            return Err(Error::Invalid(
                "Enable TLS before setting RabbitMQ certificates",
            ));
        }
        for pem in [&self.ca_certificate, &self.client_certificate]
            .into_iter()
            .flatten()
        {
            if pem.len() > 32768
                || rustls_pemfile::certs(&mut pem.as_bytes())
                    .collect::<Result<Vec<_>, _>>()
                    .ok()
                    .is_none_or(|c| c.is_empty())
            {
                return Err(Error::Invalid("Invalid RabbitMQ PEM certificate"));
            }
        }
        match (&self.client_certificate, &self.client_key) {
            (None, None) => (),
            (Some(_), Some(key))
                if key.len() <= 32768
                    && rustls_pemfile::private_key(&mut key.as_bytes())
                        .ok()
                        .flatten()
                        .is_some() => {}
            _ => {
                return Err(Error::Invalid(
                    "RabbitMQ mutual TLS requires a certificate and private key",
                ));
            }
        }
        Ok(())
    }
    /// Try each endpoint. A new worker connection is established after disconnection.
    pub async fn connect(&self) -> Result<Connection, Error> {
        self.validate()?;
        let encode = |s: &str| {
            percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
        };
        for broker in &self.brokers {
            let uri = format!(
                "{}://{}:{}@{}/{}?heartbeat=15&connection_timeout=3000",
                if self.tls { "amqps" } else { "amqp" },
                encode(&self.username),
                encode(&self.password),
                broker,
                encode(&self.virtual_host)
            );
            let tls = OwnedTLSConfig {
                cert_chain: self.ca_certificate.clone(),
                identity: self
                    .client_certificate
                    .as_ref()
                    .zip(self.client_key.as_ref())
                    .map(|(pem, key)| OwnedIdentity::PKCS8 {
                        pem: pem.as_bytes().to_vec(),
                        key: key.as_bytes().to_vec(),
                    }),
            };
            let runtime = lapin::runtime::default_runtime()
                .map_err(|_| Error::Invalid("RabbitMQ runtime is unavailable"))?;
            if let Ok(Ok(connection)) = tokio::time::timeout(
                Duration::from_secs(4),
                Connection::connect_with_config(
                    &uri,
                    ConnectionProperties::default()
                        .with_connection_name("firecrab-database-events".into()),
                    tls,
                    runtime,
                ),
            )
            .await
            {
                return Ok(connection);
            }
        }
        Err(Error::Invalid(
            "Cannot connect to RabbitMQ; check endpoints, TLS and authentication",
        ))
    }
}

/// Declare owned durable quorum queues and enable acknowledged publication.
pub async fn channel(
    connection: &Connection,
    queue: &str,
    development: bool,
) -> Result<Channel, Error> {
    let channel = connection
        .create_channel()
        .await
        .map_err(|_| Error::Invalid("RabbitMQ connection is unavailable"))?;
    let mut arguments = FieldTable::default();
    arguments.insert(
        "x-queue-type".into(),
        AMQPValue::LongString("quorum".into()),
    );
    arguments.insert(
        "x-quorum-initial-group-size".into(),
        AMQPValue::LongInt(if development { 1 } else { 3 }),
    );
    // Firecrab persists its own bounded attempt count. Connection losses must not
    // silently discard a pending event through the broker's default delivery limit.
    arguments.insert("x-delivery-limit".into(), AMQPValue::LongInt(-1));
    channel
        .queue_declare(
            format!("{queue}.failed").into(),
            QueueDeclareOptions::durable(),
            arguments.clone(),
        )
        .await
        .map_err(|_| Error::Invalid("Cannot prepare the RabbitMQ failed-event queue"))?;
    arguments.insert(
        "x-dead-letter-exchange".into(),
        AMQPValue::LongString("".into()),
    );
    arguments.insert(
        "x-dead-letter-routing-key".into(),
        AMQPValue::LongString(format!("{queue}.failed").into()),
    );
    arguments.insert(
        "x-dead-letter-strategy".into(),
        AMQPValue::LongString("at-least-once".into()),
    );
    arguments.insert(
        "x-overflow".into(),
        AMQPValue::LongString("reject-publish".into()),
    );
    arguments.insert(
        "x-max-length-bytes".into(),
        AMQPValue::LongLongInt(256 * 1024 * 1024),
    );
    channel
        .queue_declare(queue.into(), QueueDeclareOptions::durable(), arguments)
        .await
        .map_err(|_| Error::Invalid("Cannot prepare the RabbitMQ delivery queue"))?;
    channel
        .confirm_select(ConfirmSelectOptions::default())
        .await
        .map_err(|_| Error::Invalid("RabbitMQ publisher confirms are unavailable"))?;
    Ok(channel)
}

/// Mandatory routing, persistent messages and publisher confirms precede delivery.
pub async fn publish(channel: &Channel, queue: &str, id: &str, body: &[u8]) -> Result<(), Error> {
    let confirmation = channel
        .basic_publish(
            "".into(),
            queue.into(),
            BasicPublishOptions {
                mandatory: true,
                ..Default::default()
            },
            body,
            BasicProperties::default()
                .with_delivery_mode(2)
                .with_content_type("application/json".into())
                .with_message_id(id.into()),
        )
        .await
        .map_err(|_| Error::Invalid("RabbitMQ publication failed; source event retained"))?
        .await
        .map_err(|_| Error::Invalid("RabbitMQ confirmation failed; source event retained"))?;
    match confirmation {
        Confirmation::Ack(None) => Ok(()),
        _ => Err(Error::Invalid(
            "RabbitMQ did not confirm a routed, durable event",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> RabbitMQInput {
        serde_json::from_value(serde_json::json!({"brokers":["node1:5671","node2:5671","node3:5671"],"username":"firecrab","password":"private-test-password"})).unwrap()
    }
    #[test]
    fn production_rejects_plaintext_and_missing_failover_endpoints() {
        let mut config = input();
        config.validate().unwrap();
        config.tls = false;
        assert!(config.validate().is_err());
        config.tls = true;
        config.brokers.truncate(1);
        assert!(config.validate().is_err());
        config.development = true;
        config.validate().unwrap();
    }
    #[test]
    fn malformed_authentication_and_certificates_fail_before_network_access() {
        let mut config = input();
        config.password.clear();
        assert!(config.validate().is_err());
        config = input();
        config.brokers[0] = "node:5671/path?password=secret".into();
        assert!(config.validate().is_err());
        config = input();
        config.ca_certificate = Some("invalid CA".into());
        assert!(config.validate().is_err());
        config = input();
        config.client_key = Some("invalid key".into());
        assert!(config.validate().is_err());
        assert!(!format!("{config:?}").contains("private-test-password"));
    }
}
