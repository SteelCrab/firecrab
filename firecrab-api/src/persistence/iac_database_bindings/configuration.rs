//! Captured authority for legacy inbox setup until it uses a dedicated event intent.
use super::*;
use firecrab_stack::ResourceOwnership;

/// Private capability retained before broker or SQL calls; never accepted from HTTP.
#[derive(Clone)]
pub(crate) struct ConfigurationAuthority {
    owners: [ResourceOwnership; 3],
    database_hash: String,
    service_hash: String,
    binding_hash: Option<String>,
}
impl Store {
    pub(crate) fn database_binding_configuration_authority(
        &self,
        id: Uuid,
        record: &DatabaseRecord,
        service: Uuid,
    ) -> Result<ConfigurationAuthority, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let targets = [
            resource(id),
            ManagedResource {
                kind: "service".into(),
                id: service.to_string(),
            },
            ManagedResource {
                kind: "database_connection".into(),
                id: record.id.to_string(),
            },
        ];
        for target in &targets[..2] {
            if !iac_owners::public_write_allowed(&tx, target)? {
                return Err(PersistenceError::IacOwnershipConflict);
            }
        }
        super::super::iac_resources::database_idle(&tx, &targets[2])?;
        let raw: String = tx.query_row(
            "SELECT spec FROM pipe_databases WHERE id=?1",
            [record.id.to_string()],
            |r| r.get(0),
        )?;
        let database: DatabaseInput = decode(&raw)?;
        let database_hash = hash(&database)?;
        if database_hash != hash(&record.input)? {
            return Err(PersistenceError::StaleIacOwner);
        }
        let service = service_spec(&tx, service)?.ok_or(PersistenceError::InvalidStackResource)?;
        let before = binding(&tx, id)?;
        if before.as_ref().is_some_and(|b| {
            b.database_id != record.id || b.input.service_id.to_string() != targets[1].id
        }) {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let owners = [
            iac_owners::read(&tx, &targets[0])?,
            iac_owners::read(&tx, &targets[1])?,
            iac_owners::read(&tx, &targets[2])?,
        ];
        let authority = ConfigurationAuthority {
            owners,
            database_hash,
            service_hash: hash(&service)?,
            binding_hash: before.as_ref().map(hash).transpose()?,
        };
        tx.commit()?;
        Ok(authority)
    }
    pub(crate) fn save_database_binding_configuration(
        &self,
        record: &BindingRecord,
        expected: &ConfigurationAuthority,
    ) -> Result<ConfigurationAuthority, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let targets = [
            resource(record.id),
            ManagedResource {
                kind: "service".into(),
                id: record.input.service_id.to_string(),
            },
            ManagedResource {
                kind: "database_connection".into(),
                id: record.database_id.to_string(),
            },
        ];
        for (index, (target, owner)) in targets.iter().zip(&expected.owners).enumerate() {
            if &owner.resource != target || iac_owners::read(&tx, target)? != *owner {
                return Err(PersistenceError::StaleIacOwner);
            }
            if index < 2 && !iac_owners::public_write_allowed(&tx, target)? {
                return Err(PersistenceError::IacOwnershipConflict);
            }
        }
        super::super::iac_resources::database_idle(&tx, &targets[2])?;
        let raw: String = tx.query_row(
            "SELECT spec FROM pipe_databases WHERE id=?1",
            [record.database_id.to_string()],
            |r| r.get(0),
        )?;
        let database: DatabaseInput = decode(&raw)?;
        if hash(&database)? != expected.database_hash
            || service_spec(&tx, record.input.service_id)?
                .as_ref()
                .map(hash)
                .transpose()?
                != Some(expected.service_hash.clone())
            || binding(&tx, record.id)?.as_ref().map(hash).transpose()? != expected.binding_hash
        {
            return Err(PersistenceError::StaleIacOwner);
        }
        write(&tx, record)?;
        let mut next = expected.clone();
        next.binding_hash = Some(hash(record)?);
        tx.commit()?;
        Ok(next)
    }
}
