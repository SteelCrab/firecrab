//! Durable host ownership under the same cross-process lock as native Pipe changes.

use std::collections::BTreeMap;

use firecrab_helper_protocol::host_iac::{
    HostIdentity, Observation, OwnerChange, OwnerReceipt, OwnerRejection, Reply,
};
use firecrab_helper_protocol::pipe::{Config, Route};
use firecrab_stack::{ManagedResource, ResourceOwnership};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Validation or CAS rejection; public messages contain no configuration values.
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Written atomically while the privileged caller holds the Pipe transaction lock.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Ledger {
    /// Older ingress-only binaries reject this new field instead of ignoring qualified owner keys.
    #[serde(default = "current_schema_version")]
    schema_version: u16,
    /// Fresh systemd jobs/state are observed under the lock and are never persisted as configuration.
    #[serde(skip)]
    runtime_busy: bool,
    /// Root-owned address metadata proves controller ownership independently of the API database.
    #[serde(skip)]
    controller_owned: bool,
    /// Fresh validated credential digest participates in named-connector adoption proofs only.
    #[serde(skip)]
    named_credentials_sha256: Option<String>,
    /// Fresh private-registry projections exclude mutable connectivity and credential values.
    #[serde(skip)]
    addresses: BTreeMap<String, serde_json::Value>,
    /// First admitted transition binds the physical installation and API database.
    binding: Option<(HostIdentity, Uuid)>,
    /// Released records retain their monotonically increasing generations.
    owners: BTreeMap<String, ResourceOwnership>,
    /// Original exclusively owned children remain fenced after private parent-record deletion.
    #[serde(default)]
    address_pins: BTreeMap<String, serde_json::Value>,
    /// Immutable results survive acknowledgement loss and later transfers.
    receipts: BTreeMap<Uuid, OwnerReceipt>,
    /// Known no-effect rejections cannot turn into a later successful replay.
    #[serde(default)]
    rejections: BTreeMap<Uuid, OwnerRejection>,
}

/// Missing version marks the original ingress-only file; its owner keys are upgraded on read.
fn current_schema_version() -> u16 {
    1
}

impl Default for Ledger {
    fn default() -> Self {
        Self {
            schema_version: current_schema_version(),
            runtime_busy: false,
            controller_owned: false,
            named_credentials_sha256: None,
            addresses: BTreeMap::new(),
            binding: None,
            owners: BTreeMap::new(),
            address_pins: BTreeMap::new(),
            receipts: BTreeMap::new(),
            rejections: BTreeMap::new(),
        }
    }
}

/// Kind-qualified identities keep host resources with the same local ID in separate slots.
fn resource_key(resource: &ManagedResource) -> String {
    format!("{}:{}", resource.kind, resource.id)
}

/// Canonical route key; trailing slashes describe the same Nginx location.
fn route_key(route: &Route) -> String {
    firecrab_helper_protocol::host_iac::ingress_route_key(&route.host, &route.path_prefix)
}

/// Exact projection excludes credentials and changes to unrelated routes or connectors.
fn projection(config: Option<&Config>, resource: &ManagedResource) -> serde_json::Value {
    let Some(config) = config else {
        return serde_json::Value::Null;
    };
    if resource.kind == "named_tunnel" {
        return config
            .tunnel
            .as_ref()
            .filter(|tunnel| tunnel.id.to_string() == resource.id)
            .map(|tunnel| {
                serde_json::json!({"listen":config.listen,"id":tunnel.id,
                "hostname":tunnel.hostname,"routes":config.routes})
            })
            .unwrap_or(serde_json::Value::Null);
    }
    if resource.kind == "quick_tunnel" {
        if resource.id == firecrab_helper_protocol::pipe::LEGACY_QUICK_TUNNEL_ID.to_string()
            && let Some(legacy) = &config.quick_tunnel
        {
            return serde_json::json!({"listen":config.listen,"legacy":legacy});
        }
        return config
            .quick_tunnels
            .iter()
            .find(|entry| entry.id.to_string() == resource.id)
            .map(|entry| serde_json::json!({"listen":config.listen,"entry":entry}))
            .unwrap_or(serde_json::Value::Null);
    }
    if resource.id == "listener" {
        serde_json::json!({"listen":config.listen})
    } else {
        config
            .routes
            .iter()
            .find(|route| route_key(route) == resource.id)
            .map(|route| serde_json::json!({"listen":config.listen,"route":route}))
            .unwrap_or(serde_json::Value::Null)
    }
}

impl Ledger {
    /// Keep address origin and owned-child configuration in the same host-side proof.
    fn projection(&self, config: Option<&Config>, resource: &ManagedResource) -> serde_json::Value {
        if resource.kind != "firecrab_tunnel" {
            return projection(config, resource);
        }
        self.address_projection(config, self.addresses.get(&resource.id))
    }

