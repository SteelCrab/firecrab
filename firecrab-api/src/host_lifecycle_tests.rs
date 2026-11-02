use super::*;

#[tokio::test]
async fn iac_quick_tunnel_target_receipt_and_api_mirror_recover_the_original_connector() {
    let fixture = Fixture::new(1, false);
    let state = fixture.state().await;
    let mut request = fixture.request(&state);
    request.resource = firecrab_helper_protocol::host_iac::quick_tunnel_resource(Uuid::new_v4());
    request.owner.as_mut().unwrap().key = "firecrab_quick_tunnel.app".into();
    assert!(
        change(state.clone(), Uuid::new_v4(), request.clone())
            .await
            .is_err()
    );
    let connector = request.resource.id.clone();
    drop(state);
    let state = fixture.state().await;
    let recovered = change(state.clone(), Uuid::new_v4(), request.clone())
        .await
        .unwrap();
    assert_eq!(recovered.resource.id, connector);
    assert_eq!(recovered.generation, 1);
    fixture.helper.abort();
    let denied = crate::handlers::pipe::control_quick_tunnel(
        axum::extract::State(state.clone()),
        axum::extract::Path(Uuid::parse_str(&connector).unwrap()),
        axum::Extension(crate::server::RequestId(Uuid::new_v4())),
        axum::Json(serde_json::from_value(serde_json::json!({"action":"stop"})).unwrap()),
    )
    .await
    .unwrap_err();
    use axum::response::IntoResponse;
    assert_eq!(
        denied.into_response().status(),
        axum::http::StatusCode::CONFLICT
    );
    assert_eq!(fixture.target.lock().unwrap().receipts.len(), 1);
    assert_eq!(
        state.store.iac_resource_owner(&request.resource).unwrap(),
        recovered
    );
    assert_eq!(
        change(state, Uuid::new_v4(), request).await.unwrap(),
        recovered
    );
}
use firecrab_helper_protocol::{
    PROTOCOL_VERSION,
    framing::{read_frame, write_frame},
    host_iac::{HostIdentity, OwnerReceipt, OwnerRejection},
    network::{NetworkRequest, NetworkRequestEnvelope, NetworkResponseEnvelope},
};
use firecrab_stack::{Manager, ResourceOwner};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
};

