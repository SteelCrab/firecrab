//! Ownership metadata and a pure planner. Plans contain field names, never field values.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use firecrab_api_types::{
    EgressPolicy, MicroNetworkResponse, MicroServiceDeployment, MicroServiceResponse,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{Diagnostic, Stack, diagnostic, logical_name, subnet, validate};

/// Resource namespaces shared by ownership records, CLI arguments and plans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceKind {
    /// A host-local MicroNetwork.
    Network,
    /// A standalone MicroVM, excluding builders and service replicas.
    Vm,
    /// A MicroService configuration and its deployment.
    Service,
}

impl ResourceKind {
    /// Stable database and CLI representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Vm => "vm",
            Self::Service => "service",
        }
    }
}

impl std::fmt::Display for ResourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ResourceKind {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "network" => Ok(Self::Network),
            "vm" => Ok(Self::Vm),
            "service" => Ok(Self::Service),
            _ => Err("kind must be network, vm or service"),
        }
    }
}

/// Explicit ownership assignment; resource names never imply adoption.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Binding {
    /// Resource namespace.
    pub kind: ResourceKind,
    /// Stable logical manifest key within the namespace.
    pub key: String,
    /// Existing host resource UUID, specified by the operator.
    pub resource_id: Uuid,
}

/// Stack registration request; contains no manifest or credentials.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterStackRequest {
    /// Stable name matching the manifest's `name`.
    pub name: String,
}

/// Delete only the empty registration with this exact identity.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteStackRequest {
    /// Registration UUID; prevents deleting a later registration with the same name.
    pub id: Uuid,
}

/// Durable host-local identity and ownership metadata.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StackRecord {
    /// Stable registration UUID.
    pub id: Uuid,
    /// Unique name on this Firecrab host.
    pub name: String,
    /// Increases only when the binding set changes.
    pub revision: u64,
    /// Explicitly claimed host resources, in namespace/key order.
    pub bindings: Vec<Binding>,
}

/// Persisted VM configuration needed for planning. Runtime metrics are omitted.
#[derive(Debug, Clone, Serialize)]
pub struct VmSnapshot {
    /// Host resource UUID.
    pub id: Uuid,
    /// API resource name.
    pub name: String,
    /// Original template alias; this does not verify its current content identity.
    pub template: String,
    /// Current virtual CPU count.
    pub cpu: u8,
    /// Current RAM in MiB.
    pub ram: u32,
    /// Current disk capacity in GiB.
    pub disk_gb: u16,
    /// Current egress policy.
    pub egress_policy: EgressPolicy,
    /// Assigned network UUID.
    pub micro_network_id: Uuid,
    /// Assigned storage ID.
    pub storage_root: String,
    /// Current non-secret environment settings; never returned by a plan.
    pub env: BTreeMap<String, String>,
}

/// Service configuration and its last deployment snapshot.
#[derive(Debug, Clone)]
pub struct ServiceSnapshot {
    /// Saved desired service configuration.
    pub service: MicroServiceResponse,
    /// Last deployment, used to compare image/configuration and placement.
    pub deployment: Option<MicroServiceDeployment>,
}

/// Host configuration supplied by the API. The planner never fetches it itself.
#[derive(Debug, Clone, Default)]
pub struct Inventory {
    /// Existing networks, including resources outside this stack.
    pub networks: Vec<MicroNetworkResponse>,
    /// Existing standalone VMs; builders and service replicas must be excluded.
    pub vms: Vec<VmSnapshot>,
    /// Saved services and their latest deployment snapshots.
    pub services: Vec<ServiceSnapshot>,
}

/// Advisory configuration action. None of these actions executes a mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// No owned resource exists and no matching name conflicts.
    Create,
    /// Editable configuration differs; the later apply engine may need a stop.
    Update,
    /// Immutable configuration differs, including a replaced network dependency.
    Replace,
    /// Service configuration/placement has not been deployed in its declared form.
    Deploy,
    /// Declared configuration matches the observed owned resource.
    Noop,
    /// Ownership remains after its declaration is removed; pruning is disabled.
    Retain,
    /// Missing ownership target or an existing name prevents safe planning.
    Blocked,
}

