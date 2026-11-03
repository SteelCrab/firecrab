//! Native installation proofs are separate from the Linux guest's Pipe identity.

use firecrab_stack::{ChangeResourceOwner, ManagedResource, ResourceOwnership};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[cfg(feature = "native-transport")]
pub mod transport;

/// The native broker has its own authenticated channel and versioned payload.
pub const VERSION: u16 = 1;

/// Platform observed by the broker process, never inferred from the API guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    /// Native service installation on a Linux host.
    Linux,
    /// Native macOS management installation outside its Linux VM.
    Macos,
    /// Native Windows management installation outside WSL.
    Windows,
}

impl Platform {
    /// Stable identity input independent of Rust's Debug representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Macos => "macos",
            Self::Windows => "windows",
        }
    }
}

/// Both native facets share a physical installation, with separate ownership slots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Target {
    /// Actual platform running this authenticated broker.
    pub platform: Platform,
    /// Stable installation UUID selected locally by the broker.
    pub installation_id: Uuid,
    /// Digest of the native machine identity, independent of management-guest reboots.
    pub machine_sha256: String,
    /// Canonical native installation root; requests cannot select another directory.
    pub configuration_root: String,
    /// Original OS filesystem object identity, including the directory's containing volume.
    pub directory_id: String,
}

/// Canonical hashes cannot be confused with empty or case-dependent identity data.
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl Target {
    /// Validate bounded proof data; the broker must also compare the actual physical target.
    pub fn validate(&self) -> Result<(), &'static str> {
        let absolute = match self.platform {
            Platform::Linux | Platform::Macos => self.configuration_root.starts_with('/'),
            Platform::Windows => {
                let bytes = self.configuration_root.as_bytes();
                self.configuration_root.starts_with("\\\\")
                    || (bytes.len() >= 3
                        && bytes[0].is_ascii_alphabetic()
                        && bytes[1] == b':'
                        && matches!(bytes[2], b'/' | b'\\'))
            }
        };
        if self.installation_id.is_nil()
            || !digest(&self.machine_sha256)
            || !absolute
            || self.configuration_root.len() > 4096
            || self.configuration_root.chars().any(char::is_control)
            || self.directory_id.is_empty()
            || self.directory_id.split(':').any(str::is_empty)
            || self.directory_id.len() > 256
            || self
                .directory_id
                .bytes()
                .any(|byte| !byte.is_ascii_hexdigit() && byte != b':')
        {
            return Err("provide the original observed native installation identity");
        }
        Ok(())
    }
}

/// Explicit native facets; unsupported platforms must not fabricate guest-local settings.
pub fn native_kind(kind: &str) -> bool {
    matches!(kind, "management_settings" | "host_configuration")
}

/// Only the broker-selected installation UUID addresses its native facets.
pub fn validate_resource(resource: &ManagedResource, target: &Target) -> Result<(), &'static str> {
    target.validate()?;
    let mut canonical = resource.clone();
    canonical.normalize()?;
    if canonical != *resource
        || !native_kind(&resource.kind)
        || resource.id != target.installation_id.to_string()
    {
        return Err("use the exact native installation UUID and facet");
    }
    Ok(())
}

/// Secret-free physical/native configuration evidence, independent of the guest's ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Observation {
    /// Native target reached over the authenticated broker channel.
    pub target: Target,
    /// Current owner at that physical installation.
    pub ownership: ResourceOwnership,
    /// Actual persistent installation configuration exists on this target.
    pub exists: bool,
    /// Native resource/settings locks or retained operations prevent adoption.
    pub busy: bool,
    /// Original configuration projection without any credential values.
    pub configuration_sha256: String,
}

/// An admitted metadata transition cannot silently move to a different native installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnerChange {
    /// Ordinary shared ownership CAS with a caller-retained operation UUID.
    pub change: ChangeResourceOwner,
    /// Actual observed native target, including the directory object identity.
    pub target: Target,
    /// Native configuration observed before the privileged metadata commit.
    pub configuration_sha256: String,
}

impl OwnerChange {
    /// Reject another resource family or installation before any privileged effects.
    pub fn validate(&mut self) -> Result<(), &'static str> {
        self.change.validate()?;
        validate_resource(&self.change.resource, &self.target)?;
        if !digest(&self.configuration_sha256) {
            return Err("provide the observed native configuration digest");
        }
        Ok(())
    }

    /// Terminal retry identity excludes mutable observations, as in the Pipe broker.
    pub fn request_sha256(&self) -> Result<String, serde_json::Error> {
        serde_json::to_vec(&self.change).map(|bytes| format!("{:x}", Sha256::digest(bytes)))
    }
}

/// Immutable acknowledgement remains reserved until its API mirror is durable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Receipt {
    /// Original operation UUID, retained across disconnection and process restart.
    pub operation_id: Uuid,
    /// Original normalized ownership request, never a caller-supplied permission header.
    pub request_sha256: String,
    /// Native physical target that committed this result.
    pub target: Target,
    /// Result of the original ownership transition.
    pub ownership: ResourceOwnership,
    /// API acknowledged its durable local ownership projection.
    pub confirmed: bool,
}

/// Only this retained outcome can release a no-effect API reservation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Rejection {
    /// Original operation rejected before any target owner mutation.
    pub operation_id: Uuid,
    /// Exact normalized request that was rejected.
    pub request_sha256: String,
    /// Actual native installation that retained the no-effect result.
    pub target: Target,
}

