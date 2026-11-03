//! Frozen typed plans share the same authority and operation UUIDs as native work.
//! Shell changes are entirely transactional; no unfinished effect is inferred as success.

use super::{
    PersistenceError, Store,
    database_services::{decode, encode},
    iac_native, iac_owners,
};
use firecrab_stack::{
    ApplyResourcePlan, ChangeResourceOwner, ManagedResource, Manager, NativeAction,
    NativeOperation, NativeOperationState, PlanResource, ResourceConfiguration, ResourceOwner,
    ResourceOwnership, ResourcePlan, ResourcePlanAction, ResourcePlanDependency,
    ResourcePlanReceipt,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellObservation {
    name: String,
    description: Option<String>,
    revision: String,
    version: i64,
    content: String,
}

/// Original Shell serialization stays unchanged; each inner type rejects unknown fields.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum Observation {
    Shell(ShellObservation),
    AiModel(firecrab_api_types::AiModelSelection),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frozen {
    schema: u8,
    request: PlanResource,
    ownership: ResourceOwnership,
    before: Option<Observation>,
    revision: Uuid,
    expires: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    dependencies: Vec<ResourcePlanDependency>,
}

fn fingerprint<T: Serialize>(value: &T) -> Result<String, PersistenceError> {
    Ok(format!("{:x}", Sha256::digest(encode(value)?)))
}

fn shell(
    conn: &Connection,
    resource: &ManagedResource,
) -> Result<Option<ShellObservation>, PersistenceError> {
    if resource.kind != "shell" {
        return Err(PersistenceError::UnsupportedIacResource);
    }
    let row: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT name,description FROM shells WHERE id=?1",
            [&resource.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((name, description)) = row else {
        return Ok(None);
    };
    let (revision,version,content) = conn.query_row(
        "SELECT id,version,content FROM shell_revisions WHERE shell_id=?1 ORDER BY version DESC LIMIT 1",
        [&resource.id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
    Ok(Some(ShellObservation {
        name,
        description,
        revision,
        version,
        content,
    }))
}

fn observe(
    conn: &Connection,
    target: &ManagedResource,
) -> Result<Option<Observation>, PersistenceError> {
    match target.kind.as_str() {
        "shell" => shell(conn, target).map(|before| before.map(Observation::Shell)),
        "ai_model" => {
            let value: Option<(String, String)> = conn
                .query_row(
                    "SELECT connection_id,model FROM pipe_ai_defaults WHERE project_id=?1",
                    [&target.id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            value
                .map(|(connection, model)| {
                    Ok(Observation::AiModel(firecrab_api_types::AiModelSelection {
                        connection_id: connection
                            .parse()
                            .map_err(|_| PersistenceError::IacOperationIdentity)?,
                        model,
                    }))
                })
                .transpose()
        }
        _ => Err(PersistenceError::UnsupportedIacResource),
    }
}

fn model_connection(
    conn: &Connection,
    selection: &firecrab_api_types::AiModelSelection,
) -> Result<crate::pipe_ai::SavedConnection, PersistenceError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT spec FROM pipe_ai_connections WHERE id=?1",
            [selection.connection_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    let source: crate::pipe_ai::SavedConnection =
        decode(&raw.ok_or(PersistenceError::InvalidStackResource)?)?;
    if source.connection.id != selection.connection_id
        || source.connection.provider != firecrab_api_types::AiProvider::Openai
        || source.api_key.is_empty()
        || !source.connection.models.contains(&selection.model)
    {
        return Err(PersistenceError::InvalidStackResource);
    }
    Ok(source)
}

fn dependencies(
    conn: &Connection,
    desired: Option<&ResourceConfiguration>,
) -> Result<Vec<ResourcePlanDependency>, PersistenceError> {
    let Some(ResourceConfiguration::AiModel(config)) = desired else {
        return Ok(Vec::new());
    };
    model_connection(conn, &config.selection)?;
    let source = ManagedResource {
        kind: "ai_connection".into(),
        id: config.selection.connection_id.to_string(),
    };
    iac_native::ensure_idle(conn, &source)?;
    super::iac_host::ensure_resource_idle(conn, &source)?;
    Ok(vec![ResourcePlanDependency {
        ownership: iac_owners::read(conn, &source)?,
        configuration_generation: super::iac_ai_connections::generation(
            conn,
            config.selection.connection_id,
        )?,
    }])
}

fn deletable(conn: &Connection, resource: &ManagedResource) -> Result<(), PersistenceError> {
    if resource.kind == "ai_model" {
        return Ok(());
    }
    let pinned: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM vm_shells WHERE shell_id=?1)",
        [&resource.id],
        |row| row.get(0),
    )?;
    if pinned {
        return Err(PersistenceError::IacResourceBusy);
    }
    Ok(())
}

/// Intrinsic validation never reads resources, resolves credentials or mutates ownership.
pub(crate) fn validate(request: &mut PlanResource) -> Result<(), PersistenceError> {
    if request.operation_id.is_nil() || request.expected_endpoint_id.is_nil() {
        return Err(PersistenceError::InvalidIacOwner);
    }
    request
        .owner
        .validate()
        .map_err(|_| PersistenceError::InvalidIacOwner)?;
    if let Some(resource) = request.resource.as_mut() {
        resource
            .normalize()
            .map_err(|_| PersistenceError::InvalidIacOwner)?;
        if !matches!(resource.kind.as_str(), "shell" | "ai_model") {
            return Err(PersistenceError::UnsupportedIacResource);
        }
    }
    match &request.desired {
        Some(ResourceConfiguration::Shell(configuration)) => {
            if request
                .resource
                .as_ref()
                .is_some_and(|target| target.kind != "shell")
            {
                return Err(PersistenceError::InvalidIacOwner);
            }
            if !crate::handlers::shells::validate_shell_write(
                &configuration.name,
                configuration.description.as_deref(),
                &configuration.content,
            )
            .is_empty()
            {
                return Err(PersistenceError::InvalidIacOwner);
            }
        }
        Some(ResourceConfiguration::AiModel(config)) => {
            if config.project_id.is_nil()
                || config.selection.connection_id.is_nil()
                || crate::pipe_ai::validate_model(&config.selection.model).is_err()
                || request.resource.as_ref().is_some_and(|target| {
                    target.kind != "ai_model" || target.id != config.project_id.to_string()
                })
            {
                return Err(PersistenceError::InvalidIacOwner);
            }
        }
        None if request.resource.is_none() => return Err(PersistenceError::InvalidIacOwner),
        None => (),
    }
    Ok(())
}

fn authority(conn: &Connection, frozen: &Frozen) -> Result<(), PersistenceError> {
    let target = &frozen.ownership.resource;
    if iac_owners::read(conn, target)? != frozen.ownership {
        return Err(PersistenceError::StaleIacOwner);
    }
    if frozen.before.is_some() && frozen.ownership.owner.as_ref() != Some(&frozen.request.owner)
        || frozen.before.is_none() && frozen.ownership.owner.is_some()
    {
        return Err(PersistenceError::IacOwnershipConflict);
    }
    iac_native::ensure_idle(conn, target)?;
    super::iac_host::ensure_resource_idle(conn, target)?;
    super::iac_resources::ensure_independent(conn, target)?;
    if dependencies(conn, frozen.request.desired.as_ref())? != frozen.dependencies {
        return Err(PersistenceError::StaleIacExecution);
    }
    let owner = &frozen.request.owner;
    super::iac_host::ensure_available(
        conn,
        &ChangeResourceOwner {
            operation_id: frozen.request.operation_id,
            expected_endpoint_id: frozen.request.expected_endpoint_id,
            resource: target.clone(),
            expected_generation: frozen.ownership.generation,
            expected_owner: frozen.ownership.owner.clone(),
            owner: Some(owner.clone()),
        },
    )?;
    let manager = match owner.manager {
        Manager::Stack => "stack",
        Manager::Terraform => "terraform",
    };
    let occupied: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM iac_resource_owners
        WHERE manager=?1 AND owner_id=?2 AND logical_key=?3 AND (?1='terraform' OR kind=?4)
        AND (kind<>?4 OR resource_id<>?5))",
        params![
            manager,
            owner.id.to_string(),
            owner.key,
            target.kind,
            target.id
        ],
        |row| row.get(0),
    )?;
    if occupied {
        return Err(PersistenceError::IacOwnershipConflict);
    }
    if owner.manager == Manager::Stack {
        super::stack_applies::ensure_idle(conn, owner.id)?;
        super::stack_sources::ensure_idle(conn, owner.id)?;
        if !conn
            .prepare("SELECT 1 FROM stacks WHERE id=?1")?
            .exists([owner.id.to_string()])?
        {
            return Err(PersistenceError::MissingStack);
        }
        if frozen.before.is_none()
            && super::stacks::binding_count(conn, owner.id, Some(target))?
                >= firecrab_stack::MAX_RESOURCES
        {
            return Err(PersistenceError::StackBindingLimit);
        }
    }
    Ok(())
}

impl Frozen {
    fn summary(&self) -> Result<ResourcePlan, PersistenceError> {
        if self.schema != 1 {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let mut fields = Vec::new();
        let action = match (&self.before, &self.request.desired) {
            (None, Some(_)) => ResourcePlanAction::Create,
            (Some(_), None) => ResourcePlanAction::Delete,
            (Some(Observation::Shell(before)), Some(ResourceConfiguration::Shell(desired))) => {
                if before.name != desired.name {
                    fields.push("name".into());
                }
                if before.description != desired.description {
                    fields.push("description".into());
                }
                if before.content != desired.content {
                    fields.push("content".into());
                }
                if fields.is_empty() {
                    ResourcePlanAction::Noop
                } else {
                    ResourcePlanAction::Update
                }
            }
            (Some(Observation::AiModel(before)), Some(ResourceConfiguration::AiModel(desired))) => {
                if before.connection_id != desired.selection.connection_id {
                    fields.push("connectionId".into());
                }
                if before.model != desired.selection.model {
                    fields.push("model".into());
                }
                if fields.is_empty() {
                    ResourcePlanAction::Noop
                } else {
                    ResourcePlanAction::Update
                }
            }
            _ => return Err(PersistenceError::IacOperationIdentity),
        };
        Ok(ResourcePlan {
            operation_id: self.request.operation_id,
            plan_sha256: fingerprint(self)?,
            ownership: self.ownership.clone(),
            owner: self.request.owner.clone(),
            action,
            changed_fields: fields,
            expires_at_ms: self.expires,
            dependencies: self.dependencies.clone(),
        })
    }
}

fn read(conn: &Connection, id: Uuid) -> Result<Option<Frozen>, PersistenceError> {
    let raw: Option<(String, String)> = conn
        .query_row(
            "SELECT request_hash,definition FROM iac_resource_plans WHERE operation_id=?1",
            [id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((expected, raw)) = raw else {
        return Ok(None);
    };
    let mut frozen: Frozen = decode(&raw)?;
    validate(&mut frozen.request)?;
    if frozen.request.operation_id != id
        || !matches!(
            frozen.ownership.resource.kind.as_str(),
            "shell" | "ai_model"
        )
        || frozen.ownership.endpoint_id != frozen.request.expected_endpoint_id
        || frozen.revision.is_nil()
        || frozen.expires == 0
        || fingerprint(&frozen.request)? != expected
        || frozen.request.desired.as_ref().is_some_and(|desired|desired.kind()!=frozen.ownership.resource.kind
            || matches!(desired,ResourceConfiguration::AiModel(config) if config.project_id.to_string()!=frozen.ownership.resource.id))
        || frozen
            .request
            .resource
            .as_ref()
            .is_some_and(|target| target != &frozen.ownership.resource)
        || (frozen.request.resource.is_none() && frozen.before.is_some())
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    Ok(Some(frozen))
}

fn save_owner(
    conn: &Connection,
    original: &ResourceOwnership,
    owner: Option<&ResourceOwner>,
) -> Result<ResourceOwnership, PersistenceError> {
    if original.owner.as_ref() == owner {
        return Ok(original.clone());
    }
    let mut result = original.clone();
    result.generation = result
        .generation
        .checked_add(1)
        .filter(|value| *value <= i64::MAX as u64)
        .ok_or(PersistenceError::StaleIacOwner)?;
    result.owner = owner.cloned();
    conn.execute("INSERT INTO iac_resource_owners(kind,resource_id,generation,manager,owner_id,logical_key)
        VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(kind,resource_id) DO UPDATE SET generation=excluded.generation,
        manager=excluded.manager,owner_id=excluded.owner_id,logical_key=excluded.logical_key",
        params![result.resource.kind,result.resource.id,result.generation as i64,
            owner.map(|owner|match owner.manager { Manager::Stack=>"stack",Manager::Terraform=>"terraform" }),
            owner.map(|owner|owner.id.to_string()),owner.map(|owner|owner.key.as_str())])?;
    for stack in original
        .owner
        .iter()
        .chain(owner)
        .filter(|owner| owner.manager == Manager::Stack)
    {
        conn.execute(
            "UPDATE stacks SET revision=revision+1 WHERE id=?1",
            [stack.id.to_string()],
        )?;
    }
    Ok(result)
}

impl Store {
    /// Save a side-effect-free advisory plan. Replaying its UUID never re-observes inputs.
    pub(crate) fn plan_resource(
        &self,
        mut request: PlanResource,
        now: u64,
    ) -> Result<ResourcePlan, PersistenceError> {
        validate(&mut request)?;
        let request_hash = fingerprint(&request)?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if iac_owners::endpoint(&tx)? != request.expected_endpoint_id {
            return Err(PersistenceError::IacEndpointConflict);
        }
        if let Some(frozen) = read(&tx, request.operation_id)? {
            if fingerprint(&frozen.request)? != request_hash {
                return Err(PersistenceError::IacOperationIdentity);
            }
            return frozen.summary();
        }
        if iac_native::read(&tx, request.operation_id)?.is_some() {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let target = request
            .resource
            .clone()
            .unwrap_or_else(|| match &request.desired {
                Some(ResourceConfiguration::AiModel(config)) => ManagedResource {
                    kind: "ai_model".into(),
                    id: config.project_id.to_string(),
                },
                _ => ManagedResource {
                    kind: "shell".into(),
                    id: Uuid::new_v4().to_string(),
                },
            });
        let before = observe(&tx, &target)?;
        if request.resource.is_some() && before.is_none() {
            return Err(PersistenceError::InvalidStackResource);
        }
        if request.resource.is_none() && before.is_some() {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        let dependencies = dependencies(&tx, request.desired.as_ref())?;
        let frozen = Frozen {
            schema: 1,
            ownership: iac_owners::read(&tx, &target)?,
            request,
            before,
            revision: Uuid::new_v4(),
            expires: now
                .checked_add(600_000)
                .filter(|value| *value <= i64::MAX as u64)
                .ok_or(PersistenceError::IacOperationIdentity)?,
            dependencies,
        };
        authority(&tx, &frozen)?;
        if frozen.request.desired.is_none() {
            deletable(&tx, &target)?;
        }
        let summary = frozen.summary()?;
        tx.execute(
            "INSERT INTO iac_resource_plans(operation_id,request_hash,definition) VALUES(?1,?2,?3)",
            params![
                summary.operation_id.to_string(),
                request_hash,
                encode(&frozen)?
            ],
        )?;
        tx.commit()?;
        Ok(summary)
    }

    /// Read the original public plan without returning desired values or script bodies.
    pub(crate) fn resource_plan(&self, id: Uuid) -> Result<Option<ResourcePlan>, PersistenceError> {
        read(&self.lock(), id)?
            .map(|frozen| frozen.summary())
            .transpose()
    }

    /// Read a terminal acknowledgement without refreshing mutable configuration.
    pub(crate) fn resource_plan_receipt(
        &self,
        id: Uuid,
    ) -> Result<Option<ResourcePlanReceipt>, PersistenceError> {
        let raw: Option<String> = self
            .lock()
            .query_row(
                "SELECT receipt FROM iac_resource_plans WHERE operation_id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        raw.map(|raw| decode(&raw)).transpose()
    }

    /// Read only the original versioned credential source before a possible OAuth renewal.
    /// The returned private value never reaches HTTP or routine logging.
    pub(crate) fn resource_plan_ai_source(
        &self,
        id: Uuid,
        request: &ApplyResourcePlan,
        now: u64,
    ) -> Result<Option<(crate::pipe_ai::SavedConnection, u64)>, PersistenceError> {
        let conn = self.lock();
        let tx = rusqlite::Transaction::new_unchecked(&conn, TransactionBehavior::Deferred)?;
        if iac_owners::endpoint(&tx)? != request.expected_endpoint_id {
            return Err(PersistenceError::IacEndpointConflict);
        }
        let frozen = read(&tx, id)?.ok_or(PersistenceError::IacOperationIdentity)?;
        if frozen.request.expected_endpoint_id != request.expected_endpoint_id
            || frozen.summary()?.plan_sha256 != request.plan_sha256
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let completed: bool = tx.query_row(
            "SELECT receipt IS NOT NULL FROM iac_resource_plans WHERE operation_id=?1",
            [id.to_string()],
            |row| row.get(0),
        )?;
        if completed {
            return Ok(None);
        }
        let Some(ResourceConfiguration::AiModel(config)) = &frozen.request.desired else {
            return Ok(None);
        };
        if now >= frozen.expires {
            return Err(PersistenceError::StaleIacExecution);
        }
        authority(&tx, &frozen)?;
        if observe(&tx, &frozen.ownership.resource)? != frozen.before {
            return Err(PersistenceError::StaleIacExecution);
        }
        Ok(Some((
            model_connection(&tx, &config.selection)?,
            super::iac_ai_connections::generation(&tx, config.selection.connection_id)?,
        )))
    }

    /// Configuration, exact ownership and original completion are committed together.
    pub(crate) fn apply_resource_plan(
        &self,
        id: Uuid,
        request: &ApplyResourcePlan,
        now: u64,
    ) -> Result<ResourcePlanReceipt, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if iac_owners::endpoint(&tx)? != request.expected_endpoint_id {
            return Err(PersistenceError::IacEndpointConflict);
        }
        let frozen = read(&tx, id)?.ok_or(PersistenceError::IacOperationIdentity)?;
        let summary = frozen.summary()?;
        if summary.plan_sha256 != request.plan_sha256
            || frozen.request.expected_endpoint_id != request.expected_endpoint_id
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let receipt: Option<String> = tx.query_row(
            "SELECT receipt FROM iac_resource_plans WHERE operation_id=?1",
            [id.to_string()],
            |row| row.get(0),
        )?;
        if let Some(receipt) = receipt {
            return decode(&receipt);
        }
        if now >= frozen.expires || now > i64::MAX as u64 {
            return Err(PersistenceError::StaleIacExecution);
        }
        authority(&tx, &frozen)?;
        let target = &frozen.ownership.resource;
        if observe(&tx, target)? != frozen.before {
            return Err(PersistenceError::StaleIacExecution);
        }
        let before_shell = match &frozen.before {
            Some(Observation::Shell(before)) => Some(before),
            _ => None,
        };
        let mut revision = before_shell
            .map(|before| {
                before
                    .revision
                    .parse()
                    .map_err(|_| PersistenceError::IacOperationIdentity)
            })
            .transpose()?;
        match (&frozen.request.desired, summary.action) {
            (None, ResourcePlanAction::Delete) => {
                deletable(&tx, target)?;
                match target.kind.as_str() {
                    "shell" => {
                        tx.execute(
                            "DELETE FROM shell_revisions WHERE shell_id=?1",
                            [&target.id],
                        )?;
                        tx.execute("DELETE FROM shells WHERE id=?1", [&target.id])?;
                    }
                    "ai_model" => {
                        tx.execute(
                            "DELETE FROM pipe_ai_defaults WHERE project_id=?1",
                            [&target.id],
                        )?;
                    }
                    _ => return Err(PersistenceError::UnsupportedIacResource),
                }
                revision = None;
            }
            (Some(ResourceConfiguration::Shell(desired)), action)
                if action != ResourcePlanAction::Noop =>
            {
                if before_shell.is_none() {
                    tx.execute("INSERT INTO shells(id,name,description,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?4)",
                        params![target.id,desired.name,desired.description,now as i64])?;
                } else {
                    tx.execute(
                        "UPDATE shells SET name=?2,description=?3,updated_at_ms=?4 WHERE id=?1",
                        params![target.id, desired.name, desired.description, now as i64],
                    )?;
                }
                if before_shell.is_none_or(|before| before.content != desired.content) {
                    let version = before_shell
                        .map_or(Some(1), |before| before.version.checked_add(1))
                        .ok_or(PersistenceError::IacOperationIdentity)?;
                    tx.execute("INSERT INTO shell_revisions(id,shell_id,version,content,content_sha256,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6)",
                        params![frozen.revision.to_string(),target.id,version,desired.content,crate::shells::content_sha256(&desired.content),now as i64])?;
                    revision = Some(frozen.revision);
                }
            }
            (Some(ResourceConfiguration::AiModel(desired)), action)
                if action != ResourcePlanAction::Noop =>
            {
                tx.execute("INSERT INTO pipe_ai_defaults(project_id,connection_id,model) VALUES(?1,?2,?3)
                    ON CONFLICT(project_id) DO UPDATE SET connection_id=excluded.connection_id,model=excluded.model",
                    params![target.id,desired.selection.connection_id.to_string(),desired.selection.model])?;
            }
            (Some(_), ResourcePlanAction::Noop) => (),
            _ => return Err(PersistenceError::IacOperationIdentity),
        }
        let ownership = save_owner(
            &tx,
            &frozen.ownership,
            frozen
                .request
                .desired
                .as_ref()
                .map(|_| &frozen.request.owner),
        )?;
        let receipt = ResourcePlanReceipt {
            plan: summary,
            ownership,
            revision_id: revision,
        };
        iac_native::save(
            &tx,
            &NativeOperation {
                operation_id: id,
                ownership: receipt.plan.ownership.clone(),
                action: NativeAction::ApplyResourcePlan,
                state: NativeOperationState::Succeeded,
                error_code: None,
                execution_generation: 0,
                configuration_generation: None,
                retain_disks: None,
            },
        )?;
        tx.execute(
            "UPDATE iac_resource_plans SET receipt=?2 WHERE operation_id=?1 AND receipt IS NULL",
            params![id.to_string(), encode(&receipt)?],
        )?;
        tx.commit()?;
        Ok(receipt)
    }
}

#[cfg(test)]
#[path = "iac_resource_plan_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "iac_resource_model_tests.rs"]
mod model_tests;