    /// A configured parent owns only its exclusive connector; borrowed connectors remain independent.
    fn address_projection(
        &self,
        config: Option<&Config>,
        binding: Option<&serde_json::Value>,
    ) -> serde_json::Value {
        let Some(binding) = binding else {
            return serde_json::Value::Null;
        };
        let connector = if binding["ownedConnector"] == true {
            binding["connectorId"].as_str().map(|id| {
                projection(
                    config,
                    &ManagedResource {
                        kind: "quick_tunnel".into(),
                        id: id.into(),
                    },
                )
            })
        } else {
            None
        };
        serde_json::json!({"binding":binding,"ownedConnector":connector})
    }

    /// Native configuration checks follow the admitted child, even if a mutable parent file disappears.
    fn protected_projection(
        &self,
        config: Option<&Config>,
        resource: &ManagedResource,
    ) -> serde_json::Value {
        if resource.kind == "firecrab_tunnel" {
            self.address_projection(config, self.address_pins.get(&resource.id))
        } else {
            projection(config, resource)
        }
    }

    /// Populate under the host/address locks; no address secrets enter the durable owner file.
    pub(super) fn set_addresses(&mut self, addresses: BTreeMap<String, serde_json::Value>) {
        self.addresses = addresses;
    }
    /// Preserve the original ingress-only ledger while separating future host resource namespaces.
    pub(super) fn normalize_keys(&mut self) -> Result<()> {
        if self.schema_version != current_schema_version() {
            return Err("host ownership schema is unsupported".into());
        }
        let mut canonical = BTreeMap::new();
        for (key, ownership) in std::mem::take(&mut self.owners) {
            firecrab_helper_protocol::host_iac::validate_resource(&ownership.resource)?;
            let qualified = resource_key(&ownership.resource);
            let legacy_ingress =
                ownership.resource.kind == "ingress" && key == ownership.resource.id;
            if (key != qualified && !legacy_ingress)
                || canonical.insert(qualified, ownership).is_some()
            {
                return Err("host ownership namespace is inconsistent".into());
            }
        }
        self.owners = canonical;
        for ownership in self.owners.values() {
            if ownership.resource.kind == "firecrab_tunnel" && self.protected(&ownership.resource) {
                let pin = self
                    .address_pins
                    .get(&ownership.resource.id)
                    .ok_or("managed Firecrab address has no retained child identity")?;
                if pin["subdomain"].as_str() != Some(ownership.resource.id.as_str())
                    || pin["ownedConnector"].as_bool().is_none()
                    || pin["connectorId"]
                        .as_str()
                        .and_then(|id| Uuid::parse_str(id).ok())
                        .is_none_or(|id| id.is_nil())
                    || serde_json::from_value::<firecrab_helper_protocol::firecrab_tunnel::Target>(
                        pin["target"].clone(),
                    )
                    .is_err()
                {
                    return Err("managed Firecrab address child identity is invalid".into());
                }
            }
        }
        Ok(())
    }
    /// Include asynchronous native unit work in the ownership admission check.
    pub(super) fn set_runtime_busy(&mut self, busy: bool) {
        self.runtime_busy = busy;
    }
    /// Parent provenance is read from privileged metadata while the same Pipe lock is held.
    pub(super) fn set_controller_owned(&mut self, owned: bool) {
        self.controller_owned = owned;
    }
    /// Pin credential content without retaining or returning the credential document.
    pub(super) fn set_named_credentials_sha256(&mut self, digest: Option<String>) {
        self.named_credentials_sha256 = digest;
    }
    /// Terminal replays bypass mutable manifest IO, including later corruption or removal.
    pub(super) fn has_outcome(&self, operation: Uuid) -> bool {
        self.receipts.contains_key(&operation) || self.rejections.contains_key(&operation)
    }
    /// Retain both committed and no-effect outcomes before the helper sends its acknowledgement.
    pub(super) fn transition(
        &mut self,
        host: &HostIdentity,
        config: Option<&Config>,
        mut request: OwnerChange,
    ) -> Result<Reply> {
        request.validate()?;
        self.check_binding(host, request.change.expected_endpoint_id)?;
        let hash = request.request_sha256()?;
        if let Some(rejected) = self.rejections.get(&request.change.operation_id) {
            if rejected.request_sha256 != hash {
                return Err("operation ID belongs to another request".into());
            }
            return Ok(Reply::OwnerRejected {
                rejection: rejected.clone(),
            });
        }
        // A mismatching replay must never overwrite an existing committed result with a rejection.
        if let Some(receipt) = self.receipts.get(&request.change.operation_id) {
            if receipt.request_sha256 != hash {
                return Err("operation ID belongs to another request".into());
            }
            return Ok(Reply::OwnerReceipt {
                receipt: Some(receipt.clone()),
            });
        }
        match self.change(host, config, request.clone()) {
            Ok(receipt) => Ok(Reply::OwnerReceipt {
                receipt: Some(receipt),
            }),
            Err(_) => {
                let rejection = OwnerRejection {
                    operation_id: request.change.operation_id,
                    request_sha256: hash,
                    host: host.clone(),
                };
                self.rejections
                    .insert(rejection.operation_id, rejection.clone());
                Ok(Reply::OwnerRejected { rejection })
            }
        }
    }
    /// Fail closed if another API or replacement directory reaches the persisted namespace.
    fn check_binding(&self, host: &HostIdentity, endpoint: Uuid) -> Result<()> {
        if endpoint.is_nil()
            || self
                .binding
                .as_ref()
                .is_some_and(|bound| bound != &(host.clone(), endpoint))
        {
            return Err(
                "host ownership belongs to another physical installation or API endpoint".into(),
            );
        }
        Ok(())
    }

