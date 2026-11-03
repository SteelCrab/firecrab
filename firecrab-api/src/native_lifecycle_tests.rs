//! Real TLS and API/SQLite recovery with an explicit native metadata reply fixture.
use super::*;
use firecrab_helper_protocol::native_iac::{
    Platform, Receipt, Target,
    transport::{Client, Server},
};
use firecrab_stack::{Manager, ResourceOwner};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
};

async fn state(directory: &std::path::Path) -> AppState {
    AppState::with_db_file(
        crate::templates::TemplateRegistry::from_specs(directory, std::iter::empty()).unwrap(),
        directory.join("state.db"),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn iac_native_original_operation_recovers_both_lost_acknowledgements_over_real_mtls() {
    for loss_stage in [1, 2] {
        let directory = tempfile::tempdir().unwrap();
        let mut api = state(directory.path()).await;
        let endpoint = api.store.iac_endpoint_id().unwrap();
        let target = Target {
            platform: Platform::Macos,
            installation_id: Uuid::new_v4(),
            machine_sha256: "a".repeat(64),
            configuration_root: "/fixture/native-installation".into(),
            directory_id: "1:42".into(),
        };
        let ca = directory.path().join("ca");
        firecrab_pki::init_ca(&ca).unwrap();
        let server_paths =
            firecrab_pki::issue_host_cert(&ca, target.installation_id, vec!["localhost".into()], 1)
                .unwrap();
        let client_paths = firecrab_pki::issue_host_cert(&ca, endpoint, vec![], 1).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = Server::new(&server_paths, target.installation_id, endpoint).unwrap();
        let client = Arc::new(
            Client::new(
                listener.local_addr().unwrap().to_string(),
                "localhost".into(),
                target.installation_id,
                endpoint,
                &client_paths,
            )
            .unwrap(),
        );
        api.native_host = Some(client.clone());
        let receipts = Arc::new(Mutex::new(BTreeMap::<Uuid, Receipt>::new()));
        let retained = receipts.clone();
        let native = target.clone();
        let loss = Arc::new(AtomicU8::new(loss_stage));
        let dropped = loss.clone();
        let broker = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let Ok((mut stream, request)) = server.accept(socket).await else {
                    continue;
                };
                let stage = match &request {
                    Request::ChangeOwner { .. } => 1,
                    Request::ConfirmOwner { .. } => 2,
                    _ => 0,
                };
                let reply = {
                    let mut retained = retained.lock().unwrap();
                    match request {
                        Request::Identity => Reply::Identity {
                            target: native.clone(),
                            kinds: vec!["management_settings".into()],
                        },
                        Request::Observe {
                            endpoint_id,
                            resource,
                        } => Reply::Observation {
                            observation: Observation {
                                target: native.clone(),
                                ownership: ResourceOwnership {
                                    endpoint_id,
                                    resource,
                                    generation: 0,
                                    owner: None,
                                },
                                exists: true,
                                busy: false,
                                configuration_sha256: "b".repeat(64),
                            },
                        },
                        Request::ChangeOwner { request } => {
                            let receipt = retained
                                .entry(request.change.operation_id)
                                .or_insert_with(|| Receipt {
                                    operation_id: request.change.operation_id,
                                    request_sha256: request.request_sha256().unwrap(),
                                    target: native.clone(),
                                    ownership: ResourceOwnership {
                                        endpoint_id: request.change.expected_endpoint_id,
                                        resource: request.change.resource,
                                        generation: 1,
                                        owner: request.change.owner,
                                    },
                                    confirmed: false,
                                })
                                .clone();
                            Reply::OwnerReceipt {
                                receipt: Some(receipt),
                            }
                        }
                        Request::ConfirmOwner { operation_id, .. } => {
                            let receipt = retained.get_mut(&operation_id).unwrap();
                            receipt.confirmed = true;
                            Reply::OwnerReceipt {
                                receipt: Some(receipt.clone()),
                            }
                        }
                        Request::OwnerReceipt { operation_id, .. } => Reply::OwnerReceipt {
                            receipt: retained.get(&operation_id).cloned(),
                        },
                    }
                };
                if stage != 0
                    && dropped
                        .compare_exchange(stage, 0, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                {
                    continue;
                }
                Server::reply(&mut stream, &reply).await.unwrap();
            }
        });
        let request = ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: endpoint,
            resource: ManagedResource {
                kind: "management_settings".into(),
                id: target.installation_id.to_string(),
            },
            expected_generation: 0,
            expected_owner: None,
            owner: Some(ResourceOwner {
                manager: Manager::Terraform,
                id: Uuid::new_v4(),
                key: "firecrab_management_settings.host".into(),
            }),
        };
        assert!(
            change(api.clone(), Uuid::new_v4(), request.clone())
                .await
                .is_err()
        );
        assert_eq!(loss.load(Ordering::SeqCst), 0);
        assert_eq!(
            api.store.host_owner_status(request.operation_id).unwrap(),
            (false, false)
        );
        assert_eq!(
            api.store
                .native_host_owner_intent(request.operation_id)
                .unwrap()
                .unwrap()
                .target,
            target
        );
        drop(api);
        let mut api = state(directory.path()).await;
        api.native_host = Some(client);
        let result = change(api.clone(), Uuid::new_v4(), request.clone())
            .await
            .unwrap();
        assert_eq!(result.generation, 1);
        assert_eq!(result.resource, request.resource);
        assert_eq!(receipts.lock().unwrap().len(), 1);
        assert!(receipts.lock().unwrap()[&request.operation_id].confirmed);
        assert_eq!(
            api.store.host_owner_status(request.operation_id).unwrap(),
            (true, false)
        );
        broker.abort();
        api.native_host = None;
        assert_eq!(
            change(api.clone(), Uuid::new_v4(), request.clone())
                .await
                .unwrap(),
            result
        );
        let mut different = request.clone();
        different.owner.as_mut().unwrap().key.push_str("_changed");
        assert!(
            change(api.clone(), Uuid::new_v4(), different)
                .await
                .is_err()
        );
        assert_eq!(
            api.store.iac_resource_owner(&request.resource).unwrap(),
            result
        );
    }
}

