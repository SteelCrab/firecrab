use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VmState {
    Created,
    Starting,
    Running,
    Stopping,
    Stopped,
    Error,
}

impl VmState {
    pub fn can_transition(self, to: Self) -> bool {
        use VmState::{Created, Error, Running, Starting, Stopped, Stopping};
        matches!(
            (self, to),
            (Created, Starting)
                | (Starting, Running | Error)
                | (Running, Stopping | Stopped | Error)
                | (Stopping, Stopped | Error)
                | (Stopped, Starting)
                | (Error, Starting)
        )
    }

    // Deletion is record removal, not a state transition; only inactive VMs qualify.
    pub fn can_delete(self) -> bool {
        matches!(self, Self::Created | Self::Stopped | Self::Error)
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CreateVmRequest {
    pub name: String,
    pub template: String,
    pub ram: u32,
    pub cpu: u8,
}

/// A named phase of `start_vm`'s pipeline, exposed only while `state ==
/// Starting` so the dashboard can show *why* a VM hasn't reached `running`
/// yet instead of a bare spinner (`docs/task-vm-startup-progress.md`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum StartupStep {
    PreparingDisk,
    GeneratingConfig,
    StartingProcess,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VmResponse {
    pub id: Uuid,
    pub name: String,
    pub state: VmState,
    pub template: String,
    pub template_version: String,
    pub cpu: u8,
    pub ram: u32,
    /// `Some` only while `state == Starting`.
    pub startup_step: Option<StartupStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ErrorResponse {
    pub error: ApiError,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, String>,
    pub request_id: Uuid,
}

#[cfg(test)]
mod tests {
    use super::*;
    use VmState::{Created, Error, Running, Starting, Stopped, Stopping};

    const ALL_STATES: [VmState; 6] = [Created, Starting, Running, Stopping, Stopped, Error];

    #[test]
    fn transitions_follow_the_lifecycle_table() {
        let allowed = [
            (Created, Starting),
            (Starting, Running),
            (Starting, Error),
            (Running, Stopping),
            (Running, Stopped),
            (Running, Error),
            (Stopping, Stopped),
            (Stopping, Error),
            (Stopped, Starting),
            (Error, Starting),
        ];

        for from in ALL_STATES {
            for to in ALL_STATES {
                assert_eq!(
                    from.can_transition(to),
                    allowed.contains(&(from, to)),
                    "{from:?} -> {to:?}"
                );
            }
        }
    }

    #[test]
    fn deletion_is_allowed_only_for_inactive_states() {
        for state in ALL_STATES {
            assert_eq!(
                state.can_delete(),
                [Created, Stopped, Error].contains(&state),
                "{state:?}"
            );
        }
    }

    #[test]
    fn vm_states_serialize_lowercase() {
        for (state, json) in [
            (Created, "\"created\""),
            (Starting, "\"starting\""),
            (Running, "\"running\""),
            (Stopping, "\"stopping\""),
            (Stopped, "\"stopped\""),
            (Error, "\"error\""),
        ] {
            assert_eq!(serde_json::to_string(&state).unwrap(), json);
            assert_eq!(serde_json::from_str::<VmState>(json).unwrap(), state);
        }
    }

    #[test]
    fn vm_response_round_trips() {
        let response = VmResponse {
            id: Uuid::nil(),
            name: "test-vm".to_owned(),
            state: VmState::Created,
            template: "ubuntu-rootfs-26.04".to_owned(),
            template_version: "ubuntu-26.04-v1".to_owned(),
            cpu: 1,
            ram: 512,
            startup_step: None,
        };

        let json = serde_json::to_string(&response).expect("serialize response");
        assert_eq!(serde_json::from_str::<VmResponse>(&json).unwrap(), response);
    }

    #[test]
    fn startup_step_serializes_camel_case_and_is_absent_by_default() {
        for (step, json) in [
            (StartupStep::PreparingDisk, "\"preparingDisk\""),
            (StartupStep::GeneratingConfig, "\"generatingConfig\""),
            (StartupStep::StartingProcess, "\"startingProcess\""),
        ] {
            assert_eq!(serde_json::to_string(&step).unwrap(), json);
            assert_eq!(serde_json::from_str::<StartupStep>(json).unwrap(), step);
        }

        let response = VmResponse {
            id: Uuid::nil(),
            name: "test-vm".to_owned(),
            state: VmState::Starting,
            template: "ubuntu-rootfs-26.04".to_owned(),
            template_version: "ubuntu-26.04-v1".to_owned(),
            cpu: 1,
            ram: 512,
            startup_step: Some(StartupStep::PreparingDisk),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"startupStep\":\"preparingDisk\""));
    }
}
