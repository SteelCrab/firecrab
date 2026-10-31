use super::*;
use crate::oci::RegistryCredential;
use crate::persistence::{PersistenceError, Store};
use crate::pipe_auth::SavedAuth;

fn github() -> SavedAuth {
    SavedAuth::Github {
        username: "fixture-user".into(),
        token: "fixture-auth-never-echo".into(),
    }
}

fn credential(registry: &str) -> RegistryCredential {
    RegistryCredential {
        registry: registry.into(),
        username: "fixture-user".into(),
        secret: "fixture-registry-never-echo".into(),
    }
}

#[tokio::test]
async fn iac_registry_aliases_share_authority_and_protect_legacy_docker_hub_writes() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    for registry in [
        "docker.io",
        "ghcr.io",
        "registry.example.invalid:5443",
        "123456789012.dkr.ecr.us-east-1.amazonaws.com",
    ] {
        state
            .store
            .put_registry_login(&credential(registry), None)
            .unwrap();
        let resource = ManagedResource {
            kind: "registry".into(),
            id: registry.into(),
        };
        let mut change = claim_configuration(&state, &resource);
        if registry == "docker.io" {
            change["resource"]["id"] = json!("REGISTRY-1.DOCKER.IO");
        }
        let (status, owned) =
            call(&app, "POST", "/api/iac/owners/transition", change.clone()).await;
        assert_eq!(status, StatusCode::OK, "{owned}");
        assert_eq!(owned["resource"]["id"], registry);
        assert!(!owned.to_string().contains("fixture-registry-never-echo"));
        let mut replay = change.clone();
        replay["resource"]["id"] = json!(registry);
        assert_eq!(
            call(&app, "POST", "/api/iac/owners/transition", replay)
                .await
                .1,
            owned
        );
        let alias = if registry == "docker.io" {
            "REGISTRY-1.DOCKER.IO"
        } else {
            registry
        };
        let encoded = alias.replace('.', "%2e").replace(':', "%3a");
        for method in ["PUT", "DELETE"] {
            let (status, error) = call(
                &app,
                method,
                &format!("/api/pipe/registries/{encoded}"),
                json!({}),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT, "{error}");
            assert_eq!(error["error"]["code"], "iac_resource_managed");
        }
        assert!(matches!(
            state.store.put_registry_login(&credential(alias), None),
            Err(PersistenceError::IacOwnershipConflict)
        ));
        assert!(matches!(
            state.store.delete_registry_credential(alias),
            Err(PersistenceError::IacOwnershipConflict)
        ));
        if registry == "docker.io" {
            for method in ["PUT", "DELETE"] {
                assert_eq!(
                    call(&app, method, "/api/microregistry/docker-hub", json!({}))
                        .await
                        .0,
                    StatusCode::CONFLICT
                );
            }
            assert!(matches!(
                state.store.put_docker_hub_credential("late", "late-value"),
                Err(PersistenceError::IacOwnershipConflict)
            ));
            assert!(matches!(
                state.store.delete_docker_hub_credential(),
                Err(PersistenceError::IacOwnershipConflict)
            ));
        }
        assert_eq!(
            state
                .store
                .registry_credential(registry)
                .unwrap()
                .unwrap()
                .secret,
            "fixture-registry-never-echo"
        );
        assert_eq!(
            call(
                &app,
                "GET",
                &format!("/api/iac/owners/registry/{alias}"),
                Value::Null
            )
            .await
            .1,
            owned
        );
        let (status,released)=call(&app,"POST","/api/iac/owners/transition",json!({
            "operationId":Uuid::new_v4(),"expectedEndpointId":owned["endpointId"],"resource":resource,
            "expectedGeneration":1,"expectedOwner":owned["owner"],"owner":null
        })).await;
        assert_eq!(status, StatusCode::OK, "{released}");
        state.store.delete_registry_credential(registry).unwrap();
        let receipt = call(
            &app,
            "GET",
            &format!(
                "/api/iac/owner-changes/{}",
                change["operationId"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
        assert_eq!(receipt.1["ownership"], owned);
    }
}

#[tokio::test]
async fn iac_registry_rejects_derived_auth_connections_and_preserves_existing_api_errors() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    state
        .store
        .put_registry_login(&credential("ghcr.io"), None)
        .unwrap();
    state.store.put_pipe_auth(&github()).unwrap();
    let resource = ManagedResource {
        kind: "registry".into(),
        id: "ghcr.io".into(),
    };
    let change = claim_configuration(&state, &resource);
    let id = Uuid::parse_str(change["operationId"].as_str().unwrap()).unwrap();
    let (status, error) = call(&app, "POST", "/api/iac/owners/transition", change).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "iac_parent_managed");
    assert!(state.store.iac_ownership_operation(id).unwrap().is_none());
    let (status, error) = call(&app, "DELETE", "/api/pipe/registries/ghcr.io", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "pipe_auth_managed");
    let (status, public) = call(&app, "GET", "/api/pipe/registries", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!public.to_string().contains("never-echo"));
}

#[tokio::test]
async fn iac_registry_adoption_and_shared_auth_creation_have_exactly_one_winner() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    state
        .store
        .put_registry_login(&credential("ghcr.io"), None)
        .unwrap();
    let resource = ManagedResource {
        kind: "registry".into(),
        id: "ghcr.io".into(),
    };
    let change: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &resource)).unwrap();
    let first = Store::open(&root.path().join("state.db")).unwrap();
    let second = Store::open(&root.path().join("state.db")).unwrap();
    let start = std::sync::Arc::new(std::sync::Barrier::new(2));
    let ready = start.clone();
    let adopted = std::thread::spawn(move || {
        ready.wait();
        first.change_iac_resource_owner(&change)
    });
    let saved = std::thread::spawn(move || {
        start.wait();
        second.put_pipe_auth(&github())
    });
    let adopted = adopted.join().unwrap();
    let saved = saved.join().unwrap();
    assert_ne!(adopted.is_ok(), saved.is_ok());
    if adopted.is_ok() {
        assert!(matches!(saved, Err(PersistenceError::IacOwnershipConflict)));
        assert!(state.store.pipe_auths().unwrap().is_empty());
    } else {
        assert!(matches!(adopted, Err(PersistenceError::IacParentManaged)));
        assert!(
            state
                .store
                .iac_resource_owner(&resource)
                .unwrap()
                .owner
                .is_none()
        );
    }
}

#[tokio::test]
async fn iac_registry_native_ownership_also_prevents_a_new_auth_source_from_masking_it() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    state
        .store
        .put_registry_login(&credential("ghcr.io"), None)
        .unwrap();
    let stack = state.store.register_stack("registry-ownership").unwrap();
    let resource = ManagedResource {
        kind: "registry".into(),
        id: "ghcr.io".into(),
    };
    let mut change = claim_configuration(&state, &resource);
    change["owner"] = json!({"manager":"stack","id":stack.id,"key":"login"});
    assert_eq!(
        call(&app, "POST", "/api/iac/owners/transition", change)
            .await
            .0,
        StatusCode::OK
    );
    assert!(matches!(
        state.store.put_pipe_auth(&github()),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    assert!(state.store.pipe_auths().unwrap().is_empty());
}
