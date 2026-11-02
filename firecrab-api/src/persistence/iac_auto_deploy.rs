//! Ownership of an exact service's automatic image-policy slot.

use super::{PersistenceError, Store, auto_deploy, iac_owners};
use firecrab_stack::{ManagedResource, ResourceKind, ResourceOwnership};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Private server snapshot. HTTP input cannot grant automatic image-writing authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Authority {
    parent: ResourceOwnership,
    policy: ResourceOwnership,
}

pub(crate) enum DeploymentAuthority {
    Manual(Box<Authority>, Option<Uuid>),
    Automatic(Box<auto_deploy::Watch>),
}

impl Authority {
    pub(super) fn has_no_history(&self) -> bool {
        self.parent.generation == 0 && self.policy.generation == 0
    }
}

fn resource(id: Uuid, kind: &str) -> ManagedResource {
    ManagedResource {
        kind: kind.into(),
        id: id.to_string(),
    }
}

pub(super) fn authority(conn: &Connection, id: Uuid) -> Result<Authority, PersistenceError> {
    Ok(Authority {
        parent: iac_owners::read(conn, &resource(id, "service"))?,
        policy: iac_owners::read(conn, &resource(id, "service_auto_deploy"))?,
    })
}

pub(super) fn validate_owner(
    conn: &Connection,
    target: &ManagedResource,
    adopting: bool,
) -> Result<(), PersistenceError> {
    let id = Uuid::parse_str(&target.id).map_err(|_| PersistenceError::InvalidIacOwner)?;
    if adopting
        && !conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM micro_services WHERE id=?1)",
            [id.to_string()],
            |row| row.get::<_, bool>(0),
        )?
    {
        return Err(PersistenceError::InvalidStackResource);
    }
    iac_owners::ensure_resource_idle(conn, ResourceKind::Service, id)?;
    super::iac_native::ensure_idle(conn, &resource(id, "service"))?;
    if super::stacks::resource_apply_busy(conn, ResourceKind::Service, id)? {
        return Err(PersistenceError::StackApplyBusy);
    }
    if auto_deploy::read(conn, id)?.is_some_and(|watch| watch.pending.is_some()) {
        return Err(PersistenceError::IacResourceBusy);
    }
    Ok(())
}

fn public(conn: &Connection, id: Uuid) -> Result<(), PersistenceError> {
    for kind in ["service", "service_auto_deploy"] {
        if !iac_owners::public_write_allowed(conn, &resource(id, kind))? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
    }
    Ok(())
}

pub(super) fn compare(
    conn: &Connection,
    id: Uuid,
    expected: &Authority,
) -> Result<(), PersistenceError> {
    if authority(conn, id)? != *expected {
        return Err(PersistenceError::StaleIacOwner);
    }
    for kind in ["service", "service_auto_deploy"] {
        super::iac_native::ensure_idle(conn, &resource(id, kind))?;
    }
    Ok(())
}

/// Full service edits may retain an independently managed policy, but cannot replace it.
pub(super) fn policy_change(
    conn: &Connection,
    service: &firecrab_api_types::MicroServiceResponse,
) -> Result<(), PersistenceError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT spec FROM micro_services WHERE id=?1",
            [service.id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    let old = raw
        .map(|raw| {
            serde_json::from_str::<firecrab_api_types::MicroServiceSpec>(&raw)
                .map_err(|_| PersistenceError::IacOperationIdentity)
        })
        .transpose()?;
    if old.as_ref().and_then(|s| s.auto_deploy.as_ref()) != service.spec.auto_deploy.as_ref()
        && !iac_owners::public_write_allowed(conn, &resource(service.id, "service_auto_deploy"))?
    {
        return Err(PersistenceError::IacOwnershipConflict);
    }
    Ok(())
}

