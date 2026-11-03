use super::*;

fn input(store: &Store, manager: Manager) -> PlanResource {
    let id = Uuid::new_v4();
    let spec = serde_json::from_value(
        serde_json::json!({"name":"telemetry-service","image":"ghcr.io/example/app:v1",
        "port":8080,"ram":128,"cpu":1}),
    )
    .unwrap();
    store
        .insert_micro_service(&firecrab_api_types::MicroServiceResponse { id, spec })
        .unwrap();
    PlanResource {
        operation_id: Uuid::new_v4(),
        expected_endpoint_id: store.iac_endpoint_id().unwrap(),
        owner: ResourceOwner {
            manager,
            id: if manager == Manager::Stack {
                store.register_stack("telemetry-plan").unwrap().id
            } else {
                Uuid::new_v4()
            },
            key: "telemetry".into(),
        },
        resource: None,
        removal: None,
        desired: Some(ResourceConfiguration::ServiceOtlp(
            ServiceOtlpConfiguration {
                service_id: id,
                endpoint: Some("https://collector.example.test".into()),
            },
        )),
    }
}
fn apply(
    store: &Store,
    plan: &ResourcePlan,
    now: u64,
) -> Result<ResourcePlanReceipt, PersistenceError> {
    store.apply_resource_plan(
        plan.operation_id,
        &ApplyResourcePlan {
            expected_endpoint_id: plan.ownership.endpoint_id,
            plan_sha256: plan.plan_sha256.clone(),
        },
        now,
    )
}

#[test]
fn iac_resource_otlp_both_managers_preserve_noop_consent_and_borrowed_service() {
    for manager in [Manager::Stack, Manager::Terraform] {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("state.db")).unwrap();
        let mut request = input(&store, manager);
        let plan = store.plan_resource(request.clone(), 100).unwrap();
        let service: Uuid = plan.ownership.resource.id.parse().unwrap();
        let original = apply(&store, &plan, 101).unwrap();
        assert_eq!(
            store.otlp_configuration(service).unwrap(),
            Some(("https://collector.example.test/".into(), 102))
        );
        assert_eq!(
            store
                .iac_native_operation(plan.operation_id)
                .unwrap()
                .unwrap()
                .configuration_generation,
            Some(102)
        );
        assert!(
            !serde_json::to_string(&plan)
                .unwrap()
                .contains("collector.example.test")
        );
        request.resource = Some(original.ownership.resource.clone());
        request.operation_id = Uuid::new_v4();
        let noop = store.plan_resource(request.clone(), 103).unwrap();
        assert_eq!(noop.action, ResourcePlanAction::Noop);
        apply(&store, &noop, 104).unwrap();
        assert_eq!(store.otlp_configuration(service).unwrap().unwrap().1, 102);
        request.operation_id = Uuid::new_v4();
        let Some(ResourceConfiguration::ServiceOtlp(config)) = request.desired.as_mut() else {
            unreachable!()
        };
        config.endpoint = Some("https://collector.example.test/second".into());
        let change = store.plan_resource(request.clone(), 105).unwrap();
        assert_eq!(change.changed_fields, vec!["endpoint"]);
        apply(&store, &change, 106).unwrap();
        assert_eq!(store.otlp_configuration(service).unwrap().unwrap().1, 107);
        request.operation_id = Uuid::new_v4();
        let Some(ResourceConfiguration::ServiceOtlp(config)) = request.desired.as_mut() else {
            unreachable!()
        };
        config.endpoint = None;
        let disable = store.plan_resource(request.clone(), 108).unwrap();
        let disabled = apply(&store, &disable, 109).unwrap();
        assert!(store.otlp_configuration(service).unwrap().is_none());
        assert_eq!(disabled.ownership.owner, original.ownership.owner);
        assert!(matches!(
            store.delete_micro_service(service),
            Err(PersistenceError::IacManagedDependent)
        ));
        request.operation_id = Uuid::new_v4();
        let noop = store.plan_resource(request.clone(), 110).unwrap();
        assert_eq!(noop.action, ResourcePlanAction::Noop);
        apply(&store, &noop, 111).unwrap();
        request.operation_id = Uuid::new_v4();
        request.desired = None;
        let deletion = store.plan_resource(request, 112).unwrap();
        assert!(
            apply(&store, &deletion, 113)
                .unwrap()
                .ownership
                .owner
                .is_none()
        );
        assert!(store.micro_service(service).unwrap().is_some());
        assert!(store.delete_micro_service(service).unwrap());
        assert_eq!(apply(&store, &plan, u64::MAX).unwrap(), original);
        assert!(store.otlp_configuration(service).unwrap().is_none());
    }
}

