use firecrab_api_types::{
    DeployMicroServiceRequest, MicroServiceDeployment, MicroServiceResponse, ServiceInstance,
    ServicePhase, VmState,
};
use firecrab_stack::{
    Action, Binding, Inventory, ResourceKind, ServiceSnapshot, Stack, StackRecord, VmSnapshot,
    parse, plan,
};
use serde_json::json;
use uuid::Uuid;

const EXAMPLE: &str = include_str!("../../examples/stacks/basic.yaml");
const NETWORK: Uuid = Uuid::from_u128(1);
const VM: Uuid = Uuid::from_u128(2);
const SERVICE: Uuid = Uuid::from_u128(3);

#[test]
fn human_and_json_action_names_match() {
    for action in [
        Action::Create,
        Action::Update,
        Action::Replace,
        Action::Deploy,
        Action::Noop,
        Action::Retain,
        Action::Blocked,
    ] {
        let value = serde_json::to_value(action).unwrap();
        assert_eq!(action.to_string(), value.as_str().unwrap());
    }
}

#[test]
fn fingerprints_are_canonical_redacted_and_change_with_configuration_or_ownership() {
    let (stack, mut record, mut inventory) = fixture();
    let original = plan(&stack, &record, &inventory).unwrap().fingerprint;
    assert_eq!(original.len(), 64);
    assert!(original.bytes().all(|b| b.is_ascii_hexdigit()));
    record.bindings.reverse();
    inventory.networks.reverse();
    assert_eq!(
        plan(&stack, &record, &inventory).unwrap().fingerprint,
        original
    );
    let deployment = inventory.services[0].deployment.as_mut().unwrap();
    deployment.phase = ServicePhase::Degraded;
    deployment.instances[0].healthy = false;
    deployment.instances[0].state = VmState::Stopped;
    deployment.log.push("DO-NOT-PRINT runtime log".into());
    deployment.error = Some("DO-NOT-PRINT backend detail".into());
    assert_eq!(
        plan(&stack, &record, &inventory).unwrap().fingerprint,
        original
    );
    inventory.vms[0]
        .env
        .insert("MARKER".into(), "DO-NOT-PRINT".into());
    let changed = plan(&stack, &record, &inventory).unwrap();
    assert_ne!(changed.fingerprint, original);
    assert!(
        !serde_json::to_string(&changed)
            .unwrap()
            .contains("DO-NOT-PRINT")
    );
    inventory.vms[0].env.remove("MARKER");
    record.revision += 1;
    assert_ne!(
        plan(&stack, &record, &inventory).unwrap().fingerprint,
        original
    );
}

#[test]
fn fingerprints_include_deployment_identity_and_busy_state_but_not_instance_order() {
    let (stack, record, mut inventory) = fixture();
    let original = plan(&stack, &record, &inventory).unwrap().fingerprint;
    inventory.services[0]
        .deployment
        .as_mut()
        .unwrap()
        .instances
        .reverse();
    assert_eq!(
        plan(&stack, &record, &inventory).unwrap().fingerprint,
        original
    );
    inventory.services[0].deployment.as_mut().unwrap().phase = ServicePhase::Starting;
    assert_ne!(
        plan(&stack, &record, &inventory).unwrap().fingerprint,
        original
    );
    inventory.services[0].deployment.as_mut().unwrap().phase = ServicePhase::Running;
    inventory.services[0].deployment.as_mut().unwrap().id = Uuid::new_v4();
    assert_ne!(
        plan(&stack, &record, &inventory).unwrap().fingerprint,
        original
    );
}

