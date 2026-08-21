//! MicroNetwork CRUD (`public-docs/networking.md`) — a named CIDR
//! reservation that also provisions a real bridge on the host and is wired
//! into the network services VMs need: its own dnsmasq range, its own NAT
//! rule, and a default deny on traffic routed to any other network. VRF
//! (routing-table separation, so isolation can't depend on a rule being
//! present) is still follow-up work.

use std::collections::{BTreeMap, HashMap};

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use firecrab_api_types::{
    CreateMicroNetworkRequest, EgressPolicy, Ipv6AddressMode, Ipv6EgressMode, MicroNetworkBridge,
    MicroNetworkDetailResponse, MicroNetworkFirewall, MicroNetworkNat, MicroNetworkResponse,
    MicroNetworkSubnet, MicroNetworkVm, UpdateMicroNetworkRequest, VmState,
};
use firecrab_helper_protocol::network::{
    Ipv6AddressMode as ProtocolIpv6AddressMode, MicroNetworkIpv6Spec, micro_network_bridge_name,
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::handlers::vms::parse_id;
use crate::ipam::{SubnetSpec, ipv6_egress_mode};
use crate::persistence::PersistenceError;
use crate::server::RequestId;
use crate::state::AppState;

/// Smallest/largest accepted subnet, in CIDR prefix-length terms — the
/// same bounds AWS documents for a VPC's own CIDR block. The helper
/// re-validates its own (wider, 8-30) sanity bound independently; this is
/// the user-facing business rule.
const MIN_PREFIX: u8 = 16;
const MAX_PREFIX: u8 = 28;

pub async fn list_micro_networks(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
) -> Result<Json<Vec<MicroNetworkResponse>>, AppError> {
    let store = state.store.clone();
    let networks = tokio::task::spawn_blocking(move || store.list_micro_networks())
        .await
        .map_err(|_| AppError::internal(request_id.0))?
        .map_err(|error| {
            tracing::error!(request_id = %request_id.0, %error, "failed to list micro networks");
            AppError::internal(request_id.0)
        })?;
    Ok(Json(networks))
}

/// `GET /api/micro-networks/{id}`: one network broken out into the services
/// it is made of. Everything here is derived — the bridge name from the id,
/// the address plan from the CIDR, the NAT source from the subnet — so this
/// reports what is actually installed rather than a second copy of it that
/// could drift.
pub async fn get_micro_network(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<MicroNetworkDetailResponse>, AppError> {
    let id = parse_id(&id, request_id.0)?;

    let store = state.store.clone();
    let (network, vms, leases) = tokio::task::spawn_blocking(move || {
        let network = store.micro_network(id)?;
        let vms = store.load_all()?;
        let leases = store.active_leases().unwrap_or_default();
        Ok::<_, PersistenceError>((network, vms, leases))
    })
    .await
    .map_err(|_| AppError::internal(request_id.0))?
    .map_err(|error| {
        tracing::error!(request_id = %request_id.0, %error, "failed to load micro network detail");
        AppError::internal(request_id.0)
    })?;

    let network = network.ok_or_else(|| AppError::not_found(request_id.0))?;
    let subnet = SubnetSpec::parse(id, &network.subnet_cidr).ok_or_else(|| {
        tracing::error!(
            request_id = %request_id.0,
            micro_network_id = %id,
            subnet_cidr = network.subnet_cidr,
            "stored micro network subnet does not parse"
        );
        AppError::internal(request_id.0)
    })?;

    let addresses: HashMap<Uuid, (String, Option<String>)> = leases
        .into_iter()
        .map(|lease| {
            (
                lease.vm_id,
                (
                    lease.ipv4.to_string(),
                    lease.ipv6.map(|ipv6| ipv6.to_string()),
                ),
            )
        })
        .collect();
    let mut members: Vec<MicroNetworkVm> = vms
        .into_values()
        .filter(|vm| vm.micro_network_id == id)
        .map(|vm| {
            let lease = addresses.get(&vm.id);
            MicroNetworkVm {
                ipv4: lease.map(|(ipv4, _)| ipv4.clone()),
                ipv6: lease.and_then(|(_, ipv6)| ipv6.clone()),
                id: vm.id,
                name: vm.name,
                state: vm.state,
                egress_policy: vm.egress_policy,
            }
        })
        .collect();
    members.sort_by(|left, right| left.name.cmp(&right.name));

    // A stopped VM keeps its address but has no TAP, so "running" is what
    // says how many ports the bridge really has right now.
    let attached_taps = members
        .iter()
        .filter(|vm| vm.state == VmState::Running)
        .count() as u32;

    Ok(Json(MicroNetworkDetailResponse {
        id,
        name: network.name,
        subnet: MicroNetworkSubnet {
            cidr: network.subnet_cidr.clone(),
            gateway: subnet.gateway().to_string(),
            usable_addresses: subnet.usable_addresses(),
            allocated_addresses: members.iter().filter(|vm| vm.ipv4.is_some()).count() as u32,
            dhcp: format!("dnsmasq on {}", micro_network_bridge_name(id)),
            ipv6_cidr: network.ipv6_cidr.clone(),
            ipv6_gateway: network.ipv6_gateway.clone(),
            ipv6_address_mode: network.ipv6_address_mode,
            ipv6_egress: network.ipv6_egress,
        },
        bridge: MicroNetworkBridge {
            name: micro_network_bridge_name(id),
            attached_taps,
        },
        nat: MicroNetworkNat {
            // Masquerading out of the chosen (or default-route) uplink is
            // what having the internet switched on means; off withholds both
            // the NAT rule and the forward permission.
            enabled: network.internet_enabled,
            uplink: network
                .uplink
                .clone()
                .or_else(crate::handlers::network::read_uplink)
                .unwrap_or_default(),
            source_cidr: network.subnet_cidr,
            // Only a ULA prefix is ever translated; a global one reaches the
            // wire with the VM's own address, so it has no NAT source range.
            ipv6_source_cidr: network
                .ipv6_cidr
                .clone()
                .filter(|_| network.ipv6_egress == Some(Ipv6EgressMode::Nat66)),
        },
        firewall: MicroNetworkFirewall {
            east_west_blocked: false,
            cross_network_blocked: true,
            anti_spoofing: true,
            default_egress: EgressPolicy::default(),
        },
        vms: members,
    }))
}

pub async fn create_micro_network(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    ValidatedJson(req): ValidatedJson<CreateMicroNetworkRequest>,
) -> Result<(StatusCode, Json<MicroNetworkResponse>), AppError> {
    let mut fields = validate_create(&req);
    if !fields.is_empty() {
        return Err(AppError::validation(fields, request_id.0));
    }

    // Already checked by validate_create; re-parsed here (rather than
    // threaded through) since the request is consumed piecemeal below.
    let id = Uuid::new_v4();
    // The v6 prefix is resolved before the overlap check so an auto-generated
    // one is held to the same rule as an explicitly requested one.
    let ipv6 = resolve_ipv6_plan(&req, id);
    let subnet = SubnetSpec::parse(id, &req.subnet_cidr)
        .expect("validate_create already accepted this CIDR")
        .with_ipv6(ipv6);
    if let Some((field, conflict)) = overlapping_network(&state, subnet, request_id.0).await? {
        fields.insert(
            field,
            format!("overlaps {conflict}, which is already in use"),
        );
        return Err(AppError::validation(fields, request_id.0));
    }
    let gateway = subnet.gateway();

    let network = MicroNetworkResponse {
        id,
        name: req.name,
        subnet_cidr: req.subnet_cidr,
        gateway: gateway.to_string(),
        internet_enabled: req.internet_enabled,
        uplink: req.uplink,
        ipv6_cidr: ipv6.map(|ipv6| ipv6.subnet_cidr()),
        ipv6_gateway: ipv6.map(|ipv6| ipv6.gateway.to_string()),
        ipv6_address_mode: ipv6.map(|_| req.ipv6_address_mode.unwrap_or_default()),
        ipv6_egress: ipv6.as_ref().map(ipv6_egress_mode),
    };

    let store = state.store.clone();
    let record = network.clone();
    tokio::task::spawn_blocking(move || store.insert_micro_network(&record))
        .await
        .map_err(|_| AppError::internal(request_id.0))?
        .map_err(|error| {
            tracing::error!(request_id = %request_id.0, %error, "failed to persist micro network");
            AppError::internal(request_id.0)
        })?;

    let prefix = subnet.prefix;

    // Provisioned after persisting (same order as create_vm's lease
    // allocation) so a failure here rolls back the just-inserted row rather
    // than leaving a DB record with no real bridge behind it.
    if let Err(error) = state
        .network
        .ensure_micro_network_bridge(network.id, gateway, prefix, ipv6)
        .await
    {
        tracing::error!(request_id = %request_id.0, micro_network_id = %network.id, %error, "failed to provision micro network bridge");
        let store = state.store.clone();
        let _ = tokio::task::spawn_blocking(move || store.delete_micro_network(network.id)).await;
        return Err(AppError::internal(request_id.0));
    }
    // The bridge alone carries no traffic: without a dnsmasq range a VM on
    // it never gets an address, and without a NAT rule it never reaches the
    // uplink. Both are rendered from the full network set, so they have to
    // be re-pushed now rather than waiting for the next VM start.
    apply_network_services(&state, request_id.0).await;

    Ok((StatusCode::CREATED, Json(network)))
}

/// `PATCH /api/micro-networks/{id}`: switches this network's internet access
/// and/or stored uplink. The network, its bridge, its addresses and its DHCP
/// keep working either way; only what may leave it changes.
pub async fn update_micro_network(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(req): ValidatedJson<UpdateMicroNetworkRequest>,
) -> Result<Json<MicroNetworkResponse>, AppError> {
    let id = parse_id(&id, request_id.0)?;

    let store = state.store.clone();
    let network = tokio::task::spawn_blocking(move || store.micro_network(id))
        .await
        .map_err(|_| AppError::internal(request_id.0))?
        .map_err(|error| {
            tracing::error!(request_id = %request_id.0, %error, "failed to load micro network");
            AppError::internal(request_id.0)
        })?;
    let mut network = network.ok_or_else(|| AppError::not_found(request_id.0))?;

    let fields = validate_update_micro_network(&req);
    if !fields.is_empty() {
        return Err(AppError::validation(fields, request_id.0));
    }

    let previous_internet = network.internet_enabled;
    let previous_uplink = network.uplink.clone();
    let next_uplink = match req.uplink.as_deref() {
        None => previous_uplink.clone(),
        Some("") => None,
        Some(name) => Some(name.to_owned()),
    };
    if previous_internet == req.internet_enabled && next_uplink == previous_uplink {
        return Ok(Json(network));
    }

    let store = state.store.clone();
    let persist_uplink = next_uplink.clone();
    tokio::task::spawn_blocking(move || {
        store.set_micro_network_internet(id, req.internet_enabled)?;
        store.set_micro_network_uplink(id, persist_uplink)?;
        Ok::<_, PersistenceError>(())
    })
    .await
    .map_err(|_| AppError::internal(request_id.0))?
    .map_err(|error| persist_update_error(error, request_id.0))?;
    network.internet_enabled = req.internet_enabled;
    network.uplink = next_uplink;

    // Unlike create/delete, this one is not best-effort: the whole point of
    // the request is the ruleset, so a stored posture the host isn't
    // enforcing would be a wrong answer rather than a degraded one. Rolled
    // back to what it was, which is still what the host has installed.
    if let Err(error) = ensure_all_networks(&state).await {
        tracing::error!(request_id = %request_id.0, micro_network_id = %id, error, "failed to apply micro network update");
        let store = state.store.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _ = store.set_micro_network_internet(id, previous_internet);
            let _ = store.set_micro_network_uplink(id, previous_uplink);
        })
        .await;
        return Err(AppError::internal(request_id.0));
    }

    Ok(Json(network))
}