#[test]
fn iac_resource_otlp_disabled_slot_import_is_explicit_and_native_writes_obey_owner() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let mut request = input(&store, Manager::Terraform);
    let Some(ResourceConfiguration::ServiceOtlp(config)) = request.desired.as_mut() else {
        unreachable!()
    };
    config.endpoint = None;
    let resource = ManagedResource {
        kind: "service_otlp".into(),
        id: config.service_id.to_string(),
    };
    let id = config.service_id;
    request.resource = Some(resource.clone());
    assert!(store.plan_resource(request.clone(), 100).is_err());
    store
        .change_iac_resource_owner(&ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: request.expected_endpoint_id,
            resource,
            expected_generation: 0,
            expected_owner: None,
            owner: Some(request.owner.clone()),
        })
        .unwrap();
    let plan = store.plan_resource(request, 101).unwrap();
    assert_eq!(plan.action, ResourcePlanAction::Noop);
    apply(&store, &plan, 102).unwrap();
    assert!(matches!(
        store.set_otlp_endpoint(id, Some("https://other.example.test")),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    assert!(store.otlp_configuration(id).unwrap().is_none());
}

#[test]
fn iac_resource_otlp_invalid_urls_and_environment_conflicts_never_persist_values() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let mut request = input(&store, Manager::Terraform);
    let Some(ResourceConfiguration::ServiceOtlp(config)) = request.desired.as_mut() else {
        unreachable!()
    };
    config.endpoint = Some("https://private-user:private-password@collector.example.test".into());
    assert!(matches!(
        store.plan_resource(request.clone(), 100),
        Err(PersistenceError::InvalidIacOwner)
    ));
    assert!(store.resource_plan(request.operation_id).unwrap().is_none());
    let Some(ResourceConfiguration::ServiceOtlp(config)) = request.desired.as_mut() else {
        unreachable!()
    };
    config.endpoint = Some("https://collector.example.test".into());
    let mut parent = store.micro_service(config.service_id).unwrap().unwrap();
    parent
        .spec
        .env
        .insert("OTEL_SERVICE_NAME".into(), "private-env-value".into());
    store.update_micro_service(&parent).unwrap();
    assert!(matches!(
        store.plan_resource(request, 101),
        Err(PersistenceError::InvalidStackResource)
    ));
    assert!(store.otlp_configuration(parent.id).unwrap().is_none());
    assert!(
        serde_json::from_value::<ServiceOtlpConfiguration>(
            serde_json::json!({"serviceId":parent.id})
        )
        .is_err()
    );
}

#[test]
fn iac_resource_otlp_parent_authority_and_consent_drift_reject_old_plans() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let mut request = input(&store, Manager::Stack);
    let plan = store.plan_resource(request.clone(), 100).unwrap();
    let created = apply(&store, &plan, 101).unwrap();
    let id: Uuid = created.ownership.resource.id.parse().unwrap();
    request.resource = Some(created.ownership.resource);
    request.operation_id = Uuid::new_v4();
    let stale = store.plan_resource(request.clone(), 102).unwrap();
    store
        .set_otlp_endpoint(id, Some("https://collector.example.test/"))
        .unwrap();
    assert!(matches!(
        apply(&store, &stale, 103),
        Err(PersistenceError::StaleIacExecution)
    ));
    request.operation_id = Uuid::new_v4();
    let stale = store.plan_resource(request, 104).unwrap();
    let parent = &stale.dependencies[0].ownership;
    store
        .change_iac_resource_owner(&ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: parent.endpoint_id,
            resource: parent.resource.clone(),
            expected_generation: parent.generation,
            expected_owner: parent.owner.clone(),
            owner: Some(ResourceOwner {
                manager: Manager::Terraform,
                id: Uuid::new_v4(),
                key: "firecrab_service.parent".into(),
            }),
        })
        .unwrap();
    assert!(matches!(
        apply(&store, &stale, 105),
        Err(PersistenceError::StaleIacExecution)
    ));
}

