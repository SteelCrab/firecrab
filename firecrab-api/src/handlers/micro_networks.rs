//! MicroNetwork CRUD (`docs/30-tasks/task-micro-network.md`) — a named CIDR
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
    CreateMicroNetworkRequest, EgressPolicy, MicroNetworkBridge, MicroNetworkDetailResponse,
    MicroNetworkFirewall, MicroNetworkNat, MicroNetworkResponse, MicroNetworkSubnet,
    MicroNetworkVm, UpdateMicroNetworkRequest, VmState,
};
use firecrab_helper_protocol::network::micro_network_bridge_name;
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::handlers::vms::parse_id;
use crate::ipam::SubnetSpec;
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
    let subnet = SubnetSpec::parse(Some(id), &network.subnet_cidr).ok_or_else(|| {
        tracing::error!(
            request_id = %request_id.0,
            micro_network_id = %id,
            subnet_cidr = network.subnet_cidr,
            "stored micro network subnet does not parse"
        );
        AppError::internal(request_id.0)
    })?;

    let addresses: HashMap<Uuid, String> = leases
        .into_iter()
        .map(|lease| (lease.vm_id, lease.ipv4.to_string()))
        .collect();
    let mut members: Vec<MicroNetworkVm> = vms
        .into_values()
        .filter(|vm| vm.micro_network_id == Some(id))
        .map(|vm| MicroNetworkVm {
            ipv4: addresses.get(&vm.id).cloned(),
            id: vm.id,
            name: vm.name,
            state: vm.state,
            egress_policy: vm.egress_policy,
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
        },
        bridge: MicroNetworkBridge {
            name: micro_network_bridge_name(id),
            attached_taps,
        },
        nat: MicroNetworkNat {
            // Masquerading out of the host's single uplink is what having the
            // internet switched on means for a network; off withholds both
            // the NAT rule and the forward permission.
            enabled: network.internet_enabled,
            uplink: crate::handlers::network::read_uplink().unwrap_or_default(),
            source_cidr: network.subnet_cidr,
        },
        firewall: MicroNetworkFirewall {
            east_west_blocked: true,
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
    if fields.is_empty()
        && let Some(subnet) = SubnetSpec::parse(None, &req.subnet_cidr)
        && let Some(conflict) = overlapping_network(&state, subnet, request_id.0).await?
    {
        fields.insert(
            "subnetCidr".to_owned(),
            format!("overlaps {conflict}, which is already in use"),
        );
    }
    if !fields.is_empty() {
        return Err(AppError::validation(fields, request_id.0));
    }
    // Already checked by validate_create; re-parsed here (rather than
    // threaded through) since the request is consumed piecemeal below.
    let id = Uuid::new_v4();
    let subnet = SubnetSpec::parse(Some(id), &req.subnet_cidr)
        .expect("validate_create already accepted this CIDR");
    let gateway = subnet.gateway();

    let network = MicroNetworkResponse {
        id,
        name: req.name,
        subnet_cidr: req.subnet_cidr,
        gateway: gateway.to_string(),
        internet_enabled: req.internet_enabled,
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
        .ensure_micro_network_bridge(network.id, gateway, prefix)
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
/// on or off — firecrab's equivalent of attaching or detaching an AWS
/// internet gateway. The network, its bridge, its addresses and its DHCP
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

    let previous = network.internet_enabled;
    if previous == req.internet_enabled {
        return Ok(Json(network));
    }

    let store = state.store.clone();
    tokio::task::spawn_blocking(move || store.set_micro_network_internet(id, req.internet_enabled))
        .await
        .map_err(|_| AppError::internal(request_id.0))?
        .map_err(|error| match error {
            PersistenceError::MissingMicroNetwork { .. } => AppError::not_found(request_id.0),
            error => {
                tracing::error!(request_id = %request_id.0, %error, "failed to update micro network");
                AppError::internal(request_id.0)
            }
        })?;
    network.internet_enabled = req.internet_enabled;

    // Unlike create/delete, this one is not best-effort: the whole point of
    // the request is the ruleset, so a stored posture the host isn't
    // enforcing would be a wrong answer rather than a degraded one. Rolled
    // back to what it was, which is still what the host has installed.
    if let Err(error) = ensure_all_networks(&state).await {
        tracing::error!(request_id = %request_id.0, micro_network_id = %id, error, "failed to apply micro network internet toggle");
        let store = state.store.clone();
        let _ = tokio::task::spawn_blocking(move || store.set_micro_network_internet(id, previous))
            .await;
        return Err(AppError::internal(request_id.0));
    }

    Ok(Json(network))
}

/// The subnet `candidate` would collide with, if any: the built-in default
/// network or an existing MicroNetwork. Two networks sharing addresses would
/// make the host's routing table ambiguous, so the helper refuses the bridge
/// outright — this just catches it earlier, with a field the form can show.
async fn overlapping_network(
    state: &AppState,
    candidate: SubnetSpec,
    request_id: Uuid,
) -> Result<Option<String>, AppError> {
    if candidate.overlaps(&SubnetSpec::default_network()) {
        return Ok(Some("the default network".to_owned()));
    }

    let store = state.store.clone();
    let existing = tokio::task::spawn_blocking(move || store.list_micro_networks())
        .await
        .map_err(|_| AppError::internal(request_id))?
        .map_err(|error| {
            tracing::error!(request_id = %request_id, %error, "failed to list micro networks");
            AppError::internal(request_id)
        })?;

    Ok(existing.into_iter().find_map(|network| {
        SubnetSpec::parse(Some(network.id), &network.subnet_cidr)
            .filter(|subnet| subnet.overlaps(&candidate))
            .map(|_| format!("MicroNetwork {:?}", network.name))
    }))
}

/// Brings every network's host resources back to the desired state: the
/// default bridge, one bridge per MicroNetwork, the nftables ruleset and
/// dnsmasq's served interfaces. None of that survives a host reboot — they
/// are kernel objects and a child process — so it is re-applied rather than
/// assumed. Every step is idempotent, so calling this repeatedly (at daemon
/// start, on every VM start, after a network is created or deleted) costs
/// nothing when things are already in place.
pub(crate) async fn ensure_all_networks(state: &AppState) -> Result<(), String> {
    let micro_networks = crate::handlers::vms::micro_network_specs(state).await?;

    state
        .network
        .ensure_bridge()
        .await
        .map_err(|error| format!("ensure_bridge failed: {error}"))?;
    for network in &micro_networks {
        state
            .network
            .ensure_micro_network_bridge(network.micro_network_id, network.gateway, network.prefix)
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
        .ensure_firewall(micro_networks)
        .await
        .map_err(|error| format!("ensure_firewall failed: {error}"))?;
    // The apply above flushes the tables when anything changed, taking every
    // per-VM chain with it — so already-running VMs get their policy put
    // back here rather than losing egress until someone restarts them.
    crate::handlers::vms::reapply_running_vm_policies(state).await?;
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
    match SubnetSpec::parse(None, &req.subnet_cidr) {
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
    fields
}

fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
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
            }),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(created.name, "prod");
        assert_eq!(created.subnet_cidr, "172.31.0.0/24");
        assert_eq!(created.gateway, "172.31.0.1");

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

        // ... and the default network's own subnet is just as unavailable.
        let error = create_micro_network(
            State(state.clone()),
            extension(),
            ValidatedJson(CreateMicroNetworkRequest {
                name: "clash".to_owned(),
                subnet_cidr: "172.30.0.0/24".to_owned(),
                internet_enabled: true,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);

        let Json(listed) = list_micro_networks(State(state), extension())
            .await
            .unwrap();
        assert_eq!(listed.len(), 1, "no rejected attempt may leave a record");
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
        assert!(detail.firewall.east_west_blocked);
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

        let subnet = SubnetSpec::parse(Some(network.id), &network.subnet_cidr).unwrap();
        let mut member = crate::handlers::vms::test_support::record("member", Uuid::new_v4());
        member.state = VmState::Stopped;
        member.micro_network_id = Some(network.id);
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
        assert!(operations.contains(&"ensure_bridge"), "{operations:?}");
        assert!(operations.contains(&"ensure_firewall"), "{operations:?}");
        assert!(operations.contains(&"sync_dhcp_leases"), "{operations:?}");
        // Sanity: the two really are distinct networks, not one counted twice.
        assert_ne!(first.id, second.id);
    }

    #[tokio::test]
    async fn reconcile_puts_a_running_vms_policy_back_after_the_ruleset_flush() {
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

        // A VM that is running right now, with the lease its policy is keyed
        // on — the state a toggle has to not break.
        let mut vm = crate::handlers::vms::test_support::record("live", Uuid::new_v4());
        vm.state = VmState::Running;
        state.store.insert(&vm).expect("seed vm");
        state
            .store
            .allocate_lease(vm.id, SubnetSpec::default_network())
            .expect("seed lease");
        log.lock().unwrap().clear();

        ensure_all_networks(&state).await.expect("reconcile");

        let operations = log.lock().unwrap().clone();
        let firewall = operations
            .iter()
            .position(|op| *op == "ensure_firewall")
            .expect("the global ruleset is applied");
        let policy = operations
            .iter()
            .position(|op| *op == "apply_vm_policy")
            .unwrap_or_else(|| panic!("a running VM's policy must be reinstalled: {operations:?}"));
        assert!(
            firewall < policy,
            "the reinstall has to come after the flush, or it is flushed away again: {operations:?}"
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
        crate::network::test_support::spawn_recording_helper(&socket_path, Some("ensure_bridge"));
        let state =
            state.with_test_network(crate::network::NetworkClient::with_socket_path(socket_path));

        // A VM start propagates this; only the daemon-startup caller logs it.
        let error = ensure_all_networks(&state).await.unwrap_err();
        assert!(error.contains("ensure_bridge"), "{error}");
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

        let subnet = SubnetSpec::parse(Some(network.id), &network.subnet_cidr).unwrap();
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
            });
            assert!(
                !fields.contains_key("subnetCidr"),
                "{cidr} should be accepted"
            );
        }
    }

    #[test]
    fn a_created_network_reports_the_gateway_derived_from_its_cidr() {
        let subnet = SubnetSpec::parse(Some(Uuid::nil()), "172.31.0.0/24").unwrap();
        assert_eq!(subnet.gateway().to_string(), "172.31.0.1");
    }
}