    /// Observe without binding an unused host or granting write permission.
    pub(super) fn observe(
        &self,
        host: &HostIdentity,
        endpoint: Uuid,
        resource: &ManagedResource,
        config: Option<&Config>,
    ) -> Result<Observation> {
        firecrab_helper_protocol::host_iac::validate_resource(resource)?;
        self.check_binding(host, endpoint)?;
        let mut value = self.projection(config, resource);
        if resource.kind == "named_tunnel" && !value.is_null() {
            value["credentialsSha256"] = serde_json::json!(self.named_credentials_sha256);
        }
        Ok(Observation {
            host: host.clone(),
            ownership: self.owners.get(&resource_key(resource)).cloned().unwrap_or(
                ResourceOwnership {
                    endpoint_id: endpoint,
                    resource: resource.clone(),
                    generation: 0,
                    owner: None,
                },
            ),
            exists: !value.is_null(),
            parent_controlled: self.controller_owned
                || self.address_owns_child(resource)
                || value
                    .get("route")
                    .and_then(|route| route.get("owner"))
                    .is_some_and(|owner| !owner.is_null()),
            configuration_sha256: format!("{:x}", Sha256::digest(serde_json::to_vec(&value)?)),
            busy: self.runtime_busy
                || self
                    .receipts
                    .values()
                    .any(|receipt| !receipt.confirmed && receipt.ownership.resource == *resource),
        })
    }

    /// Commit only the original observed CAS and retain the reservation until API confirmation.
    pub(super) fn change(
        &mut self,
        host: &HostIdentity,
        config: Option<&Config>,
        mut request: OwnerChange,
    ) -> Result<OwnerReceipt> {
        request.validate()?;
        let digest = request.request_sha256()?;
        self.check_binding(host, request.change.expected_endpoint_id)?;
        if let Some(receipt) = self.receipts.get(&request.change.operation_id) {
            if receipt.request_sha256 != digest {
                return Err("operation ID belongs to a different ownership request".into());
            }
            return Ok(receipt.clone());
        }
        let observation = self.observe(
            host,
            request.change.expected_endpoint_id,
            &request.change.resource,
            config,
        )?;
        if request.host != *host || request.configuration_sha256 != observation.configuration_sha256
        {
            return Err("physical host or configuration changed after observation".into());
        }
        if observation.busy {
            return Err("host ownership transition awaits API confirmation".into());
        }
        if request.change.owner.as_ref().is_some_and(|owner| {
            self.owners.values().any(|current| {
                current.resource != request.change.resource
                    && current.owner.as_ref() == Some(owner)
                    && (owner.manager == firecrab_stack::Manager::Terraform
                        || current.resource.kind == request.change.resource.kind)
            })
        }) {
            return Err("logical owner address already controls another host resource".into());
        }
        if observation.ownership.generation != request.change.expected_generation
            || observation.ownership.owner != request.change.expected_owner
        {
            return Err("host ownership changed after observation".into());
        }
        if request.change.owner.is_some() && (!observation.exists || observation.parent_controlled)
        {
            return Err("only an existing independent host resource can be adopted".into());
        }
        let address_pin = if request.change.resource.kind == "firecrab_tunnel"
            && request.change.owner.is_some()
        {
            let address = self
                .addresses
                .get(&request.change.resource.id)
                .ok_or("saved Firecrab address is unavailable")?;
            if observation.ownership.owner.is_some()
                && self.address_pins.get(&request.change.resource.id) != Some(address)
            {
                return Err("restore the original Firecrab address identity or explicitly release its ownership".into());
            }
            Some(address.clone())
        } else {
            None
        };
        let mut ownership = observation.ownership;
        if ownership.owner != request.change.owner {
            ownership.generation = ownership
                .generation
                .checked_add(1)
                .filter(|generation| *generation <= i64::MAX as u64)
                .ok_or("ownership generation exhausted")?;
        }
        ownership.owner = request.change.owner;
        let receipt = OwnerReceipt {
            operation_id: request.change.operation_id,
            request_sha256: digest,
            host: host.clone(),
            ownership,
            confirmed: false,
        };
        self.binding = Some((host.clone(), receipt.ownership.endpoint_id));
        if let Some(pin) = address_pin {
            self.address_pins
                .insert(receipt.ownership.resource.id.clone(), pin);
        }
        self.owners.insert(
            resource_key(&receipt.ownership.resource),
            receipt.ownership.clone(),
        );
        self.receipts.insert(receipt.operation_id, receipt.clone());
        Ok(receipt)
    }

