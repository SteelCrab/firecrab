use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VmState {
    Created,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CreateVmRequest {
    pub name: String,
    pub template: String,
    pub ram: u32,
    pub cpu: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VmResponse {
    pub id: Uuid,
    pub name: String,
    pub state: VmState,
    pub template: String,
    pub cpu: f64,
    pub ram: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_response_round_trips() {
        let response = VmResponse {
            id: Uuid::nil(),
            name: "test-vm".to_owned(),
            state: VmState::Created,
            template: "ubuntu-rootfs-26.04".to_owned(),
            cpu: 1.0,
            ram: 512,
        };

        let json = serde_json::to_string(&response).expect("serialize response");
        assert_eq!(serde_json::from_str::<VmResponse>(&json).unwrap(), response);
    }
}
