//! Wire types shared between `firecrab-api` and `firecrab-frontend`'s
//! generated bindings: request/response bodies and the VM lifecycle state
//! machine.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The egress policies the API may request for a VM. New policies are added
/// here and mirrored in `firecrab-net-helper`'s own (deliberately separate)
/// `EgressPolicy`; the helper is the trust boundary and re-validates every
/// ID it receives rather than trusting this type directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressPolicy {
    /// Outbound to non-reserved destinations (the internet) is permitted.
    #[default]
    Internet,
    /// No outbound egress; only gateway-local services (DHCP/DNS) reach it.
    Isolated,
}

impl EgressPolicy {
    /// The wire ID carried in `NetworkRequest::ApplyVmPolicy.egress_policy`.
    pub fn id(self) -> &'static str {
        match self {
            EgressPolicy::Internet => "internet",
            EgressPolicy::Isolated => "isolated",
        }
    }
}

impl fmt::Display for EgressPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

/// Reject an unknown ID rather than silently defaulting, so a client typo
/// surfaces as a validation error instead of an unexpected network posture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownEgressPolicy(pub String);

impl FromStr for EgressPolicy {
    type Err = UnknownEgressPolicy;

    fn from_str(id: &str) -> Result<Self, Self::Err> {
        match id {
            "internet" => Ok(EgressPolicy::Internet),
            "isolated" => Ok(EgressPolicy::Isolated),
            other => Err(UnknownEgressPolicy(other.to_owned())),
        }
    }
}

/// A VM's lifecycle state, serialized lowercase over the API.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VmState {
    /// Record exists, no Firecracker process has ever run for it.
    Created,
    /// `start_vm`'s pipeline is running (see [`StartupStep`]).
    Starting,
    /// Firecracker process is up and the guest has booted.
    Running,
    /// Shutdown requested, process not yet confirmed gone.
    Stopping,
    /// Process exited cleanly.
    Stopped,
    /// Process exited unexpectedly or a start attempt failed.
    Error,
}

impl VmState {
    /// Whether the lifecycle table allows moving from `self` to `to`.
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

    /// Whether the VM record may be deleted — deletion is record removal,
    /// not a state transition, so only inactive VMs qualify.
    pub fn can_delete(self) -> bool {
        matches!(self, Self::Created | Self::Stopped | Self::Error)
    }

    /// Resource edits (cpu/ram/disk) only take effect on the *next* start, so
    /// they're only meaningful while no Firecracker process is live.
    pub fn can_edit_resources(self) -> bool {
        matches!(self, Self::Created | Self::Stopped | Self::Error)
    }
}

/// Body for `POST /api/vms`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreateVmRequest {
    /// 1–64 chars, alphanumeric plus `.`/`_`/`-`.
    pub name: String,
    /// Template registry alias (e.g. `ubuntu-26.04`), not a specific version.
    pub template: String,
    /// RAM in MiB; must be a power of two in the accepted range.
    pub ram: u32,
    /// vCPU count.
    pub cpu: u8,
    /// Disk capacity in GiB; rejected below the template rootfs's own size.
    pub disk_gb: u16,
    /// Outbound network posture; defaults to `Internet` so existing clients
    /// that don't send this field are unaffected.
    #[serde(default)]
    pub egress_policy: EgressPolicy,
}

/// Body for `PUT /api/vms/{id}`: replaces cpu/ram/disk for a VM that isn't
/// currently running. Takes effect on the next start, not live.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdateVmResourcesRequest {
    /// New RAM in MiB.
    pub ram: u32,
    /// New vCPU count.
    pub cpu: u8,
    /// New disk capacity in GiB; must be >= the VM's current size.
    pub disk_gb: u16,
    /// New outbound network posture; defaults to `Internet`.
    #[serde(default)]
    pub egress_policy: EgressPolicy,
}

/// A named phase of `start_vm`'s pipeline, exposed only while `state ==
/// Starting` so the dashboard can show *why* a VM hasn't reached `running`
/// yet instead of a bare spinner (`docs/task-vm-startup-progress.md`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum StartupStep {
    /// Copying/growing the template rootfs into the VM's own disk file.
    PreparingDisk,
    /// Writing the Firecracker `firecracker-config.json`.
    GeneratingConfig,
    /// Spawning the Firecracker process and waiting for it to come up.
    StartingProcess,
    /// Waiting for the guest to confirm (over its serial console) that
    /// DHCP and DNS actually came up, since there's no guest agent to ask
    /// directly (`docs/task-guest-network-configuration.md`).
    ConfiguringNetwork,
}

