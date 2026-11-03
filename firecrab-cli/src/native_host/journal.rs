//! Native ownership results and reservations are committed together, before acknowledgement.

use firecrab_helper_protocol::native_iac::{
    Observation, OwnerChange, Receipt, Rejection, Reply, Target,
};
use firecrab_stack::{ManagedResource, Manager, ResourceOwnership};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

/// Bounded validation errors never contain configuration or credential values.
type Result<T> = std::result::Result<T, &'static str>;

/// Only privileged native observation constructs this input; it is not deserializable.
pub(super) struct Snapshot {
    /// Native configuration exists independently of its current owner.
    pub(super) exists: bool,
    /// Native application or uncertain recovery prevents ownership admission.
    pub(super) busy: bool,
    /// Exact original native configuration projection.
    pub(super) configuration_sha256: String,
}

/// Physical binding remains distinct from the API namespace selected by the first success.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Journal {
    /// Unsupported writers must reject the entire file.
    schema: u16,
    /// Original physical native installation, including the directory object identity.
    target: Target,
    /// First successful ownership operation binds this installation's API namespace.
    endpoint: Option<Uuid>,
    /// Both facets retain released generations instead of reusing an unowned generation zero.
    owners: BTreeMap<String, ResourceOwnership>,
    /// Terminal results retain the original request, independent of later native drift.
    outcomes: BTreeMap<Uuid, Outcome>,
    /// Interrupted native maintenance must not silently become an idle installation.
    #[serde(default)]
    maintenance: BTreeMap<Uuid, super::maintenance::Entry>,
}

/// Known no-effect outcomes remain scoped even before the first successful namespace binding.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Outcome {
    /// Committed ownership and target reservation.
    Applied { receipt: Receipt },
    /// Rejected before owner mutation, with an immutable normalized request hash.
    Rejected {
        endpoint: Uuid,
        rejection: Rejection,
    },
}

impl Journal {
    /// Reading an unused installation does not claim ownership or create infrastructure.
    pub(super) fn new(target: Target) -> Self {
        Self {
            schema: 1,
            target,
            endpoint: None,
            owners: BTreeMap::new(),
            outcomes: BTreeMap::new(),
            maintenance: BTreeMap::new(),
        }
    }

    /// A copied or replaced profile cannot become the original operation's new physical target.
    pub(super) fn check_target(&self, target: &Target) -> Result<()> {
        target.validate()?;
        if self.schema != 1 || self.target != *target {
            return Err("native ownership belongs to another physical installation or schema");
        }
        for (kind, owner) in &self.owners {
            firecrab_helper_protocol::native_iac::validate_resource(&owner.resource, target)?;
            if kind != &owner.resource.kind
                || Some(owner.endpoint_id) != self.endpoint
                || owner.generation > i64::MAX as u64
            {
                return Err("native ownership namespace is inconsistent");
            }
        }
        for (id, entry) in &self.maintenance {
            entry.request.validate()?;
            if *id != entry.request.operation_id || self.outcomes.contains_key(id) {
                return Err("native maintenance operation identity is inconsistent");
            }
        }
        Ok(())
    }

    /// Metadata request fields cannot replace an already bound API database.
    fn check_endpoint(&self, endpoint: Uuid) -> Result<()> {
        if endpoint.is_nil() || self.endpoint.is_some_and(|original| original != endpoint) {
            return Err("native ownership belongs to another API endpoint");
        }
        Ok(())
    }

    /// Immutable replies are read before mutable native settings or runtime observations.
    pub(super) fn outcome(&self, endpoint: Uuid, operation: Uuid) -> Result<Option<Reply>> {
        match self.outcomes.get(&operation) {
            None => {
                self.check_endpoint(endpoint)?;
                Ok(None)
            }
            Some(Outcome::Applied { receipt }) if receipt.ownership.endpoint_id == endpoint => {
                Ok(Some(Reply::OwnerReceipt {
                    receipt: Some(receipt.clone()),
                }))
            }
            Some(Outcome::Rejected {
                endpoint: original,
                rejection,
            }) if *original == endpoint => Ok(Some(Reply::OwnerRejected {
                rejection: rejection.clone(),
            })),
            _ => Err("native operation belongs to another API endpoint"),
        }
    }