/// The subnet `candidate` would collide with, if any existing MicroNetwork.
/// Two networks sharing addresses would make the host's routing table
/// ambiguous, so the helper refuses the bridge outright — this just catches
/// it earlier, with a field the form can show.
/// Returns the offending request field and the network it collides with.
/// Both families are checked: two networks sharing a v6 prefix leave the
/// host's routing table just as ambiguous as two sharing a v4 subnet.
async fn overlapping_network(
    state: &AppState,
    candidate: SubnetSpec,
    request_id: Uuid,
) -> Result<Option<(String, String)>, AppError> {
    let store = state.store.clone();
    let existing = tokio::task::spawn_blocking(move || store.list_micro_networks())
        .await
        .map_err(|_| AppError::internal(request_id))?
        .map_err(|error| {
            tracing::error!(request_id = %request_id, %error, "failed to list micro networks");
            AppError::internal(request_id)
        })?;

    Ok(existing.into_iter().find_map(|network| {
        let existing = SubnetSpec::from_micro_network(&network)?;
        let label = format!("MicroNetwork {:?}", network.name);
        if existing.overlaps(&candidate) {
            return Some(("subnetCidr".to_owned(), label));
        }
        let (existing_v6, candidate_v6) = (existing.ipv6?, candidate.ipv6?);
        existing_v6
            .overlaps(&candidate_v6)
            .then_some(("ipv6Cidr".to_owned(), label))
    }))
}

/// The IPv6 plan a create request asks for, or `None` when it asks for no
/// IPv6 — the default, so a request that says nothing about v6 gets the
/// IPv4-only network it has always got. Naming either an `ipv6Cidr` or an
/// `ipv6AddressMode` turns it on; the prefix then defaults to a per-host
/// Unique Local `/64` derived from this network's own id and the mode to
/// SLAAC (`public-docs/networking.md`).
fn resolve_ipv6_plan(req: &CreateMicroNetworkRequest, id: Uuid) -> Option<MicroNetworkIpv6Spec> {
    if req.ipv6_cidr.is_none() && req.ipv6_address_mode.is_none() {
        return None;
    }
    let address_mode = match req.ipv6_address_mode.unwrap_or_default() {
        Ipv6AddressMode::Dhcpv6 => ProtocolIpv6AddressMode::Dhcpv6,
        Ipv6AddressMode::Slaac => ProtocolIpv6AddressMode::Slaac,
    };
    Some(match req.ipv6_cidr.as_deref() {
        Some(cidr) => SubnetSpec::parse_ipv6(cidr, address_mode)
            .expect("validate_create already accepted this prefix"),
        None => MicroNetworkIpv6Spec {
            address_mode,
            ..crate::ipam::auto_ula_prefix(&crate::ipam::host_id(), id)
        },
    })
}

