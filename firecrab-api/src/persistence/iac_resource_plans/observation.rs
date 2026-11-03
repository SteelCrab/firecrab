//! Configuration refresh is separate from immutable plan/operation acknowledgements.
use super::*;
use firecrab_stack::{RESOURCE_PLAN_KINDS, ResourceState};

#[cfg(test)]
mod tests;

impl Store {
    /// Observe typed configuration and authority atomically, without provider/guest traffic.
    pub(crate) fn resource_state(
        &self,
        vault: &crate::secrets::Vault,
        resource: &ManagedResource,
    ) -> Result<ResourceState, PersistenceError> {
        let mut target = resource.clone();
        target
            .normalize()
            .map_err(|_| PersistenceError::InvalidIacOwner)?;
        if !RESOURCE_PLAN_KINDS.contains(&target.kind.as_str()) {
            return Err(PersistenceError::UnsupportedIacResource);
        }
        if target.kind == "secret" {
            return vault
                .read(|contents| Ok(self.resource_state_with_vault(&target, Some(contents))))
                .map_err(|_| PersistenceError::IacVaultUnavailable)?;
        }
        self.resource_state_with_vault(&target, None)
    }

    fn resource_state_with_vault(
        &self,
        target: &ManagedResource,
        vault: Option<&crate::secrets::Contents>,
    ) -> Result<ResourceState, PersistenceError> {
        let conn = self.lock();
        let tx = rusqlite::Transaction::new_unchecked(&conn, TransactionBehavior::Deferred)?;
        let mut state = ResourceState {
            ownership: iac_owners::read(&tx, target)?,
            configuration: None,
            revision_id: None,
            configuration_generation: None,
            active_operation_ids: Vec::new(),
        };
        let mut operations = tx.prepare(
            "SELECT operation_id FROM iac_native_operations o
            WHERE state IN ('running','needs_recovery') AND ((kind=?1 AND resource_id=?2)
            OR EXISTS(SELECT 1 FROM iac_native_dependencies d WHERE d.operation_id=o.operation_id
                AND d.kind=?1 AND d.resource_id=?2)) ORDER BY operation_id",
        )?;
        let ids = operations.query_map(params![target.kind, target.id], |row| {
            row.get::<_, String>(0)
        })?;
        for id in ids {
            state.active_operation_ids.push(
                id?.parse()
                    .map_err(|_| PersistenceError::IacOperationIdentity)?,
            );
        }
        let Some(current) = observe(&tx, target, vault)? else {
            return Ok(state);
        };
        let id = target
            .id
            .parse()
            .map_err(|_| PersistenceError::IacOperationIdentity)?;
        state.configuration = Some(match current {
            Observation::Shell(value) => {
                state.revision_id = Some(
                    value
                        .revision
                        .parse()
                        .map_err(|_| PersistenceError::IacOperationIdentity)?,
                );
                ResourceConfiguration::Shell(firecrab_api_types::CreateShellRequest {
                    name: value.name,
                    description: value.description,
                    content: value.content,
                })
            }
            Observation::AiModel(selection) => {
                ResourceConfiguration::AiModel(firecrab_stack::AiModelConfiguration {
                    project_id: id,
                    selection,
                })
            }
            Observation::SandboxAccess(policy) => {
                ResourceConfiguration::SandboxAccess(firecrab_stack::SandboxAccessConfiguration {
                    principal: id,
                    policy,
                    revocation_generation: None,
                })
            }
            Observation::Secret(value) => {
                let (configuration, version) = secret::public_configuration(value);
                state.configuration_generation = Some(version);
                ResourceConfiguration::Secret(configuration)
            }
            Observation::SecurityGroup(configuration) => {
                ResourceConfiguration::SecurityGroup(configuration)
            }
            Observation::ServiceOtlp(value) => {
                let (configuration, epoch) = service_otlp::public_configuration(value)?;
                state.configuration_generation = epoch;
                ResourceConfiguration::ServiceOtlp(configuration)
            }
        });
        Ok(state)
    }
}
