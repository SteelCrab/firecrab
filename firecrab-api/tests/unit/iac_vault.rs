use super::*;

#[tokio::test]
async fn iac_vault_retry_survives_corruption_but_new_changes_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    for kind in ["secret", "secret_connection"] {
        let resource = configuration_resource(&state, root.path(), network, kind);
        let change = claim_configuration(&state, &resource);
        let (status, owned) =
            call(&app, "POST", "/api/iac/owners/transition", change.clone()).await;
        assert_eq!(status, StatusCode::OK, "{owned}");
        let vault = root.path().join("state.secrets/vault.bin");
        let encrypted = std::fs::read(&vault).unwrap();
        std::fs::write(&vault, b"corrupted ciphertext").unwrap();
        assert_eq!(
            call(&app, "POST", "/api/iac/owners/transition", change.clone())
                .await
                .1,
            owned
        );
        let mut conflicting = change.clone();
        conflicting["owner"]["key"] = json!("different-address");
        assert_eq!(
            call(&app, "POST", "/api/iac/owners/transition", conflicting)
                .await
                .0,
            StatusCode::CONFLICT
        );
        let release = json!({"operationId":Uuid::new_v4(),"expectedEndpointId":owned["endpointId"],
            "resource":resource,"expectedGeneration":1,"expectedOwner":owned["owner"],"owner":null});
        let operation = Uuid::parse_str(release["operationId"].as_str().unwrap()).unwrap();
        let (status, error) = call(&app, "POST", "/api/iac/owners/transition", release).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{error}");
        assert!(!error.to_string().contains("fixture-credential-never-echo"));
        assert!(
            state
                .store
                .iac_ownership_operation(operation)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            state
                .store
                .iac_resource_owner(&resource)
                .unwrap()
                .generation,
            1
        );
        std::fs::write(&vault, encrypted).unwrap();
    }
    for file in ["state.db", "state.db-wal", "state.secrets/vault.bin"] {
        if let Ok(bytes) = std::fs::read(root.path().join(file)) {
            assert!(
                !bytes
                    .windows(b"fixture-credential-never-echo".len())
                    .any(|part| part == b"fixture-credential-never-echo")
            );
        }
    }
}

