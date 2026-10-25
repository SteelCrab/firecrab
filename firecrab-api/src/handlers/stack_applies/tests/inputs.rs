use super::*;
use crate::templates::{TemplateSpec, TemplateVersion};
use std::{path::Path, sync::Arc};

fn install(state: &AppState, root: &Path, version: &str, boot: &str) -> Arc<TemplateVersion> {
    std::fs::write(
        root.join(format!("kernel-{version}")),
        format!("kernel-{version}"),
    )
    .unwrap();
    std::fs::write(
        root.join(format!("rootfs-{version}")),
        format!("rootfs-{version}"),
    )
    .unwrap();
    state
        .templates
        .register_spec(TemplateSpec {
            alias: "test-template".into(),
            version: version.into(),
            kernel: format!("kernel-{version}").into(),
            rootfs: format!("rootfs-{version}").into(),
            initrd: None,
            boot_args: boot.into(),
        })
        .unwrap()
}

pub(super) fn pin_record(state: &AppState, root: &Path, vm: &mut crate::model::VmRecord) {
    let template = install(state, root, "v1", "console=ttyS0");
    vm.template = template.name.clone();
    vm.template_version = template.version.clone();
    vm.template_kernel_sha256 = template.kernel.sha256().into();
    vm.template_rootfs_sha256 = template.rootfs.sha256().into();
    vm.template_boot_args_sha256 = template.boot_args_sha256();
}

fn vm_stack() -> Value {
    let mut stack = declaration();
    stack["microVMs"] = json!({"worker":{"name":"worker","template":"test-template","network":"app","ram":128,"cpu":1,"diskGb":1}});
    stack
}

#[tokio::test]
async fn stack_apply_rejects_alias_repoint_same_version_content_change_and_tampering_before_mutation()
 {
    let root = tempfile::tempdir().unwrap();
    let (router, state, calls) = fixture(root.path(), None).await;
    install(&state, root.path(), "v1", "console=ttyS0");
    let stack = vm_stack();
    let old = planned(&router, &stack, Uuid::new_v4()).await;
    install(&state, root.path(), "v2", "console=ttyS0");
    let (code, error) = request(&router, "POST", "/api/stacks/demo/apply", old).await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "stale_stack_plan");

    let same_version = planned(&router, &stack, Uuid::new_v4()).await;
    install(&state, root.path(), "v2", "console=ttyS0 quiet");
    let (_, error) = request(&router, "POST", "/api/stacks/demo/apply", same_version).await;
    assert_eq!(error["error"]["code"], "stale_stack_plan");

    let tamper = planned(&router, &stack, Uuid::new_v4()).await;
    std::fs::write(root.path().join("rootfs-v2"), "DO-NOT-PRINT-tampered").unwrap();
    let (code, error) = request(&router, "POST", "/api/stacks/demo/apply", tamper).await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "stack_template_changed");
    assert!(!error.to_string().contains("DO-NOT-PRINT"));
    assert!(calls.lock().unwrap().is_empty());
    assert!(state.store.list_micro_networks().unwrap().is_empty());
    assert!(
        state
            .store
            .stack("demo")
            .unwrap()
            .unwrap()
            .bindings
            .is_empty()
    );
}

