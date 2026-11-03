use super::*;
use firecrab_api_types::{PortProtocol, SecurityGroupDirection, SecurityGroupRule};

fn input(store: &Store, manager: Manager) -> PlanResource {
    let network = Uuid::new_v4();
    store
        .insert_micro_network(
            &serde_json::from_value(serde_json::json!({"id":network,"name":"borrowed-network",
        "subnetCidr":"172.31.84.0/24","gateway":"172.31.84.1","internetEnabled":true}))
            .unwrap(),
        )
        .unwrap();
    PlanResource {
        operation_id: Uuid::new_v4(),
        expected_endpoint_id: store.iac_endpoint_id().unwrap(),
        owner: ResourceOwner {
            manager,
            id: if manager == Manager::Stack {
                store.register_stack("policy-plan").unwrap().id
            } else {
                Uuid::new_v4()
            },
            key: "firewall".into(),
        },
        resource: None,
        removal: None,
        desired: Some(ResourceConfiguration::SecurityGroup(
            SecurityGroupConfiguration {
                name: "policy".into(),
                micro_network_id: network,
                rules: vec![SecurityGroupRule {
                    direction: SecurityGroupDirection::Ingress,
                    protocol: PortProtocol::Tcp,
                    port_from: 443,
                    port_to: 443,
                    peer_cidr: "10.2.3.4/24".into(),
                }],
            },
        )),
    }
}

#[test]
fn iac_resource_security_group_missing_service_phase_or_instances_never_proves_inactivity() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let plan = store
        .plan_resource(input(&store, Manager::Stack), 100)
        .unwrap();
    let receipt = apply(&store, &plan, 101).unwrap();
    let id: Uuid = receipt.ownership.resource.id.parse().unwrap();
    let service = Uuid::new_v4().to_string();
    for mut deployment in [
        serde_json::json!({"instances":[]}),
        serde_json::json!({"phase":null,"instances":[]}),
        serde_json::json!({"phase":"unknown","instances":[]}),
        serde_json::json!({"phase":"stopped"}),
        serde_json::json!({"phase":"stopped","instances":null}),
        serde_json::json!({"phase":"stopped","instances":{}}),
    ] {
        deployment["spec"] = serde_json::json!({"securityGroupId":id});
        store
            .lock()
            .execute(
                "INSERT INTO micro_service_deployments(service_id,deployment) VALUES(?1,?2)
            ON CONFLICT(service_id) DO UPDATE SET deployment=excluded.deployment",
                params![service, deployment.to_string()],
            )
            .unwrap();
        assert!(matches!(
            store.replace_security_group_rules(id, &[]),
            Err(PersistenceError::IacResourceBusy)
        ));
        assert!(matches!(
            store.delete_security_group(id),
            Err(PersistenceError::IacResourceBusy)
        ));
        let removal = PlanResource {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: receipt.ownership.endpoint_id,
            owner: receipt.ownership.owner.clone().unwrap(),
            resource: Some(receipt.ownership.resource.clone()),
            desired: None,
            removal: None,
        };
        assert!(matches!(
            store.plan_resource(removal, 102),
            Err(PersistenceError::IacResourceBusy)
        ));
        assert_eq!(store.security_group(id).unwrap().unwrap().rules.len(), 1);
    }
    store
        .lock()
        .execute(
            "DELETE FROM micro_service_deployments WHERE service_id=?1",
            [service],
        )
        .unwrap();
    store.replace_security_group_rules(id, &[]).unwrap();
    assert!(store.security_group(id).unwrap().unwrap().rules.is_empty());
    assert!(store.delete_security_group(id).unwrap());
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
fn iac_resource_security_group_both_managers_preserve_rules_network_and_original_receipt() {
    for manager in [Manager::Stack, Manager::Terraform] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let store = Store::open(&path).unwrap();
        let mut request = input(&store, manager);
        let plan = store.plan_resource(request.clone(), 100).unwrap();
        let network: Uuid = plan.dependencies[0].ownership.resource.id.parse().unwrap();
        assert_eq!(plan.action, ResourcePlanAction::Create);
        assert!(store.list_security_groups().unwrap().is_empty());
        let created = apply(&store, &plan, 101).unwrap();
        let id = created.ownership.resource.id.parse().unwrap();
        assert_eq!(
            store.security_group(id).unwrap().unwrap().rules[0].peer_cidr,
            "10.2.3.0/24"
        );
        request.operation_id = Uuid::new_v4();
        request.resource = Some(created.ownership.resource.clone());
        let noop = store.plan_resource(request.clone(), 102).unwrap();
        assert_eq!(noop.action, ResourcePlanAction::Noop);
        assert_eq!(
            apply(&store, &noop, 103).unwrap().ownership,
            created.ownership
        );
        if manager == Manager::Terraform {
            assert!(matches!(
                store.rename_security_group(id, "forbidden"),
                Err(PersistenceError::IacOwnershipConflict)
            ));
        }
        request.operation_id = Uuid::new_v4();
        let Some(ResourceConfiguration::SecurityGroup(config)) = request.desired.as_mut() else {
            unreachable!()
        };
        config.name = "renamed".into();
        config.rules.push(SecurityGroupRule {
            direction: SecurityGroupDirection::Egress,
            protocol: PortProtocol::Udp,
            port_from: 53,
            port_to: 53,
            peer_cidr: "10.9.0.0/16".into(),
        });
        let update = store.plan_resource(request.clone(), 104).unwrap();
        assert_eq!(update.changed_fields, vec!["name", "rules"]);
        apply(&store, &update, 105).unwrap();
        let group = store.security_group(id).unwrap().unwrap();
        assert_eq!(group.name, "renamed");
        assert_eq!(group.rules.len(), 2);
        assert_eq!(group.rules[1].port_from, 53);
        request.operation_id = Uuid::new_v4();
        request.desired = None;
        let deletion = store.plan_resource(request, 106).unwrap();
        assert!(
            apply(&store, &deletion, 107)
                .unwrap()
                .ownership
                .owner
                .is_none()
        );
        assert!(store.security_group(id).unwrap().is_none());
        assert!(store.micro_network(network).unwrap().is_some());
        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert_eq!(apply(&reopened, &plan, u64::MAX).unwrap(), created);
        assert!(reopened.security_group(id).unwrap().is_none());
    }
}

