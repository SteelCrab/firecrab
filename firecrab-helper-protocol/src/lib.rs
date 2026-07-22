//! Wire protocol shared between `firecrab-api` and the privileged
//! `firecrab-net-helper`/other helper daemons: framed request/response
//! envelopes carried over a Unix socket, versioned so mismatched builds
//! fail fast instead of misparsing each other's messages.

/// Length-prefixed frame read/write helpers shared by all helper protocols.
pub mod framing;
/// Network-helper-specific request/response payloads (bridge, TAP, firewall).
pub mod network;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Wire protocol version; bumped on any breaking envelope/request change.
pub const PROTOCOL_VERSION: u16 = 1;
/// Linux `IFNAMSIZ - 1`: the longest name a network interface can have.
pub const MAX_INTERFACE_NAME_LEN: usize = 15;

/// A request understood by the (currently unused) generic helper protocol;
/// see [`network::NetworkRequest`] for the network-helper's own requests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HelperRequest {
    /// Provision runtime state for a starting VM.
    PrepareRuntime {
        /// The VM this runtime belongs to.
        vm_id: Uuid,
        /// Identifies this particular runtime instance.
        runtime_id: Uuid,
    },
    /// Tear down runtime state for a stopped/deleted VM.
    RemoveRuntime {
        /// The VM this runtime belongs to.
        vm_id: Uuid,
        /// Identifies this particular runtime instance.
        runtime_id: Uuid,
    },
}

/// A request tagged with the protocol version the sender speaks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestEnvelope {
    /// Sender's [`PROTOCOL_VERSION`].
    pub version: u16,
    /// The actual request payload.
    pub request: HelperRequest,
}

/// Errors from validating a [`RequestEnvelope`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// The envelope's version doesn't match [`PROTOCOL_VERSION`].
    UnsupportedVersion(u16),
}

impl RequestEnvelope {
    /// Unwraps the request if `version` matches [`PROTOCOL_VERSION`].
    pub fn validate(self) -> Result<HelperRequest, ProtocolError> {
        if self.version != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion(self.version));
        }
        Ok(self.request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_protocol_version() {
        let envelope = RequestEnvelope {
            version: PROTOCOL_VERSION + 1,
            request: HelperRequest::PrepareRuntime {
                vm_id: Uuid::nil(),
                runtime_id: Uuid::nil(),
            },
        };

        assert_eq!(
            envelope.validate(),
            Err(ProtocolError::UnsupportedVersion(PROTOCOL_VERSION + 1))
        );
    }
}
