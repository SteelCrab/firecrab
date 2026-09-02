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

    /// Env may be replaced while `Running`; the guest service is restarted.
    pub fn can_edit_env(self) -> bool {
        matches!(
            self,
            Self::Created | Self::Stopped | Self::Error | Self::Running
        )
    }
}

/// Body for `POST /api/vms`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    /// MicroNetwork to place this VM in. Required — firecrab has no
    /// implicit default subnet; create a MicroNetwork first.
    pub micro_network_id: Uuid,
    /// Storage root id from `GET /api/storage` / `FIRECRAB_STORAGE_ROOTS`.
    /// Omitted (or null) uses the first registered root (the legacy
    /// `data/vms` layout when the env var is unset).
    #[serde(default)]
    pub storage_root: Option<String>,
    /// Shell repository ids to pin at create time. Each id resolves to its
    /// **latest** revision and is stored as an immutable pin until updated.
    #[serde(default)]
    pub shell_ids: Vec<Uuid>,
    /// Inbound port forwarding rules (DNAT) from host ports to guest ports.
    #[serde(default)]
    pub port_forwards: Vec<PortForward>,
    /// Per-VM environment applied on the next start. Omitted = `{}`.
    /// Values are stored and written into the guest in plaintext.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Network protocol for a port forward rule.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum PortProtocol {
    #[default]
    Tcp,
    Udp,
}

impl std::fmt::Display for PortProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tcp => write!(f, "tcp"),
            Self::Udp => write!(f, "udp"),
        }
    }
}

/// An inbound port forwarding rule (DNAT).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct PortForward {
    /// Host port (1–65535).
    pub host_port: u16,
    /// Guest port inside the VM (1–65535).
    pub guest_port: u16,
    /// Protocol (`tcp` or `udp`).
    #[serde(default)]
    pub protocol: PortProtocol,
}

/// Body for `PUT /api/vms/{id}/shells`: replace pinned shells (inactive VMs).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdateVmShellsRequest {
    /// Shell ids to pin (latest revision each). Empty clears all pins.
    pub shell_ids: Vec<Uuid>,
}

/// Body for `PUT /api/vms/{id}/port-forwards`: replace port forwarding rules.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdateVmPortForwardsRequest {
    /// Inbound port forwarding rules (DNAT).
    pub port_forwards: Vec<PortForward>,
}

/// One pinned shell revision on a VM (`public-docs` shell repository).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ShellRef {
    /// Catalog shell id.
    pub shell_id: Uuid,
    /// Immutable revision id pinned for this VM.
    pub revision_id: Uuid,
    /// Monotonic revision number within the shell (1, 2, …).
    pub version: u32,
    /// Shell display name at pin time (current catalog name).
    pub name: String,
}

/// A shell in the catalog (latest revision summary).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ShellResponse {
    pub id: Uuid,
    pub name: String,
    /// Optional operator note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Latest revision number, or 0 if somehow empty (should not happen).
    pub latest_version: u32,
    /// Latest revision id, if any revision exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_revision_id: Option<Uuid>,
    /// SHA-256 (hex) of the latest revision body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_sha256: Option<String>,
    /// Created at, Unix ms.
    pub created_at_ms: u64,
    /// Updated when a new revision is added, Unix ms.
    pub updated_at_ms: u64,
}

/// Full shell detail including revision history (bodies omitted except latest).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ShellDetailResponse {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    /// Newest first.
    pub revisions: Vec<ShellRevisionSummary>,
    /// Full body of the latest revision (for edit UI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_content: Option<String>,
}

/// One immutable shell revision (metadata; body only on create response).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ShellRevisionSummary {
    pub id: Uuid,
    pub version: u32,
    pub content_sha256: String,
    pub created_at_ms: u64,
    /// Byte length of the script body.
    pub size_bytes: u32,
}

/// Body for `POST /api/shells` — creates the shell and its first revision.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreateShellRequest {
    /// 1–64 safe name characters.
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Script body (`/bin/sh`). Size-capped by the API.
    pub content: String,
}

/// Body for `POST /api/shells/{id}/revisions` — appends an immutable revision.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreateShellRevisionRequest {
    /// New script body.
    pub content: String,
}

/// Response after creating a shell or a revision, and for
/// `GET /api/shells/{id}/revisions/{revision_id}` (includes full body).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ShellRevisionResponse {
    pub shell_id: Uuid,
    pub revision_id: Uuid,
    pub version: u32,
    pub content_sha256: String,
    pub content: String,
    pub created_at_ms: u64,
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
    /// Replacement environment map. Omitted (`None`) keeps the stored map;
    /// `Some({})` clears it.
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
}

/// A named phase of `start_vm`'s pipeline, exposed only while `state ==
/// Starting` so the dashboard can show *why* a VM hasn't reached `running`
/// yet instead of a bare spinner (`public-docs/api.md`).
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
    /// directly (`public-docs/networking.md`).
    ConfiguringNetwork,
}

/// How one [`StartupStep`] ended, or that it hasn't yet.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum StartupStepOutcome {
    /// Still in progress — `ended_at_ms` is `None`.
    Running,
    /// Finished and moved on to the next step.
    Succeeded,
    /// The start failed here. No later step ever began.
    Failed,
}

/// One pass through a [`StartupStep`], with the wall-clock times it spanned.
///
/// The dashboard polls every few seconds, which is coarser than the fastest
/// steps take — timing them client-side would round a 2-second disk copy up
/// to the poll interval or miss it entirely. So the server records when each
/// step opened and closed, and the client only formats it
/// (`public-docs/api.md`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StartupStepRun {
    /// Which step this was.
    pub step: StartupStep,
    /// When it began, in milliseconds since the Unix epoch.
    pub started_at_ms: u64,
    /// When it ended, or `None` while it is still running.
    pub ended_at_ms: Option<u64>,
    /// Whether it finished, failed, or is still going.
    pub outcome: StartupStepOutcome,
    /// Failure reason, only ever set on a `Failed` step.
    pub detail: Option<String>,
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
    /// Allocated IPv4 address, if this VM currently holds an active lease
    /// (see `Store::active_lease` — allocated at create, kept through
    /// stop/start, freed only on delete).
    pub ipv4: Option<String>,
    /// Allocated IPv6 address, when this VM's MicroNetwork is dual-stack.
    /// `null` for an IPv4-only network (`public-docs/networking.md`).
    pub ipv6: Option<String>,
    /// Allocated MAC address, alongside `ipv4`.
    pub mac: Option<String>,
    /// Deterministic guest hostname (`fc-<12 hex>`, see
    /// `firecrab_helper_protocol::network::guest_hostname`) — always
    /// present once the VM record exists, independent of lease state.
    pub hostname: String,
    /// Every startup step of the most recent start attempt, in order, with
    /// the times it spanned. Empty for a VM that has never been started, and
    /// kept after the start finishes so the timeline stays readable.
    pub startup_timeline: Vec<StartupStepRun>,
    /// MicroNetwork this VM belongs to. Fixed at creation — its lease comes
    /// out of that network's subnet (`public-docs/networking.md`).
    pub micro_network_id: Uuid,
    /// Storage root id this VM's disk lives under (`{root}/vms/{id}/`).
    /// Fixed at creation so a later config change cannot orphan the files.
    pub storage_root: String,
    /// Guest OS CPU busy percent from the Firecrab Guest Agent
    /// (`FIRECRAB_USAGE cpu_pct=`). `None` when the agent has not reported yet
    /// or the VM is not running. Not host Firecracker process CPU.
    pub cpu_usage_percent: Option<f32>,
    /// Guest used memory in MiB (`MemTotal − MemAvailable`). `None` until the
    /// guest agent reports.
    pub memory_used_mib: Option<u64>,
    /// Guest total memory in MiB (`MemTotal`). `None` until the agent reports.
    pub memory_total_mib: Option<u64>,
    /// Guest used memory as a percent of MemTotal. `None` until reported.
    pub memory_used_percent: Option<f32>,
    /// Recent guest-agent samples for sparklines (oldest first, bounded).
    /// Empty when the VM is not running or has never been sampled.
    pub usage_history: Vec<VmUsageSample>,
    /// Shell revisions pinned on this VM (injected on each start).
    #[serde(default)]
    pub shell_refs: Vec<ShellRef>,
    /// Inbound port forwarding rules (DNAT) from host ports to guest ports.
    #[serde(default)]
    pub port_forwards: Vec<PortForward>,
    /// Per-VM environment. Empty is valid. Stored and applied in plaintext.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// SHA256 fingerprint of the guest SSH host key. `null` until first start
    /// has generated the per-VM host key (`public-docs/oci.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_host_fingerprint: Option<String>,
}

