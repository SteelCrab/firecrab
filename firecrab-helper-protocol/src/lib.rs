pub mod framing;
pub mod network;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_INTERFACE_NAME_LEN: usize = 15;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HelperRequest {
    PrepareRuntime { vm_id: Uuid, runtime_id: Uuid },
    RemoveRuntime { vm_id: Uuid, runtime_id: Uuid },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestEnvelope {
    pub version: u16,
    pub request: HelperRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    UnsupportedVersion(u16),
}

impl RequestEnvelope {
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