/// Restricted metadata messages; no request chooses a native root, executable or arbitrary command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Discover only the native installation selected by this broker.
    Identity,
    /// Observe an exact facet without changing infrastructure or owner metadata.
    Observe {
        /// Stable API database to which any new transition must belong.
        endpoint_id: Uuid,
        /// Exact native installation UUID and facet.
        resource: ManagedResource,
    },
    /// Commit only ownership metadata, with the original target/configuration proof.
    ChangeOwner {
        /// Original normalized CAS request.
        request: Box<OwnerChange>,
    },
    /// Acknowledge the durable API mirror without modifying the original result.
    ConfirmOwner {
        /// Original transition whose reservation is being released.
        operation_id: Uuid,
        /// Original API namespace, never a replacement database.
        endpoint_id: Uuid,
    },
    /// Read a terminal result before consulting mutable native configuration.
    OwnerReceipt {
        /// Caller-retained transition UUID.
        operation_id: Uuid,
        /// API namespace that originally admitted the operation.
        endpoint_id: Uuid,
    },
}

/// Typed replies cannot be mistaken for workflow execution grants or credential values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    /// Actual native target and only the facets supported by this installation.
    Identity {
        /// Physical native target, outside the management guest where applicable.
        target: Target,
        /// Implemented native facets; an unsupported platform cannot advertise fabricated settings.
        kinds: Vec<String>,
    },
    /// Current native ownership and configuration proof.
    Observation {
        /// Fresh native observation obtained under its own settings/resource locks.
        observation: Observation,
    },
    /// Original immutable transition result and mirror acknowledgement state.
    OwnerReceipt {
        /// Absent only when the operation was never admitted on this target.
        receipt: Option<Receipt>,
    },
    /// Durable proof of an original rejection with no owner mutation.
    OwnerRejected {
        /// Retained original no-effect result.
        rejection: Rejection,
    },
}

/// The outer broker protocol is independent of the guest helper's network envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    /// Native protocol version, checked before admitting metadata work.
    pub version: u16,
    /// Restricted operation carried by the authenticated transport.
    pub request: Request,
}

impl Envelope {
    /// Bind the API namespace to its verified client certificate, never a caller header.
    pub fn authorize(&self, endpoint: Uuid) -> Result<(), &'static str> {
        self.validate()?;
        let expected = match &self.request {
            Request::Identity => endpoint,
            Request::ChangeOwner { request } => request.change.expected_endpoint_id,
            Request::Observe { endpoint_id, .. }
            | Request::ConfirmOwner { endpoint_id, .. }
            | Request::OwnerReceipt { endpoint_id, .. } => *endpoint_id,
        };
        if endpoint.is_nil() || expected != endpoint {
            return Err("native request does not belong to its authenticated API endpoint");
        }
        Ok(())
    }

    /// A mismatched native broker must fail closed rather than fall back to the guest.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != VERSION {
            return Err("unsupported native ownership protocol version");
        }
        match &self.request {
            Request::Identity => Ok(()),
            Request::ChangeOwner { request } => request.clone().validate(),
            Request::Observe {
                endpoint_id,
                resource,
            } if !endpoint_id.is_nil() => {
                let mut canonical = resource.clone();
                canonical.normalize()?;
                if canonical != *resource
                    || !native_kind(&resource.kind)
                    || Uuid::parse_str(&resource.id)
                        .ok()
                        .is_none_or(|id| id.is_nil())
                {
                    return Err("provide the exact native installation and facet");
                }
                Ok(())
            }
            Request::ConfirmOwner {
                operation_id,
                endpoint_id,
            }
            | Request::OwnerReceipt {
                operation_id,
                endpoint_id,
            } if !operation_id.is_nil() && !endpoint_id.is_nil() => Ok(()),
            _ => Err("provide the original API and operation identities"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_facets_reject_guest_resources_and_other_installations() {
        let target = Target {
            platform: Platform::Windows,
            installation_id: Uuid::from_u128(42),
            machine_sha256: "a".repeat(64),
            configuration_root: r"C:\Firecrab".into(),
            directory_id: "1234:abcdef".into(),
        };
        let mut resource = ManagedResource {
            kind: "management_settings".into(),
            id: target.installation_id.to_string(),
        };
        validate_resource(&resource, &target).unwrap();
        resource.kind = "vm".into();
        assert!(validate_resource(&resource, &target).is_err());
        resource.kind = "host_configuration".into();
        resource.id = Uuid::from_u128(43).to_string();
        assert!(validate_resource(&resource, &target).is_err());
        let mut replaced = target.clone();
        replaced.directory_id.clear();
        assert!(replaced.validate().is_err());
        assert!(
            Envelope {
                version: VERSION + 1,
                request: Request::Identity
            }
            .validate()
            .is_err()
        );
        let guest = ManagedResource {
            kind: "vm".into(),
            id: Uuid::from_u128(42).to_string(),
        };
        assert!(
            Envelope {
                version: VERSION,
                request: Request::Observe {
                    endpoint_id: Uuid::from_u128(548),
                    resource: guest,
                }
            }
            .validate()
            .is_err()
        );
        assert!(
            serde_json::from_value::<Envelope>(serde_json::json!({
                "version":VERSION,"request":{"action":"identity"},"home":"/caller-selected-root"
            }))
            .is_err()
        );
    }
}