/// `GET /api/vms/{id}/ssh-host-key` — guest host key after first start.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SshHostKeyResponse {
    /// `SHA256:…` fingerprint (`ssh-keygen -lf`).
    pub fingerprint: String,
    /// OpenSSH public key line.
    pub public_key: String,
}

/// What a live host-key check concluded.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SshHostKeyCheckStatus {
    /// The guest presented the key Firecrab injected.
    Match,
    /// The guest answered with a different key.
    Mismatch,
    /// Port 22 did not answer, so nothing was compared.
    Unreachable,
    /// The VM has no address yet, so nothing was scanned.
    NoAddress,
    /// No host key on disk yet — the VM has never started.
    NoHostKey,
}

/// `GET /api/vms/{id}/ssh-host-key/check` — what the guest answers with now.
///
/// Runs on the Firecrab host, so it replaces the operator pasting
/// `ssh-keyscan | ssh-keygen -lf` output back into the dashboard.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SshHostKeyCheckResponse {
    /// Outcome the dashboard renders.
    pub status: SshHostKeyCheckStatus,
    /// Address scanned. `null` when the VM has no address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Fingerprint Firecrab injected into the guest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    /// Fingerprint the guest presented. `null` unless the scan answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<String>,
    /// Why the scan could not answer, for the unreachable case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One guest-agent usage sample for dashboard graphs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VmUsageSample {
    /// Unix epoch milliseconds when the sample was taken.
    pub at_ms: u64,
    /// Guest CPU busy percent for the interval ending at this sample.
    pub cpu_usage_percent: Option<f32>,
    /// Guest used memory in MiB at this sample.
    pub memory_used_mib: Option<u64>,
    /// Guest total memory in MiB at this sample.
    pub memory_total_mib: Option<u64>,
    /// Guest used memory percent of MemTotal at this sample.
    pub memory_used_percent: Option<f32>,
}

/// One selectable place a VM disk may be created — env root, default, or a
/// MicroStorage (`public-docs/storage.md`). Clients
/// pick by `id` only; paths are never free-form on create.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StorageRootResponse {
    /// Stable id used in `CreateVmRequest.storage_root` / assign.
    pub id: String,
    /// Operator-facing label (MicroStorage name, or the id for env/default).
    pub name: String,
    /// Registered mount path.
    pub path: String,
    /// Free capacity on that filesystem, in GiB (0 if unreadable).
    pub available_gib: u64,
    /// Total capacity of that filesystem, in GiB (0 if unreadable).
    pub total_gib: u64,
    /// Where this root came from: `default`, `env`, or `micro_storage`.
    pub kind: String,
}

/// A MicroStorage — a named host path that VMs can put disks on
/// (EBS analogue; 4주차 MicroStorage).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MicroStorageResponse {
    /// Stable identifier (also used as `storageRoot` on VMs).
    pub id: Uuid,
    /// User-supplied name.
    pub name: String,
    /// Absolute host path registered for this pool.
    pub path: String,
    /// Free capacity in GiB (0 if unreadable).
    pub available_gib: u64,
    /// Total capacity in GiB (0 if unreadable).
    pub total_gib: u64,
}

/// Body for `POST /api/micro-storages`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreateMicroStorageRequest {
    /// 1–64 chars, same convention as VM names.
    pub name: String,
    /// Absolute host directory path (created if missing).
    pub path: String,
}

/// Detail for `GET /api/micro-storages/{id}`: the pool plus VMs using it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MicroStorageDetailResponse {
    /// Stable identifier.
    pub id: Uuid,
    /// User-supplied name.
    pub name: String,
    /// Absolute host path.
    pub path: String,
    /// Free capacity in GiB.
    pub available_gib: u64,
    /// Total capacity in GiB.
    pub total_gib: u64,
    /// VMs whose `storageRoot` points at this pool.
    pub vms: Vec<MicroStorageVm>,
}

/// A VM listed under a MicroStorage detail response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MicroStorageVm {
    /// VM id.
    pub id: Uuid,
    /// VM name.
    pub name: String,
    /// Lifecycle state.
    pub state: VmState,
    /// Disk capacity in GiB.
    pub disk_gb: u16,
}

/// Body for `PUT /api/vms/{id}/storage` — reassign a VM to another storage
/// root before (or after) its disk exists, with guards in the handler.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AssignVmStorageRequest {
    /// Target storage root id (`default`, env id, or MicroStorage UUID).
    pub storage_root: String,
}

/// A mounted host partition/filesystem that can become a MicroStorage path
/// (`GET /api/storage/devices`). firecrab never creates or formats partitions
/// — it only discovers already-mounted paths the operator can register.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StorageDeviceResponse {
    /// Kernel device name when known (e.g. `nvme0n1p1`), else empty.
    pub device: String,
    /// Absolute mount path (the value used as MicroStorage `path`).
    pub mountpoint: String,
    /// Filesystem type (`ext4`, `xfs`, …) when known.
    pub fstype: String,
    /// Total size in GiB (0 if unknown).
    pub size_gib: u64,
    /// Free space in GiB (0 if unreadable).
    pub available_gib: u64,
    /// `part`, `disk`, or `other` when reported by lsblk; else empty.
    pub kind: String,
}

/// How guests in a dual-stack MicroNetwork obtain their IPv6 address.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Ipv6AddressMode {
    /// Router advertisements only — the guest derives its address from its
    /// own MAC (EUI-64), which is the address the API stored for it.
    #[default]
    Slaac,
    /// Stateful DHCPv6, from a per-VM reservation like the IPv4 one.
    Dhcpv6,
}

impl Ipv6AddressMode {
    /// The wire ID, shared with `firecrab-helper-protocol`'s own enum.
    pub fn id(self) -> &'static str {
        match self {
            Ipv6AddressMode::Slaac => "slaac",
            Ipv6AddressMode::Dhcpv6 => "dhcpv6",
        }
    }
}

impl fmt::Display for Ipv6AddressMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

/// How a MicroNetwork's IPv6 traffic leaves the host. Reported rather than
/// chosen: it follows from the prefix's scope, so a network given a global
/// prefix cannot be silently masqueraded, and one on Unique Local space
/// cannot be left unroutable.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Ipv6EgressMode {
    /// Unique Local prefix: masqueraded out of the host's uplink.
    Nat66,
    /// Global prefix: forwarded untranslated, so VMs hold public addresses.
    Direct,
}