#[tokio::test]
async fn stack_apply_uses_the_approved_version_after_alias_repoint_and_retains_its_pin_on_restart()
{
    let root = tempfile::tempdir().unwrap();
    let (router, state, _) = fixture(root.path(), None).await;
    let v1 = install(&state, root.path(), "v1", "console=ttyS0");
    let mut stack = vm_stack();
    stack["microVMs"]["worker"]["templatePin"] = serde_json::to_value(v1.stack_pin()).unwrap();
    let id = Uuid::new_v4();
    let body = planned(&router, &stack, id).await;
    install(&state, root.path(), "v2", "console=ttyS0 quiet");
    let (code, error) = request(&router, "POST", "/api/stacks/demo/apply", body.clone()).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{error}");
    let status = terminal(&router, id).await;
    assert_eq!(status.state, ApplyState::Succeeded, "{status:?}");
    let vm = state
        .store
        .load_all()
        .unwrap()
        .into_values()
        .next()
        .unwrap();
    assert_eq!(vm.template_version, "v1");
    assert_eq!(status.inputs.templates["worker"], v1.stack_pin());
    assert_eq!(
        state.store.stack_vm_pin(vm.id, true).unwrap(),
        Some(v1.stack_pin())
    );
    let reopened = crate::persistence::Store::open(&root.path().join("state.db")).unwrap();
    assert_eq!(
        reopened
            .stored_stack_apply(id)
            .unwrap()
            .unwrap()
            .status
            .inputs,
        status.inputs
    );
    assert_eq!(
        reopened.stack_vm_pin(vm.id, true).unwrap(),
        Some(v1.stack_pin())
    );
    assert_eq!(
        request(&router, "POST", "/api/stacks/demo/apply", body)
            .await
            .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(state.store.load_all().unwrap().len(), 1);
    // A later start must enforce the old approval even after the registry has
    // accepted a same-version reinstall with an added initrd.
    std::fs::write(root.path().join("initrd"), "new-initrd").unwrap();
    let changed = state
        .templates
        .register_spec(TemplateSpec {
            alias: "test-template".into(),
            version: "v1".into(),
            kernel: "kernel-v1".into(),
            rootfs: "rootfs-v1".into(),
            initrd: Some("initrd".into()),
            boot_args: "console=ttyS0".into(),
        })
        .unwrap();
    stack["microVMs"]["worker"]["templatePin"] = serde_json::to_value(changed.stack_pin()).unwrap();
    let (_, plan) = request(&router, "POST", "/api/stacks/demo/plan", stack).await;
    assert_eq!(plan["changes"][1]["action"], "replace");
    let error = crate::handlers::vms::start_vm(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        axum::extract::Path(vm.id.to_string()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "internal_error");
    let failed = state.vms.lock().unwrap()[&vm.id].clone();
    assert_eq!(failed.state, firecrab_api_types::VmState::Error);
    assert!(
        failed
            .startup_timeline
            .last()
            .unwrap()
            .detail
            .as_ref()
            .unwrap()
            .starts_with("Approved stack template content changed")
    );
    assert!(state.processes.lock().unwrap().is_empty());
    assert!(
        state.store.load_all().unwrap()[&vm.id]
            .disk_generation
            .is_none()
    );
    install(&state, root.path(), "v1", "console=ttyS0");
    std::fs::write(root.path().join("kernel-v1"), "DO-NOT-PRINT-tampered").unwrap();
    crate::handlers::vms::start_vm(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        axum::extract::Path(vm.id.to_string()),
    )
    .await
    .unwrap_err();
    let failed = state.vms.lock().unwrap()[&vm.id].clone();
    let detail = failed
        .startup_timeline
        .last()
        .unwrap()
        .detail
        .as_ref()
        .unwrap();
    assert!(
        detail.starts_with("Approved stack template artifact changed"),
        "{detail}"
    );
    assert!(!detail.contains("DO-NOT-PRINT"));
    assert!(failed.disk_generation.is_none());
    assert!(state.processes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn git_source_blocks_independent_service_writers_even_when_paused_and_releases_explicitly() {
    let root = tempfile::tempdir().unwrap();
    let (router, state, _) = fixture(root.path(), None).await;
    let service: firecrab_api_types::MicroServiceResponse = serde_json::from_value(json!({
        "id":Uuid::new_v4(), "name":"web", "image":format!("ghcr.io/team/app@sha256:{}", "a".repeat(64)), "port":8080,"ram":128,"cpu":1,
    })).unwrap();
    state.store.insert_micro_service(&service).unwrap();
    let binding = Binding {
        kind: ResourceKind::Service,
        key: "web".into(),
        resource_id: service.id,
    };
    state.store.bind_stack_resource("demo", &binding).unwrap();
    let source: firecrab_stack::ConfigureGitSource = serde_json::from_value(json!({
        "expectedGeneration":0,"source":{"repository":"https://github.com/example/infra","ref":"main","manifestPath":"stack.yaml","pollIntervalSeconds":60,"paused":true}
    })).unwrap();
    state
        .store
        .configure_stack_source("demo", &source, 0)
        .unwrap();
    for (method, suffix, body) in [
        ("PUT", "", serde_json::to_value(&service.spec).unwrap()),
        ("DELETE", "", Value::Null),
        ("POST", "/deployment", json!({"hostPort":30100})),
        ("POST", "/stop", Value::Null),
        (
            "POST",
            "/updates",
            json!({"operationId":Uuid::new_v4(),"spec":service.spec,"strategy":"rolling"}),
        ),
        (
            "POST",
            "/rollback",
            json!({"operationId":Uuid::new_v4(),"releaseId":Uuid::new_v4(),"strategy":"rolling"}),
        ),
        (
            "POST",
            "/deployment/retry",
            json!({"operationId":Uuid::new_v4()}),
        ),
    ] {
        let (code, error) = request(
            &router,
            method,
            &format!("/api/micro-services/{}{suffix}", service.id),
            body,
        )
        .await;
        assert_eq!(code, StatusCode::CONFLICT, "{method} {suffix}: {error}");
        assert_eq!(error["error"]["code"], "stack_source_managed");
    }
    let body = planned(&router, &declaration(), Uuid::new_v4()).await;
    let (code, error) = request(&router, "POST", "/api/stacks/demo/apply", body).await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "stack_source_managed");
    assert_eq!(
        state.store.micro_service(service.id).unwrap(),
        Some(service.clone())
    );
    assert!(
        state
            .store
            .service_deployment(service.id)
            .unwrap()
            .is_none()
    );
    state.store.unbind_stack_resource("demo", &binding).unwrap();
    assert_eq!(
        request(
            &router,
            "DELETE",
            &format!("/api/micro-services/{}", service.id),
            Value::Null
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
}
