//! Host-side ownership transitions retain their target reservation until the API confirms its mirror.
use firecrab_stack::{ChangeResourceOwner, ManagedResource, ResourceOwnership};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Physical installation, rather than the guest or API process making a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostIdentity {
    /// Stable native Linux machine identity; a boot ID would prevent legitimate restart recovery.
    pub machine_id: String,
    /// Canonical configuration root selected by the installed host broker.
    pub configuration_root: String,
    /// Device containing the originally observed configuration directory.
    pub device: u64,
    /// Directory inode retained across the ownership transition.
    pub inode: u64,
}

/// A host observation never grants write authority and contains no credential values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Observation {
    /// Actual physical host and root that produced the observation.
    pub host: HostIdentity,
    /// Original owner and generation on that host.
    pub ownership: ResourceOwnership,
    /// Whether the exact independent listener or route exists.
    pub exists: bool,
    /// A service controller owns this configuration; standalone adoption is forbidden.
    pub parent_controlled: bool,
    /// Frozen configuration projection, excluding credentials and unrelated resources.
    pub configuration_sha256: String,
    /// An unfinished host operation prevents transfer even after its caller disappears.
    pub busy: bool,
}

/// Original host proof accompanies the API's ordinary ownership CAS request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnerChange {
    /// Exact API endpoint, resource and original/desired owner identities.
    pub change: ChangeResourceOwner,
    /// Physical target cannot change between observation and privileged commit.
    pub host: HostIdentity,
    /// Configuration whose ownership was observed; stale proofs cannot adopt another generation.
    pub configuration_sha256: String,
}
impl OwnerChange {
    /// Validate identities and configuration pins before any privileged mutation.
    pub fn validate(&mut self) -> Result<(), &'static str> {
        self.change.validate()?;
        validate_resource(&self.change.resource)?;
        if self.host.machine_id.len() != 32
            || !self
                .host
                .machine_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || self.host.configuration_root.is_empty()
            || !self.host.configuration_root.starts_with('/')
            || self.host.configuration_root.chars().any(char::is_control)
            || self.configuration_sha256.len() != 64
            || !self
                .configuration_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("provide the observed physical host and configuration digest");
        }
        Ok(())
    }
    /// Request identity excludes fresh observations so a terminal retry can be read after drift.
    pub fn request_sha256(&self) -> Result<String, serde_json::Error> {
        serde_json::to_vec(&self.change).map(|bytes| format!("{:x}", Sha256::digest(bytes)))
    }
}

/// The target keeps its reservation until the corresponding API owner row is durable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnerReceipt {
    /// Caller-retained transition ID, independent of connection lifetime.
    pub operation_id: Uuid,
    /// Original API request identity, retained even after later configuration changes.
    pub request_sha256: String,
    /// Physical installation on which the transition committed.
    pub host: HostIdentity,
    /// Immutable acknowledgement of the original transition.
    pub ownership: ResourceOwnership,
    /// True only after the API acknowledged its durable local owner projection.
    pub confirmed: bool,
}

/// A retained no-effect rejection permits a new observation without abandoning uncertain work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnerRejection {
    /// Original operation, permanently rejected before a target metadata commit.
    pub operation_id: Uuid,
    /// Original normalized request; reusing its ID with another request is forbidden.
    pub request_sha256: String,
    /// Actual physical installation which retained this no-effect result.
    pub host: HostIdentity,
}

/// Restricted metadata operations; owner fields never authorize arbitrary host configuration writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Observe an exact slot without adopting it or creating infrastructure.
    Observe {
        /// API database identity used before this host has an ownership namespace.
        endpoint_id: Uuid,
        /// Exact listener or route identity on the installed host.
        resource: ManagedResource,
    },
    /// Commit owner metadata while preserving all configuration and bytes.
    ChangeOwner {
        /// Validated API CAS and original physical/configuration proof.
        request: Box<OwnerChange>,
    },
    /// Release only the reservation whose API mirror is already durable.
    ConfirmOwner {
        /// Original transition whose target reservation is being acknowledged.
        operation_id: Uuid,
        /// API endpoint to which the target namespace is bound.
        endpoint_id: Uuid,
    },
    /// Read an immutable receipt before consulting mutable presence or readiness.
    OwnerReceipt {
        /// Original transition ID, never a newly generated retry identity.
        operation_id: Uuid,
        /// API endpoint to which the receipt belongs.
        endpoint_id: Uuid,
    },
}

/// Responses remain typed; a caller cannot mistake a configuration snapshot for a transfer receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    /// A durable no-effect result, distinct from unavailable or uncertain transport.
    OwnerRejected {
        /// Original operation can be safely closed without releasing someone else's reservation.
        rejection: OwnerRejection,
    },
    /// Current physical presence and host-side ownership.
    Observation {
        /// Secret-free observation of the exact requested slot.
        observation: Observation,
    },
    /// Original host transition, including its API-confirmation state.
    OwnerReceipt {
        /// Absent only when that transition was never admitted on this host.
        receipt: Option<OwnerReceipt>,
    },
}

/// Resource families whose ownership is committed by the privileged Pipe broker.
pub fn is_pipe_kind(kind: &str) -> bool {
    matches!(kind, "ingress" | "quick_tunnel" | "named_tunnel")
}

/// Only broker-backed identities are accepted; no arbitrary unit or file path crosses this boundary.
pub fn validate_resource(resource: &ManagedResource) -> Result<(), &'static str> {
    let mut canonical = resource.clone();
    canonical.normalize()?;
    if canonical != *resource {
        return Err("use the canonical host resource identity");
    }
    if matches!(resource.kind.as_str(), "quick_tunnel" | "named_tunnel") {
        return Uuid::parse_str(&resource.id)
            .ok()
            .filter(|id| !id.is_nil())
            .map(|_| ())
            .ok_or("provide the exact non-nil connector UUID");
    }
    if resource.kind != "ingress" {
        return Err("unsupported host ownership resource");
    }
    if resource.id == "listener" {
        return Ok(());
    }
    if let Some(hash) = resource.id.strip_prefix("route:sha256:") {
        return if hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(())
        } else {
            Err("provide the canonical route digest")
        };
    }
    let Some((host, path)) = resource
        .id
        .strip_prefix("route:")
        .and_then(|value| value.split_once('/'))
    else {
        return Err("use listener or the exact route hostname and path");
    };
    let path = format!("/{path}");
    if !crate::pipe::hostname(host)
        || crate::pipe::reserved_quick_host(host)
        || !crate::pipe::path(&path)
        || (path.len() > 1 && path.ends_with('/'))
    {
        return Err("provide a canonical route hostname and path");
    }
    Ok(())
}

/// One canonical anonymous-connector slot, including the retained legacy connector UUID.
pub fn quick_tunnel_resource(id: Uuid) -> ManagedResource {
    ManagedResource {
        kind: "quick_tunnel".into(),
        id: id.to_string(),
    }
}

/// One configured named connector; its credentials and hostname remain bound to this UUID.
pub fn named_tunnel_resource(id: Uuid) -> ManagedResource {
    ManagedResource {
        kind: "named_tunnel".into(),
        id: id.to_string(),
    }
}

/// Existing long literal paths remain addressable without exceeding the shared identity limit.
/// Only the canonical key produced from an actual route is considered present by the host broker.
pub fn ingress_route_key(host: &str, path: &str) -> String {
    let path = path.trim_end_matches('/');
    let key = format!("route:{host}{}", if path.is_empty() { "/" } else { path });
    if key.len() <= 512 {
        key
    } else {
        format!("route:sha256:{:x}", Sha256::digest(key.as_bytes()))
    }
}