    /// A native caller cannot change a managed facet or an unconfirmed release.
    pub(super) fn protected(&self, kind: &str) -> bool {
        self.owners.get(kind).is_some_and(|owner| owner.owner.is_some())
            || self.outcomes.values().any(|outcome| matches!(outcome,
                Outcome::Applied { receipt } if !receipt.confirmed && receipt.ownership.resource.kind == kind))
    }

    pub(super) fn maintenance_busy(&self) -> bool {
        self.maintenance.values().any(|entry| !entry.completed)
    }

    /// Ownership observation is secret-free and creates no permission to execute native changes.
    pub(super) fn observe(
        &self,
        endpoint: Uuid,
        resource: &ManagedResource,
        snapshot: &Snapshot,
    ) -> Result<Observation> {
        self.check_endpoint(endpoint)?;
        firecrab_helper_protocol::native_iac::validate_resource(resource, &self.target)?;
        Ok(Observation { target: self.target.clone(), ownership: self.owners.get(&resource.kind).cloned()
            .unwrap_or(ResourceOwnership { endpoint_id: endpoint, resource: resource.clone(), generation: 0, owner: None }),
            exists: snapshot.exists, busy: snapshot.busy || self.maintenance_busy() || self.outcomes.values().any(|outcome| matches!(outcome,
                Outcome::Applied { receipt } if !receipt.confirmed && receipt.ownership.resource == *resource)),
            configuration_sha256: snapshot.configuration_sha256.clone() })
    }

    /// Retain either the original successful CAS or a known no-effect rejection.
    pub(super) fn change(
        &mut self,
        mut request: OwnerChange,
        snapshot: Option<&Snapshot>,
    ) -> Result<Reply> {
        request.validate()?;
        if self.maintenance.contains_key(&request.change.operation_id) {
            return Err("operation UUID belongs to native maintenance");
        }
        let hash = request
            .request_sha256()
            .map_err(|_| "invalid native request identity")?;
        if let Some(reply) = self.outcome(
            request.change.expected_endpoint_id,
            request.change.operation_id,
        )? {
            let previous = match &reply {
                Reply::OwnerReceipt {
                    receipt: Some(receipt),
                } => &receipt.request_sha256,
                Reply::OwnerRejected { rejection } => &rejection.request_sha256,
                _ => return Err("native operation outcome is invalid"),
            };
            if previous != &hash {
                return Err("operation UUID belongs to another native request");
            }
            return Ok(reply);
        }
        self.check_endpoint(request.change.expected_endpoint_id)?;
        if request.target != self.target {
            return Err("original native target changed after observation");
        }
        let snapshot = snapshot.ok_or("native configuration observation is unavailable")?;
        match self.commit(&request, snapshot, hash.clone()) {
            Ok(receipt) => {
                self.endpoint = Some(receipt.ownership.endpoint_id);
                self.owners.insert(
                    receipt.ownership.resource.kind.clone(),
                    receipt.ownership.clone(),
                );
                self.outcomes.insert(
                    receipt.operation_id,
                    Outcome::Applied {
                        receipt: receipt.clone(),
                    },
                );
                Ok(Reply::OwnerReceipt {
                    receipt: Some(receipt),
                })
            }
            Err(_) => {
                let rejection = Rejection {
                    operation_id: request.change.operation_id,
                    request_sha256: hash,
                    target: self.target.clone(),
                };
                self.outcomes.insert(
                    rejection.operation_id,
                    Outcome::Rejected {
                        endpoint: request.change.expected_endpoint_id,
                        rejection: rejection.clone(),
                    },
                );
                Ok(Reply::OwnerRejected { rejection })
            }
        }
    }