#[test]
fn iac_resource_otlp_receipt_failure_rolls_back_setting_owner_and_operation() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let request = input(&store, Manager::Terraform);
    let plan = store.plan_resource(request, 100).unwrap();
    let id = plan.ownership.resource.id.parse().unwrap();
    store.lock().execute_batch("CREATE TRIGGER reject_otlp_ack BEFORE UPDATE OF receipt ON iac_resource_plans BEGIN SELECT RAISE(ABORT,'injected_lost_ack'); END").unwrap();
    assert!(apply(&store, &plan, 101).is_err());
    assert!(store.otlp_configuration(id).unwrap().is_none());
    assert!(
        store
            .iac_resource_owner(&plan.ownership.resource)
            .unwrap()
            .owner
            .is_none()
    );
    assert!(
        store
            .iac_native_operations(&plan.ownership.resource)
            .unwrap()
            .is_empty()
    );
    store
        .lock()
        .execute_batch("DROP TRIGGER reject_otlp_ack")
        .unwrap();
    apply(&store, &plan, 102).unwrap();
    assert_eq!(store.otlp_configuration(id).unwrap().unwrap().1, 103);
}

#[test]
fn iac_resource_otlp_service_writers_recheck_current_environment_in_both_orders() {
    for setting_first in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let store = Store::open(&path).unwrap();
        let request = input(&store, Manager::Stack);
        let Some(ResourceConfiguration::ServiceOtlp(config)) = &request.desired else {
            unreachable!()
        };
        let id = config.service_id;
        let second = Store::open(&path).unwrap();
        let original = store.micro_service(id).unwrap().unwrap();
        let mut conflict = original.clone();
        conflict.spec.env.insert(
            "OTEL_SERVICE_NAME".into(),
            "private-conflicting-value".into(),
        );
        let authority = store.auto_deploy_authority(id).unwrap();
        let deployment: firecrab_api_types::MicroServiceDeployment =
            serde_json::from_value(serde_json::json!({
                "id":Uuid::new_v4(),"serviceId":id,"phase":"running","spec":conflict.spec,
                "request":{"hostPort":0},"digest":null,"instances":[],"log":[],"error":null
            }))
            .unwrap();
        let admission = super::super::super::iac_auto_deploy::DeploymentAuthority::Manual(
            Box::new(authority),
            None,
        );
        if setting_first {
            let plan = store.plan_resource(request, 100).unwrap();
            apply(&store, &plan, 101).unwrap();
            assert!(matches!(
                second.update_micro_service(&conflict),
                Err(PersistenceError::InvalidStackResource)
            ));
            assert_eq!(second.micro_service(id).unwrap().unwrap(), original);
            assert!(matches!(
                second.save_authorized_deployment(&deployment, &admission),
                Err(PersistenceError::InvalidStackResource)
            ));
            assert!(second.service_deployment(id).unwrap().is_none());
            let mut busy = deployment.clone();
            busy.spec = original.spec.clone();
            busy.phase = firecrab_api_types::ServicePhase::Queued;
            second
                .save_authorized_deployment(&busy, &admission)
                .unwrap();
            let removal = PlanResource {
                operation_id: Uuid::new_v4(),
                expected_endpoint_id: plan.ownership.endpoint_id,
                owner: plan.owner,
                resource: Some(plan.ownership.resource),
                desired: None,
                removal: None,
            };
            assert!(matches!(
                store.plan_resource(removal, 102),
                Err(PersistenceError::IacResourceBusy)
            ));
        } else {
            let plan = store.plan_resource(request, 100).unwrap();
            second
                .save_authorized_deployment(&deployment, &admission)
                .unwrap();
            let error = apply(&store, &plan, 101).unwrap_err();
            assert!(
                matches!(error, PersistenceError::InvalidStackResource),
                "{error:?}"
            );
            assert!(store.otlp_configuration(id).unwrap().is_none());
            assert!(matches!(
                store.set_otlp_endpoint(id, Some("https://collector.example.test")),
                Err(PersistenceError::InvalidStackResource)
            ));
            assert!(store.otlp_configuration(id).unwrap().is_none());
        }
    }
}