/// Outcome of the most recent `POST /api/vms/{id}/packages/update` run for
/// this VM — transient like `startup_step` (see
/// `docs/task-guest-network-configuration.md`'s sibling doc for the console-
/// sentinel pattern this reuses), not persisted across a restart.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "state"
)]
pub enum PackageUpdateStatus {
    /// The update command is running on the guest's console; no verdict yet.
    Running,
    /// The update command completed with an exit code of `0`.
    Succeeded {
        /// Tail of the command's console output, for a quick look without
        /// opening the terminal.
        output_tail: String,
    },
    /// The update command completed with a non-zero exit code, or the
    /// console closed/timed out before it could.
    Failed {
        /// Human-readable failure reason.
        reason: String,
        /// Tail of the command's console output, if any was captured.
        output_tail: String,
    },
}

/// A VM record as returned by the list/detail/create/update endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VmResponse {
    /// Stable identifier, also the `data/vms/<id>/` directory name.
    pub id: Uuid,
    /// User-supplied name.
    pub name: String,
    /// Current lifecycle state.
    pub state: VmState,
    /// Template alias this VM was created from.
    pub template: String,
    /// Pinned template version the alias resolved to at creation time.
    pub template_version: String,
    /// vCPU count.
    pub cpu: u8,
    /// RAM in MiB.
    pub ram: u32,
    /// Disk capacity in GiB.
    pub disk_gb: u16,
    /// `Some` only while `state == Starting`.
    pub startup_step: Option<StartupStep>,
    /// Outbound network posture.
    pub egress_policy: EgressPolicy,
    /// Status of the most recent (or in-progress) OS package update run,
    /// if one has ever been triggered for this VM.
    pub package_update: Option<PackageUpdateStatus>,
    /// Allocated IPv4 address, if this VM currently holds an active lease
    /// (see `Store::active_lease` — allocated at create, kept through
    /// stop/start, freed only on delete).
    pub ipv4: Option<String>,
    /// Allocated MAC address, alongside `ipv4`.
    pub mac: Option<String>,
    /// Deterministic guest hostname (`fc-<12 hex>`, see
    /// `firecrab_helper_protocol::network::guest_hostname`) — always
    /// present once the VM record exists, independent of lease state.
    pub hostname: String,
}

/// Response for `GET /api/network`: the host network firecrab has set up,
/// read-only for now (see `task-network-configuration-dashboard.md` — making
/// this genuinely editable needs a larger IPAM/bridge refactor).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NetworkInfoResponse {
    /// Name of the shared Linux bridge every VM's TAP attaches to.
    pub bridge_name: String,
    /// The Firecrab VPC subnet, as a CIDR string.
    pub subnet_cidr: String,
    /// The bridge's own address on the subnet (every VM's default gateway).
    pub gateway: String,
}

/// Response for `GET /api/host`: point-in-time host resource usage, for a
/// dashboard status panel.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostStatusResponse {
    /// 1-minute load average (`/proc/loadavg`'s first field).
    pub load_average_1m: f64,
    /// Total RAM, in MiB.
    pub memory_total_mib: u64,
    /// Currently available (not just free) RAM, in MiB.
    pub memory_available_mib: u64,
    /// Total capacity of the filesystem backing the VM data directory, in GiB.
    pub disk_total_gib: u64,
    /// Available capacity of that same filesystem, in GiB.
    pub disk_available_gib: u64,
    /// Seconds since the host booted (`/proc/uptime`'s first field).
    pub uptime_seconds: u64,
}

/// The VM's captured serial console output (see
/// `firecrab-api/src/firecracker.rs`'s `console.log` tee), capped so a long
/// boot doesn't turn this into an unbounded response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VmLogResponse {
    /// Captured serial console output, capped in size.
    pub console_log: String,
    /// `true` if the on-disk log exceeds the cap and `console_log` is only
    /// the first portion of it.
    pub truncated: bool,
}

/// JSON error body wrapper: `{"error": {...}}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ErrorResponse {
    /// The structured error payload.
    pub error: ApiError,
}

/// Structured API error: a machine-readable `code`, a human `message`, and
/// optional per-field validation detail.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    /// Machine-readable error code (e.g. `validation_error`).
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Field name → error message, for request validation failures.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, String>,
    /// Correlates this error with server-side logs.
    pub request_id: Uuid,
}

#[cfg(test)]
mod tests {
    use super::*;
    use VmState::{Created, Error, Running, Starting, Stopped, Stopping};

    const ALL_STATES: [VmState; 6] = [Created, Starting, Running, Stopping, Stopped, Error];

    #[test]
    fn egress_policy_id_round_trips_through_from_str() {
        for policy in [EgressPolicy::Internet, EgressPolicy::Isolated] {
            assert_eq!(policy.id().parse(), Ok(policy));
        }
    }

    #[test]
    fn egress_policy_unknown_ids_are_rejected_not_defaulted() {
        assert_eq!(
            "wide-open".parse::<EgressPolicy>(),
            Err(UnknownEgressPolicy("wide-open".to_owned()))
        );
        // A CIDR must never be accepted as a policy ID.
        assert!("0.0.0.0/0".parse::<EgressPolicy>().is_err());
    }

    #[test]
    fn egress_policy_default_is_internet() {
        assert_eq!(EgressPolicy::default(), EgressPolicy::Internet);
    }

