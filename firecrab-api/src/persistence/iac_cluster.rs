//! IaC authority does not replace authenticated controller/worker ownership.

use super::PersistenceError;
use firecrab_api_types::{ClusterService, HostReport, PlacementPhase, VmPlacement, VmState};
use firecrab_stack::ManagedResource;
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

/// Public intents and ownership changes share the enclosing IMMEDIATE transaction.
pub(super) fn public_write(
    conn: &Connection,
    kind: &str,
    id: Uuid,
) -> Result<(), PersistenceError> {
    let resource = ManagedResource {
        kind: kind.into(),
        id: id.to_string(),
    };
    if !super::iac_owners::public_write_allowed(conn, &resource)? {
        return Err(PersistenceError::IacOwnershipConflict);
    }
    Ok(())
}

pub(super) fn validate(
    conn: &Connection,
    resource: &ManagedResource,
    adopting: bool,
) -> Result<(), PersistenceError> {
    let query = match resource.kind.as_str() {
        "cluster_host" => "SELECT report,NULL FROM cluster_hosts WHERE id=?1",
        "cluster_placement" => "SELECT data,claim FROM cluster_placements WHERE id=?1",
        "cluster_service" => "SELECT data,claim FROM cluster_services WHERE id=?1",
        _ => return Err(PersistenceError::UnsupportedIacResource),
    };
    let row: Option<(String, Option<String>)> = conn
        .query_row(query, [&resource.id], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    let Some((raw, claim)) = row else {
        return if adopting {
            Err(PersistenceError::InvalidStackResource)
        } else {
            Ok(())
        };
    };
    let corrupt = || PersistenceError::CorruptRecord {
        id: resource.kind.clone(),
        reason: "invalid controller identity".into(),
    };
    let expected = Uuid::parse_str(&resource.id).map_err(|_| PersistenceError::InvalidIacOwner)?;
    let busy = match resource.kind.as_str() {
        "cluster_host" => {
            let report: HostReport = serde_json::from_str(&raw).map_err(|_| corrupt())?;
            if report.id != expected {
                return Err(corrupt());
            }
            // Heartbeats remain authenticated observations. Drain is separate desired configuration.
            false
        }
        "cluster_placement" => {
            let placement: VmPlacement = serde_json::from_str(&raw).map_err(|_| corrupt())?;
            if placement.id != expected
                || placement.vm.host_id.is_nil()
                || !Uuid::parse_str(&placement.vm.resource_id).is_ok_and(|id| !id.is_nil())
                || placement.observed.as_ref().is_some_and(|vm| {
                    Uuid::parse_str(&placement.vm.resource_id).ok() != Some(vm.id)
                })
                || placement.target.network.host_id != placement.vm.host_id
                || placement.target.storage.host_id != placement.vm.host_id
            {
                return Err(corrupt());
            }
            if adopting && placement.phase == PlacementPhase::Deleted {
                return Err(PersistenceError::InvalidStackResource);
            }
            claim.is_some()
                || matches!(
                    placement.phase,
                    PlacementPhase::Pending | PlacementPhase::Uncertain
                )
                || placement.observed.as_ref().is_some_and(|vm| {
                    matches!(
                        vm.state,
                        VmState::Starting
                            | VmState::Stopping
                            | VmState::Pausing
                            | VmState::Resuming
                    )
                })
        }
        "cluster_service" => {
            let service: ClusterService = serde_json::from_str(&raw).map_err(|_| corrupt())?;
            if service.id != expected || service.hosts.iter().any(|host| host.host_id.is_nil()) {
                return Err(corrupt());
            }
            claim.is_some()
                || service.pending
                || service.hosts.iter().any(|host| {
                    host.observed_generation != Some(host.intent.generation)
                        || host.deployment.as_ref().is_some_and(|deployment| {
                            deployment.phase.is_busy()
                                || deployment
                                    .update
                                    .as_ref()
                                    .is_some_and(|update| !update.completed)
                                || deployment
                                    .recovery
                                    .as_ref()
                                    .is_some_and(|recovery| recovery.repair.is_some())
                        })
                })
        }
        _ => return Err(PersistenceError::UnsupportedIacResource),
    };
    if busy {
        return Err(PersistenceError::IacResourceBusy);
    }
    Ok(())
}