#[tokio::test]
async fn iac_vault_import_checks_all_writers_before_replacing_any_value() {
    use crate::secrets::{Change, ChangeSummary, Direction, Plan, PlannedValue};
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    let secret = configuration_resource(&state, root.path(), network, "secret");
    let connection = configuration_resource(&state, root.path(), network, "secret_connection");
    let (status, unmanaged) = call(
        &app,
        "POST",
        "/api/secrets",
        json!({"name":"UNMANAGED","value":"unmanaged-before"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let plan = Uuid::new_v4();
    state
        .secrets
        .write(|contents| {
            contents.plans.push(Plan {
                account: None,
                id: plan,
                connection_id: Uuid::parse_str(&connection.id).unwrap(),
                connection_version: 1,
                direction: Direction::Import,
                created_at: crate::secrets::now_ms(),
                values: vec![
                    PlannedValue {
                        summary: ChangeSummary {
                            name: "UNMANAGED".into(),
                            change: Change::Update,
                        },
                        value: "unmanaged-after".into(),
                        local_id: Some(Uuid::parse_str(unmanaged["id"].as_str().unwrap()).unwrap()),
                        local_version: Some(1),
                        remote_value: None,
                    },
                    PlannedValue {
                        summary: ChangeSummary {
                            name: "OWNED_SECRET".into(),
                            change: Change::Update,
                        },
                        value: "owned-after".into(),
                        local_id: Some(Uuid::parse_str(&secret.id).unwrap()),
                        local_version: Some(1),
                        remote_value: None,
                    },
                ],
            });
            Ok(())
        })
        .unwrap();
    let (status, owned) = call(
        &app,
        "POST",
        "/api/iac/owners/transition",
        claim_configuration(&state, &secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let file = root.path().join("state.secrets/vault.bin");
    let before = std::fs::read(&file).unwrap();
    let path = format!("/api/secrets/plans/{plan}/apply");
    let selection = json!({"names":["UNMANAGED","OWNED_SECRET"]});
    let (status, error) = call(&app, "POST", &path, selection.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "iac_resource_managed");
    assert_eq!(std::fs::read(&file).unwrap(), before);
    let current = state
        .secrets
        .read(|contents| {
            Ok(contents
                .secrets
                .iter()
                .find(|item| item.summary.name == "UNMANAGED")
                .unwrap()
                .value
                .clone())
        })
        .unwrap();
    assert_eq!(current, "unmanaged-before");
    // Explicit operator reveal remains separate from ordinary IaC observation.
    let (status, revealed) = call(
        &app,
        "POST",
        &format!("/api/secrets/{}/reveal", secret.id),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(revealed["value"], "fixture-credential-never-echo");
    let released = call(
        &app,
        "POST",
        "/api/iac/owners/transition",
        json!({"operationId":Uuid::new_v4(),
        "expectedEndpointId":owned["endpointId"],"resource":secret,"expectedGeneration":1,
        "expectedOwner":owned["owner"],"owner":null}),
    )
    .await;
    assert_eq!(released.0, StatusCode::OK, "{}", released.1);
    assert_eq!(call(&app, "POST", &path, selection).await.0, StatusCode::OK);
    state
        .secrets
        .read(|contents| {
            assert!(contents.plans.iter().all(|item| item.id != plan));
            assert_eq!(
                contents
                    .secrets
                    .iter()
                    .find(|item| item.summary.name == "UNMANAGED")
                    .unwrap()
                    .value,
                "unmanaged-after"
            );
            assert_eq!(
                contents
                    .secrets
                    .iter()
                    .find(|item| item.summary.name == "OWNED_SECRET")
                    .unwrap()
                    .value,
                "owned-after"
            );
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn iac_vault_native_writer_checks_authority_inside_the_file_lock() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    let resource = configuration_resource(&state, root.path(), network, "secret");
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/iac/owners/transition",
            claim_configuration(&state, &resource)
        )
        .await
        .0,
        StatusCode::OK
    );
    // Direct invocation represents blocking work already dispatched before its
    // HTTP guard or waiter ended. It must check the latest committed authority.
    let error = crate::handlers::secrets::update(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(resource.id),
        ValidatedJson(
            serde_json::from_value(json!({
                "name":"OWNED_SECRET","version":1,"value":"late-overwrite"
            }))
            .unwrap(),
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "iac_resource_managed");
    state
        .secrets
        .read(|contents| {
            assert_eq!(contents.secrets[0].value, "fixture-credential-never-echo");
            assert_eq!(contents.secrets[0].summary.version, 1);
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn iac_vault_adoption_waits_for_the_real_file_lock() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    let resource = configuration_resource(&state, root.path(), network, "secret");
    let change = claim_configuration(&state, &resource);
    let operation = Uuid::parse_str(change["operationId"].as_str().unwrap()).unwrap();
    let vault = state.secrets.clone();
    let (locked, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let holding = tokio::task::spawn_blocking(move || {
        vault
            .read(|_| {
                locked.send(()).unwrap();
                wait.recv().unwrap();
                Ok(())
            })
            .unwrap()
    });
    ready.await.unwrap();
    let mut adoption =
        tokio::spawn(async move { call(&app, "POST", "/api/iac/owners/transition", change).await });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), &mut adoption)
            .await
            .is_err()
    );
    assert_eq!(
        state
            .store
            .iac_resource_owner(&resource)
            .unwrap()
            .generation,
        0
    );
    assert!(
        state
            .store
            .iac_ownership_operation(operation)
            .unwrap()
            .is_none()
    );
    release.send(()).unwrap();
    holding.await.unwrap();
    let (status, owner) = adoption.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{owner}");
    assert_eq!(owner["generation"], 1);
}
