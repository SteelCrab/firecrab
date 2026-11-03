use super::*;
use firecrab_api_types::{AiAuthentication, AiModelSelection};
use firecrab_stack::AiModelConfiguration;

fn source() -> crate::pipe_ai::SavedConnection {
    serde_json::from_value(serde_json::json!({
        "connection":{"id":Uuid::new_v4(),"name":"fixture-account","provider":"openai","authentication":"api_key",
            "endpoint":"https://api.openai.com/v1/responses","models":["verified-a","verified-b"],"streaming":true,"toolCalls":true},
        "api_key":"private-original-model-source","oauth":null
    })).unwrap()
}
fn input(
    store: &Store,
    source: &crate::pipe_ai::SavedConnection,
    manager: Manager,
) -> PlanResource {
    PlanResource {
        operation_id: Uuid::new_v4(),
        expected_endpoint_id: store.iac_endpoint_id().unwrap(),
        owner: ResourceOwner {
            manager,
            id: if manager == Manager::Stack {
                store.register_stack("model-plan").unwrap().id
            } else {
                Uuid::new_v4()
            },
            key: if manager == Manager::Stack {
                "default-model".into()
            } else {
                "firecrab_ai_model.project".into()
            },
        },
        resource: None,
        desired: Some(ResourceConfiguration::AiModel(AiModelConfiguration {
            project_id: Uuid::new_v4(),
            selection: AiModelSelection {
                connection_id: source.connection.id,
                model: "verified-a".into(),
            },
        })),
    }
}
fn apply_input(plan: &ResourcePlan) -> ApplyResourcePlan {
    ApplyResourcePlan {
        expected_endpoint_id: plan.ownership.endpoint_id,
        plan_sha256: plan.plan_sha256.clone(),
    }
}
fn apply(
    store: &Store,
    plan: &ResourcePlan,
    now: u64,
) -> Result<ResourcePlanReceipt, PersistenceError> {
    store.apply_resource_plan(plan.operation_id, &apply_input(plan), now)
}

#[test]
fn iac_resource_model_both_workflows_keep_borrowed_source_and_apply_noop_update_delete() {
    for manager in [Manager::Stack, Manager::Terraform] {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("state.db")).unwrap();
        let source = source();
        store.save_ai_connection(&source).unwrap();
        let source_resource = ManagedResource {
            kind: "ai_connection".into(),
            id: source.connection.id.to_string(),
        };
        let source_owner = store
            .change_iac_resource_owner(&ChangeResourceOwner {
                operation_id: Uuid::new_v4(),
                expected_endpoint_id: store.iac_endpoint_id().unwrap(),
                resource: source_resource,
                expected_generation: 0,
                expected_owner: None,
                owner: Some(ResourceOwner {
                    manager: Manager::Terraform,
                    id: Uuid::new_v4(),
                    key: "firecrab_ai_connection.borrowed".into(),
                }),
            })
            .unwrap();
        let mut request = input(&store, &source, manager);
        let plan = store.plan_resource(request.clone(), 100).unwrap();
        assert_eq!(plan.dependencies.len(), 1);
        assert_eq!(plan.dependencies[0].ownership, source_owner);
        assert_eq!(plan.dependencies[0].configuration_generation, 1);
        let project: Uuid = plan.ownership.resource.id.parse().unwrap();
        assert!(store.ai_default(project).unwrap().is_none());
        let created = apply(&store, &plan, 101).unwrap();
        assert!(created.revision_id.is_none());
        assert_eq!(
            store.ai_default(project).unwrap().unwrap().model,
            "verified-a"
        );
        request.resource = Some(created.ownership.resource.clone());
        request.operation_id = Uuid::new_v4();
        let noop = store.plan_resource(request.clone(), 102).unwrap();
        assert_eq!(noop.action, ResourcePlanAction::Noop);
        assert_eq!(
            apply(&store, &noop, 103).unwrap().ownership,
            created.ownership
        );
        request.operation_id = Uuid::new_v4();
        let Some(ResourceConfiguration::AiModel(model)) = request.desired.as_mut() else {
            unreachable!()
        };
        model.selection.model = "verified-b".into();
        let updated = store.plan_resource(request.clone(), 104).unwrap();
        assert_eq!(updated.changed_fields, vec!["model"]);
        apply(&store, &updated, 105).unwrap();
        assert_eq!(
            store.ai_default(project).unwrap().unwrap().model,
            "verified-b"
        );
        request.operation_id = Uuid::new_v4();
        request.desired = None;
        let deletion = store.plan_resource(request, 106).unwrap();
        assert!(deletion.dependencies.is_empty());
        let deleted = apply(&store, &deletion, 107).unwrap();
        assert!(deleted.ownership.owner.is_none());
        assert!(store.ai_default(project).unwrap().is_none());
        assert_eq!(
            store.iac_resource_owner(&source_owner.resource).unwrap(),
            source_owner
        );
        assert_eq!(
            store
                .ai_connection(source.connection.id)
                .unwrap()
                .unwrap()
                .api_key,
            source.api_key
        );
        assert!(
            !serde_json::to_string(&created)
                .unwrap()
                .contains(&source.api_key)
        );
    }
}