#[test]
fn iac_resource_security_group_rejects_network_rebinding_and_rule_writes_while_vm_is_active() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let mut request = input(&store, Manager::Stack);
    let plan = store.plan_resource(request.clone(), 100).unwrap();
    let created = apply(&store, &plan, 101).unwrap();
    let id: Uuid = created.ownership.resource.id.parse().unwrap();
    let network: Uuid = plan.dependencies[0].ownership.resource.id.parse().unwrap();
    let vm:crate::model::VmRecord=serde_json::from_value(serde_json::json!({"id":Uuid::new_v4(),"name":"dependent-vm",
        "state":"running","template":"alpine","cpu":1,"ram":128,"micro_network_id":network,"security_group_id":id})).unwrap();
    store.insert(&vm).unwrap();
    request.resource = Some(created.ownership.resource);
    request.operation_id = Uuid::new_v4();
    let Some(ResourceConfiguration::SecurityGroup(config)) = request.desired.as_mut() else {
        unreachable!()
    };
    config.rules.clear();
    let changed = store.plan_resource(request.clone(), 102).unwrap();
    assert!(matches!(
        apply(&store, &changed, 103),
        Err(PersistenceError::IacResourceBusy)
    ));
    assert!(matches!(
        store.replace_security_group_rules(id, &[]),
        Err(PersistenceError::IacResourceBusy)
    ));
    assert_eq!(store.security_group(id).unwrap().unwrap().rules.len(), 1);
    request.operation_id = Uuid::new_v4();
    request.desired = None;
    assert!(matches!(
        store.plan_resource(request.clone(), 104),
        Err(PersistenceError::IacManagedDependent)
    ));
    request.operation_id = Uuid::new_v4();
    request.desired = Some(ResourceConfiguration::SecurityGroup(
        SecurityGroupConfiguration {
            name: "different-network".into(),
            micro_network_id: Uuid::new_v4(),
            rules: vec![],
        },
    ));
    assert!(store.plan_resource(request, 105).is_err());
    let mut stopped = vm.clone();
    stopped.state = crate::model::VmState::Stopped;
    store.update(&stopped).unwrap();
    apply(&store, &changed, 106).unwrap();
    assert!(store.security_group(id).unwrap().unwrap().rules.is_empty());
}

#[test]
fn iac_resource_security_group_source_authority_and_definition_drift_require_new_plan() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let mut request = input(&store, Manager::Stack);
    let plan = store.plan_resource(request.clone(), 100).unwrap();
    let created = apply(&store, &plan, 101).unwrap();
    let id = created.ownership.resource.id.parse().unwrap();
    request.resource = Some(created.ownership.resource);
    request.operation_id = Uuid::new_v4();
    let stale = store.plan_resource(request.clone(), 102).unwrap();
    store.rename_security_group(id, "native-change").unwrap();
    assert!(matches!(
        apply(&store, &stale, 103),
        Err(PersistenceError::StaleIacExecution)
    ));
    request.operation_id = Uuid::new_v4();
    let stale = store.plan_resource(request, 104).unwrap();
    let source = &stale.dependencies[0].ownership;
    store
        .change_iac_resource_owner(&ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: source.endpoint_id,
            resource: source.resource.clone(),
            expected_generation: source.generation,
            expected_owner: source.owner.clone(),
            owner: Some(ResourceOwner {
                manager: Manager::Terraform,
                id: Uuid::new_v4(),
                key: "firecrab_network.borrowed".into(),
            }),
        })
        .unwrap();
    assert!(matches!(
        apply(&store, &stale, 105),
        Err(PersistenceError::StaleIacExecution)
    ));
}