impl fmt::Display for Ipv6EgressMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Ipv6EgressMode::Nat66 => "nat66",
            Ipv6EgressMode::Direct => "direct",
        })
    }
}

/// Request body for `POST /api/micro-networks`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMicroNetworkRequest {
    /// 1–64 chars, alphanumeric plus `.`/`_`/`-` (same convention as VM names).
    pub name: String,
    /// The network's own CIDR block (e.g. `172.31.0.0/24`) — just the
    /// reserved address range, mirroring how an AWS VPC is created with a
    /// CIDR block before any subnet/route table/gateway exists.
    pub subnet_cidr: String,
    /// Whether its VMs may reach the internet. Omitted means `true`, so a
    /// client written before the toggle existed still gets the connected
    /// network it expects.
    #[serde(default = "internet_enabled_default")]
    pub internet_enabled: bool,
    /// Host NIC this network should masquerade out of. Omitted or `null`
    /// means the host default-route iface. An empty string is rejected by
    /// the API (400), not stored as auto.
    #[serde(default)]
    pub uplink: Option<String>,
    /// The network's IPv6 prefix, alongside — never instead of —
    /// `subnet_cidr`. Giving one turns IPv6 on for the network: a Unique
    /// Local `/64` egresses through NAT66, a global one is forwarded
    /// untranslated (`public-docs/networking.md`). Omitted with
    /// `ipv6_address_mode` set, the API generates a per-host ULA `/64`.
    #[serde(default)]
    pub ipv6_cidr: Option<String>,
    /// How guests in it get a v6 address. Omitted alongside an
    /// `ipv6_cidr` means SLAAC; omitted with no `ipv6_cidr` either means
    /// the network is IPv4-only, which is what a request that says nothing
    /// about IPv6 gets.
    #[serde(default)]
    pub ipv6_address_mode: Option<Ipv6AddressMode>,
}

/// Body for `PATCH /api/micro-networks/{id}`: flips one network's internet
/// access and optionally its host uplink. CIDR stays immutable — its VMs'
/// addresses were handed out of it.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdateMicroNetworkRequest {
    /// The new posture: `false` withholds NAT and drops anything this
    /// network's VMs try to send outside it.
    pub internet_enabled: bool,
    /// Omitted leaves the stored uplink unchanged. A name sets it. An
    /// empty string resets to auto (the host default-route iface).
    #[serde(default)]
    pub uplink: Option<String>,
}

/// Serde default for the `internet_enabled` fields: connected, which is what
/// every MicroNetwork was before the toggle existed.
fn internet_enabled_default() -> bool {
    true
}

/// A MicroNetwork — one of firecrab's own virtual networks
/// (`public-docs/networking.md`). A named CIDR reservation backed by a real
/// host bridge; routing-table separation and VM membership are follow-up work.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MicroNetworkResponse {
    /// Stable identifier.
    pub id: Uuid,
    /// User-supplied name.
    pub name: String,
    /// The network's reserved CIDR block.
    pub subnet_cidr: String,
    /// Its gateway — the first host address of `subnet_cidr`, which is also
    /// the address its bridge holds on the host. Derived, never stored.
    pub gateway: String,
    /// Whether its VMs may reach anything outside Firecrab. `false` is a
    /// closed network: no NAT, and nothing routed out of it
    /// (`public-docs/networking.md`).
    pub internet_enabled: bool,
    /// Stored host NIC for NAT, or `null` to use the host default route.
    pub uplink: Option<String>,
    /// The network's IPv6 prefix, or `null` for a network created before
    /// dual-stack existed (those stay IPv4-only until recreated).
    pub ipv6_cidr: Option<String>,
    /// Its IPv6 gateway — the first address of `ipv6_cidr`, held by the
    /// bridge. Derived, never stored, exactly like `gateway`.
    pub ipv6_gateway: Option<String>,
    /// How its guests obtain a v6 address, alongside `ipv6_cidr`.
    pub ipv6_address_mode: Option<Ipv6AddressMode>,
    /// How its v6 traffic leaves the host, derived from the prefix's scope.
    pub ipv6_egress: Option<Ipv6EgressMode>,
}

/// Response for `GET /api/network`: the host network firecrab has set up,
/// read-only for now (see `public-docs/networking.md` — making
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
    /// The host's outbound interface, resolved from its IPv4 default route.
    pub uplink: String,
    /// Host interfaces the create/detail picker can offer (`lo`, `fct*`,
    /// and `mnb*` are omitted). `uplink` remains the default-route iface.
    pub interfaces: Vec<String>,
}

/// Response for `GET /api/micro-networks/{id}`: one network broken out into
/// the services it is actually made of, so the dashboard can show what a
/// MicroNetwork gives a VM rather than just its name and CIDR
/// (`public-docs/networking.md`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MicroNetworkDetailResponse {
    /// Stable identifier — the id every host resource below is derived from.
    pub id: Uuid,
    /// User-supplied name.
    pub name: String,
    /// Address plan and how much of it is in use.
    pub subnet: MicroNetworkSubnet,
    /// The Linux bridge this network's VMs attach to.
    pub bridge: MicroNetworkBridge,
    /// Outbound address translation for this network's subnet.
    pub nat: MicroNetworkNat,
    /// The isolation rules that apply to traffic in this network.
    pub firewall: MicroNetworkFirewall,
    /// Every VM currently placed in this network.
    pub vms: Vec<MicroNetworkVm>,
}

/// The address plan of a [`MicroNetworkDetailResponse`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MicroNetworkSubnet {
    /// Reserved CIDR block.
    pub cidr: String,
    /// First host address, held by the bridge and handed to guests as their
    /// default gateway.
    pub gateway: String,
    /// How many addresses can be handed out (network/gateway/broadcast are
    /// reserved and never counted).
    pub usable_addresses: u32,
    /// How many of those are currently leased.
    pub allocated_addresses: u32,
    /// Where guests get their address from.
    pub dhcp: String,
    /// The network's IPv6 prefix, or `null` when it is IPv4-only.
    pub ipv6_cidr: Option<String>,
    /// The bridge's own address in that prefix, handed to guests as their
    /// v6 router.
    pub ipv6_gateway: Option<String>,
    /// How guests in it obtain a v6 address.
    pub ipv6_address_mode: Option<Ipv6AddressMode>,
    /// How its v6 traffic leaves the host (NAT66 or direct), from the
    /// prefix's scope.
    pub ipv6_egress: Option<Ipv6EgressMode>,
}

/// The host bridge backing a MicroNetwork.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MicroNetworkBridge {
    /// Interface name, derived from the network id (never user-supplied).
    pub name: String,
    /// How many VM TAPs are expected on it — i.e. running VMs in this
    /// network. A stopped VM keeps its address but has no TAP.
    pub attached_taps: u32,
}

/// Outbound NAT for a MicroNetwork.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MicroNetworkNat {
    /// Whether this network's subnet is masqueraded out of the host.
    pub enabled: bool,
    /// The host interface it egresses through.
    pub uplink: String,
    /// Masquerade source range.
    pub source_cidr: String,
    /// Masquerade source prefix for IPv6, or `null` when the network is
    /// IPv4-only or holds a global prefix that is never translated.
    pub ipv6_source_cidr: Option<String>,
}