fn fixture() -> (Stack, StackRecord, Inventory) {
    let stack = parse(EXAMPLE).unwrap();
    let record = StackRecord {
        id: Uuid::from_u128(10),
        name: "demo".to_owned(),
        revision: 3,
        bindings: vec![
            Binding {
                kind: ResourceKind::Network,
                key: "app".to_owned(),
                resource_id: NETWORK,
            },
            Binding {
                kind: ResourceKind::Vm,
                key: "worker".to_owned(),
                resource_id: VM,
            },
            Binding {
                kind: ResourceKind::Service,
                key: "web".to_owned(),
                resource_id: SERVICE,
            },
        ],
    };
    let desired = &stack.micro_vms["worker"];
    let pin = template_pin();
    let vm = VmSnapshot {
        id: VM,
        name: desired.name.clone(),
        template: desired.template.clone(),
        template_version: pin.version.clone(),
        template_kernel_sha256: pin.kernel_sha256.clone(),
        template_rootfs_sha256: pin.rootfs_sha256.clone(),
        template_boot_args_sha256: pin.boot_args_sha256.clone(),
        template_pin: Some(pin.clone()),
        cpu: desired.cpu,
        ram: desired.ram,
        disk_gb: desired.disk_gb,
        egress_policy: desired.egress_policy,
        micro_network_id: NETWORK,
        storage_root: "default".to_owned(),
        env: desired.env.clone(),
    };
    let service = &stack.micro_services["web"];
    let deployment = MicroServiceDeployment {
        started_at: 0,
        finished_at: None,
        update: None,
        id: Uuid::from_u128(20),
        service_id: SERVICE,
        phase: ServicePhase::Running,
        spec: service.config.clone(),
        request: DeployMicroServiceRequest {
            micro_network_id: Some(NETWORK),
            host_port: service.host_port,
        },
        digest: Some("sha256:placeholder".to_owned()),
        instances: (0..service.config.replicas)
            .map(|i| ServiceInstance {
                vm_id: Uuid::from_u128(30 + u128::from(i)),
                ipv4: format!("172.31.20.{}", 10 + i),
                host_port: service.host_port + i,
                state: VmState::Running,
                healthy: true,
            })
            .collect(),
        log: Vec::new(),
        error: None,
        domain_route_owned: false,
        internal_route_owned: false,
    };
    let inventory = Inventory {
        templates: vec![firecrab_stack::TemplateSnapshot { name: desired.template.clone(), pin, current: true }],
        networks: vec![serde_json::from_value(json!({"id":NETWORK,"name":"demo-app","subnetCidr":"172.31.20.0/24","gateway":"172.31.20.1","internetEnabled":true})).unwrap()],
        vms: vec![vm],
        services: vec![ServiceSnapshot { service: MicroServiceResponse { id: SERVICE, spec: service.config.clone() }, deployment: Some(deployment) }],
    };
    (stack, record, inventory)
}

#[test]
fn matching_owned_configuration_has_a_stable_empty_diff() {
    let (stack, record, inventory) = fixture();
    let first = plan(&stack, &record, &inventory).unwrap();
    assert_eq!(first, plan(&stack, &record, &inventory).unwrap());
    assert!(!first.blocked);
    assert_eq!(first.revision, 3);
    assert!(
        first
            .changes
            .iter()
            .all(|change| change.action == Action::Noop && change.fields.is_empty())
    );
    assert_eq!(
        serde_json::from_str::<firecrab_stack::Plan>(&serde_json::to_string(&first).unwrap())
            .unwrap(),
        first
    );
}

#[test]
fn an_empty_host_produces_creates_in_dependency_order() {
    let (stack, mut record, inventory) = fixture();
    record.bindings.clear();
    let empty = Inventory {
        templates: inventory.templates,
        ..Inventory::default()
    };
    let result = plan(&stack, &record, &empty).unwrap();
    assert_eq!(
        result.changes.iter().map(|c| c.kind).collect::<Vec<_>>(),
        [
            ResourceKind::Network,
            ResourceKind::Vm,
            ResourceKind::Service
        ]
    );
    assert!(
        result
            .changes
            .iter()
            .all(|change| change.action == Action::Create)
    );
    assert!(!result.blocked);
}