#[test]
fn iac_resource_model_rejects_source_rotation_and_same_uuid_disconnect_recreation() {
    for recreate in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("state.db")).unwrap();
        let mut source = source();
        store.save_ai_connection(&source).unwrap();
        let request = input(&store, &source, Manager::Terraform);
        let plan = store.plan_resource(request, 100).unwrap();
        if recreate {
            store.delete_ai_connection(source.connection.id).unwrap();
        } else {
            source.api_key = "private-rotated-source".into();
        }
        store.save_ai_connection(&source).unwrap();
        assert!(matches!(
            apply(&store, &plan, 101),
            Err(PersistenceError::StaleIacExecution)
        ));
        assert!(
            store
                .ai_default(plan.ownership.resource.id.parse().unwrap())
                .unwrap()
                .is_none()
        );
        assert_eq!(
            super::super::iac_ai_connections::generation(&store.lock(), source.connection.id)
                .unwrap(),
            if recreate { 3 } else { 2 }
        );
    }
}

#[test]
fn iac_resource_model_original_oauth_identity_renews_without_advancing_configuration_version() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let mut original = source();
    original.connection.authentication = AiAuthentication::Chatgpt;
    original.oauth=Some(serde_json::from_value(serde_json::json!({"client_id":"original-client","subject":"original-subject",
        "refresh_token":"private-old-refresh","expires_at":100,"earliest_refresh_at":0,"scope":"openid profile resource.invoke"})).unwrap());
    store.save_ai_connection(&original).unwrap();
    let plan = store
        .plan_resource(input(&store, &original, Manager::Terraform), 100)
        .unwrap();
    let mut renewed = original.clone();
    renewed.api_key = "private-new-access".into();
    let oauth = renewed.oauth.as_mut().unwrap();
    oauth.refresh_token = "private-new-refresh".into();
    oauth.expires_at = 1000;
    store.refresh_ai_connection(&original, &renewed, 1).unwrap();
    assert_eq!(
        super::super::iac_ai_connections::generation(&store.lock(), original.connection.id)
            .unwrap(),
        1
    );
    let (selected, generation) = store
        .resource_plan_ai_source(plan.operation_id, &apply_input(&plan), 101)
        .unwrap()
        .unwrap();
    assert_eq!(generation, 1);
    assert_eq!(selected.api_key, renewed.api_key);
    assert_eq!(selected.oauth.unwrap().subject, "original-subject");
    let mut foreign = renewed.clone();
    foreign.oauth.as_mut().unwrap().subject = "other-subject".into();
    assert!(matches!(
        store.refresh_ai_connection(&renewed, &foreign, 1),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    let receipt = apply(&store, &plan, 102).unwrap();
    let public = serde_json::to_string(&receipt).unwrap();
    for value in [
        "private-old-refresh",
        "private-new-refresh",
        "private-new-access",
        "original-subject",
    ] {
        assert!(!public.contains(value));
    }
}

#[test]
fn iac_resource_model_complete_receipt_never_reads_deleted_credentials_or_recreates_default() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let source = source();
    store.save_ai_connection(&source).unwrap();
    let mut request = input(&store, &source, Manager::Terraform);
    let plan = store.plan_resource(request.clone(), 100).unwrap();
    let receipt = apply(&store, &plan, 101).unwrap();
    assert!(matches!(
        store.delete_ai_connection(source.connection.id),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    request.operation_id = Uuid::new_v4();
    request.resource = Some(receipt.ownership.resource.clone());
    request.desired = None;
    let deletion = store.plan_resource(request, 102).unwrap();
    apply(&store, &deletion, 103).unwrap();
    store.delete_ai_connection(source.connection.id).unwrap();
    drop(store);
    let reopened = Store::open(&path).unwrap();
    assert!(
        reopened
            .resource_plan_ai_source(plan.operation_id, &apply_input(&plan), u64::MAX)
            .unwrap()
            .is_none()
    );
    assert_eq!(apply(&reopened, &plan, u64::MAX).unwrap(), receipt);
    assert!(
        reopened
            .ai_default(plan.ownership.resource.id.parse().unwrap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn iac_resource_model_legacy_connection_gets_one_initial_version_without_changing_credentials() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let source = source();
    let old = rusqlite::Connection::open(&path).unwrap();
    old.execute_batch(super::super::pipe_ai::CONNECTIONS_SQL)
        .unwrap();
    let original = serde_json::to_string(&source).unwrap();
    old.execute(
        "INSERT INTO pipe_ai_connections(id,spec) VALUES(?1,?2)",
        params![source.connection.id.to_string(), original],
    )
    .unwrap();
    drop(old);
    let store = Store::open(&path).unwrap();
    let plan = store
        .plan_resource(input(&store, &source, Manager::Terraform), 100)
        .unwrap();
    assert_eq!(plan.dependencies[0].configuration_generation, 0);
    store.save_ai_connection(&source).unwrap();
    assert_eq!(
        super::super::iac_ai_connections::generation(&store.lock(), source.connection.id).unwrap(),
        0
    );
    apply(&store, &plan, 101).unwrap();
    assert_eq!(
        store
            .ai_connection(source.connection.id)
            .unwrap()
            .unwrap()
            .api_key,
        source.api_key
    );
}

#[test]
fn iac_resource_model_old_oauth_worker_cannot_refresh_an_identical_recreated_connection() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let mut original = source();
    original.connection.authentication = AiAuthentication::Chatgpt;
    original.oauth=Some(serde_json::from_value(serde_json::json!({"client_id":"original-client","subject":"original-subject",
        "refresh_token":"private-old-refresh","expires_at":100,"earliest_refresh_at":0,"scope":"openid profile resource.invoke"})).unwrap());
    store.save_ai_connection(&original).unwrap();
    let (old, version) = store
        .ai_connection_with_generation(original.connection.id)
        .unwrap()
        .unwrap();
    let mut renewed = old.clone();
    renewed.api_key = "private-new-access".into();
    renewed.oauth.as_mut().unwrap().refresh_token = "private-new-refresh".into();
    store.delete_ai_connection(original.connection.id).unwrap();
    store.save_ai_connection(&original).unwrap();
    assert!(matches!(
        store.refresh_ai_connection(&old, &renewed, version),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    let (current, current_version) = store
        .ai_connection_with_generation(original.connection.id)
        .unwrap()
        .unwrap();
    assert_eq!(current.api_key, original.api_key);
    assert_eq!(current_version, 3);
}

#[test]
fn iac_resource_model_rejects_unverified_models_and_never_silently_adopts_existing_defaults() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let source = source();
    store.save_ai_connection(&source).unwrap();
    let mut request = input(&store, &source, Manager::Terraform);
    let Some(ResourceConfiguration::AiModel(config)) = request.desired.as_mut() else {
        unreachable!()
    };
    config.selection.model = "unverified".into();
    assert!(matches!(
        store.plan_resource(request.clone(), 100),
        Err(PersistenceError::InvalidStackResource)
    ));
    let Some(ResourceConfiguration::AiModel(config)) = request.desired.as_mut() else {
        unreachable!()
    };
    config.selection.model = "verified-a".into();
    store
        .save_ai_default(config.project_id, &config.selection)
        .unwrap();
    assert!(matches!(
        store.plan_resource(request, 101),
        Err(PersistenceError::IacOwnershipConflict)
    ));
}