/// The isolation posture applied to a MicroNetwork's traffic. These are
/// properties of the rendered ruleset, not per-network toggles — they are
/// reported so the dashboard can state what is enforced instead of implying
/// a network is unprotected just because nothing is shown.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MicroNetworkFirewall {
    /// Whether VMs inside this network are prevented from reaching each
    /// other. Firecrab permits this traffic, so this is currently false.
    pub east_west_blocked: bool,
    /// Traffic routed to any other Firecrab network is dropped.
    pub cross_network_blocked: bool,
    /// A VM may only send from its own leased IP/MAC.
    pub anti_spoofing: bool,
    /// Outbound posture is decided per VM (see [`MicroNetworkVm`]), not per
    /// network — this names the default a new VM gets.
    pub default_egress: EgressPolicy,
}

/// One VM's placement in a MicroNetwork.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MicroNetworkVm {
    /// The VM's id.
    pub id: Uuid,
    /// Its name.
    pub name: String,
    /// Its lifecycle state.
    pub state: VmState,
    /// Its address in this network, if it currently holds a lease.
    pub ipv4: Option<String>,
    /// Its IPv6 address in this network, when the network is dual-stack.
    pub ipv6: Option<String>,
    /// Its own outbound posture.
    pub egress_policy: EgressPolicy,
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

/// `GET /api/update`, and the `--json` output of `firecrab update --check`.
/// One type for both so the CLI's report and the API's answer cannot drift.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckResponse {
    /// Version of the build that answered (`CARGO_PKG_VERSION`).
    pub current: String,
    /// Newest release's `tag_name` with any leading `v` stripped. `None` when
    /// the check could not reach GitHub or the tag did not parse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<String>,
    /// True only when `latest` parsed and is strictly newer than `current`.
    pub update_available: bool,
    /// One-line reason there is no `latest` (unreachable, rate limited,
    /// unparsable tag). `None` on a successful check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `POST /api/update`: the detached updater was launched, nothing more.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStartResponse {
    /// Version this host is running at the moment the updater was launched.
    pub current: String,
    /// PID of the spawned `firecrab update --apply`, for journal correlation.
    pub pid: u32,
}

/// One entry from `GET /api/images` — a template registry alias the create
/// form can offer, with digests and a disk floor. Host paths are never
/// exposed (`public-docs/images.md`).
///
/// Uninstalled built-in templates still appear so the dashboard can offer
/// **official package links** (`package_url`) and kick off
/// `POST /api/images/{alias}/install`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImageResponse {
    /// Stable user-facing alias (`ubuntu-26.04`, …) accepted by create.
    pub alias: String,
    /// Pinned version tag the alias currently resolves to (or will after install).
    pub version: String,
    /// Upstream kernel release when the image uses a managed kernel. Distro
    /// kernels built into an M2Image may not expose a standalone release tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_version: Option<String>,
    /// Kernel filename (not a host path), useful when the release is distro
    /// supplied or otherwise has no managed catalog version.
    #[serde(default)]
    pub kernel_image: String,
    /// SHA256 of the kernel artifact (hex). Empty when not installed yet.
    pub kernel_sha256: String,
    /// SHA256 of the rootfs artifact (hex). Empty when not installed yet.
    pub rootfs_sha256: String,
    /// SHA256 of the initrd, when this template needs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initrd_sha256: Option<String>,
    /// Smallest disk size (GiB) that can hold this template's rootfs (0 if unknown).
    pub min_disk_gb: u16,
    /// On-disk rootfs image length in bytes (0 when not installed / unknown).
    /// This is the real artifact size, not the ceiled `min_disk_gb` floor.
    #[serde(default)]
    pub rootfs_size_bytes: u64,
    /// Whether the artifacts are present and verified on this host.
    pub installed: bool,
    /// Package download URL for this alias when `FIRECRAB_IMAGE_BASE_URL` is
    /// set (`{base}/{alias}.tar.zst`). `None` when remote install is off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_url: Option<String>,
    /// Whether a package archive for this alias is already staged locally
    /// (`image_install::staged_package_exists`), so
    /// `POST /api/images/{alias}/install` can extract it with no download at
    /// all. Deliberately independent of `package_url`: a web bootstrap
    /// (`handlers::bootstrap`) stages one on a host that has no
    /// `FIRECRAB_IMAGE_BASE_URL` configured, and the dashboard has to be
    /// able to offer that install without a remote URL to point at.
    #[serde(default)]
    pub package_staged: bool,
    /// How the staged package was created. Absent for legacy packages whose
    /// provenance predates origin tracking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_origin: Option<PackageOrigin>,
    /// Short operator-facing note (may be empty).
    pub description: String,
    /// Whether the installed rootfs has `/etc/firecrab/services.d/app`.
    /// Uninstalled and catalog-only rows are `false`.
    #[serde(default)]
    pub has_guest_service: bool,
}

/// A kernel release known to this Firecracker build, plus its local cache
/// state. Kernels are managed independently from M2Image rootfs artifacts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct KernelResponse {
    /// Upstream Linux release, for example `7.2.2`.
    pub version: String,
    /// Firecracker architecture label (`x86_64` or `aarch64`).
    pub architecture: String,
    /// Kernel filename inside the verified package.
    pub image: String,
    /// SHA256 of the unpacked kernel image.
    pub image_sha256: String,
    /// SHA256 of the compressed MicroRegistry package.
    pub package_sha256: String,
    /// Local unpacked image size when installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// Whether a verified copy is present in the local kernel cache.
    pub installed: bool,
    /// Whether an installed image currently references this kernel.
    pub in_use: bool,
    /// Remote package URL when kernel downloads are enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_url: Option<String>,
}

/// Request body for `PUT /api/images/{alias}/kernel`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateImageKernelRequest {
    /// Managed kernel release to pair with the image.
    pub kernel_version: String,
}

/// Status + log for `GET/POST /api/kernels/{version}/install`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct KernelInstallResponse {
    /// Kernel release targeted by this job.
    pub version: String,
    /// Current job state.
    pub status: ImageInstallStatus,
    /// Multi-line acquisition and verification log.
    pub log: String,
    /// Epoch millis when the attempt started, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    /// Epoch millis when the attempt ended, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<u64>,
    /// Bytes downloaded from the package source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloaded_bytes: Option<u64>,
    /// Total package bytes advertised by the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
}

/// Producer of a package in the host's local `.packages` cache.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PackageOrigin {
    MicroRegistry,
    MicroBoot,
}

/// One verified M2Image package advertised by Firecrab MicroRegistry.
///
/// This is deliberately separate from [`ImageResponse`]. The image endpoint
/// describes templates available to the VM create form; this type describes
/// immutable packages published in the remote registry and the matching state
/// on the current host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MicroRegistryImageResponse {
    /// Stable package alias, such as `ubuntu-26.04`.
    pub alias: String,
    /// Monotonically increasing publisher version for this alias.
    pub version: String,
    /// Registry-relative package object key.
    pub package: String,
    /// SHA256 of the compressed package archive.
    pub sha256: String,
    /// Smallest disk (GiB) that can contain the published rootfs.
    pub min_disk_gb: u16,
    /// RFC 3339 publication timestamp supplied by the registry.
    pub published_at: String,
    /// The matching template is already registered on this host.
    pub installed: bool,
    /// The package is verified and waiting in this host's local cache.
    pub package_staged: bool,
    /// Producer of the staged package, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_origin: Option<PackageOrigin>,
    /// Firecrab knows how to install this alias from a package archive.
    pub downloadable: bool,
}

