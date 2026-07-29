//! MicroNetwork CRUD (`docs/task-micro-network.md`) — a named CIDR
//! reservation that also provisions a real bridge on the host and is wired
//! into the network services VMs need: its own dnsmasq range, its own NAT
//! rule, and a default deny on traffic routed to any other network. VRF
//! (routing-table separation, so isolation can't depend on a rule being
//! present) is still follow-up work.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use firecrab_api_types::{CreateMicroNetworkRequest, MicroNetworkResponse};
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

/// Re-pushes the firewall ruleset and DHCP config for the current set of
/// MicroNetworks. Best-effort: a failure here leaves the just-created
/// network without working DHCP/NAT until the next VM start re-pushes the
/// same snapshot, which is a degraded network rather than a wrong one — so
/// it is logged instead of failing (and rolling back) an otherwise
/// successful create.
async fn apply_network_services(state: &AppState, request_id: Uuid) {
    let specs = match crate::handlers::vms::micro_network_specs(state).await {
        Ok(specs) => specs,
        Err(error) => {
            tracing::warn!(request_id = %request_id, error, "micro network snapshot failed");
            return;
        }
    };
    if let Err(error) = state.network.ensure_firewall(specs).await {
        tracing::warn!(request_id = %request_id, %error, "firewall resync failed");
    }
    if let Err(error) = crate::handlers::vms::sync_dhcp_leases(state).await {
        tracing::warn!(request_id = %request_id, error, "dhcp resync failed");
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
