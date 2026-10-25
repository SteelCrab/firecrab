//! State admission and encrypted credential distribution over real mutual TLS.
use super::*;
use crate::secrets::{Secret, SecretSummary};

fn credential(state: &AppState, value: &str) -> ClusterServiceCredential {
    let id = Uuid::new_v4();
    state
        .secrets
        .write(|contents| {
            contents.secrets.push(Secret {
                summary: SecretSummary {
                    id,
                    name: "session-token".into(),
                    description: String::new(),
                    version: 1,
                    created_at: 0,
                    updated_at: 0,
                    source: None,
                },
                value: value.into(),
            });
            Ok(())
        })
        .unwrap();
    ClusterServiceCredential {
        secret_id: id,
        version: 1,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invalid_state_and_credentials_leave_no_service_or_partial_worker_deployment() {
    let world = World::new(2).await;
    let url = format!("{}/api/cluster/services", world.url);
    let spec = service_spec(
        &world
            .hosts
            .iter()
            .map(|h| h.report.clone())
            .collect::<Vec<_>>(),
    );
    let reference = credential(&world.controller, "contract-test-private-value");
    let mut cases = Vec::new();
    for value in [
        "/data/sessions".to_owned(),
        "redis://127.0.0.1".into(),
        "x".repeat(4097),
        "invalid\0value".into(),
    ] {
        let invalid = credential(&world.controller, &value);
        let mut body = serde_json::to_value(&spec).unwrap();
        body["state"] = serde_json::json!({"mode":"external_state","sessions":"SESSION_URL"});
        body["credentials"]["SESSION_URL"] = serde_json::to_value(invalid).unwrap();
        cases.push(body);
    }
    for mode in ["host_local_durable", "sqlite", "memory", "disk_failover"] {
        let mut body = serde_json::to_value(&spec).unwrap();
        body["state"]["mode"] = mode.into();
        cases.push(body);
    }
    let mut body = serde_json::to_value(&spec).unwrap();
    body["state"]["requestsDuplicateSafe"] = false.into();
    cases.push(body);
    let mut body = serde_json::to_value(&spec).unwrap();
    body["state"]["backgroundWork"] = "exclusive".into();
    cases.push(body);
    for value in [
        None,
        Some("/data/sessions"),
        Some("sqlite:///data/db"),
        Some("file:///uploads"),
        Some("redis://127.0.0.1"),
        Some("http://localhost./sessions"),
        Some("redis://[::1]"),
        Some("redis://[::ffff:127.0.0.1]"),
        Some("redis://[::]"),
        Some("redis://0.0.0.0"),
    ] {
        let mut body = serde_json::to_value(&spec).unwrap();
        body["state"] = serde_json::json!({"mode":"external_state","sessions":"SESSION_URL"});
        if let Some(value) = value {
            body["service"]["env"]["SESSION_URL"] = value.into();
        }
        cases.push(body);
    }
    let mut body = serde_json::to_value(&spec).unwrap();
    body["state"]["mode"] = "external_state".into();
    cases.push(body);
    let mut body = serde_json::to_value(&spec).unwrap();
    body["state"]["sessions"] = "SESSION_URL".into();
    body["service"]["env"]["SESSION_URL"] = "redis://shared.test".into();
    cases.push(body);
    for reference in [
        ClusterServiceCredential {
            secret_id: Uuid::new_v4(),
            version: 1,
        },
        ClusterServiceCredential {
            version: 2,
            ..reference.clone()
        },
        ClusterServiceCredential {
            version: 0,
            ..reference.clone()
        },
        ClusterServiceCredential {
            secret_id: Uuid::nil(),
            version: 1,
        },
    ] {
        let mut body = serde_json::to_value(&spec).unwrap();
        body["credentials"]["TOKEN"] = serde_json::to_value(reference).unwrap();
        cases.push(body);
    }
    for name in ["", "BAD=NAME", "TOKEN"] {
        let mut body = serde_json::to_value(&spec).unwrap();
        body["credentials"][name] = serde_json::to_value(&reference).unwrap();
        body["service"]["env"][name] = "collision".into();
        cases.push(body);
    }
    let mut body = serde_json::to_value(&spec).unwrap();
    body["service"]["secrets"]["TOKEN"] = reference.secret_id.to_string().into();
    cases.push(body);
    for (index, body) in cases.into_iter().enumerate() {
        let response = world
            .client()
            .post(&url)
            .header("Idempotency-Key", format!("invalid-{index}"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "case {index}");
        assert!(
            !response
                .text()
                .await
                .unwrap()
                .contains("contract-test-private-value")
        );
        assert!(
            world
                .controller
                .store
                .cluster_services()
                .unwrap()
                .is_empty()
        );
        for host in &world.hosts {
            assert!(
                host.state
                    .store
                    .cluster_service_workers()
                    .unwrap()
                    .is_empty()
            );
            assert!(host.state.store.list_micro_services().unwrap().is_empty());
            assert!(host.state.store.load_all().unwrap().is_empty());
        }
    }
    assert!(
        world
            .controller
            .store
            .cluster_hosts(now_ms())
            .unwrap()
            .iter()
            .all(|host| host.reserved == HostCapacity::default())
    );
    let response = world
        .client()
        .post(&url)
        .header("Idempotency-Key", "valid")
        .json(&spec)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    let service: ClusterService = response.json().await.unwrap();
    let mut invalid = spec;
    invalid.credentials.insert(
        "TOKEN".into(),
        ClusterServiceCredential {
            secret_id: Uuid::new_v4(),
            version: 1,
        },
    );
    let response = world
        .client()
        .put(format!("{url}/{}", service.id))
        .header("Idempotency-Key", "invalid-update")
        .json(&UpdateClusterServiceRequest {
            expected_generation: 1,
            spec: invalid,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(
        world
            .controller
            .store
            .cluster_service(service.id)
            .unwrap()
            .generation,
        1
    );
    assert_eq!(
        world
            .controller
            .store
            .cluster_service_operations(service.id)
            .unwrap()
            .len(),
        1
    );
    for host in &world.hosts {
        assert!(
            host.state
                .store
                .cluster_service_worker(service.id)
                .unwrap()
                .is_none_or(|receipt| receipt.intent.generation == 1)
        );
    }
}

#[test]
fn release_generation_also_fences_configuration_and_credential_scope() {
    let (_root, store, _) = world(1);
    let service = create(&store, service_spec(&[report(1)]));
    let original = service.hosts[0].intent.clone();
    let owner = Uuid::from_u128(500);
    store
        .cluster_service_worker_admit(service.id, owner, original.clone())
        .unwrap();
    for index in 0..4 {
        let mut changed = original.clone();
        changed.generation += 1;
        match index {
            0 => {
                changed.spec.env.insert("CONFIG".into(), "different".into());
            }
            1 => changed.state.background_work = ClusterServiceBackgroundWork::DuplicateSafe,
            2 => {
                changed.credentials.insert(
                    "TOKEN".into(),
                    ClusterServiceCredential {
                        secret_id: Uuid::new_v4(),
                        version: 1,
                    },
                );
            }
            _ => changed.deployment_id = Uuid::new_v4(),
        }
        assert!(matches!(
            store.cluster_service_worker_admit(service.id, owner, changed),
            Err(ClusterError::Conflict(_))
        ));
    }
    assert_eq!(
        store
            .cluster_service_worker(service.id)
            .unwrap()
            .unwrap()
            .intent,
        original
    );
}

async fn wait_snapshots(world: &World, service: &ClusterService) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let ready = world
            .hosts
            .iter()
            .zip(&service.hosts)
            .all(|(host, target)| {
                host.state
                    .store
                    .cluster_service_worker(service.id)
                    .unwrap()
                    .is_some_and(|r| r.intent == target.intent)
                    && host
                        .state
                        .secrets
                        .read(|contents| {
                            Ok(contents
                                .service_environments
                                .contains_key(&target.intent.deployment_id))
                        })
                        .unwrap_or(false)
            });
        if ready {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "credential fan-out did not settle for generation {}: {:?}",
            service.generation,
            world
                .hosts
                .iter()
                .map(|host| (
                    host.state
                        .store
                        .cluster_service_worker(service.id)
                        .unwrap()
                        .map(|r| (r.intent.generation, r.error)),
                    host.state
                        .secrets
                        .read(|contents| Ok(contents
                            .service_environments
                            .keys()
                            .copied()
                            .collect::<Vec<_>>()))
                        .unwrap_or_default()
                ))
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pinned_credentials_survive_source_rotation_controller_restart_and_request_movement() {
    let world = World::new(2).await;
    let token = "contract-test-private-session-token";
    let reference = credential(&world.controller, token);
    let mut spec = service_spec(
        &world
            .hosts
            .iter()
            .map(|h| h.report.clone())
            .collect::<Vec<_>>(),
    );
    spec.state.mode = ClusterServiceStateMode::ExternalState;
    spec.state.sessions = Some("SESSION_URL".into());
    spec.state.background_work = ClusterServiceBackgroundWork::DuplicateSafe;
    spec.service
        .env
        .insert("SESSION_URL".into(), "http://sessions.test".into());
    spec.credentials
        .insert("SESSION_TOKEN".into(), reference.clone());
    let url = format!("{}/api/cluster/services", world.url);
    let response = world
        .client()
        .post(&url)
        .header("Idempotency-Key", "pinned")
        .json(&spec)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    let service: ClusterService = response.json().await.unwrap();
    wait_snapshots(&world, &service).await;
    world
        .controller
        .secrets
        .write(|contents| {
            contents.secrets[0].summary.version = 2;
            contents.secrets[0].value = "rotated-source-token".into();
            Ok(())
        })
        .unwrap();
    let mut restarted = state(&world.root.path().join("controller")).await;
    restarted.cluster = world.controller.cluster.clone();
    reconcile_service(restarted.clone(), service.id, true)
        .await
        .unwrap();
    let retry = world
        .client()
        .post(&url)
        .header("Idempotency-Key", "pinned")
        .json(&spec)
        .send()
        .await
        .unwrap();
    assert_eq!(retry.status(), 202);
    assert_eq!(retry.json::<ClusterService>().await.unwrap().generation, 1);
    let mut environments = Vec::new();
    for (host, target) in world.hosts.iter().zip(&service.hosts) {
        let _lock = host.state.cluster.lock_worker(service.id).await;
        assert_eq!(target.intent.credentials, spec.credentials);
        assert_eq!(target.intent.release_generation, 1);
        assert_eq!(target.intent.spec.image, spec.service.image);
        host.state
            .store
            .insert_micro_service(&MicroServiceResponse {
                id: service.id,
                spec: target.intent.spec.clone(),
            })
            .unwrap();
        let mut deployment = stopped(service.id, &target.intent);
        deployment.phase = ServicePhase::Running;
        let mut vm = crate::handlers::vms::test_support::record("session-replica", Uuid::new_v4());
        vm.env = target.intent.spec.env.clone();
        host.state.store.insert(&vm).unwrap();
        host.state.vms.lock().unwrap().insert(vm.id, vm.clone());
        deployment.instances.push(ServiceInstance {
            vm_id: vm.id,
            ipv4: "127.0.0.1".into(),
            host_port: 30100,
            state: VmState::Running,
            healthy: true,
        });
        host.state
            .store
            .save_service_deployment(&deployment)
            .unwrap();
        let reopened = state(
            &world
                .root
                .path()
                .join(format!("worker-{}", host.report.id.as_u128())),
        )
        .await;
        let env = crate::service_runtime::secrets::guest_env(&reopened, &vm)
            .await
            .unwrap();
        assert_eq!(env["SESSION_TOKEN"], token);
        assert!(!vm.env.contains_key("SESSION_TOKEN"));
        environments.push(env);
        let public =
            serde_json::to_string(&host.state.store.cluster_service_worker(service.id).unwrap())
                .unwrap();
        assert!(!public.contains(token));
        for file in ["state.db", "state.secrets/vault.bin"] {
            let bytes = std::fs::read(
                world
                    .root
                    .path()
                    .join(format!("worker-{}", host.report.id.as_u128()))
                    .join(file),
            )
            .unwrap();
            assert!(
                !bytes
                    .windows(token.len())
                    .any(|window| window == token.as_bytes())
            );
        }
    }
    assert_eq!(environments[0], environments[1]);
    let public = world
        .client()
        .get(format!("{url}/{}/operations", service.id))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!public.contains(token) && !public.contains("rotated-source-token"));
    for file in ["state.db", "state.secrets/vault.bin"] {
        let bytes = std::fs::read(world.root.path().join("controller").join(file)).unwrap();
        assert!(
            !bytes
                .windows(token.len())
                .any(|window| window == token.as_bytes())
        );
    }
    external_session_moves_between_replicas(environments, token).await;
    let mut next = spec;
    next.credentials.get_mut("SESSION_TOKEN").unwrap().version = 2;
    let response = world
        .client()
        .put(format!("{url}/{}", service.id))
        .header("Idempotency-Key", "rotate-release")
        .json(&UpdateClusterServiceRequest {
            expected_generation: 1,
            spec: next,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    let updated: ClusterService = response.json().await.unwrap();
    wait_snapshots(&world, &updated).await;
    for (host, target) in world.hosts.iter().zip(&updated.hosts) {
        assert_eq!(target.intent.release_generation, 2);
        let values = host
            .state
            .secrets
            .read(
                |contents| Ok(contents.service_environments[&target.intent.deployment_id].clone()),
            )
            .unwrap();
        assert_eq!(values["SESSION_TOKEN"], "rotated-source-token");
    }
}

#[test]
fn external_sessions_uploads_and_database_share_the_pinned_configuration() {
    let (_root, store, _) = world(2);
    let mut spec = service_spec(&[report(1), report(2)]);
    spec.state = ClusterServiceState {
        mode: ClusterServiceStateMode::ExternalState,
        sessions: Some("SESSION_URL".into()),
        uploads: Some("UPLOAD_URL".into()),
        database: Some("DATABASE_URL".into()),
        ..Default::default()
    };
    spec.service.env = BTreeMap::from([
        (
            "SESSION_URL".into(),
            "rediss://sessions.example:6379/0".into(),
        ),
        ("UPLOAD_URL".into(), "s3://shared-uploads".into()),
        (
            "DATABASE_URL".into(),
            "postgresql://database.example/app".into(),
        ),
    ]);
    let service = create(&store, spec.clone());
    for host in service.hosts {
        assert_eq!(host.intent.state, spec.state);
        assert_eq!(host.intent.spec.env, spec.service.env);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_rejects_credential_scope_changes_and_stop_needs_no_controller_values() {
    let world = World::new(1).await;
    let reference = credential(&world.controller, "pinned-worker-token");
    let mut spec = service_spec(&[world.hosts[0].report.clone()]);
    spec.credentials.insert("TOKEN".into(), reference);
    let url = format!("{}/api/cluster/services", world.url);
    let response = world
        .client()
        .post(&url)
        .header("Idempotency-Key", "scoped")
        .json(&spec)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    let service: ClusterService = response.json().await.unwrap();
    wait_snapshots(&world, &service).await;
    let host = &world.hosts[0];
    let worker_url = format!(
        "{}/api/cluster/worker/services/{}",
        host.report.endpoint, service.id
    );
    let client = world.controller.cluster.client(host.report.id).unwrap();
    for values in [
        serde_json::json!({}),
        serde_json::json!({"TOKEN":"pinned-worker-token","UNSCOPED":"extra"}),
        serde_json::json!({"TOKEN":"changed-worker-token"}),
    ] {
        let response = client
            .put(&worker_url)
            .json(&serde_json::json!({"intent":service.hosts[0].intent,"credentialValues":values}))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_client_error() || response.status().is_server_error());
        let stored = host
            .state
            .secrets
            .read(|contents| {
                Ok(contents.service_environments[&service.hosts[0].intent.deployment_id].clone())
            })
            .unwrap();
        assert_eq!(stored["TOKEN"], "pinned-worker-token");
    }
    world
        .controller
        .secrets
        .write(|contents| {
            contents.cluster_credentials.clear();
            contents.secrets.clear();
            Ok(())
        })
        .unwrap();
    let response = world
        .client()
        .post(format!("{url}/{}/stop", service.id))
        .header("Idempotency-Key", "stop-scoped")
        .json(&StopClusterServiceRequest {
            expected_generation: 1,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        reconcile_service(world.controller.clone(), service.id, true)
            .await
            .unwrap();
        if world
            .controller
            .store
            .cluster_service(service.id)
            .unwrap()
            .observed
            == ClusterServiceObserved::Stopped
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Stop depended on unavailable controller credentials"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        host.state
            .secrets
            .read(|contents| Ok(contents.service_environments.is_empty()))
            .unwrap()
    );
}

/// Application fixture uses the injected endpoint/token; no guest boot is claimed.
async fn external_session_moves_between_replicas(
    environments: Vec<BTreeMap<String, String>>,
    token: &str,
) {
    use axum::{
        Router,
        extract::Path,
        http::{HeaderMap, Method, StatusCode},
        routing::any,
    };
    let sessions = Arc::new(std::sync::Mutex::new(BTreeMap::<String, String>::new()));
    let store = Router::new().route(
        "/sessions/{id}",
        any({
            let sessions = sessions.clone();
            let token = token.to_owned();
            move |Path(id): Path<String>, method: Method, headers: HeaderMap, body: String| {
                let sessions = sessions.clone();
                let token = token.clone();
                async move {
                    if headers.get("Authorization").and_then(|v| v.to_str().ok())
                        != Some(token.as_str())
                    {
                        return (StatusCode::UNAUTHORIZED, String::new());
                    }
                    if method == Method::PUT {
                        sessions.lock().unwrap().insert(id.clone(), body);
                    }
                    let value = sessions
                        .lock()
                        .unwrap()
                        .get(&id)
                        .cloned()
                        .unwrap_or_default();
                    (StatusCode::OK, value)
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let store_task = tokio::spawn(async move { axum::serve(listener, store).await.unwrap() });
    let mut replicas = Vec::new();
    for env in environments {
        let client = reqwest::Client::builder()
            .no_proxy()
            .resolve("sessions.test", address)
            .build()
            .unwrap();
        let app = Router::new().route(
            "/session",
            any(move |method: Method, body: String| {
                let client = client.clone();
                let env = env.clone();
                async move {
                    let response = client
                        .request(
                            method,
                            format!("{}/sessions/shared-cookie", env["SESSION_URL"]),
                        )
                        .header("Authorization", &env["SESSION_TOKEN"])
                        .body(body)
                        .send()
                        .await
                        .unwrap();
                    assert_eq!(response.status(), 200);
                    response.text().await.unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/session", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        replicas.push((endpoint, task));
    }
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .put(&replicas[0].0)
            .body("session-survives-movement")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "session-survives-movement"
    );
    assert_eq!(
        client
            .get(&replicas[1].0)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "session-survives-movement"
    );
    for (_, task) in replicas {
        task.abort();
    }
    store_task.abort();
}