#[test]
fn existing_names_never_imply_adoption_and_dependencies_are_blocked() {
    let (stack, mut record, inventory) = fixture();
    record.bindings.clear();
    let result = plan(&stack, &record, &inventory).unwrap();
    assert!(result.blocked);
    assert!(
        result
            .changes
            .iter()
            .all(|c| c.action == Action::Blocked && c.resource_id.is_none())
    );
}

#[test]
fn missing_owned_network_blocks_its_dependents_without_recreating_it() {
    let (stack, record, mut inventory) = fixture();
    inventory.networks.clear();
    let result = plan(&stack, &record, &inventory).unwrap();
    assert!(result.blocked);
    assert!(result.changes.iter().all(|c| c.action == Action::Blocked));
}

#[test]
fn changing_an_immutable_network_propagates_to_vms_and_deployments() {
    let (mut stack, record, inventory) = fixture();
    stack.micro_networks.get_mut("app").unwrap().subnet_cidr = "172.31.22.0/24".to_owned();
    let result = plan(&stack, &record, &inventory).unwrap();
    assert_eq!(
        result.changes.iter().map(|c| c.action).collect::<Vec<_>>(),
        [Action::Replace, Action::Replace, Action::Deploy]
    );
    assert_eq!(result.changes[1].fields, ["network"]);
}

#[test]
fn mutable_vm_fields_update_but_disk_shrink_requires_replacement() {
    let (mut stack, record, inventory) = fixture();
    stack.micro_vms.get_mut("worker").unwrap().ram = 512;
    let result = plan(&stack, &record, &inventory).unwrap();
    assert_eq!(result.changes[1].action, Action::Update);
    assert_eq!(result.changes[1].fields, ["ram"]);
    stack.micro_vms.get_mut("worker").unwrap().disk_gb = 1;
    assert_eq!(
        plan(&stack, &record, &inventory).unwrap().changes[1].action,
        Action::Replace
    );
}

#[test]
fn plan_redacts_both_desired_and_actual_environment_values() {
    let (mut stack, record, mut inventory) = fixture();
    stack
        .micro_vms
        .get_mut("worker")
        .unwrap()
        .env
        .insert("TOKEN".to_owned(), "desired-DO-NOT-PRINT".to_owned());
    inventory.vms[0]
        .env
        .insert("TOKEN".to_owned(), "actual-DO-NOT-PRINT".to_owned());
    stack
        .micro_services
        .get_mut("web")
        .unwrap()
        .config
        .env
        .insert("TOKEN".to_owned(), "service-DO-NOT-PRINT".to_owned());
    let result = plan(&stack, &record, &inventory).unwrap();
    let output = serde_json::to_string(&result).unwrap();
    assert!(!output.contains("DO-NOT-PRINT"));
    assert!(result.changes[1].fields.contains(&"env".to_owned()));
    assert!(result.changes[2].fields.contains(&"config.env".to_owned()));
}

#[test]
fn omitted_storage_preserves_placement_and_explicit_storage_requires_replacement() {
    let (mut stack, record, mut inventory) = fixture();
    inventory.vms[0].storage_root = "ssd".to_owned();
    assert_eq!(
        plan(&stack, &record, &inventory).unwrap().changes[1].action,
        Action::Noop
    );
    stack.micro_vms.get_mut("worker").unwrap().storage_root = Some("default".to_owned());
    assert_eq!(
        plan(&stack, &record, &inventory).unwrap().changes[1].action,
        Action::Replace
    );
}

#[test]
fn removed_declarations_are_retained_in_stable_reverse_dependency_order() {
    let (mut stack, mut record, inventory) = fixture();
    stack.micro_vms.clear();
    stack.micro_services.clear();
    let first = plan(&stack, &record, &inventory).unwrap();
    assert_eq!(
        first.changes.iter().map(|c| c.action).collect::<Vec<_>>(),
        [Action::Noop, Action::Retain, Action::Retain]
    );
    record.bindings.reverse();
    assert_eq!(first, plan(&stack, &record, &inventory).unwrap());
    assert_eq!(first.changes[1].kind, ResourceKind::Service);
}