/// Recheck a private claim inside the transaction that accepts its deployment.
pub(super) fn admit(
    conn: &Connection,
    record: &firecrab_api_types::MicroServiceDeployment,
    claim: &auto_deploy::Watch,
) -> Result<(), PersistenceError> {
    let id = record.service_id;
    if !allowed(conn, id, claim)? {
        return Err(PersistenceError::StaleIacOwner);
    }
    let saved = auto_deploy::read(conn, id)?.ok_or(PersistenceError::StaleIacOwner)?;
    if saved.generation != claim.generation
        || saved.status.pause.is_some()
        || !saved.pending.as_ref().is_some_and(|p| {
            p.operation == record.id
                && claim
                    .pending
                    .as_ref()
                    .is_some_and(|c| c.previous == p.previous && c.digest == p.digest)
        })
    {
        return Err(PersistenceError::StaleIacOwner);
    }
    let raw: String = conn.query_row(
        "SELECT deployment FROM micro_service_deployments WHERE service_id=?1",
        [id.to_string()],
        |row| row.get(0),
    )?;
    let previous: firecrab_api_types::MicroServiceDeployment =
        serde_json::from_str(&raw).map_err(|_| PersistenceError::IacOperationIdentity)?;
    let pending = claim
        .pending
        .as_ref()
        .ok_or(PersistenceError::StaleIacOwner)?;
    let mut expected = previous.spec.clone();
    let repository = claim
        .status
        .policy
        .as_ref()
        .and_then(|p| p.source())
        .ok_or(PersistenceError::StaleIacOwner)?
        .0;
    expected.image = format!("{repository}@{}", pending.digest);
    if previous.id != pending.previous
        || expected != record.spec
        || previous.phase.is_busy()
        || previous.update.as_ref().is_some_and(|u| !u.completed)
    {
        return Err(PersistenceError::StaleIacOwner);
    }
    Ok(())
}

pub(super) fn allowed(
    conn: &Connection,
    id: Uuid,
    watch: &auto_deploy::Watch,
) -> Result<bool, PersistenceError> {
    let current = authority(conn, id)?;
    if watch
        .authority
        .as_ref()
        .is_some_and(|expected| *expected != current)
    {
        return Ok(false);
    }
    // Older unbound private claims cannot cross any ownership history.
    if watch.authority.is_none()
        && (current.parent.generation != 0 || current.policy.generation != 0)
    {
        return Ok(false);
    }
    if current
        .parent
        .owner
        .as_ref()
        .is_some_and(|o| o.manager == firecrab_stack::Manager::Terraform)
    {
        return Ok(false);
    }
    for snapshot in [&current.parent, &current.policy] {
        if let Err(error) = super::iac_native::ensure_idle(conn, &snapshot.resource) {
            return match error {
                PersistenceError::IacResourceBusy => Ok(false),
                other => Err(other),
            };
        }
        if let Some(owner) = snapshot
            .owner
            .as_ref()
            .filter(|o| o.manager == firecrab_stack::Manager::Stack)
        {
            for result in [
                super::stack_applies::ensure_idle(conn, owner.id),
                super::stack_sources::ensure_idle(conn, owner.id),
            ] {
                match result {
                    Ok(()) => {}
                    Err(PersistenceError::StackApplyBusy | PersistenceError::StackSourceBusy) => {
                        return Ok(false);
                    }
                    Err(error) => return Err(error),
                }
            }
        }
    }
    Ok(!super::stacks::resource_apply_busy(
        conn,
        ResourceKind::Service,
        id,
    )?)
}

impl Store {
    pub(crate) fn service_policy_authority(
        &self,
        service: &firecrab_api_types::MicroServiceResponse,
    ) -> Result<Authority, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        policy_change(&tx, service)?;
        let snapshot = authority(&tx, service.id)?;
        if snapshot
            .parent
            .owner
            .as_ref()
            .is_some_and(|o| o.manager == firecrab_stack::Manager::Terraform)
        {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        tx.commit()?;
        Ok(snapshot)
    }

    pub(crate) fn auto_deploy_authority(&self, id: Uuid) -> Result<Authority, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        public(&tx, id)?;
        let snapshot = authority(&tx, id)?;
        tx.commit()?;
        Ok(snapshot)
    }

    pub(crate) fn automatic_watch_allowed(
        &self,
        id: Uuid,
        watch: &auto_deploy::Watch,
    ) -> Result<bool, PersistenceError> {
        allowed(&self.lock(), id, watch)
    }

    pub(crate) fn save_tag_watch_checked(
        &self,
        id: Uuid,
        watch: &auto_deploy::Watch,
        expected_generation: Uuid,
        control: bool,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let snapshot = watch
            .authority
            .as_ref()
            .ok_or(PersistenceError::StaleIacOwner)?;
        compare(&tx, id, snapshot)?;
        if control {
            public(&tx, id)?;
        }
        if auto_deploy::read(&tx, id)?.is_none_or(|saved| saved.generation != expected_generation) {
            return Err(PersistenceError::StaleIacOwner);
        }
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM micro_services WHERE id=?1)",
            [id.to_string()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(PersistenceError::StaleIacOwner);
        }
        auto_deploy::write(&tx, id, watch)?;
        tx.commit()?;
        Ok(())
    }
}
