//! MicroNetwork CRUD (`docs/task-micro-network.md`) — a named CIDR
//! reservation that now also provisions a real bridge on the host, mirroring
//! how an AWS VPC creation reserves a CIDR block and immediately gets a real
//! implicit router. VRF/firewall namespacing and VM membership are still
//! follow-up work (see the task doc's remaining scope).

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::str::FromStr;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use firecrab_api_types::{CreateMicroNetworkRequest, MicroNetworkResponse};
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::handlers::vms::parse_id;
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
    let fields = validate_create(&req);
    if !fields.is_empty() {
        return Err(AppError::validation(fields, request_id.0));
    }
    // Already checked by validate_create; re-parsed here (rather than
    // threaded through) since the request is consumed piecemeal below.
    let (network_address, prefix) =
        parse_cidr(&req.subnet_cidr).expect("validate_create already accepted this CIDR");
    let gateway = gateway_for(network_address);

    let network = MicroNetworkResponse {
        id: Uuid::new_v4(),
        name: req.name,
        subnet_cidr: req.subnet_cidr,
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

    Ok((StatusCode::CREATED, Json(network)))
}

pub async fn delete_micro_network(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let id = parse_id(&id, request_id.0)?;

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
    match parse_cidr(&req.subnet_cidr) {
        Some((_, prefix)) if !(MIN_PREFIX..=MAX_PREFIX).contains(&prefix) => {
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

/// The MicroNetwork's own address on its subnet — first host address after
/// the network address, same convention as the default network's own
/// `172.30.0.0/24` -> `172.30.0.1` (`firecrab-net-helper/src/bridge.rs`).
fn gateway_for(network_address: Ipv4Addr) -> Ipv4Addr {
    Ipv4Addr::from(u32::from(network_address) + 1)
}

fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Parses `text` as an IPv4 CIDR (`a.b.c.d/prefix`), returning the address
/// and prefix length if both parts are valid.
fn parse_cidr(text: &str) -> Option<(Ipv4Addr, u8)> {
    let (address, prefix) = text.split_once('/')?;
    let address = Ipv4Addr::from_str(address).ok()?;
    let prefix: u8 = prefix.parse().ok()?;
    (prefix <= 32).then_some((address, prefix))
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
    fn parse_cidr_accepts_a_valid_ipv4_cidr_and_rejects_the_rest() {
        assert_eq!(
            parse_cidr("172.31.0.0/24"),
            Some((Ipv4Addr::new(172, 31, 0, 0), 24))
        );
        assert_eq!(parse_cidr("172.31.0.0"), None);
        assert_eq!(parse_cidr("172.31.0.0/33"), None);
        assert_eq!(parse_cidr("not-an-ip/24"), None);
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
    fn gateway_for_is_the_first_host_address() {
        assert_eq!(
            gateway_for(Ipv4Addr::new(172, 31, 0, 0)),
            Ipv4Addr::new(172, 31, 0, 1)
        );
    }
}