#[test]
fn normalization_and_transient_service_metrics_do_not_create_drift() {
    let (stack, record, mut inventory) = fixture();
    inventory.networks[0].subnet_cidr = "172.31.20.42/24".to_owned();
    let deployment = inventory.services[0].deployment.as_mut().unwrap();
    deployment.instances[0].healthy = false;
    deployment.instances[0].state = VmState::Stopped;
    deployment.phase = ServicePhase::Degraded;
    deployment.log.push("transient status".to_owned());
    assert!(
        plan(&stack, &record, &inventory)
            .unwrap()
            .changes
            .iter()
            .all(|c| c.action == Action::Noop)
    );
}

#[test]
fn undeployed_and_busy_services_are_distinguished() {
    let (stack, record, mut inventory) = fixture();
    inventory.services[0].deployment.as_mut().unwrap().phase = ServicePhase::Starting;
    let busy = plan(&stack, &record, &inventory).unwrap();
    assert!(busy.blocked);
    assert_eq!(busy.changes[2].action, Action::Blocked);
    inventory.services[0].deployment = None;
    assert_eq!(
        plan(&stack, &record, &inventory).unwrap().changes[2].action,
        Action::Deploy
    );
}

#[test]
fn unfinished_deploy_recovery_blocks_noop_sync_and_is_part_of_approval() {
    let (stack, record, mut inventory) = fixture();
    let ready = plan(&stack, &record, &inventory).unwrap();
    let previous = inventory.services[0].deployment.as_ref().unwrap().clone();
    let deployment = inventory.services[0].deployment.as_mut().unwrap();
    deployment.phase = ServicePhase::Failed;
    deployment.update = Some(Box::new(firecrab_api_types::ServiceUpdate {
        failure: Some("DO-NOT-PRINT-runtime-error".into()),
        cancelled: false,
        previous: Box::new(previous),
        requested_spec: stack.micro_services["web"].config.clone(),
        strategy: firecrab_api_types::DeployStrategy::Rolling,
        rollback_of: None,
        restoring: true,
        routed_new: false,
        transition: None,
        retiring: vec![],
        completed: false,
    }));
    let recovering = plan(&stack, &record, &inventory).unwrap();
    assert!(recovering.blocked);
    assert_eq!(recovering.changes[2].action, Action::Blocked);
    assert_ne!(recovering.fingerprint, ready.fingerprint);
    assert!(
        !serde_json::to_string(&recovering)
            .unwrap()
            .contains("DO-NOT-PRINT")
    );
    inventory.services[0]
        .deployment
        .as_mut()
        .unwrap()
        .update
        .as_mut()
        .unwrap()
        .completed = true;
    let recovered = plan(&stack, &record, &inventory).unwrap();
    assert!(!recovered.blocked);
    assert_eq!(recovered.changes[2].action, Action::Noop);
    assert_eq!(recovered.fingerprint, ready.fingerprint);
}

#[test]
fn duplicate_ownership_and_mismatched_stack_identity_are_rejected() {
    let (stack, mut record, inventory) = fixture();
    record.bindings.push(record.bindings[0].clone());
    assert!(plan(&stack, &record, &inventory).is_err());
    record.bindings.pop();
    record.name = "other".to_owned();
    assert!(plan(&stack, &record, &inventory).is_err());
}

#[test]
fn a_rename_cannot_overwrite_another_uuid() {
    let (mut stack, record, mut inventory) = fixture();
    let mut other = inventory.networks[0].clone();
    other.id = Uuid::from_u128(100);
    other.name = "occupied".to_owned();
    inventory.networks.push(other);
    stack.micro_networks.get_mut("app").unwrap().name = "occupied".to_owned();
    let result = plan(&stack, &record, &inventory).unwrap();
    assert!(result.blocked);
    assert!(result.changes.iter().all(|c| c.action == Action::Blocked));
}