#[tokio::test]
async fn iac_native_missing_broker_never_adopts_guest_descriptor_or_creates_an_intent() {
    let directory = tempfile::tempdir().unwrap();
    let api = state(directory.path()).await;
    let request = ChangeResourceOwner {
        operation_id: Uuid::new_v4(),
        expected_endpoint_id: api.store.iac_endpoint_id().unwrap(),
        resource: ManagedResource {
            kind: "management_settings".into(),
            id: Uuid::new_v4().to_string(),
        },
        expected_generation: 0,
        expected_owner: None,
        owner: Some(ResourceOwner {
            manager: Manager::Terraform,
            id: Uuid::new_v4(),
            key: "firecrab_management_settings.host".into(),
        }),
    };
    assert!(
        change(api.clone(), Uuid::new_v4(), request.clone())
            .await
            .is_err()
    );
    assert!(
        api.store
            .native_host_owner_intent(request.operation_id)
            .unwrap()
            .is_none()
    );
    assert!(api.store.list_iac_resource_owners().unwrap().is_empty());
}

#[tokio::test]
async fn iac_native_management_and_service_configuration_use_distinct_authenticated_realms() {
    let directory = tempfile::tempdir().unwrap();
    let mut api = state(directory.path()).await;
    let endpoint = api.store.iac_endpoint_id().unwrap();
    let ca = directory.path().join("ca");
    firecrab_pki::init_ca(&ca).unwrap();
    let client_paths = firecrab_pki::issue_host_cert(&ca, endpoint, vec![], 1).unwrap();
    let state_owner = Uuid::new_v4();
    let mut brokers = Vec::new();
    let mut clients = Vec::new();
    let mut resources = Vec::new();
    for (platform, kind, root) in [
        (Platform::Macos, "management_settings", "/fixture/outer"),
        (Platform::Linux, "host_configuration", "/etc/firecrab"),
    ] {
        let target = Target {
            platform,
            installation_id: Uuid::new_v4(),
            machine_sha256: "a".repeat(64),
            configuration_root: root.into(),
            directory_id: "1:42:1234".into(),
        };
        let server_paths =
            firecrab_pki::issue_host_cert(&ca, target.installation_id, vec!["localhost".into()], 1)
                .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = Server::new(&server_paths, target.installation_id, endpoint).unwrap();
        clients.push(Arc::new(
            Client::new(
                listener.local_addr().unwrap().to_string(),
                "localhost".into(),
                target.installation_id,
                endpoint,
                &client_paths,
            )
            .unwrap(),
        ));
        resources.push(ManagedResource {
            kind: kind.into(),
            id: target.installation_id.to_string(),
        });
        brokers.push(tokio::spawn(async move {
            let mut receipts = BTreeMap::<Uuid, Receipt>::new();
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let Ok((mut stream, request)) = server.accept(socket).await else {
                    continue;
                };
                let reply = match request {
                    Request::Identity => Reply::Identity {
                        target: target.clone(),
                        kinds: vec![kind.into()],
                    },
                    Request::Observe {
                        endpoint_id,
                        resource,
                    } => {
                        assert_eq!(resource.kind, kind);
                        assert_eq!(resource.id, target.installation_id.to_string());
                        Reply::Observation {
                            observation: Observation {
                                target: target.clone(),
                                ownership: ResourceOwnership {
                                    endpoint_id,
                                    resource,
                                    generation: 0,
                                    owner: None,
                                },
                                exists: true,
                                busy: false,
                                configuration_sha256: "b".repeat(64),
                            },
                        }
                    }
                    Request::ChangeOwner { request } => {
                        assert_eq!(request.target, target);
                        assert_eq!(request.change.resource.kind, kind);
                        let receipt = Receipt {
                            operation_id: request.change.operation_id,
                            request_sha256: request.request_sha256().unwrap(),
                            target: target.clone(),
                            ownership: ResourceOwnership {
                                endpoint_id: request.change.expected_endpoint_id,
                                resource: request.change.resource,
                                generation: 1,
                                owner: request.change.owner,
                            },
                            confirmed: false,
                        };
                        receipts.insert(receipt.operation_id, receipt.clone());
                        Reply::OwnerReceipt {
                            receipt: Some(receipt),
                        }
                    }
                    Request::ConfirmOwner { operation_id, .. } => {
                        let receipt = receipts.get_mut(&operation_id).unwrap();
                        receipt.confirmed = true;
                        Reply::OwnerReceipt {
                            receipt: Some(receipt.clone()),
                        }
                    }
                    Request::OwnerReceipt { operation_id, .. } => Reply::OwnerReceipt {
                        receipt: receipts.get(&operation_id).cloned(),
                    },
                };
                Server::reply(&mut stream, &reply).await.unwrap();
            }
        }));
    }
    api.native_host = Some(clients[0].clone());
    api.native_configuration = Some(clients[1].clone());
    let discovery = crate::handlers::iac::identity(
        axum::extract::State(api.clone()),
        axum::Extension(crate::server::RequestId(Uuid::new_v4())),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(discovery["nativeTargets"].as_array().unwrap().len(), 2);
    for resource in &resources {
        assert!(
            discovery["transferKinds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|kind| kind == &resource.kind)
        );
        let request = ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: endpoint,
            resource: resource.clone(),
            expected_generation: 0,
            expected_owner: None,
            owner: Some(ResourceOwner {
                manager: Manager::Terraform,
                id: state_owner,
                key: format!("firecrab_{}.host", resource.kind),
            }),
        };
        assert_eq!(
            change(api.clone(), Uuid::new_v4(), request)
                .await
                .unwrap()
                .resource,
            *resource
        );
    }
    let records = api.store.list_iac_resource_owners().unwrap();
    assert_eq!(records.len(), 2);
    assert_ne!(records[0].resource.id, records[1].resource.id);
    for broker in brokers {
        broker.abort();
    }
}