/// Brings every **explicit** MicroNetwork's host resources back to the
/// desired state: one bridge per network, the nftables ruleset, and
/// dnsmasq's served interfaces. There is no implicit default bridge.
/// None of that survives a host reboot — they are kernel objects and a
/// child process — so it is re-applied rather than assumed. Every step is
/// idempotent, so calling this repeatedly (at daemon start, on every VM
/// start, after a network is created or deleted) costs nothing when things
/// are already in place. Zero networks is a valid state (no bridges).
pub(crate) async fn ensure_all_networks(state: &AppState) -> Result<(), String> {
    let _network_guard = state.network_mutations.lock().await;
    ensure_all_networks_locked(state).await
}

/// [`ensure_all_networks`] once the caller owns `network_mutations`. VM setup
/// holds the same guard through its TAP and policy apply, preventing a stale
/// concurrent snapshot from removing a newly installed VM policy.
pub(crate) async fn ensure_all_networks_locked(state: &AppState) -> Result<(), String> {
    let micro_networks = crate::handlers::vms::micro_network_specs(state).await?;
    let vm_policies = crate::handlers::vms::active_vm_policy_specs(state).await?;

    for network in &micro_networks {
        state
            .network
            .ensure_micro_network_bridge(
                network.micro_network_id,
                network.gateway,
                network.prefix,
                network.ipv6,
            )
            .await
            .map_err(|error| {
                format!(
                    "ensure_micro_network_bridge failed for {}: {error}",
                    network.micro_network_id
                )
            })?;
    }
    // Both are rendered from the full network set, so they have to be
    // re-pushed whenever that set might have changed — a bridge on its own
    // carries no traffic: without a dnsmasq range a VM on it never gets an
    // address, and without a NAT rule it never reaches the uplink.
    state
        .network
        .ensure_firewall(micro_networks, vm_policies)
        .await
        .map_err(|error| format!("ensure_firewall failed: {error}"))?;
    crate::handlers::vms::sync_dhcp_leases(state).await
}

/// [`ensure_all_networks`] for callers that must not fail because of it: a
/// created network is already persisted and provisioned, and a deleted one
/// is already gone, so a failure here degrades the network (no DHCP/NAT
/// until the next VM start re-pushes the same snapshot) rather than making
/// the request wrong. Logged instead of rolled back.
async fn apply_network_services(state: &AppState, request_id: Uuid) {
    if let Err(error) = ensure_all_networks(state).await {
        tracing::warn!(request_id = %request_id, error, "network service resync failed");
    }
}

pub async fn delete_micro_network(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let id = parse_id(&id, request_id.0)?;

    // A network still holding leases has VMs whose addresses come out of its
    // subnet and whose TAPs hang off its bridge — deleting it would strand
    // them, so it's refused while any lease is active.
    let store = state.store.clone();
    let in_use = tokio::task::spawn_blocking(move || store.micro_network_has_active_leases(id))
        .await
        .map_err(|_| AppError::internal(request_id.0))?
        .map_err(|error| {
            tracing::error!(request_id = %request_id.0, %error, "failed to check micro network leases");
            AppError::internal(request_id.0)
        })?;
    if in_use {
        return Err(AppError::in_use(
            "MicroNetwork still has VMs in it",
            request_id.0,
        ));
    }

    // Torn down before the record is deleted: if this fails, the record
    // stays so the delete is safely retriable instead of orphaning a bridge
    // no MicroNetwork row points at anymore.
    if let Err(error) = state.network.remove_micro_network_bridge(id).await {
        tracing::error!(request_id = %request_id.0, micro_network_id = %id, %error, "failed to remove micro network bridge");
        return Err(AppError::internal(request_id.0));
    }

    let store = state.store.clone();
    tokio::task::spawn_blocking(move || store.delete_micro_network(id))
        .await
        .map_err(|_| AppError::internal(request_id.0))?
        .map_err(|error| match error {
            PersistenceError::MissingMicroNetwork { .. } => AppError::not_found(request_id.0),
            error => {
                tracing::error!(request_id = %request_id.0, %error, "failed to delete micro network");
                AppError::internal(request_id.0)
            }
        })?;
    // Same reason as create: the removed network has to disappear from the
    // firewall ruleset and dnsmasq's served interfaces too.
    apply_network_services(&state, request_id.0).await;
    Ok(StatusCode::NO_CONTENT)
}

fn validate_create(req: &CreateMicroNetworkRequest) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    if !valid_name(&req.name) {
        fields.insert(
            "name".to_owned(),
            "must be 1-64 ASCII letters, numbers, '.', '_' or '-'".to_owned(),
        );
    }
    match SubnetSpec::parse(Uuid::nil(), &req.subnet_cidr) {
        Some(subnet) if !(MIN_PREFIX..=MAX_PREFIX).contains(&subnet.prefix) => {
            fields.insert(
                "subnetCidr".to_owned(),
                format!("prefix must be between /{MIN_PREFIX} and /{MAX_PREFIX}"),
            );
        }
        Some(_) => {}
        None => {
            fields.insert(
                "subnetCidr".to_owned(),
                "must be an IPv4 CIDR, e.g. 172.31.0.0/24".to_owned(),
            );
        }
    }
    if let Some(uplink) = &req.uplink
        && let Some(message) = uplink_field_error(uplink)
    {
        fields.insert("uplink".to_owned(), message);
    }
    if let Some(cidr) = &req.ipv6_cidr
        && let Some(message) = ipv6_cidr_field_error(cidr)
    {
        fields.insert("ipv6Cidr".to_owned(), message);
    }
    fields
}

/// Why an explicitly requested IPv6 prefix cannot back a MicroNetwork, if it
/// can't. Only a `/64` is usable: SLAAC's EUI-64 interface identifier is
/// exactly 64 bits, so any other length hands guests a prefix they cannot
/// build their stored address from. The prefix must also be Unique Local
/// (`fc00::/7`) or global unicast (`2000::/3`). The helper re-validates all
/// of this independently; this is the user-facing field error.
fn ipv6_cidr_field_error(cidr: &str) -> Option<String> {
    let malformed =
        || Some("must be an IPv6 /64, e.g. fd00:1234:5678::/64 or 2001:db8:1::/64".to_owned());
    let Some(ipv6) = SubnetSpec::parse_ipv6(cidr, ProtocolIpv6AddressMode::Slaac) else {
        return malformed();
    };
    if ipv6.prefix != 64 {
        return Some("prefix must be /64".to_owned());
    }
    if !ipv6.is_routable_scope() {
        return Some("must be a unique-local (fc00::/7) or global (2000::/3) prefix".to_owned());
    }
    None
}

fn persist_update_error(error: PersistenceError, request_id: Uuid) -> AppError {
    match error {
        PersistenceError::MissingMicroNetwork { .. } => AppError::not_found(request_id),
        error => {
            tracing::error!(request_id = %request_id, %error, "failed to update micro network");
            AppError::internal(request_id)
        }
    }
}

fn validate_update_micro_network(req: &UpdateMicroNetworkRequest) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    if let Some(uplink) = req.uplink.as_deref()
        && !uplink.is_empty()
        && let Some(message) = uplink_field_error(uplink)
    {
        fields.insert("uplink".to_owned(), message);
    }
    fields
}