/// Response for `GET /api/microregistry`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MicroRegistryResponse {
    /// Public catalog URL used for this response.
    pub source: String,
    /// Published packages, sorted by alias.
    pub images: Vec<MicroRegistryImageResponse>,
}

/// Lifecycle of an image install job (`POST/GET /api/images/{alias}/install`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ImageInstallStatus {
    /// No install has been requested for this alias in this process.
    Idle,
    /// Download / verify / register is in progress.
    Running,
    /// Template is registered and available for create.
    Succeeded,
    /// Last attempt failed; see `log` for details. Can retry.
    Failed,
}

/// Status + log for `GET /api/images/{alias}/install` (and the POST response).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImageInstallResponse {
    /// Template alias this job targets.
    pub alias: String,
    /// Current job status.
    pub status: ImageInstallStatus,
    /// Multi-line progress log (download · verify · register steps).
    pub log: String,
    /// Epoch millis when the current/last attempt started, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    /// Epoch millis when the attempt finished, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<u64>,
    /// Bytes streamed into the package download cache. Omitted for image
    /// extraction jobs and before a package server has answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloaded_bytes: Option<u64>,
    /// Total package bytes advertised by the package server, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
}

/// What `GET /api/oci/inspect` resolved a reference to on this host.
///
/// Metadata only: the response names the manifest this host would pull and
/// the alias `POST /api/oci/import` will claim. It does not start an import.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OciInspectResponse {
    /// Registry host the reference resolved to.
    pub registry: String,
    /// Repository path, with Docker Hub's implicit `library/` filled in.
    pub repository: String,
    /// The tag or digest that was resolved.
    pub version: String,
    /// Whether that version can never be repointed at other content.
    pub immutable: bool,
    /// Digest of the manifest this host would pull.
    pub digest: String,
    /// The architecture that manifest runs, as a catalog label.
    pub architecture: String,
    /// True when the registry answered with a manifest rather than an index,
    /// so no per-platform selection took place.
    pub single_platform: bool,
    /// Template alias `POST /api/oci/import` will claim for this reference.
    pub alias: String,
}

/// Request body for `POST /api/oci/import`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OciImportRequest {
    /// An image reference as typed at a `docker pull`, e.g. `nginx:1.27`.
    pub reference: String,
}

/// Stored Docker Hub login used by OCI inspect/import. The secret is never
/// included in a response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DockerHubCredentialResponse {
    /// A username and access token are stored on this host.
    pub configured: bool,
    /// Docker Hub username. Omitted when nothing is stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

/// Body for `PUT /api/microregistry/docker-hub`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DockerHubCredentialRequest {
    /// Docker Hub username.
    pub username: String,
    /// Account password or personal access token. Write-only.
    pub secret: String,
}

/// Request body for `POST /api/microregistry/register`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MicroRegistryRegisterRequest {
    /// Installed template alias to publish into this host's catalog.
    pub alias: String,
    /// Operator-supplied catalog version for this registration.
    pub version: String,
}

/// Lifecycle of one from-scratch distro bootstrap session (`handlers::bootstrap`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BootstrapStatus {
    /// Builder VM is being created/started.
    Booting,
    /// The bootstrap script (download + chroot install + mkfs) is running
    /// on the builder VM's console.
    Running,
    /// VM stopped; extracting rootfs/kernel/initrd from its disk and
    /// packaging them into `{alias}.tar.zst`.
    Packaging,
    /// Package written to the local install cache; builder VM deleted.
    Succeeded,
    /// Failed at any stage; see `log`. Builder VM has been deleted.
    Failed,
}

/// A named phase of one bootstrap session, exposed so the dashboard can
/// show *where* a multi-minute run is instead of a single opaque status.
/// Deliberately coarser than the code's own phase boundaries — four boxes
/// that mean something to an operator, mirroring [`StartupStep`]'s four —
/// with the fine-grained detail of the longest one left to the live
/// console instead of more enum variants
/// (`public-docs/images.md`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BootstrapStep {
    /// Resolving the MicroBoot builder source, creating the builder VM,
    /// and waiting for a shell to answer on its console.
    StartingBuilderVm,
    /// The guest script is running: download, chroot install, mkfs. By far
    /// the longest phase, and the one the live console is for.
    InstallingSystem,
    /// Builder VM stopped; its disk is being dumped and compressed into
    /// `{alias}.tar.zst`.
    Packaging,
    /// Package staged; tearing the builder VM down.
    Finalizing,
}

/// How one [`BootstrapStep`] ended, or that it hasn't yet. Structurally
/// identical to [`StartupStepOutcome`] but kept separate, matching how
/// `BootstrapStatus` and `VmState` are separate types rather than one
/// shared lifecycle enum.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BootstrapStepOutcome {
    /// Still in progress — `ended_at_ms` is `None`.
    Running,
    /// Finished and moved on to the next step.
    Succeeded,
    /// The session failed here. No later step ever began.
    Failed,
}

/// One pass through a [`BootstrapStep`], with the wall-clock times it
/// spanned. Server-timed for the same reason [`StartupStepRun`] is: the
/// dashboard's poll interval is far coarser than the fastest steps take.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapStepRun {
    pub step: BootstrapStep,
    pub started_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    pub outcome: BootstrapStepOutcome,
    /// Failure reason, only ever set on a `Failed` step.
    pub detail: Option<String>,
}