    /// Calculate the next owner without mutating state on validation or generation failure.
    fn commit(&self, request: &OwnerChange, snapshot: &Snapshot, hash: String) -> Result<Receipt> {
        let current = self.observe(
            request.change.expected_endpoint_id,
            &request.change.resource,
            snapshot,
        )?;
        if current.busy
            || request.configuration_sha256 != current.configuration_sha256
            || current.ownership.generation != request.change.expected_generation
            || current.ownership.owner != request.change.expected_owner
            || (request.change.owner.is_some() && !current.exists)
        {
            return Err("native configuration, ownership or operation changed after observation");
        }
        if request.change.owner.as_ref().is_some_and(|owner| {
            self.owners.values().any(|other| {
                other.resource != request.change.resource
                    && other.owner.as_ref() == Some(owner)
                    && (owner.manager == Manager::Terraform
                        || other.resource.kind == request.change.resource.kind)
            })
        }) {
            return Err("logical native owner address is already occupied");
        }
        let mut ownership = current.ownership;
        if ownership.owner != request.change.owner {
            ownership.generation = ownership
                .generation
                .checked_add(1)
                .filter(|generation| *generation <= i64::MAX as u64)
                .ok_or("native generation exhausted")?;
        }
        ownership.owner = request.change.owner.clone();
        Ok(Receipt {
            operation_id: request.change.operation_id,
            request_sha256: hash,
            target: self.target.clone(),
            ownership,
            confirmed: false,
        })
    }

    /// Only acknowledgement of the exact API mirror releases the retained target reservation.
    pub(super) fn confirm(&mut self, endpoint: Uuid, operation: Uuid) -> Result<Receipt> {
        self.check_endpoint(endpoint)?;
        let Some(Outcome::Applied { receipt }) = self.outcomes.get_mut(&operation) else {
            return Err("original native ownership receipt is unavailable");
        };
        if receipt.ownership.endpoint_id != endpoint {
            return Err("native operation belongs to another API endpoint");
        }
        receipt.confirmed = true;
        Ok(receipt.clone())
    }

    /// A local maintenance lease never grants permission to mutate an IaC-owned installation.
    #[cfg(any(target_os = "linux", test))]
    pub(super) fn begin_maintenance(
        &mut self,
        request: &super::maintenance::Request,
    ) -> Result<bool> {
        request.validate()?;
        if let Some(entry) = self.maintenance.get(&request.operation_id) {
            if entry.request != *request {
                return Err("native maintenance operation belongs to another request");
            }
            return Ok(!entry.completed);
        }
        if self.outcomes.contains_key(&request.operation_id) {
            return Err("native maintenance UUID belongs to ownership work");
        }
        if self.protected("management_settings")
            || self.protected("host_configuration")
            || self.maintenance.values().any(|entry| !entry.completed)
        {
            return Err("native installation is managed or has unresolved maintenance");
        }
        self.maintenance.insert(
            request.operation_id,
            super::maintenance::Entry {
                request: request.clone(),
                completed: false,
            },
        );
        Ok(true)
    }