fn uplink_field_error(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("must be a host interface name, or omitted for auto".to_owned());
    }
    if !valid_uplink_name(name) {
        return Some("must be 1-15 ASCII letters, numbers, '.', '_', ':' or '-'".to_owned());
    }
    if name == "lo" || name.starts_with("fct") || name.starts_with("mnb") {
        return Some("cannot be loopback or a Firecrab-owned interface".to_owned());
    }
    if !crate::handlers::network::host_interface_exists(name) {
        return Some(format!("{name} is not a host interface"));
    }
    None
}

fn valid_uplink_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=15).contains(&bytes.len())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Test-only fixtures shared with sibling handler modules (e.g.
/// `handlers::builder_vm`) that need a MicroNetwork already in the store,
/// without duplicating this module's own create-then-validate flow.
#[cfg(test)]
pub(crate) mod test_support {
    use firecrab_api_types::MicroNetworkResponse;
    use uuid::Uuid;

    use super::AppState;
    use crate::ipam::SubnetSpec;

    /// Inserts a MicroNetwork with internet egress enabled directly into
    /// the store, bypassing `create_micro_network`'s HTTP validation —
    /// callers just need a network `builder_micro_network_id` can find.
    pub(crate) fn seed_internet_micro_network(state: &AppState) -> Uuid {
        let id = Uuid::new_v4();
        let cidr = "172.31.0.0/24";
        let subnet = SubnetSpec::parse(id, cidr).expect("valid literal CIDR");
        let network = MicroNetworkResponse {
            id,
            name: "test-net".to_owned(),
            subnet_cidr: cidr.to_owned(),
            gateway: subnet.gateway().to_string(),
            internet_enabled: true,
            uplink: None,
            ipv6_cidr: None,
            ipv6_gateway: None,
            ipv6_address_mode: None,
            ipv6_egress: None,
        };
        state
            .store
            .insert_micro_network(&network)
            .expect("seed micro network");
        id
    }
}

#[cfg(test)]
mod tests {
    use axum::extract::Extension;
    use axum::response::IntoResponse;
    use tempfile::tempdir;

    use super::*;
    use crate::server::RequestId;
    use crate::templates::TemplateRegistry;

    async fn test_state(root: &std::path::Path) -> AppState {
        let templates = TemplateRegistry::from_specs(root, std::iter::empty())
            .expect("empty template spec list should always verify");
        let state = AppState::with_db_file(templates, root.join("state.db"))
            .await
            .expect("fresh temp db should open cleanly");

        let socket_path = root.join("net-helper.sock");
        crate::network::test_support::spawn_always_ok_helper(&socket_path);
        state.with_test_network(crate::network::NetworkClient::with_socket_path(socket_path))
    }

    fn extension() -> Extension<RequestId> {
        Extension(RequestId(Uuid::new_v4()))
    }

    #[tokio::test]
    async fn create_then_list_then_delete_round_trips_through_the_handlers() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let (status, Json(created)) = create_micro_network(
            State(state.clone()),
            extension(),
            ValidatedJson(CreateMicroNetworkRequest {
                name: "prod".to_owned(),
                subnet_cidr: "172.31.0.0/24".to_owned(),
                internet_enabled: true,
                uplink: None,
                ipv6_cidr: None,
                ipv6_address_mode: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(created.name, "prod");
        assert_eq!(created.subnet_cidr, "172.31.0.0/24");
        assert_eq!(created.gateway, "172.31.0.1");
        assert_eq!(created.uplink, None);

        let Json(listed) = list_micro_networks(State(state.clone()), extension())
            .await
            .unwrap();
        assert_eq!(listed, vec![created.clone()]);

        let status = delete_micro_network(
            State(state.clone()),
            extension(),
            Path(created.id.to_string()),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::NO_CONTENT);

        let Json(listed) = list_micro_networks(State(state), extension())
            .await
            .unwrap();
        assert!(listed.is_empty());
    }

    #[tokio::test]
    async fn create_rejects_an_invalid_request_without_touching_the_store() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let error = create_micro_network(
            State(state.clone()),
            extension(),
            ValidatedJson(CreateMicroNetworkRequest {
                name: String::new(),
                subnet_cidr: "not-a-cidr".to_owned(),
                internet_enabled: true,
                uplink: None,
                ipv6_cidr: None,
                ipv6_address_mode: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);

        let Json(listed) = list_micro_networks(State(state), extension())
            .await
            .unwrap();
        assert!(listed.is_empty());
    }

    async fn create_network(state: &AppState, name: &str, cidr: &str) -> MicroNetworkResponse {
        let (_, Json(created)) = create_micro_network(
            State(state.clone()),
            extension(),
            ValidatedJson(CreateMicroNetworkRequest {
                name: name.to_owned(),
                subnet_cidr: cidr.to_owned(),
                internet_enabled: true,
                uplink: None,
                ipv6_cidr: None,
                ipv6_address_mode: None,
            }),
        )
        .await
        .expect("create micro network");
        created
    }

    #[tokio::test]
    async fn a_cidr_overlapping_an_existing_network_is_a_field_error_not_a_rollback() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        create_network(&state, "prod", "172.31.0.0/24").await;

        // Same block, and a wider block that swallows it: both ambiguous for
        // the host's routing table, so both are refused.
        for cidr in ["172.31.0.0/24", "172.31.0.0/16"] {
            let error = create_micro_network(
                State(state.clone()),
                extension(),
                ValidatedJson(CreateMicroNetworkRequest {
                    name: "clash".to_owned(),
                    subnet_cidr: cidr.to_owned(),
                    internet_enabled: true,
                    uplink: None,
                    ipv6_cidr: None,
                    ipv6_address_mode: Default::default(),
                }),
            )
            .await
            .unwrap_err();
            assert_eq!(
                error.into_response().status(),
                StatusCode::BAD_REQUEST,
                "{cidr} should be rejected as overlapping"
            );
        }

        // A second distinct network that does not overlap is fine.
        let (_, Json(other)) = create_micro_network(
            State(state.clone()),
            extension(),
            ValidatedJson(CreateMicroNetworkRequest {
                name: "other".to_owned(),
                subnet_cidr: "172.30.0.0/24".to_owned(),
                internet_enabled: true,
                uplink: None,
                ipv6_cidr: None,
                ipv6_address_mode: None,
            }),
        )
        .await
        .expect("non-overlapping CIDR is allowed");
        assert_eq!(other.subnet_cidr, "172.30.0.0/24");

        let Json(listed) = list_micro_networks(State(state), extension())
            .await
            .unwrap();
        assert_eq!(listed.len(), 2, "only rejected attempts leave no record");
    }

    #[tokio::test]
    async fn create_without_ipv6_fields_stays_ipv4_only() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let created = create_network(&state, "prod", "172.31.0.0/24").await;

        assert_eq!(created.ipv6_cidr, None);
        assert_eq!(created.ipv6_gateway, None);
        assert_eq!(created.ipv6_address_mode, None);
        assert_eq!(created.ipv6_egress, None);
    }

    #[tokio::test]
    async fn create_with_an_address_mode_and_no_cidr_gets_an_auto_ula_prefix() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let (_, Json(created)) = create_micro_network(
            State(state.clone()),
            extension(),
            ValidatedJson(CreateMicroNetworkRequest {
                name: "prod".to_owned(),
                subnet_cidr: "172.31.0.0/24".to_owned(),
                internet_enabled: true,
                uplink: None,
                ipv6_cidr: None,
                ipv6_address_mode: Some(Ipv6AddressMode::Slaac),
            }),
        )
        .await
        .expect("turning IPv6 on without a prefix generates a ULA");

        // A per-host ULA /64, which is not routable off-host and therefore
        // egresses through NAT66.
        let cidr = created.ipv6_cidr.clone().expect("an auto prefix");
        assert!(
            cidr.starts_with("fd"),
            "{cidr} should be RFC 4193 ULA space"
        );
        assert!(cidr.ends_with("/64"), "{cidr} should be a /64");
        assert_eq!(created.ipv6_egress, Some(Ipv6EgressMode::Nat66));
        assert_eq!(created.ipv6_address_mode, Some(Ipv6AddressMode::Slaac));
        assert!(created.ipv6_gateway.is_some());

        // A second network on the same host gets its own prefix.
        let (_, Json(other)) = create_micro_network(
            State(state),
            extension(),
            ValidatedJson(CreateMicroNetworkRequest {
                name: "stage".to_owned(),
                subnet_cidr: "172.32.0.0/24".to_owned(),
                internet_enabled: true,
                uplink: None,
                ipv6_cidr: None,
                ipv6_address_mode: Some(Ipv6AddressMode::Slaac),
            }),
        )
        .await
        .expect("a second IPv6-on network");
        assert_ne!(other.ipv6_cidr, created.ipv6_cidr);
    }

    #[tokio::test]
    async fn create_with_a_global_prefix_reports_direct_egress() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let (_, Json(created)) = create_micro_network(
            State(state.clone()),
            extension(),
            ValidatedJson(CreateMicroNetworkRequest {
                name: "public".to_owned(),
                subnet_cidr: "172.31.0.0/24".to_owned(),
                internet_enabled: true,
                uplink: None,
                ipv6_cidr: Some("2001:db8:1::/64".to_owned()),
                ipv6_address_mode: Some(Ipv6AddressMode::Dhcpv6),
            }),
        )
        .await
        .expect("a global prefix is accepted");

        assert_eq!(created.ipv6_cidr.as_deref(), Some("2001:db8:1::/64"));
        assert_eq!(created.ipv6_gateway.as_deref(), Some("2001:db8:1::1"));
        // Publicly routable, so its VMs keep their own addresses on the wire.
        assert_eq!(created.ipv6_egress, Some(Ipv6EgressMode::Direct));
        assert_eq!(created.ipv6_address_mode, Some(Ipv6AddressMode::Dhcpv6));
    }

    #[tokio::test]
    async fn create_rejects_an_ipv6_cidr_a_network_cannot_be_built_on() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        for cidr in [
            "not-a-cidr",
            "2001:db8::/48", // SLAAC's EUI-64 needs exactly a /64
            "fe80::/64",     // link-local addresses no network of its own
            "ff02::/64",     // multicast
            "fec0::/64",     // deprecated site-local
            "100::/64",      // discard-only
            "64:ff9b::/64",  // NAT64
            "172.31.0.0/24", // an IPv4 CIDR in the v6 field
        ] {
            let error = create_micro_network(
                State(state.clone()),
                extension(),
                ValidatedJson(CreateMicroNetworkRequest {
                    name: "bad".to_owned(),
                    subnet_cidr: "172.31.0.0/24".to_owned(),
                    internet_enabled: true,
                    uplink: None,
                    ipv6_cidr: Some(cidr.to_owned()),
                    ipv6_address_mode: Some(Ipv6AddressMode::Slaac),
                }),
            )
            .await
            .unwrap_err();
            assert_eq!(
                error.into_response().status(),
                StatusCode::BAD_REQUEST,
                "{cidr} should be rejected"
            );
        }

        let Json(listed) = list_micro_networks(State(state), extension())
            .await
            .unwrap();
        assert!(listed.is_empty(), "a rejected request leaves no record");
    }

    #[tokio::test]
    async fn an_ipv6_prefix_overlapping_an_existing_network_is_a_field_error() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let (_, Json(_)) = create_micro_network(
            State(state.clone()),
            extension(),
            ValidatedJson(CreateMicroNetworkRequest {
                name: "prod".to_owned(),
                subnet_cidr: "172.31.0.0/24".to_owned(),
                internet_enabled: true,
                uplink: None,
                ipv6_cidr: Some("2001:db8:1::/64".to_owned()),
                ipv6_address_mode: Some(Ipv6AddressMode::Slaac),
            }),
        )
        .await
        .unwrap();

        // The v4 subnets differ, so only the v6 prefix collides — and one
        // prefix on two bridges is as ambiguous for the host's routing table
        // as a shared v4 subnet is.
        let error = create_micro_network(
            State(state.clone()),
            extension(),
            ValidatedJson(CreateMicroNetworkRequest {
                name: "clash".to_owned(),
                subnet_cidr: "172.32.0.0/24".to_owned(),
                internet_enabled: true,
                uplink: None,
                ipv6_cidr: Some("2001:db8:1::/64".to_owned()),
                ipv6_address_mode: Some(Ipv6AddressMode::Slaac),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);

        let Json(listed) = list_micro_networks(State(state), extension())
            .await
            .unwrap();
        assert_eq!(listed.len(), 1);
    }

    #[tokio::test]
    async fn detail_reports_the_v6_plan_alongside_the_v4_one() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let (_, Json(network)) = create_micro_network(
            State(state.clone()),
            extension(),
            ValidatedJson(CreateMicroNetworkRequest {
                name: "prod".to_owned(),
                subnet_cidr: "172.31.0.0/24".to_owned(),
                internet_enabled: true,
                uplink: None,
                ipv6_cidr: None,
                ipv6_address_mode: Some(Ipv6AddressMode::Slaac),
            }),
        )
        .await
        .expect("IPv6-on create");

        let Json(detail) = get_micro_network(
            State(state.clone()),
            extension(),
            Path(network.id.to_string()),
        )
        .await
        .unwrap();

        assert_eq!(detail.subnet.ipv6_cidr, network.ipv6_cidr);
        assert_eq!(detail.subnet.ipv6_gateway, network.ipv6_gateway);
        assert_eq!(detail.subnet.ipv6_egress, Some(Ipv6EgressMode::Nat66));
        // A ULA prefix is masqueraded, so it is a NAT source range.
        assert_eq!(detail.nat.ipv6_source_cidr, network.ipv6_cidr);
    }

    #[tokio::test]
    async fn detail_breaks_the_network_out_into_its_services() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let network = create_network(&state, "prod", "172.31.0.0/24").await;

        let Json(detail) = get_micro_network(
            State(state.clone()),
            extension(),
            Path(network.id.to_string()),
        )
        .await
        .unwrap();

        assert_eq!(detail.id, network.id);
        assert_eq!(detail.name, "prod");
        // Subnet: /24 minus network/gateway/broadcast.
        assert_eq!(detail.subnet.cidr, "172.31.0.0/24");
        assert_eq!(detail.subnet.gateway, "172.31.0.1");
        assert_eq!(detail.subnet.usable_addresses, 253);
        assert_eq!(detail.subnet.allocated_addresses, 0);
        // Bridge name is derived from the id, the same way the helper does it.
        assert_eq!(
            detail.bridge.name,
            micro_network_bridge_name(network.id),
            "the reported bridge must be the one the helper actually creates"
        );
        assert_eq!(detail.bridge.attached_taps, 0);
        // NAT masquerades this network's own subnet.
        assert!(detail.nat.enabled);
        assert_eq!(detail.nat.source_cidr, "172.31.0.0/24");
        // Firewall posture is what the rendered ruleset enforces.
        assert!(!detail.firewall.east_west_blocked);
        assert!(detail.firewall.cross_network_blocked);
        assert!(detail.firewall.anti_spoofing);
        assert!(detail.vms.is_empty());
    }

    #[tokio::test]
    async fn detail_lists_only_the_vms_placed_in_that_network() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let network = create_network(&state, "prod", "172.31.0.0/24").await;
        let other = create_network(&state, "stage", "172.32.0.0/24").await;

        let subnet = SubnetSpec::parse(network.id, &network.subnet_cidr).unwrap();
        let mut member = crate::handlers::vms::test_support::record("member", Uuid::new_v4());
        member.state = VmState::Stopped;
        member.micro_network_id = network.id;
        let lease = state.store.allocate_lease(member.id, subnet).unwrap();
        state.store.insert(&member).unwrap();

        let Json(detail) = get_micro_network(
            State(state.clone()),
            extension(),
            Path(network.id.to_string()),
        )
        .await
        .unwrap();
        assert_eq!(detail.vms.len(), 1);
        assert_eq!(detail.vms[0].name, "member");
        assert_eq!(detail.vms[0].ipv4, Some(lease.ipv4.to_string()));
        assert_eq!(detail.subnet.allocated_addresses, 1);
        // Stopped VMs hold an address but have no TAP on the bridge.
        assert_eq!(detail.bridge.attached_taps, 0);

        let Json(other_detail) =
            get_micro_network(State(state), extension(), Path(other.id.to_string()))
                .await
                .unwrap();
        assert!(
            other_detail.vms.is_empty(),
            "a VM must only show up under its own network"
        );
    }

    #[tokio::test]
    async fn reconcile_reprovisions_every_network_including_ones_with_no_vms() {
        let directory = tempdir().unwrap();
        let templates = TemplateRegistry::from_specs(directory.path(), std::iter::empty())
            .expect("empty template spec list should always verify");
        let state = AppState::with_db_file(templates, directory.path().join("state.db"))
            .await
            .expect("fresh temp db should open cleanly");
        let socket_path = directory.path().join("net-helper.sock");
        let (_task, log) = crate::network::test_support::spawn_recording_helper(&socket_path, None);
        let state =
            state.with_test_network(crate::network::NetworkClient::with_socket_path(socket_path));

        let first = create_network(&state, "prod", "172.31.0.0/24").await;
        let second = create_network(&state, "stage", "172.32.0.0/24").await;
        log.lock().unwrap().clear();

        // Neither network has a VM in it, so nothing else would ever touch
        // them again — a host reboot would leave both bridges gone.
        ensure_all_networks(&state).await.expect("reconcile");

        let operations = log.lock().unwrap().clone();
        assert_eq!(
            operations
                .iter()
                .filter(|op| **op == "ensure_micro_network_bridge")
                .count(),
            2,
            "each MicroNetwork's bridge must be re-ensured: {operations:?}"
        );
        assert!(!operations.contains(&"ensure_bridge"), "{operations:?}");
        assert!(operations.contains(&"ensure_firewall"), "{operations:?}");
        assert!(operations.contains(&"sync_dhcp_leases"), "{operations:?}");
        // Sanity: the two really are distinct networks, not one counted twice.
        assert_ne!(first.id, second.id);
    }

    #[tokio::test]
    async fn reconcile_sends_active_vm_policy_in_the_atomic_firewall_snapshot() {
        let directory = tempdir().unwrap();
        let templates = TemplateRegistry::from_specs(directory.path(), std::iter::empty())
            .expect("empty template spec list should always verify");
        let state = AppState::with_db_file(templates, directory.path().join("state.db"))
            .await
            .expect("fresh temp db should open cleanly");
        let socket_path = directory.path().join("net-helper.sock");
        let (_task, requests) = crate::network::test_support::spawn_policy_collision_helper(
            &socket_path,
            std::iter::empty(),
        );
        let state =
            state.with_test_network(crate::network::NetworkClient::with_socket_path(socket_path));

        // A VM that is running right now, with the lease its policy is keyed
        // on — the state a toggle has to not break.
        let mut vm = crate::handlers::vms::test_support::record("live", Uuid::new_v4());
        vm.state = VmState::Running;
        state.store.insert(&vm).expect("seed vm");
        state
            .store
            .allocate_lease(vm.id, SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)))
            .expect("seed lease");
        requests.lock().unwrap().clear();

        ensure_all_networks(&state).await.expect("reconcile");

        let requests = requests.lock().unwrap();
        let snapshots = requests
            .iter()
            .filter_map(|request| match request {
                firecrab_helper_protocol::network::NetworkRequest::EnsureFirewall {
                    vm_policies,
                    ..
                } => Some(vm_policies),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(snapshots.len(), 1, "requests: {requests:?}");
        assert_eq!(snapshots[0].len(), 1);
        assert_eq!(snapshots[0][0].vm_id, vm.id);
        assert!(
            !requests.iter().any(|request| matches!(
                request,
                firecrab_helper_protocol::network::NetworkRequest::ApplyVmPolicy { .. }
            )),
            "the snapshot must not require a second non-atomic apply: {requests:?}"
        );
    }

    #[tokio::test]
    async fn reconcile_reports_a_helper_failure_instead_of_swallowing_it() {
        let directory = tempdir().unwrap();
        let templates = TemplateRegistry::from_specs(directory.path(), std::iter::empty())
            .expect("empty template spec list should always verify");
        let state = AppState::with_db_file(templates, directory.path().join("state.db"))
            .await
            .expect("fresh temp db should open cleanly");
        let socket_path = directory.path().join("net-helper.sock");
        crate::network::test_support::spawn_recording_helper(&socket_path, Some("ensure_firewall"));
        let state =
            state.with_test_network(crate::network::NetworkClient::with_socket_path(socket_path));

        // With no MicroNetworks, ensure_all_networks still pushes firewall+dhcp.
        // Force a helper failure on ensure_firewall.
        let error = ensure_all_networks(&state).await.unwrap_err();
        assert!(error.contains("ensure_firewall"), "{error}");
    }

    #[tokio::test]
    async fn switching_the_internet_off_changes_what_the_helper_is_told_about_the_network() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let created = create_network(&state, "closed", "172.31.0.0/24").await;
        assert!(created.internet_enabled, "a new network starts connected");

        let Json(updated) = update_micro_network(
            State(state.clone()),
            extension(),
            Path(created.id.to_string()),
            ValidatedJson(UpdateMicroNetworkRequest {
                internet_enabled: false,
                uplink: None,
            }),
        )
        .await
        .expect("toggle the internet off");
        assert!(!updated.internet_enabled);

        // What actually matters: the spec the helper renders NAT and the
        // forward rules from now says the network is closed.
        let specs = crate::handlers::vms::micro_network_specs(&state)
            .await
            .expect("micro network specs");
        assert_eq!(specs.len(), 1);
        assert!(!specs[0].internet_enabled);

        // And the detail view reports it rather than the old hardcoded true.
        let Json(detail) = get_micro_network(
            State(state.clone()),
            extension(),
            Path(created.id.to_string()),
        )
        .await
        .expect("detail");
        assert!(!detail.nat.enabled);

        // Back on again — the toggle is not one-way.
        let Json(reopened) = update_micro_network(
            State(state.clone()),
            extension(),
            Path(created.id.to_string()),
            ValidatedJson(UpdateMicroNetworkRequest {
                internet_enabled: true,
                uplink: None,
            }),
        )
        .await
        .expect("toggle the internet back on");
        assert!(reopened.internet_enabled);
    }

    #[tokio::test]
    async fn a_toggle_the_helper_rejects_leaves_the_stored_posture_as_it_was() {
        let directory = tempdir().unwrap();
        let templates = TemplateRegistry::from_specs(directory.path(), std::iter::empty())
            .expect("empty template spec list should always verify");
        let state = AppState::with_db_file(templates, directory.path().join("state.db"))
            .await
            .expect("fresh temp db should open cleanly");
        let socket_path = directory.path().join("net-helper.sock");
        // Fails only the ruleset call, so the network is still created.
        crate::network::test_support::spawn_recording_helper(&socket_path, Some("ensure_firewall"));
        let state =
            state.with_test_network(crate::network::NetworkClient::with_socket_path(socket_path));
        let created = create_network(&state, "prod", "172.31.0.0/24").await;

        let error = update_micro_network(
            State(state.clone()),
            extension(),
            Path(created.id.to_string()),
            ValidatedJson(UpdateMicroNetworkRequest {
                internet_enabled: false,
                uplink: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );

        // Rolled back: reporting a closed network the host is still routing
        // out of would be worse than reporting the failure.
        let Json(listed) = list_micro_networks(State(state), extension())
            .await
            .unwrap();
        assert!(
            listed[0].internet_enabled,
            "a failed apply must not leave the stored posture ahead of the host"
        );
    }

    #[tokio::test]
    async fn toggling_a_network_that_does_not_exist_is_a_404() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let error = update_micro_network(
            State(state),
            extension(),
            Path(Uuid::new_v4().to_string()),
            ValidatedJson(UpdateMicroNetworkRequest {
                internet_enabled: false,
                uplink: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn detail_reports_not_found_for_an_unknown_id() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let error = get_micro_network(State(state), extension(), Path(Uuid::new_v4().to_string()))
            .await
            .unwrap_err();
        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_network_with_an_active_lease_cannot_be_deleted() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let network = create_network(&state, "prod", "172.31.0.0/24").await;

        let subnet = SubnetSpec::parse(network.id, &network.subnet_cidr).unwrap();
        let lease = state
            .store
            .allocate_lease(Uuid::new_v4(), subnet)
            .expect("allocate a lease inside the network");
        // The address really does come out of the MicroNetwork's own subnet,
        // not the default one.
        assert!(lease.ipv4.to_string().starts_with("172.31.0."));

        let error = delete_micro_network(
            State(state.clone()),
            extension(),
            Path(network.id.to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);

        // Releasing the lease unblocks the delete.
        state.store.release_lease(lease.vm_id).unwrap();
        let status = delete_micro_network(State(state), extension(), Path(network.id.to_string()))
            .await
            .unwrap();
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_reports_not_found_for_an_unknown_id() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let error =
            delete_micro_network(State(state), extension(), Path(Uuid::new_v4().to_string()))
                .await
                .unwrap_err();
        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_rolls_back_the_record_when_bridge_provisioning_fails() {
        let directory = tempdir().unwrap();
        let templates = TemplateRegistry::from_specs(directory.path(), std::iter::empty())
            .expect("empty template spec list should always verify");
        let state = AppState::with_db_file(templates, directory.path().join("state.db"))
            .await
            .expect("fresh temp db should open cleanly");
        let socket_path = directory.path().join("net-helper.sock");
        crate::network::test_support::spawn_recording_helper(
            &socket_path,
            Some("ensure_micro_network_bridge"),
        );
        let state =
            state.with_test_network(crate::network::NetworkClient::with_socket_path(socket_path));

        let result = create_micro_network(
            State(state.clone()),
            extension(),
            ValidatedJson(CreateMicroNetworkRequest {
                name: "doomed".to_owned(),
                subnet_cidr: "172.31.0.0/24".to_owned(),
                internet_enabled: true,
                uplink: None,
                ipv6_cidr: None,
                ipv6_address_mode: None,
            }),
        )
        .await;

        assert!(result.is_err());
        let Json(listed) = list_micro_networks(State(state), extension())
            .await
            .unwrap();
        assert!(
            listed.is_empty(),
            "a failed bridge provisioning must roll back the just-inserted record"
        );
    }

    #[test]
    fn valid_name_accepts_alnum_dot_underscore_dash_and_rejects_the_rest() {
        assert!(valid_name("prod"));
        assert!(valid_name("prod-1.2_3"));
        assert!(!valid_name(""));
        assert!(!valid_name(&"a".repeat(65)));
        assert!(!valid_name(".starts-with-dot"));
    }

    #[test]
    fn validate_create_reports_both_fields_independently() {
        let fields = validate_create(&CreateMicroNetworkRequest {
            name: String::new(),
            subnet_cidr: "not-a-cidr".to_owned(),
            internet_enabled: true,
            uplink: None,
            ipv6_cidr: None,
            ipv6_address_mode: Default::default(),
        });
        assert!(fields.contains_key("name"));
        assert!(fields.contains_key("subnetCidr"));
    }

    #[test]
    fn validate_create_rejects_a_prefix_outside_the_accepted_range() {
        for cidr in ["172.31.0.0/8", "172.31.0.0/30"] {
            let fields = validate_create(&CreateMicroNetworkRequest {
                name: "prod".to_owned(),
                subnet_cidr: cidr.to_owned(),
                internet_enabled: true,
                uplink: None,
                ipv6_cidr: None,
                ipv6_address_mode: Default::default(),
            });
            assert!(
                fields.contains_key("subnetCidr"),
                "{cidr} should be rejected"
            );
        }
        for cidr in ["172.31.0.0/16", "172.31.0.0/24", "172.31.0.0/28"] {
            let fields = validate_create(&CreateMicroNetworkRequest {
                name: "prod".to_owned(),
                subnet_cidr: cidr.to_owned(),
                internet_enabled: true,
                uplink: None,
                ipv6_cidr: None,
                ipv6_address_mode: Default::default(),
            });
            assert!(
                !fields.contains_key("subnetCidr"),
                "{cidr} should be accepted"
            );
        }
    }

    #[test]
    fn a_created_network_reports_the_gateway_derived_from_its_cidr() {
        let subnet = SubnetSpec::parse(Uuid::nil(), "172.31.0.0/24").unwrap();
        assert_eq!(subnet.gateway().to_string(), "172.31.0.1");
    }

    fn existing_uplink() -> String {
        crate::handlers::network::read_host_interfaces()
            .into_iter()
            .next()
            .expect("test host has a picker interface")
    }

    async fn validation_fields(error: AppError) -> BTreeMap<String, String> {
        let response = error.into_response();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        serde_json::from_value(json["error"]["fields"].clone()).unwrap()
    }

    #[tokio::test]
    async fn create_stores_a_valid_uplink_and_list_returns_the_stored_value() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let uplink = existing_uplink();

        let (_, Json(created)) = create_micro_network(
            State(state.clone()),
            extension(),
            ValidatedJson(CreateMicroNetworkRequest {
                name: "prod".to_owned(),
                subnet_cidr: "172.31.0.0/24".to_owned(),
                internet_enabled: true,
                uplink: Some(uplink.clone()),
                ipv6_cidr: None,
                ipv6_address_mode: Default::default(),
            }),
        )
        .await
        .expect("create with uplink");
        assert_eq!(created.uplink.as_deref(), Some(uplink.as_str()));

        let Json(listed) = list_micro_networks(State(state.clone()), extension())
            .await
            .unwrap();
        assert_eq!(listed[0].uplink.as_deref(), Some(uplink.as_str()));

        let Json(detail) =
            get_micro_network(State(state), extension(), Path(created.id.to_string()))
                .await
                .unwrap();
        assert_eq!(detail.nat.uplink, uplink);
    }

    #[tokio::test]
    async fn create_rejects_empty_malformed_and_missing_uplinks_without_a_row() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        for (uplink, reason) in [
            (Some(String::new()), "empty"),
            (Some("eth0/foo".to_owned()), "malformed"),
            (Some("lo".to_owned()), "loopback"),
            (Some("nosuchiface0".to_owned()), "missing"),
        ] {
            let error = create_micro_network(
                State(state.clone()),
                extension(),
                ValidatedJson(CreateMicroNetworkRequest {
                    name: "prod".to_owned(),
                    subnet_cidr: "172.31.0.0/24".to_owned(),
                    internet_enabled: true,
                    uplink,
                    ipv6_cidr: None,
                    ipv6_address_mode: Default::default(),
                }),
            )
            .await
            .unwrap_err();
            let fields = validation_fields(error).await;
            assert!(
                fields.contains_key("uplink"),
                "{reason} should be a field error: {fields:?}"
            );
        }

        let Json(listed) = list_micro_networks(State(state), extension())
            .await
            .unwrap();
        assert!(listed.is_empty());
    }

    #[tokio::test]
    async fn patch_sets_a_valid_uplink_and_omitted_leaves_the_stored_value() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let created = create_network(&state, "prod", "172.31.0.0/24").await;
        assert_eq!(created.uplink, None);
        let uplink = existing_uplink();

        let Json(updated) = update_micro_network(
            State(state.clone()),
            extension(),
            Path(created.id.to_string()),
            ValidatedJson(UpdateMicroNetworkRequest {
                internet_enabled: true,
                uplink: Some(uplink.clone()),
            }),
        )
        .await
        .expect("patch uplink");
        assert_eq!(updated.uplink.as_deref(), Some(uplink.as_str()));

        let Json(toggled) = update_micro_network(
            State(state.clone()),
            extension(),
            Path(created.id.to_string()),
            ValidatedJson(UpdateMicroNetworkRequest {
                internet_enabled: false,
                uplink: None,
            }),
        )
        .await
        .expect("patch internet only");
        assert!(!toggled.internet_enabled);
        assert_eq!(toggled.uplink.as_deref(), Some(uplink.as_str()));

        let Json(listed) = list_micro_networks(State(state.clone()), extension())
            .await
            .unwrap();
        assert_eq!(listed[0].uplink.as_deref(), Some(uplink.as_str()));

        let Json(reset) = update_micro_network(
            State(state.clone()),
            extension(),
            Path(created.id.to_string()),
            ValidatedJson(UpdateMicroNetworkRequest {
                internet_enabled: false,
                uplink: Some(String::new()),
            }),
        )
        .await
        .expect("empty uplink resets to auto");
        assert_eq!(reset.uplink, None);
        assert!(!reset.internet_enabled);
    }

    #[tokio::test]
    async fn patch_rejects_a_bad_uplink_without_changing_the_row() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let created = create_network(&state, "prod", "172.31.0.0/24").await;

        for uplink in ["eth0/foo".to_owned(), "nosuchiface0".to_owned()] {
            let error = update_micro_network(
                State(state.clone()),
                extension(),
                Path(created.id.to_string()),
                ValidatedJson(UpdateMicroNetworkRequest {
                    internet_enabled: true,
                    uplink: Some(uplink),
                }),
            )
            .await
            .unwrap_err();
            let fields = validation_fields(error).await;
            assert!(fields.contains_key("uplink"), "{fields:?}");
        }

        let Json(listed) = list_micro_networks(State(state), extension())
            .await
            .unwrap();
        assert_eq!(listed[0].uplink, None);
        assert!(listed[0].internet_enabled);
    }

    #[tokio::test]
    async fn a_failed_apply_rolls_back_internet_and_uplink() {
        let directory = tempdir().unwrap();
        let templates = TemplateRegistry::from_specs(directory.path(), std::iter::empty())
            .expect("empty template spec list should always verify");
        let state = AppState::with_db_file(templates, directory.path().join("state.db"))
            .await
            .expect("fresh temp db should open cleanly");
        let socket_path = directory.path().join("net-helper.sock");
        crate::network::test_support::spawn_recording_helper(&socket_path, Some("ensure_firewall"));
        let state =
            state.with_test_network(crate::network::NetworkClient::with_socket_path(socket_path));
        let created = create_network(&state, "prod", "172.31.0.0/24").await;
        let uplink = existing_uplink();

        let error = update_micro_network(
            State(state.clone()),
            extension(),
            Path(created.id.to_string()),
            ValidatedJson(UpdateMicroNetworkRequest {
                internet_enabled: false,
                uplink: Some(uplink),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );

        let Json(listed) = list_micro_networks(State(state), extension())
            .await
            .unwrap();
        assert!(listed[0].internet_enabled);
        assert_eq!(listed[0].uplink, None);
    }

    #[test]
    fn validate_create_and_update_reject_a_bad_uplink() {
        let fields = validate_create(&CreateMicroNetworkRequest {
            name: "prod".to_owned(),
            subnet_cidr: "172.31.0.0/24".to_owned(),
            internet_enabled: true,
            uplink: Some(String::new()),
            ipv6_cidr: None,
            ipv6_address_mode: Default::default(),
        });
        assert!(fields.contains_key("uplink"));

        let fields = validate_update_micro_network(&UpdateMicroNetworkRequest {
            internet_enabled: true,
            uplink: Some("eth0/foo".to_owned()),
        });
        assert!(fields.contains_key("uplink"));

        let fields = validate_update_micro_network(&UpdateMicroNetworkRequest {
            internet_enabled: true,
            uplink: None,
        });
        assert!(fields.is_empty());

        let fields = validate_update_micro_network(&UpdateMicroNetworkRequest {
            internet_enabled: true,
            uplink: Some(String::new()),
        });
        assert!(
            fields.is_empty(),
            "empty PATCH uplink resets to auto, it is not a field error"
        );
    }

    #[test]
    fn persist_update_error_maps_missing_network_to_not_found() {
        let error = persist_update_error(
            PersistenceError::MissingMicroNetwork { id: Uuid::nil() },
            Uuid::nil(),
        );
        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn persist_update_error_maps_other_failures_to_internal() {
        let error = persist_update_error(
            PersistenceError::CorruptRecord {
                id: "mn".to_owned(),
                reason: "env is not a JSON object of strings".to_owned(),
            },
            Uuid::nil(),
        );
        assert_eq!(
            error.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