/// Status + log for one bootstrap session
/// (`POST /api/images/{alias}/bootstrap`, `GET`/`DELETE /api/images/bootstrap/{bootstrapId}`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapResponse {
    pub bootstrap_id: Uuid,
    /// The target being bootstrapped (`alpine-3.24.1`, `ubuntu-26.04`, or `rocky-9.8`).
    pub alias: String,
    /// Which template the disposable builder VM booted from — always the
    /// internal MicroBoot alias since `firecrab_api::microboot` replaced
    /// the old "pick an already-installed template" logic. Deliberately
    /// still reported: it is diagnostic provenance for this one session,
    /// not an installable image, and a bootstrap that failed early is much
    /// harder to reason about without knowing what it booted. Unlike
    /// `/api/images`, nothing here invites the reader to install it.
    pub source_alias: String,
    /// Builder VM id, so the dashboard can reuse the existing console
    /// WebSocket (`/ws/vms/{id}/console`) to show live output.
    pub vm_id: Uuid,
    pub status: BootstrapStatus,
    /// The step currently open, `None` once the session is terminal.
    #[serde(default)]
    pub current_step: Option<BootstrapStep>,
    /// Every step this session has entered, in order. `#[serde(default)]`
    /// so a dashboard talking to an older server still deserializes.
    #[serde(default)]
    pub step_timeline: Vec<BootstrapStepRun>,
    pub log: String,
    pub started_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<u64>,
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
    fn egress_policy_displays_as_its_id() {
        assert_eq!(EgressPolicy::Internet.to_string(), "internet");
        assert_eq!(EgressPolicy::Isolated.to_string(), "isolated");
    }

    #[test]
    fn create_vm_request_defaults_egress_policy_to_internet_when_absent() {
        let json = r#"{"name":"vm","template":"ubuntu-rootfs-26.04","ram":512,"cpu":1,"diskGb":2,"microNetworkId":"00000000-0000-0000-0000-000000000001"}"#;
        let request: CreateVmRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.egress_policy, EgressPolicy::Internet);
        assert!(request.shell_ids.is_empty());
        assert!(request.env.is_empty());

        let serialized = serde_json::to_value(&request).unwrap();
        assert_eq!(serialized["diskGb"], 2);
        assert_eq!(serialized["egressPolicy"], "internet");
        assert_eq!(
            serialized["microNetworkId"],
            "00000000-0000-0000-0000-000000000001"
        );
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
        let json = r#"{"name":"test-vm","template":"ubuntu-26.04","ram":512,"cpu":1,"diskGb":4,"microNetworkId":"00000000-0000-0000-0000-000000000001"}"#;
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
                env: None,
            }
        );
    }

    #[test]
    fn update_vm_resources_request_omitted_env_deserializes_to_none() {
        let json = r#"{"ram":1024,"cpu":2,"diskGb":8}"#;
        let request: UpdateVmResourcesRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.env, None);
    }

    #[test]
    fn create_and_update_env_deserialize_camel_case_object() {
        let create: CreateVmRequest = serde_json::from_str(
            r#"{"name":"vm","template":"ubuntu-26.04","ram":512,"cpu":1,"diskGb":2,"microNetworkId":"00000000-0000-0000-0000-000000000001","env":{"B":"2","A":"1"}}"#,
        )
        .unwrap();
        assert_eq!(
            create.env.into_iter().collect::<Vec<_>>(),
            vec![
                ("A".to_owned(), "1".to_owned()),
                ("B".to_owned(), "2".to_owned())
            ]
        );

        let update: UpdateVmResourcesRequest =
            serde_json::from_str(r#"{"ram":512,"cpu":1,"diskGb":2,"env":{"APP_NAME":"web"}}"#)
                .unwrap();
        assert_eq!(
            update
                .env
                .as_ref()
                .and_then(|env| env.get("APP_NAME"))
                .map(String::as_str),
            Some("web")
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
    fn running_may_edit_env_but_not_cpu_ram_or_disk() {
        use VmState::*;
        assert!(Running.can_edit_env());
        assert!(!Running.can_edit_resources());
        assert!(!Starting.can_edit_env());
        assert!(!Stopping.can_edit_env());
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
            startup_timeline: Vec::new(),
            egress_policy: EgressPolicy::Internet,
            ipv4: Some("172.30.0.5".to_owned()),
            ipv6: None,
            mac: Some("02:fc:00:00:00:05".to_owned()),
            hostname: "fc-abc123456789".to_owned(),
            micro_network_id: Uuid::nil(),
            storage_root: "default".to_owned(),
            cpu_usage_percent: Some(12.5),
            memory_used_mib: Some(180),
            memory_total_mib: Some(512),
            memory_used_percent: Some(35.2),
            usage_history: vec![VmUsageSample {
                at_ms: 1_700_000_000_000,
                cpu_usage_percent: Some(12.5),
                memory_used_mib: Some(180),
                memory_total_mib: Some(512),
                memory_used_percent: Some(35.2),
            }],
            shell_refs: Vec::new(),
            port_forwards: Vec::new(),
            env: BTreeMap::from([("FOO".to_owned(), "bar".to_owned())]),
            ssh_host_fingerprint: Some("SHA256:test".to_owned()),
        };

        let json = serde_json::to_string(&response).expect("serialize response");
        assert_eq!(serde_json::from_str::<VmResponse>(&json).unwrap(), response);
        assert!(json.contains("\"env\":{\"FOO\":\"bar\"}"));
        assert!(json.contains("\"cpuUsagePercent\":12.5"));
        assert!(json.contains("\"memoryUsedMib\":180"));
        assert!(json.contains("\"memoryTotalMib\":512"));
        assert!(json.contains("\"memoryUsedPercent\":35.2"));
        assert!(json.contains("\"usageHistory\""));
        assert!(!json.contains("packageUpdate"));
    }

    /// The dashboard switches on these exact strings
    /// (`firecrab-frontend/src/bindings/SshHostKeyCheckStatus.ts`).
    #[test]
    fn ssh_host_key_check_status_serializes_camel_case() {
        for (status, json) in [
            (SshHostKeyCheckStatus::Match, "\"match\""),
            (SshHostKeyCheckStatus::Mismatch, "\"mismatch\""),
            (SshHostKeyCheckStatus::Unreachable, "\"unreachable\""),
            (SshHostKeyCheckStatus::NoAddress, "\"noAddress\""),
            (SshHostKeyCheckStatus::NoHostKey, "\"noHostKey\""),
        ] {
            assert_eq!(serde_json::to_string(&status).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<SshHostKeyCheckStatus>(json).unwrap(),
                status
            );
        }
    }

    /// Absent fields must not reach the client as `null` keys.
    #[test]
    fn ssh_host_key_check_omits_what_the_scan_never_learned() {
        let check = SshHostKeyCheckResponse {
            status: SshHostKeyCheckStatus::NoAddress,
            address: None,
            expected: Some("SHA256:test".to_owned()),
            observed: None,
            detail: None,
        };
        let json = serde_json::to_string(&check).expect("serialize check");
        assert_eq!(
            serde_json::from_str::<SshHostKeyCheckResponse>(&json).unwrap(),
            check
        );
        assert!(json.contains("\"status\":\"noAddress\""), "{json}");
        assert!(json.contains("\"expected\":\"SHA256:test\""), "{json}");
        assert!(!json.contains("observed"), "{json}");
        assert!(!json.contains("detail"), "{json}");
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
            startup_timeline: Vec::new(),
            egress_policy: EgressPolicy::Internet,
            ipv4: None,
            ipv6: None,
            mac: None,
            hostname: "fc-abc123456789".to_owned(),
            micro_network_id: Uuid::nil(),
            storage_root: "default".to_owned(),
            cpu_usage_percent: None,
            memory_used_mib: None,
            memory_total_mib: None,
            memory_used_percent: None,
            usage_history: Vec::new(),
            shell_refs: Vec::new(),
            port_forwards: Vec::new(),
            env: BTreeMap::new(),
            ssh_host_fingerprint: None,
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"env\":{}"));
        assert!(json.contains("\"startupStep\":\"preparingDisk\""));
        assert!(json.contains("\"cpuUsagePercent\":null"));
        assert!(json.contains("\"memoryUsedMib\":null"));
        assert!(json.contains("\"memoryTotalMib\":null"));
        assert!(json.contains("\"memoryUsedPercent\":null"));
        assert!(json.contains("\"usageHistory\":[]"));
    }

    #[test]
    fn image_response_serializes_camel_case_without_host_paths() {
        let image = ImageResponse {
            alias: "ubuntu-26.04".to_owned(),
            version: "ubuntu-26.04-v2".to_owned(),
            kernel_version: Some("7.2.2".to_owned()),
            kernel_image: "vmlinux-7.2.2-x86_64".to_owned(),
            kernel_sha256: "k".repeat(64),
            rootfs_sha256: "r".repeat(64),
            initrd_sha256: None,
            min_disk_gb: 2,
            rootfs_size_bytes: 2 * 1024 * 1024 * 1024u64,
            installed: true,
            package_url: Some("http://127.0.0.1:8765/ubuntu-26.04.tar.zst".to_owned()),
            package_staged: true,
            package_origin: Some(PackageOrigin::MicroRegistry),
            description: String::new(),
            has_guest_service: true,
        };
        let json = serde_json::to_value(&image).unwrap();
        assert_eq!(json["alias"], "ubuntu-26.04");
        assert_eq!(json["hasGuestService"], true);
        assert_eq!(json["packageStaged"], true);
        assert_eq!(json["packageOrigin"], "microRegistry");
        assert_eq!(json["minDiskGb"], 2);
        assert_eq!(json["rootfsSizeBytes"], 2 * 1024 * 1024 * 1024u64);
        assert_eq!(json["kernelSha256"], "k".repeat(64));
        assert_eq!(json["kernelVersion"], "7.2.2");
        assert_eq!(json["kernelImage"], "vmlinux-7.2.2-x86_64");
        assert_eq!(json["installed"], true);
        assert_eq!(
            json["packageUrl"],
            "http://127.0.0.1:8765/ubuntu-26.04.tar.zst"
        );
        assert!(json.get("initrdSha256").is_none());
        let omitted = serde_json::from_value::<ImageResponse>(serde_json::json!({
            "alias": "ubuntu-26.04",
            "version": "v1",
            "kernelImage": "vmlinux",
            "kernelSha256": "k",
            "rootfsSha256": "r",
            "minDiskGb": 2,
            "installed": false,
            "description": ""
        }))
        .unwrap();
        assert!(!omitted.has_guest_service);
    }

    #[test]
    fn kernel_management_types_keep_the_wire_names_and_status() {
        let kernel = KernelResponse {
            version: "7.2.2".to_owned(),
            architecture: "x86_64".to_owned(),
            image: "vmlinux-7.2.2-x86_64".to_owned(),
            image_sha256: "a".repeat(64),
            package_sha256: "b".repeat(64),
            size_bytes: Some(52 * 1024 * 1024),
            installed: true,
            in_use: false,
            package_url: Some(
                "https://registry.example/kernel/7.2.2/x86_64/vmlinux-7.2.2.tar.zst".to_owned(),
            ),
        };
        let json = serde_json::to_value(&kernel).unwrap();
        assert_eq!(json["imageSha256"], "a".repeat(64));
        assert_eq!(json["packageSha256"], "b".repeat(64));
        assert_eq!(json["sizeBytes"], 52 * 1024 * 1024);
        assert_eq!(json["inUse"], false);

        let request: UpdateImageKernelRequest = serde_json::from_value(serde_json::json!({
            "kernelVersion": "7.2.2"
        }))
        .unwrap();
        assert_eq!(request.kernel_version, "7.2.2");

        let job = KernelInstallResponse {
            version: "7.2.2".to_owned(),
            status: ImageInstallStatus::Succeeded,
            log: "kernel ready".to_owned(),
            started_at_ms: Some(1),
            ended_at_ms: Some(2),
            downloaded_bytes: None,
            total_bytes: None,
        };
        let json = serde_json::to_value(job).unwrap();
        assert_eq!(json["status"], "succeeded");
        assert_eq!(json["startedAtMs"], 1);
        assert!(json.get("downloadedBytes").is_none());
    }

    #[test]
    fn oci_inspect_response_serializes_camel_case() {
        let response = OciInspectResponse {
            registry: "registry-1.docker.io".to_owned(),
            repository: "library/nginx".to_owned(),
            version: "1.27".to_owned(),
            immutable: false,
            digest: format!("sha256:{}", "a".repeat(64)),
            architecture: "x86_64".to_owned(),
            single_platform: false,
            alias: "nginx-1.27".to_owned(),
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["singlePlatform"], false);
        assert_eq!(json["alias"], "nginx-1.27");
        assert_eq!(json["immutable"], false);
        assert_eq!(
            serde_json::from_value::<OciInspectResponse>(json).unwrap(),
            response
        );
    }

    #[test]
    fn oci_import_request_round_trips_camel_case() {
        let request = OciImportRequest {
            reference: "nginx:1.27".to_owned(),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(json, r#"{"reference":"nginx:1.27"}"#);
        assert_eq!(
            serde_json::from_str::<OciImportRequest>(&json).unwrap(),
            request
        );
    }

    #[test]
    fn docker_hub_credential_response_never_serializes_the_secret() {
        let configured = DockerHubCredentialResponse {
            configured: true,
            username: Some("pista".to_owned()),
        };
        let json = serde_json::to_value(&configured).unwrap();
        assert_eq!(json["configured"], true);
        assert_eq!(json["username"], "pista");
        assert!(json.get("secret").is_none());

        let empty = DockerHubCredentialResponse {
            configured: false,
            username: None,
        };
        let json = serde_json::to_value(&empty).unwrap();
        assert_eq!(json["configured"], false);
        assert!(json.get("username").is_none());
        assert!(json.get("secret").is_none());
    }

    #[test]
    fn docker_hub_credential_request_round_trips_camel_case() {
        let request = DockerHubCredentialRequest {
            username: "pista".to_owned(),
            secret: "dckr_pat_example".to_owned(),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(json, r#"{"username":"pista","secret":"dckr_pat_example"}"#);
        assert_eq!(
            serde_json::from_str::<DockerHubCredentialRequest>(&json).unwrap(),
            request
        );
    }

    #[test]
    fn microregistry_register_request_round_trips_camel_case() {
        let request = MicroRegistryRegisterRequest {
            alias: "nginx-1.27".to_owned(),
            version: "1".to_owned(),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(json, r#"{"alias":"nginx-1.27","version":"1"}"#);
        assert_eq!(
            serde_json::from_str::<MicroRegistryRegisterRequest>(&json).unwrap(),
            request
        );
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
            uplink: "eth0".to_owned(),
            interfaces: vec!["eth0".to_owned(), "eth1".to_owned()],
        };
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(
            json,
            "{\"bridgeName\":\"fcbr0\",\"subnetCidr\":\"172.30.0.0/24\",\"gateway\":\"172.30.0.1\",\"uplink\":\"eth0\",\"interfaces\":[\"eth0\",\"eth1\"]}"
        );
    }

    #[test]
    fn create_micro_network_request_deserializes_camel_case() {
        let json = r#"{"name":"prod","subnetCidr":"172.31.0.0/24"}"#;
        let request: CreateMicroNetworkRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.name, "prod");
        assert_eq!(request.subnet_cidr, "172.31.0.0/24");
        assert_eq!(request.uplink, None);

        let serialized = serde_json::to_value(&request).unwrap();
        assert_eq!(serialized["subnetCidr"], "172.31.0.0/24");
        assert_eq!(serialized["internetEnabled"], true);
    }

    #[test]
    fn create_micro_network_request_deserializes_an_uplink() {
        let json = r#"{"name":"prod","subnetCidr":"172.31.0.0/24","uplink":"eth1"}"#;
        let request: CreateMicroNetworkRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.uplink.as_deref(), Some("eth1"));
    }

    #[test]
    fn update_micro_network_request_omitted_uplink_is_unchanged() {
        let request: UpdateMicroNetworkRequest =
            serde_json::from_str(r#"{"internetEnabled":false}"#).unwrap();
        assert!(!request.internet_enabled);
        assert_eq!(request.uplink, None);
    }

    #[test]
    fn update_micro_network_request_empty_uplink_is_reset_auto() {
        let request: UpdateMicroNetworkRequest =
            serde_json::from_str(r#"{"internetEnabled":true,"uplink":""}"#).unwrap();
        assert_eq!(request.uplink.as_deref(), Some(""));
    }

    #[test]
    fn a_dual_stack_vm_reports_both_of_its_addresses() {
        let json = serde_json::to_string(&MicroNetworkVm {
            id: Uuid::nil(),
            name: "web".to_owned(),
            state: VmState::Running,
            ipv4: Some("172.31.0.5".to_owned()),
            ipv6: Some("fd00:1::5".to_owned()),
            egress_policy: EgressPolicy::Internet,
        })
        .unwrap();
        assert!(json.contains("\"ipv6\":\"fd00:1::5\""));
    }

    #[test]
    fn a_dual_stack_subnet_panel_reports_its_prefix_and_egress_mode() {
        let subnet = MicroNetworkSubnet {
            cidr: "172.31.0.0/24".to_owned(),
            gateway: "172.31.0.1".to_owned(),
            usable_addresses: 253,
            allocated_addresses: 1,
            dhcp: "dnsmasq".to_owned(),
            ipv6_cidr: Some("2001:db8:1::/64".to_owned()),
            ipv6_gateway: Some("2001:db8:1::1".to_owned()),
            ipv6_address_mode: Some(Ipv6AddressMode::Slaac),
            ipv6_egress: Some(Ipv6EgressMode::Direct),
        };
        let json = serde_json::to_string(&subnet).unwrap();
        assert!(json.contains("\"ipv6Cidr\":\"2001:db8:1::/64\""));
        assert!(json.contains("\"ipv6Egress\":\"direct\""));
    }

    #[test]
    fn create_micro_network_request_defaults_to_ipv4_only() {
        // No v6 fields: IPv4-only, matching a client that never heard of
        // dual-stack and a dashboard that left the IPv6 select off.
        let json = r#"{"name":"prod","subnetCidr":"172.31.0.0/24"}"#;
        let request: CreateMicroNetworkRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.ipv6_cidr, None);
        assert_eq!(request.ipv6_address_mode, None);
    }

    #[test]
    fn create_micro_network_request_deserializes_an_explicit_ipv6_plan() {
        let json = r#"{"name":"prod","subnetCidr":"172.31.0.0/24",
            "ipv6Cidr":"2001:db8:1::/64","ipv6AddressMode":"dhcpv6"}"#;
        let request: CreateMicroNetworkRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.ipv6_cidr.as_deref(), Some("2001:db8:1::/64"));
        assert_eq!(request.ipv6_address_mode, Some(Ipv6AddressMode::Dhcpv6));
    }

    #[test]
    fn ipv6_address_modes_use_their_wire_names() {
        assert_eq!(
            serde_json::to_string(&Ipv6AddressMode::Dhcpv6).unwrap(),
            "\"dhcpv6\""
        );
        assert_eq!(Ipv6AddressMode::Slaac.id(), "slaac");
        assert_eq!(Ipv6AddressMode::default(), Ipv6AddressMode::Slaac);
    }

    #[test]
    fn ipv6_egress_modes_use_their_wire_names() {
        assert_eq!(
            serde_json::to_string(&Ipv6EgressMode::Nat66).unwrap(),
            "\"nat66\""
        );
        assert_eq!(
            serde_json::to_string(&Ipv6EgressMode::Direct).unwrap(),
            "\"direct\""
        );
    }

    #[test]
    fn a_dual_stack_micro_network_response_carries_its_v6_plan() {
        let response = MicroNetworkResponse {
            id: Uuid::from_u128(0x1234),
            name: "prod".to_owned(),
            subnet_cidr: "172.31.0.0/24".to_owned(),
            gateway: "172.31.0.1".to_owned(),
            internet_enabled: true,
            uplink: None,
            ipv6_cidr: Some("fd00:1::/64".to_owned()),
            ipv6_gateway: Some("fd00:1::1".to_owned()),
            ipv6_address_mode: Some(Ipv6AddressMode::Slaac),
            ipv6_egress: Some(Ipv6EgressMode::Nat66),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"ipv6Cidr\":\"fd00:1::/64\""));
        assert!(json.contains("\"ipv6Gateway\":\"fd00:1::1\""));
        assert!(json.contains("\"ipv6AddressMode\":\"slaac\""));
        assert!(json.contains("\"ipv6Egress\":\"nat66\""));
        assert_eq!(
            serde_json::from_str::<MicroNetworkResponse>(&json).unwrap(),
            response
        );
    }

    #[test]
    fn micro_network_response_round_trips() {
        let response = MicroNetworkResponse {
            id: Uuid::from_u128(0x1234),
            name: "prod".to_owned(),
            subnet_cidr: "172.31.0.0/24".to_owned(),
            gateway: "172.31.0.1".to_owned(),
            internet_enabled: true,
            uplink: None,
            ipv6_cidr: None,
            ipv6_gateway: None,
            ipv6_address_mode: None,
            ipv6_egress: None,
        };
        let json = serde_json::to_string(&response).unwrap();
        let decoded: MicroNetworkResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, response);
        assert!(json.contains("\"subnetCidr\":\"172.31.0.0/24\""));
        assert!(json.contains("\"uplink\":null"));
    }

    #[test]
    fn bootstrap_step_run_serializes_camel_case_for_the_dashboard() {
        let run = BootstrapStepRun {
            step: BootstrapStep::InstallingSystem,
            started_at_ms: 1_700_000_000_000,
            ended_at_ms: None,
            outcome: BootstrapStepOutcome::Running,
            detail: None,
        };
        let json = serde_json::to_value(&run).expect("serialize");
        assert_eq!(json["step"], "installingSystem");
        assert_eq!(json["startedAtMs"], 1_700_000_000_000_u64);
        assert_eq!(json["endedAtMs"], serde_json::Value::Null);
        assert_eq!(json["outcome"], "running");
    }

    #[test]
    fn bootstrap_response_carries_an_empty_timeline_by_default() {
        let json = serde_json::json!({
            "bootstrapId": "00000000-0000-0000-0000-000000000000",
            "alias": "alpine-3.24.1",
            "sourceAlias": "__microboot",
            "vmId": "00000000-0000-0000-0000-000000000000",
            "status": "booting",
            "log": "",
            "startedAtMs": 0,
        });
        let parsed: BootstrapResponse = serde_json::from_value(json).expect("deserialize");
        assert!(parsed.step_timeline.is_empty());
        assert_eq!(parsed.current_step, None);
    }

    #[test]
    fn update_check_response_serializes_camel_case() {
        let response = UpdateCheckResponse {
            current: "0.1.1".to_owned(),
            latest: Some("0.1.2".to_owned()),
            update_available: true,
            error: None,
        };
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(
            json,
            "{\"current\":\"0.1.1\",\"latest\":\"0.1.2\",\"updateAvailable\":true}"
        );
        assert_eq!(
            serde_json::from_str::<UpdateCheckResponse>(&json).unwrap(),
            response
        );
    }

    #[test]
    fn update_check_response_reports_a_failed_check_without_a_latest() {
        let response = UpdateCheckResponse {
            current: "0.1.1".to_owned(),
            latest: None,
            update_available: false,
            error: Some("unreachable: connection refused".to_owned()),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(
            !json.contains("\"latest\""),
            "absent latest must be omitted: {json}"
        );
        assert!(json.contains("\"error\":\"unreachable: connection refused\""));
        assert_eq!(
            serde_json::from_str::<UpdateCheckResponse>(&json).unwrap(),
            response
        );
    }

    #[test]
    fn update_start_response_serializes_camel_case() {
        let response = UpdateStartResponse {
            current: "0.1.1".to_owned(),
            pid: 4242,
        };
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(json, "{\"current\":\"0.1.1\",\"pid\":4242}");
        assert_eq!(
            serde_json::from_str::<UpdateStartResponse>(&json).unwrap(),
            response
        );
    }
}