impl Action {
    /// Stable CLI and JSON action name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Replace => "replace",
            Self::Deploy => "deploy",
            Self::Noop => "noop",
            Self::Retain => "retain",
            Self::Blocked => "blocked",
        }
    }
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One redacted plan entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    /// Resource namespace.
    pub kind: ResourceKind,
    /// Manifest or retained ownership key.
    pub key: String,
    /// Bound UUID, or null for an unbound declaration.
    pub resource_id: Option<Uuid>,
    /// Suggested action, never an executed operation.
    pub action: Action,
    /// Known schema field names that differ; values are never included.
    pub fields: Vec<String>,
    /// Static reason for a blocked or retained resource.
    pub reason: Option<String>,
}

/// Read-only comparison against one observed host inventory.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    /// Opaque configuration/ownership identity; apply rejects a stale observation.
    #[serde(default)]
    pub fingerprint: String,
    /// Registration identity on the selected host.
    pub stack_id: Uuid,
    /// Manifest/registration name.
    pub name: String,
    /// Ownership revision observed while planning; not an apply authorization.
    pub revision: u64,
    /// True when ownership or a missing dependency prevents a safe comparison.
    pub blocked: bool,
    /// Network dependencies first, then VMs/services; retained entries last.
    pub changes: Vec<Change>,
}