#[test]
fn explicit_uuid_ownership_remains_unambiguous_when_host_names_are_duplicated() {
    let (stack, record, mut inventory) = fixture();
    let mut other = inventory.networks[0].clone();
    other.id = Uuid::from_u128(100);
    other.subnet_cidr = "172.31.21.0/24".to_owned();
    inventory.networks.push(other);
    let mut other = inventory.vms[0].clone();
    other.id = Uuid::from_u128(101);
    inventory.vms.push(other);
    let result = plan(&stack, &record, &inventory).unwrap();
    assert!(!result.blocked);
    assert!(result.changes.iter().all(|c| c.action == Action::Noop));
}

fn template_pin() -> firecrab_stack::TemplatePin {
    firecrab_stack::TemplatePin {
        version: "v1".into(),
        kernel_sha256: "a".repeat(64),
        rootfs_sha256: "b".repeat(64),
        initrd_sha256: None,
        boot_args_sha256: "c".repeat(64),
    }
}

#[test]
fn approval_tracks_selected_template_content_and_explicit_pins_survive_alias_updates() {
    let (mut stack, record, mut inventory) = fixture();
    let original = plan(&stack, &record, &inventory).unwrap();
    assert_eq!(original.inputs.templates["worker"], template_pin());
    assert_eq!(original.inputs.service_digests["web"].len(), 71);
    let mut next = inventory.templates[0].clone();
    next.pin.version = "v2".into();
    next.pin.initrd_sha256 = Some("d".repeat(64));
    inventory.templates[0].current = false;
    inventory.templates.push(next);
    let changed = plan(&stack, &record, &inventory).unwrap();
    assert_ne!(original.fingerprint, changed.fingerprint);
    assert_eq!(changed.changes[1].action, Action::Replace);

    stack.micro_vms.get_mut("worker").unwrap().template_pin = Some(template_pin());
    let pinned = plan(&stack, &record, &inventory).unwrap();
    assert_eq!(pinned.changes[1].action, Action::Noop);
    inventory.templates[1].pin.kernel_sha256 = "e".repeat(64);
    assert_eq!(plan(&stack, &record, &inventory).unwrap(), pinned);
    inventory.templates[0].pin.initrd_sha256 = Some("f".repeat(64));
    let missing = plan(&stack, &record, &inventory).unwrap();
    assert!(missing.blocked);
    assert_eq!(missing.changes[1].action, Action::Blocked);
    assert!(!missing.inputs.templates.contains_key("worker"));
}

#[test]
fn template_provenance_drift_requires_replacement_instead_of_a_resource_update() {
    let (stack, record, inventory) = fixture();
    for field in ["version", "kernel", "rootfs", "boot", "initrd"] {
        let mut inventory = inventory.clone();
        let vm = &mut inventory.vms[0];
        match field {
            "version" => vm.template_version = "other".into(),
            "kernel" => vm.template_kernel_sha256 = "d".repeat(64),
            "rootfs" => vm.template_rootfs_sha256 = "d".repeat(64),
            "boot" => vm.template_boot_args_sha256 = "d".repeat(64),
            _ => vm.template_pin.as_mut().unwrap().initrd_sha256 = Some("d".repeat(64)),
        }
        let plan = plan(&stack, &record, &inventory).unwrap();
        assert_eq!(plan.changes[1].action, Action::Replace, "{field}");
        assert_eq!(plan.changes[1].fields, ["template"]);
    }
}

#[test]
fn template_pin_validation_is_bounded_and_never_echoes_submitted_hashes() {
    let (mut stack, _, _) = fixture();
    let mut pin = template_pin();
    pin.kernel_sha256 = "DO-NOT-PRINT-secret".into();
    stack.micro_vms.get_mut("worker").unwrap().template_pin = Some(pin);
    let errors = firecrab_stack::validate(&stack).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.path == "microVMs.worker.templatePin")
    );
    assert!(
        !serde_json::to_string(&errors)
            .unwrap()
            .contains("DO-NOT-PRINT")
    );
}
