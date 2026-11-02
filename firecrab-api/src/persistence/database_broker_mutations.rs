//! Connection verification cannot replace broker inputs across a native event reservation.
use super::{
    PersistenceError, Store,
    database_services::{decode, encode},
    iac_native, iac_owners,
};
use crate::pipe_databases::{events::RabbitMQInput, managed_rabbitmq::Cluster};
use firecrab_stack::{ManagedResource, ResourceOwnership};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};

pub(super) fn resource() -> ManagedResource {
    ManagedResource {
        kind: "database_rabbitmq".into(),
        id: "default".into(),
    }
}
fn fingerprint<T: serde::Serialize>(value: &T) -> Result<String, PersistenceError> {
    Ok(format!("{:x}", Sha256::digest(encode(value)?)))
}
fn configuration(conn: &Connection) -> Result<Option<RabbitMQInput>, PersistenceError> {
    let raw: Option<String> = conn
        .query_row("SELECT spec FROM database_rabbitmq WHERE id=1", [], |r| {
            r.get(0)
        })
        .optional()?;
    raw.as_deref().map(decode).transpose()
}
fn managed(conn: &Connection) -> Result<Option<Cluster>, PersistenceError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT spec FROM database_managed_rabbitmq WHERE id=1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    raw.as_deref().map(decode).transpose()
}
fn managed_configuration(value: Option<&Cluster>) -> Result<String, PersistenceError> {
    let value = value.map(|c| {
        let mut c = c.clone();
        c.phase.clear();
        c.error = None;
        c
    });
    fingerprint(&value)
}
pub(crate) struct BrokerAuthority {
    owner: ResourceOwnership,
    input: String,
    managed: String,
}
fn allowed(conn: &Connection) -> Result<(), PersistenceError> {
    if !iac_owners::public_write_allowed(conn, &resource())? {
        return Err(PersistenceError::IacOwnershipConflict);
    }
    Ok(())
}
impl Store {
    pub(crate) fn database_broker_authority(&self) -> Result<BrokerAuthority, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        allowed(&tx)?;
        let authority = BrokerAuthority {
            owner: iac_owners::read(&tx, &resource())?,
            input: fingerprint(&configuration(&tx)?)?,
            managed: fingerprint(&managed(&tx)?)?,
        };
        tx.commit()?;
        Ok(authority)
    }
    /// External selection and stopping managed supervision commit together.
    pub(crate) fn configure_external_database_broker(
        &self,
        input: &RabbitMQInput,
        expected: &BrokerAuthority,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        allowed(&tx)?;
        if iac_owners::read(&tx, &resource())? != expected.owner
            || fingerprint(&configuration(&tx)?)? != expected.input
            || fingerprint(&managed(&tx)?)? != expected.managed
        {
            return Err(PersistenceError::StaleIacOwner);
        }
        let retained: bool =
            tx.query_row("SELECT EXISTS(SELECT 1 FROM database_triggers)", [], |r| {
                r.get(0)
            })?;
        if retained {
            return Err(PersistenceError::IacResourceBusy);
        }
        if let Some(mut cluster) = managed(&tx)? {
            cluster.enabled = false;
            tx.execute(
                "UPDATE database_managed_rabbitmq SET spec=?1 WHERE id=1",
                [encode(&cluster)?],
            )?;
        }
        tx.execute("INSERT INTO database_rabbitmq VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET spec=excluded.spec",[encode(input)?])?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn configure_managed_database_broker(
        &self,
        cluster: &Cluster,
        expected: &BrokerAuthority,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        allowed(&tx)?;
        if iac_owners::read(&tx, &resource())? != expected.owner
            || fingerprint(&configuration(&tx)?)? != expected.input
            || fingerprint(&managed(&tx)?)? != expected.managed
        {
            return Err(PersistenceError::StaleIacOwner);
        }
        let retained: bool =
            tx.query_row("SELECT EXISTS(SELECT 1 FROM database_triggers)", [], |r| {
                r.get(0)
            })?;
        if retained {
            return Err(PersistenceError::IacResourceBusy);
        }
        tx.execute("INSERT INTO database_managed_rabbitmq VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET spec=excluded.spec",[encode(cluster)?])?;
        tx.commit()?;
        Ok(())
    }
    pub(super) fn save_database_broker_input(
        conn: &Connection,
        input: &RabbitMQInput,
    ) -> Result<(), PersistenceError> {
        if let Some(current) = configuration(conn)?
            && fingerprint(&current)? == fingerprint(input)?
        {
            return Ok(());
        }
        allowed(conn)?;
        conn.execute("INSERT INTO database_rabbitmq VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET spec=excluded.spec",params![encode(input)?])?;
        Ok(())
    }
    pub(super) fn save_database_broker_observation(
        conn: &Connection,
        cluster: &Cluster,
    ) -> Result<(), PersistenceError> {
        let before = managed(conn)?;
        if managed_configuration(before.as_ref())? != managed_configuration(Some(cluster))? {
            iac_native::ensure_idle(conn, &resource())?;
        }
        if before
            .as_ref()
            .is_some_and(|old| old.id != cluster.id || (!old.enabled && cluster.enabled))
        {
            return Err(PersistenceError::StaleIacOwner);
        }
        conn.execute("INSERT INTO database_managed_rabbitmq VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET spec=excluded.spec",[encode(cluster)?])?;
        Ok(())
    }
}