/// Compare declared fields to explicitly owned resources without adopting or mutating them.
///
/// A plan is advisory and may become stale immediately. This API does not authorize apply,
/// check host capacity, verify installed image content, or promise deployment readiness.
pub fn plan(
    stack: &Stack,
    record: &StackRecord,
    inventory: &Inventory,
) -> Result<Plan, Vec<Diagnostic>> {
    validate(stack)?;
    if stack.name != record.name {
        return Err(vec![diagnostic("name", "must match the registered stack")]);
    }
    let mut bindings = BTreeMap::new();
    let mut ids = BTreeSet::new();
    for binding in &record.bindings {
        if !logical_name(&binding.key)
            || binding.resource_id.is_nil()
            || bindings
                .insert((binding.kind, binding.key.as_str()), binding.resource_id)
                .is_some()
            || !ids.insert((binding.kind, binding.resource_id))
        {
            return Err(vec![diagnostic(
                "$",
                "invalid or conflicting ownership metadata",
            )]);
        }
    }
    let mut changes = Vec::new();
    let mut networks = BTreeMap::new();
    for (key, desired) in &stack.micro_networks {
        let id = bindings
            .get(&(ResourceKind::Network, key.as_str()))
            .copied();
        let current = id.and_then(|id| inventory.networks.iter().find(|n| n.id == id));
        let mut change = entry(
            ResourceKind::Network,
            key,
            id,
            current.is_some(),
            inventory.networks.iter().any(|n| n.name == desired.name),
        );
        if let Some(current) = current {
            difference(&mut change, "name", desired.name != current.name);
            difference(
                &mut change,
                "subnetCidr",
                subnet(&desired.subnet_cidr) != subnet(&current.subnet_cidr),
            );
            difference(
                &mut change,
                "internetEnabled",
                desired.internet_enabled != current.internet_enabled,
            );
            change.action = if change
                .fields
                .iter()
                .any(|f| matches!(f.as_str(), "name" | "subnetCidr"))
            {
                Action::Replace
            } else if !change.fields.is_empty() {
                Action::Update
            } else {
                Action::Noop
            };
        }
        block_name_collision(
            &mut change,
            inventory.networks.iter().any(|n| {
                Some(n.id) != id
                    && n.name == desired.name
                    && current.is_none_or(|owned| owned.name != desired.name)
            }),
        );
        networks.insert(key.as_str(), (id, change.action));
        changes.push(change);
    }
    for (key, desired) in &stack.micro_vms {
        let id = bindings.get(&(ResourceKind::Vm, key.as_str())).copied();
        let current = id.and_then(|id| inventory.vms.iter().find(|vm| vm.id == id));
        let mut change = entry(
            ResourceKind::Vm,
            key,
            id,
            current.is_some(),
            inventory.vms.iter().any(|vm| vm.name == desired.name),
        );
        let (network_id, network_action) = networks[desired.network.as_str()];
        if let Some(current) = current {
            difference(&mut change, "name", desired.name != current.name);
            difference(
                &mut change,
                "template",
                desired.template != current.template,
            );
            difference(
                &mut change,
                "network",
                network_id != Some(current.micro_network_id) || network_action == Action::Replace,
            );
            difference(
                &mut change,
                "storageRoot",
                desired
                    .storage_root
                    .as_ref()
                    .is_some_and(|root| *root != current.storage_root),
            );
            difference(&mut change, "cpu", desired.cpu != current.cpu);
            difference(&mut change, "ram", desired.ram != current.ram);
            difference(&mut change, "diskGb", desired.disk_gb != current.disk_gb);
            difference(
                &mut change,
                "egressPolicy",
                desired.egress_policy != current.egress_policy,
            );
            difference(&mut change, "env", desired.env != current.env);
            change.action = if desired.disk_gb < current.disk_gb
                || change
                    .fields
                    .iter()
                    .any(|f| matches!(f.as_str(), "name" | "template" | "network" | "storageRoot"))
            {
                Action::Replace
            } else if !change.fields.is_empty() {
                Action::Update
            } else {
                Action::Noop
            };
        }
        block_dependency(&mut change, network_action);
        block_name_collision(
            &mut change,
            inventory.vms.iter().any(|vm| {
                Some(vm.id) != id
                    && vm.name == desired.name
                    && current.is_none_or(|owned| owned.name != desired.name)
            }),
        );
        changes.push(change);
    }
    for (key, desired) in &stack.micro_services {
        let id = bindings
            .get(&(ResourceKind::Service, key.as_str()))
            .copied();
        let current = id.and_then(|id| inventory.services.iter().find(|s| s.service.id == id));
        let mut change = entry(
            ResourceKind::Service,
            key,
            id,
            current.is_some(),
            inventory
                .services
                .iter()
                .any(|s| s.service.spec.name == desired.config.name),
        );
        let (network_id, network_action) = networks[desired.network.as_str()];
        if let Some(current) = current {
            let desired_fields =
                serde_json::to_value(&desired.config).expect("service configuration is JSON");
            let current_fields =
                serde_json::to_value(&current.service.spec).expect("service configuration is JSON");
            for (field, value) in desired_fields
                .as_object()
                .expect("configuration is an object")
            {
                difference(
                    &mut change,
                    &format!("config.{field}"),
                    Some(value) != current_fields.get(field),
                );
            }
            let deployed = current.deployment.as_ref();
            difference(
                &mut change,
                "deployment",
                deployed.is_none_or(|d| {
                    d.spec != desired.config
                        || d.instances.len() != usize::from(desired.config.replicas)
                }),
            );
            difference(
                &mut change,
                "network",
                network_action == Action::Replace
                    || deployed.is_none_or(|d| {
                        d.request.micro_network_id != network_id || network_id.is_none()
                    }),
            );
            difference(
                &mut change,
                "hostPort",
                deployed.is_none_or(|d| d.request.host_port != desired.host_port),
            );
            change.action = if change.fields.iter().any(|f| f.starts_with("config.")) {
                Action::Update
            } else if !change.fields.is_empty() {
                Action::Deploy
            } else {
                Action::Noop
            };
        }
        block_dependency(&mut change, network_action);
        block_name_collision(
            &mut change,
            inventory.services.iter().any(|s| {
                Some(s.service.id) != id
                    && s.service.spec.name == desired.config.name
                    && current.is_none_or(|owned| owned.service.spec.name != desired.config.name)
            }),
        );
        if current.is_some_and(|s| s.deployment.as_ref().is_some_and(|d| d.phase.is_busy())) {
            change.action = Action::Blocked;
            change.reason =
                Some("service deployment is in progress; plan again after it completes".to_owned());
        }
        changes.push(change);
    }
    for ((kind, key), resource_id) in bindings.iter().rev() {
        let declared = match kind {
            ResourceKind::Network => stack.micro_networks.contains_key(*key),
            ResourceKind::Vm => stack.micro_vms.contains_key(*key),
            ResourceKind::Service => stack.micro_services.contains_key(*key),
        };
        if !declared {
            changes.push(Change {
                kind: *kind,
                key: (*key).to_owned(),
                resource_id: Some(*resource_id),
                action: Action::Retain,
                fields: Vec::new(),
                reason: Some("declaration removed; pruning is disabled".to_owned()),
            });
        }
    }
    Ok(Plan {
        fingerprint: fingerprint(stack, record, inventory),
        stack_id: record.id,
        name: stack.name.clone(),
        revision: record.revision,
        blocked: changes.iter().any(|c| c.action == Action::Blocked),
        changes,
    })
}