    #[test]
    fn egress_policy_serializes_as_its_snake_case_id() {
        let json = serde_json::to_string(&EgressPolicy::Isolated).unwrap();
        assert_eq!(json, "\"isolated\"");
    }

    #[test]
    fn create_vm_request_defaults_egress_policy_to_internet_when_absent() {
        let json = r#"{"name":"vm","template":"ubuntu-rootfs-26.04","ram":512,"cpu":1,"diskGb":2}"#;
        let request: CreateVmRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.egress_policy, EgressPolicy::Internet);
    }

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
    fn create_vm_request_deserializes_camel_case_disk_gb() {
        let json = r#"{"name":"test-vm","template":"ubuntu-26.04","ram":512,"cpu":1,"diskGb":4}"#;
        let request: CreateVmRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.disk_gb, 4);
    }

    #[test]
    fn update_vm_resources_request_deserializes_camel_case_disk_gb() {
        let json = r#"{"ram":1024,"cpu":2,"diskGb":8}"#;
        let request: UpdateVmResourcesRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            request,
            UpdateVmResourcesRequest {
                ram: 1024,
                cpu: 2,
                disk_gb: 8,
                egress_policy: EgressPolicy::Internet,
            }
        );
    }

    #[test]
    fn only_inactive_states_allow_resource_edits() {
        for state in ALL_STATES {
            let expected = matches!(state, Created | Stopped | Error);
            assert_eq!(state.can_edit_resources(), expected, "{state:?}");
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
            disk_gb: 2,
            startup_step: None,
            egress_policy: EgressPolicy::Internet,
            package_update: Some(PackageUpdateStatus::Failed {
                reason: "exited with code 100".to_owned(),
                output_tail: "E: Unable to locate package\n".to_owned(),
            }),
            ipv4: Some("172.30.0.5".to_owned()),
            mac: Some("02:fc:00:00:00:05".to_owned()),
            hostname: "fc-abc123456789".to_owned(),
        };

        let json = serde_json::to_string(&response).expect("serialize response");
        assert_eq!(serde_json::from_str::<VmResponse>(&json).unwrap(), response);
    }

    #[test]
    fn package_update_status_is_internally_tagged_camel_case() {
        assert_eq!(
            serde_json::to_string(&PackageUpdateStatus::Running).unwrap(),
            "{\"state\":\"running\"}"
        );
        let succeeded = PackageUpdateStatus::Succeeded {
            output_tail: "done\n".to_owned(),
        };
        let json = serde_json::to_string(&succeeded).unwrap();
        assert_eq!(json, "{\"state\":\"succeeded\",\"outputTail\":\"done\\n\"}");
        assert_eq!(
            serde_json::from_str::<PackageUpdateStatus>(&json).unwrap(),
            succeeded
        );
    }

    #[test]
    fn startup_step_serializes_camel_case_and_is_absent_by_default() {
        for (step, json) in [
            (StartupStep::PreparingDisk, "\"preparingDisk\""),
            (StartupStep::GeneratingConfig, "\"generatingConfig\""),
            (StartupStep::StartingProcess, "\"startingProcess\""),
            (StartupStep::ConfiguringNetwork, "\"configuringNetwork\""),
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
            disk_gb: 2,
            startup_step: Some(StartupStep::PreparingDisk),
            egress_policy: EgressPolicy::Internet,
            package_update: None,
            ipv4: None,
            mac: None,
            hostname: "fc-abc123456789".to_owned(),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"startupStep\":\"preparingDisk\""));
    }

    #[test]
    fn vm_log_response_round_trips_camel_case() {
        let response = VmLogResponse {
            console_log: "booting...\n".to_owned(),
            truncated: true,
        };

        let json = serde_json::to_string(&response).expect("serialize response");
        assert!(json.contains("\"consoleLog\":\"booting...\\n\""));
        assert!(json.contains("\"truncated\":true"));
        assert_eq!(
            serde_json::from_str::<VmLogResponse>(&json).unwrap(),
            response
        );
    }

    #[test]
    fn host_status_response_serializes_camel_case() {
        let json = serde_json::to_string(&HostStatusResponse::default()).unwrap();
        assert_eq!(
            json,
            "{\"loadAverage1m\":0.0,\"memoryTotalMib\":0,\"memoryAvailableMib\":0,\
             \"diskTotalGib\":0,\"diskAvailableGib\":0,\"uptimeSeconds\":0}"
        );
    }

    #[test]
    fn network_info_response_serializes_camel_case() {
        let response = NetworkInfoResponse {
            bridge_name: "fcbr0".to_owned(),
            subnet_cidr: "172.30.0.0/24".to_owned(),
            gateway: "172.30.0.1".to_owned(),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(
            json,
            "{\"bridgeName\":\"fcbr0\",\"subnetCidr\":\"172.30.0.0/24\",\"gateway\":\"172.30.0.1\"}"
        );
    }
}