    /// Acknowledge the API mirror without changing the original owner result.
    pub(super) fn confirm(
        &mut self,
        host: &HostIdentity,
        endpoint: Uuid,
        operation: Uuid,
    ) -> Result<OwnerReceipt> {
        self.check_binding(host, endpoint)?;
        let receipt = self
            .receipts
            .get_mut(&operation)
            .ok_or("host ownership operation does not exist")?;
        receipt.confirmed = true;
        Ok(receipt.clone())
    }

    /// Read before mutable configuration lookup so an original acknowledgement survives later drift.
    pub(super) fn receipt(
        &self,
        host: &HostIdentity,
        endpoint: Uuid,
        operation: Uuid,
    ) -> Result<Option<OwnerReceipt>> {
        self.check_binding(host, endpoint)?;
        Ok(self.receipts.get(&operation).cloned())
    }

    /// Native configuration writes may preserve, but cannot edit, another workflow's exact slots.
    pub(super) fn check_configuration(
        &self,
        current: Option<&Config>,
        next: &Config,
    ) -> Result<()> {
        for ownership in self.owners.values() {
            if ownership.resource.kind == "named_tunnel"
                && self.protected(&ownership.resource)
                && (next.quick_tunnel.is_some()
                    || next
                        .tunnel
                        .as_ref()
                        .is_some_and(|tunnel| tunnel.id.to_string() != ownership.resource.id))
            {
                return Err("named tunnel slot is managed; release its original ownership before replacement".into());
            }
            if self.protected(&ownership.resource)
                && self.protected_projection(current, &ownership.resource)
                    != self.protected_projection(Some(next), &ownership.resource)
            {
                return Err(
                    "Pipe resource is managed or has an unconfirmed ownership transition".into(),
                );
            }
        }
        Ok(())
    }

    /// A shared ingress unit action changes every route's declared runtime state.
    pub(super) fn check_ingress_control(&self) -> Result<()> {
        if self
            .owners
            .values()
            .any(|owner| self.protected(&owner.resource))
        {
            return Err(
                "ingress unit has managed resources or an unconfirmed ownership transition".into(),
            );
        }
        Ok(())
    }

    /// An exact connector control cannot use shared service endpoints to bypass its owner.
    pub(super) fn check_resource_control(&self, resource: &ManagedResource) -> Result<()> {
        if self.protected(resource) || self.address_owns_child(resource) {
            return Err("connector is managed or has an unconfirmed ownership transition".into());
        }
        Ok(())
    }

    /// A missing parent file cannot turn its admitted connector into an independent resource.
    fn address_owns_child(&self, resource: &ManagedResource) -> bool {
        resource.kind == "quick_tunnel"
            && self.owners.values().any(|parent| {
                parent.resource.kind == "firecrab_tunnel"
                    && self.protected(&parent.resource)
                    && self
                        .address_pins
                        .get(&parent.resource.id)
                        .is_some_and(|pin| {
                            pin["ownedConnector"] == true
                                && pin["connectorId"].as_str() == Some(resource.id.as_str())
                        })
            })
    }

    /// Named connectors share one native unit, including after external configuration deletion.
    pub(super) fn check_named_control(&self) -> Result<()> {
        if self
            .owners
            .values()
            .any(|owner| owner.resource.kind == "named_tunnel" && self.protected(&owner.resource))
        {
            return Err(
                "named tunnel slot is managed or has an unconfirmed ownership transition".into(),
            );
        }
        Ok(())
    }

    /// Even a released owner remains reserved until its mirror is durably acknowledged.
    fn protected(&self, resource: &ManagedResource) -> bool {
        self.owners
            .get(&resource_key(resource))
            .is_some_and(|owner| owner.owner.is_some())
            || self
                .receipts
                .values()
                .any(|receipt| !receipt.confirmed && receipt.ownership.resource == *resource)
    }
}

#[cfg(test)]
#[path = "owner_ledger_tests.rs"]
mod tests;