struct Target {
    host: HostIdentity,
    ownership: Option<ResourceOwnership>,
    receipts: BTreeMap<Uuid, OwnerReceipt>,
    reject: bool,
}
impl Target {
    fn reply(&mut self, request: Request) -> Reply {
        match request {
            Request::Observe {
                endpoint_id,
                resource,
            } => Reply::Observation {
                observation: Observation {
                    host: self.host.clone(),
                    ownership: self.ownership.clone().unwrap_or(ResourceOwnership {
                        endpoint_id,
                        resource,
                        generation: 0,
                        owner: None,
                    }),
                    exists: true,
                    parent_controlled: false,
                    configuration_sha256: "b".repeat(64),
                    busy: false,
                },
            },
            Request::ChangeOwner { request } => {
                if self.reject {
                    return Reply::OwnerRejected {
                        rejection: OwnerRejection {
                            operation_id: request.change.operation_id,
                            request_sha256: request.request_sha256().unwrap(),
                            host: self.host.clone(),
                        },
                    };
                }
                let receipt = self
                    .receipts
                    .entry(request.change.operation_id)
                    .or_insert_with(|| OwnerReceipt {
                        operation_id: request.change.operation_id,
                        request_sha256: request.request_sha256().unwrap(),
                        host: self.host.clone(),
                        ownership: ResourceOwnership {
                            endpoint_id: request.change.expected_endpoint_id,
                            resource: request.change.resource.clone(),
                            generation: request.change.expected_generation
                                + u64::from(request.change.expected_owner != request.change.owner),
                            owner: request.change.owner,
                        },
                        confirmed: false,
                    })
                    .clone();
                self.ownership = Some(receipt.ownership.clone());
                Reply::OwnerReceipt {
                    receipt: Some(receipt),
                }
            }
            Request::ConfirmOwner { operation_id, .. } => {
                let receipt = self.receipts.get_mut(&operation_id).unwrap();
                receipt.confirmed = true;
                Reply::OwnerReceipt {
                    receipt: Some(receipt.clone()),
                }
            }
            Request::OwnerReceipt { operation_id, .. } => Reply::OwnerReceipt {
                receipt: self.receipts.get(&operation_id).cloned(),
            },
        }
    }
}
struct Fixture {
    directory: tempfile::TempDir,
    target: Arc<Mutex<Target>>,
    loss: Arc<AtomicU8>,
    helper: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.helper.abort();
    }
}
impl Fixture {
    fn new(loss: u8, reject: bool) -> Self {
        let directory = tempfile::Builder::new()
            .prefix("fc-host")
            .tempdir_in("/private/tmp")
            .or_else(|_| {
                tempfile::Builder::new()
                    .prefix("fc-host")
                    .tempdir_in("/tmp")
            })
            .unwrap();
        let listener =
            tokio::net::UnixListener::bind(directory.path().join("helper.sock")).unwrap();
        let target = Arc::new(Mutex::new(Target {
            host: HostIdentity {
                machine_id: "a".repeat(32),
                configuration_root: "/etc/firecrab".into(),
                device: 1,
                inode: 42,
            },
            ownership: None,
            receipts: BTreeMap::new(),
            reject,
        }));
        let losses = Arc::new(AtomicU8::new(loss));
        let state = target.clone();
        let lose = losses.clone();
        let helper = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let envelope: NetworkRequestEnvelope = read_frame(&mut stream).await.unwrap();
                let NetworkRequest::HostIac { request } = envelope.request else {
                    panic!("unexpected helper operation")
                };
                let stage = match request {
                    Request::ChangeOwner { .. } => 1,
                    Request::ConfirmOwner { .. } => 2,
                    _ => 0,
                };
                let reply = state.lock().unwrap().reply(request);
                if stage != 0
                    && lose
                        .compare_exchange(stage, 0, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                {
                    continue;
                }
                write_frame(
                    &mut stream,
                    &NetworkResponseEnvelope {
                        version: PROTOCOL_VERSION,
                        request_id: envelope.request_id,
                        result: Ok(()),
                        pipe: None,
                        host_iac: Some(reply),
                    },
                )
                .await
                .unwrap();
            }
        });
        Self {
            directory,
            target,
            loss: losses,
            helper,
        }
    }
    async fn state(&self) -> AppState {
        AppState::with_db_file(
            crate::templates::TemplateRegistry::from_specs(
                self.directory.path(),
                std::iter::empty(),
            )
            .unwrap(),
            self.directory.path().join("state.db"),
        )
        .await
        .unwrap()
        .with_test_network(crate::network::NetworkClient::with_socket_path(
            self.directory.path().join("helper.sock"),
        ))
    }
    fn request(&self, state: &AppState) -> ChangeResourceOwner {
        ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: state.store.iac_endpoint_id().unwrap(),
            resource: ManagedResource {
                kind: "ingress".into(),
                id: "listener".into(),
            },
            expected_generation: 0,
            expected_owner: None,
            owner: Some(ResourceOwner {
                manager: Manager::Terraform,
                id: Uuid::new_v4(),
                key: "firecrab_ingress.listener".into(),
            }),
        }
    }
}
#[tokio::test]
async fn iac_host_lost_target_or_confirmation_reply_recovers_after_api_restart_without_a_second_transfer()
 {
    for stage in [1, 2] {
        let fixture = Fixture::new(stage, false);
        let state = fixture.state().await;
        let request = fixture.request(&state);
        assert!(
            change(state.clone(), Uuid::new_v4(), request.clone())
                .await
                .is_err()
        );
        assert_eq!(fixture.loss.load(Ordering::SeqCst), 0);
        assert_eq!(
            state.store.host_owner_status(request.operation_id).unwrap(),
            (false, false)
        );
        drop(state);
        let state = fixture.state().await;
        let result = change(state.clone(), Uuid::new_v4(), request.clone())
            .await
            .unwrap();
        assert_eq!(result.generation, 1);
        assert_eq!(fixture.target.lock().unwrap().receipts.len(), 1);
        assert_eq!(
            state.store.host_owner_status(request.operation_id).unwrap(),
            (true, false)
        );
        fixture.helper.abort();
        assert_eq!(
            change(state.clone(), Uuid::new_v4(), request.clone())
                .await
                .unwrap(),
            result
        );
        let mut substituted = request;
        substituted.owner = None;
        assert!(change(state, Uuid::new_v4(), substituted).await.is_err());
    }
}
#[tokio::test]
async fn iac_host_known_no_effect_rejection_closes_reservation_and_is_readable_offline() {
    let fixture = Fixture::new(0, true);
    let state = fixture.state().await;
    let request = fixture.request(&state);
    assert!(
        change(state.clone(), Uuid::new_v4(), request.clone())
            .await
            .is_err()
    );
    assert_eq!(
        state.store.host_owner_status(request.operation_id).unwrap(),
        (true, true)
    );
    assert_eq!(
        state
            .store
            .iac_resource_owner(&request.resource)
            .unwrap()
            .owner,
        None
    );
    fixture.helper.abort();
    let result = change(state, Uuid::new_v4(), request).await;
    assert!(result.is_err());
}
#[tokio::test]
async fn iac_host_observation_rejects_a_replaced_helper_target_instead_of_returning_a_local_owner()
{
    let fixture = Fixture::new(0, false);
    let state = fixture.state().await;
    let request = fixture.request(&state);
    change(state.clone(), Uuid::new_v4(), request.clone())
        .await
        .unwrap();
    fixture.target.lock().unwrap().host.inode += 1;
    assert!(
        observe(&state, Uuid::new_v4(), request.resource)
            .await
            .is_err()
    );
}