/// Hash canonical desired/observed configuration, excluding metrics and live health.
pub fn fingerprint(stack: &Stack, record: &StackRecord, inventory: &Inventory) -> String {
    let mut networks = inventory.networks.iter().collect::<Vec<_>>();
    networks.sort_by_key(|n| n.id);
    let mut vms = inventory.vms.iter().collect::<Vec<_>>();
    vms.sort_by_key(|v| v.id);
    let mut services = inventory.services.iter().collect::<Vec<_>>();
    services.sort_by_key(|s| s.service.id);
    let services = services
        .into_iter()
        .map(|s| {
            let deployment = s.deployment.as_ref().map(|d| {
                let mut instances = d.instances.iter().map(|i| i.vm_id).collect::<Vec<_>>();
                instances.sort();
                (
                    d.id,
                    &d.spec,
                    &d.request,
                    &d.digest,
                    d.phase.is_busy(),
                    d.domain_route_owned,
                    instances,
                )
            });
            (&s.service, deployment)
        })
        .collect::<Vec<_>>();
    let mut bindings = record.bindings.iter().collect::<Vec<_>>();
    bindings.sort_by_key(|b| (b.kind, &b.key, b.resource_id));
    let bytes = serde_json::to_vec(&(
        stack,
        record.id,
        &record.name,
        record.revision,
        bindings,
        networks,
        vms,
        services,
    ))
    .expect("stack configuration is JSON");
    format!("{:x}", Sha256::digest(bytes))
}

fn entry(
    kind: ResourceKind,
    key: &str,
    id: Option<Uuid>,
    exists: bool,
    name_exists: bool,
) -> Change {
    let (action, reason) = if id.is_some() && !exists {
        (
            Action::Blocked,
            Some("owned resource is missing; repair or explicitly unbind its UUID"),
        )
    } else if id.is_none() && name_exists {
        (
            Action::Blocked,
            Some("resource name already exists; explicit UUID binding is required"),
        )
    } else if id.is_none() {
        (Action::Create, None)
    } else {
        (Action::Noop, None)
    };
    Change {
        kind,
        key: key.to_owned(),
        resource_id: id,
        action,
        fields: Vec::new(),
        reason: reason.map(str::to_owned),
    }
}

fn difference(change: &mut Change, field: &str, differs: bool) {
    if differs {
        change.fields.push(field.to_owned());
    }
}

fn block_dependency(change: &mut Change, network_action: Action) {
    if network_action == Action::Blocked {
        change.action = Action::Blocked;
        change.reason = Some("network dependency is blocked".to_owned());
    }
}

fn block_name_collision(change: &mut Change, conflict: bool) {
    if conflict {
        change.action = Action::Blocked;
        change.reason = Some(
            "resource name conflicts with another UUID; no implicit adoption or overwrite"
                .to_owned(),
        );
    }
}