    /// Only explicit completion of the original request clears its durable busy state.
    #[cfg(any(target_os = "linux", test))]
    pub(super) fn finish_maintenance(
        &mut self,
        request: &super::maintenance::Request,
    ) -> Result<()> {
        let entry = self
            .maintenance
            .get_mut(&request.operation_id)
            .ok_or("original native maintenance is unavailable")?;
        if entry.request != *request {
            return Err("native maintenance operation belongs to another request");
        }
        entry.completed = true;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub(super) fn check_maintenance(&self, request: &super::maintenance::Request) -> Result<()> {
        use super::maintenance::Action;
        request.validate()?;
        let entry = self
            .maintenance
            .get(&request.operation_id)
            .ok_or("original native maintenance is unavailable")?;
        let action_matches = entry.request.action == request.action
            || (entry.request.action == Action::Reinstall
                && matches!(request.action, Action::Install | Action::Uninstall));
        if entry.completed
            || entry.request.input_sha256 != request.input_sha256
            || !action_matches
            || self.protected("management_settings")
            || self.protected("host_configuration")
        {
            return Err("native maintenance does not authorize this original installer step");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use firecrab_helper_protocol::native_iac::Platform;
    use firecrab_stack::{ChangeResourceOwner, ResourceOwner};

    fn target() -> Target {
        Target {
            platform: Platform::Macos,
            installation_id: Uuid::from_u128(42),
            machine_sha256: "a".repeat(64),
            configuration_root: "/fixture-native".into(),
            directory_id: "1:2".into(),
        }
    }
    fn snapshot() -> Snapshot {
        Snapshot {
            exists: true,
            busy: false,
            configuration_sha256: "b".repeat(64),
        }
    }
    fn request() -> OwnerChange {
        OwnerChange {
            target: target(),
            configuration_sha256: snapshot().configuration_sha256,
            change: ChangeResourceOwner {
                operation_id: Uuid::new_v4(),
                expected_endpoint_id: Uuid::from_u128(548),
                resource: ManagedResource {
                    kind: "management_settings".into(),
                    id: target().installation_id.to_string(),
                },
                expected_generation: 0,
                expected_owner: None,
                owner: Some(ResourceOwner {
                    manager: Manager::Terraform,
                    id: Uuid::from_u128(1),
                    key: "firecrab_management_settings.host".into(),
                }),
            },
        }
    }

    #[test]
    fn native_receipts_survive_reload_release_and_drift_without_repeating_the_original_change() {
        let mut state = Journal::new(target());
        let original = request();
        let Reply::OwnerReceipt {
            receipt: Some(receipt),
        } = state.change(original.clone(), Some(&snapshot())).unwrap()
        else {
            panic!("expected original receipt")
        };
        assert_eq!(receipt.ownership.generation, 1);
        assert!(!receipt.confirmed);
        assert!(state.protected("management_settings"));
        let mut state: Journal =
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        state.check_target(&target()).unwrap();
        state
            .confirm(
                original.change.expected_endpoint_id,
                original.change.operation_id,
            )
            .unwrap();
        let mut release = original.clone();
        release.change.operation_id = Uuid::new_v4();
        release.change.expected_generation = 1;
        release.change.expected_owner = original.change.owner.clone();
        release.change.owner = None;
        let Reply::OwnerReceipt {
            receipt: Some(released),
        } = state.change(release.clone(), Some(&snapshot())).unwrap()
        else {
            panic!("expected release receipt")
        };
        assert_eq!(released.ownership.generation, 2);
        assert!(state.protected("management_settings"));
        state
            .confirm(
                release.change.expected_endpoint_id,
                release.change.operation_id,
            )
            .unwrap();
        assert!(!state.protected("management_settings"));
        let Reply::OwnerReceipt {
            receipt: Some(retried),
        } = state.change(original.clone(), None).unwrap()
        else {
            panic!("expected retained receipt")
        };
        assert_eq!(retried.ownership, receipt.ownership);
        assert_eq!(
            state
                .observe(
                    original.change.expected_endpoint_id,
                    &original.change.resource,
                    &snapshot()
                )
                .unwrap()
                .ownership,
            released.ownership
        );
        let mut replaced = target();
        replaced.directory_id = "1:3".into();
        assert!(state.check_target(&replaced).is_err());
        let mut changed = original;
        changed.change.owner.as_mut().unwrap().key = "firecrab_management_settings.other".into();
        assert!(state.change(changed, None).is_err());
    }

    #[test]
    fn original_native_rejection_does_not_become_a_later_success_or_follow_another_api() {
        let mut state = Journal::new(target());
        let mut stale = request();
        stale.configuration_sha256 = "0".repeat(64);
        let rejected = state.change(stale.clone(), Some(&snapshot())).unwrap();
        assert!(matches!(rejected, Reply::OwnerRejected { .. }));
        let mut successor = request();
        successor.change.expected_endpoint_id = Uuid::from_u128(549);
        state.change(successor.clone(), Some(&snapshot())).unwrap();
        assert_eq!(state.change(stale.clone(), None).unwrap(), rejected);
        assert_eq!(
            state
                .outcome(stale.change.expected_endpoint_id, stale.change.operation_id)
                .unwrap(),
            Some(rejected)
        );
        let mut other_api = stale;
        other_api.change.expected_endpoint_id = successor.change.expected_endpoint_id;
        assert!(state.change(other_api, None).is_err());
        assert_eq!(
            state
                .observe(
                    successor.change.expected_endpoint_id,
                    &successor.change.resource,
                    &snapshot()
                )
                .unwrap()
                .ownership
                .generation,
            1
        );
    }
}
