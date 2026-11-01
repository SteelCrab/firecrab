use super::super::tests::{bind, fixture};
use super::*;
async fn setup() -> (tempfile::TempDir, AppState, Binding, Target) {
    let (dir, state, seed) = fixture().await;
    bind(&state, &seed).await;
    let b = binding(&state, seed.project_id).unwrap();
    let target = Target {
        project: b.project_id,
        database: b.database_id,
        volume: b.volume_id,
        generation: b.generation,
        runtime: guest::Binding {
            vm_id: b.vm_id,
            runtime_id: Uuid::new_v4(),
        },
        caller: Caller::default(),
        directory: "api".into(),
    };
    (dir, state, b, target)
}
fn operation(db: &db::ProjectDb, pending: bool) -> (WriteRequest, u64) {
    let before = db.observe_file("api/auth.txt", b"before").unwrap();
    let request = WriteRequest {
        path: "auth.txt".into(),
        operation: Uuid::new_v4(),
        expected: before.metadata.id,
        content: b"after".to_vec(),
    };
    db.stage_file(Operation {
        id: request.operation,
        path: "api/auth.txt".into(),
        expected: request.expected,
        content: request.content.clone(),
        author: SandboxPrincipal::Local,
        restored_from: None,
        created_at: 1,
        version: None,
        state: "pending".into(),
    })
    .unwrap();
    let after = if pending {
        0
    } else {
        db.finish_file(request.operation).unwrap().metadata.id
    };
    (request, after)
}
#[tokio::test]
async fn confirmed_receipts_remain_queryable_while_stopped_without_repeating_a_write() {
    let (_dir, state, b, target) = setup().await;
    let db = open(&state, &b).unwrap();
    let (request, after) = operation(&db, false);
    drop(db);
    let trace = settle(
        state.clone(),
        target.clone(),
        Caller::default(),
        request.clone(),
        Uuid::new_v4(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(trace.path, "api/auth.txt");
    assert_eq!(trace.before_version, request.expected);
    assert_eq!(trace.after_version, after);
    let cached = write(
        state.clone(),
        target.clone(),
        request.clone(),
        Uuid::new_v4(),
    )
    .await
    .unwrap();
    assert_eq!(cached.version.id, after);
    let mut changed = request.clone();
    changed.content = b"different".to_vec();
    assert!(
        write(
            state.clone(),
            target.clone(),
            changed.clone(),
            Uuid::new_v4()
        )
        .await
        .is_err()
    );
    assert!(
        settle(
            state.clone(),
            target.clone(),
            Caller::default(),
            changed,
            Uuid::new_v4()
        )
        .await
        .is_err()
    );
    let mut absent = request;
    absent.operation = Uuid::new_v4();
    assert!(
        settle(
            state.clone(),
            target,
            Caller::default(),
            absent,
            Uuid::new_v4()
        )
        .await
        .unwrap()
        .is_none()
    );
}
#[tokio::test]
async fn pending_writes_stay_pending_when_a_receipt_cannot_be_confirmed() {
    let (_dir, state, b, target) = setup().await;
    let db = open(&state, &b).unwrap();
    let (request, _) = operation(&db, true);
    drop(db);
    assert!(
        settle(
            state.clone(),
            target,
            Caller::default(),
            request.clone(),
            Uuid::new_v4()
        )
        .await
        .is_err()
    );
    let db = open(&state, &b).unwrap();
    let stored = db.file_operation(request.operation).unwrap().unwrap();
    assert_eq!(stored.state, "pending");
    assert!(stored.version.is_none());
}
#[tokio::test]
async fn generation_and_exact_boot_are_fenced_before_native_mutation() {
    let (_dir, state, b, mut target) = setup().await;
    let request = WriteRequest {
        path: "auth.txt".into(),
        operation: Uuid::new_v4(),
        expected: 0,
        content: b"after".to_vec(),
    };
    target.generation += 1;
    assert!(
        write(
            state.clone(),
            target.clone(),
            request.clone(),
            Uuid::new_v4()
        )
        .await
        .is_err()
    );
    target.generation = b.generation;
    let db = open(&state, &b).unwrap();
    db.set_import_sha(&"a".repeat(64)).unwrap();
    db.mark_initialized().unwrap();
    drop(db);
    let current = Uuid::new_v4();
    state.store.verify_project_workspace(&b, current).unwrap();
    {
        let mut vms = state.vms.lock().unwrap();
        let vm = vms.get_mut(&b.vm_id).unwrap();
        vm.state = VmState::Running;
        vm.last_runtime_id = Some(current);
    }
    assert_ne!(current, target.runtime.runtime_id);
    let error = write(state.clone(), target, request.clone(), Uuid::new_v4())
        .await
        .err()
        .unwrap();
    assert_eq!(error.code(), "workspace_conflict");
    let b = binding(&state, b.project_id).unwrap();
    let db = open(&state, &b).unwrap();
    assert!(db.file_operation(request.operation).unwrap().is_none());
}
#[tokio::test]
async fn project_write_and_original_operation_authority_are_required() {
    let (_dir, state, b, target) = setup().await;
    let db = open(&state, &b).unwrap();
    let (request, _) = operation(&db, false);
    drop(db);
    let host = Uuid::new_v4();
    let actor: Caller = serde_json::from_value(
        serde_json::json!({"principal":SandboxPrincipal::Host(host),"operator":false}),
    )
    .unwrap();
    state
        .store
        .save_sandbox_access(
            host,
            &SandboxAccessPolicy {
                grants: vec![SandboxResourceGrant {
                    resource: SandboxResource::Project { id: b.project_id },
                    permissions: vec![SandboxPermission::Read],
                }],
            },
        )
        .unwrap();
    let mut other = target.clone();
    other.caller = actor;
    assert!(
        write(state.clone(), other, request.clone(), Uuid::new_v4())
            .await
            .is_err()
    );
    state
        .store
        .save_sandbox_access(
            host,
            &SandboxAccessPolicy {
                grants: vec![SandboxResourceGrant {
                    resource: SandboxResource::Project { id: b.project_id },
                    permissions: vec![SandboxPermission::Read, SandboxPermission::Write],
                }],
            },
        )
        .unwrap();
    assert!(
        settle(state, target, actor, request, Uuid::new_v4())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn every_folder_rpc_requires_current_project_and_vm_execution_grants() {
    let (_dir, state, b, _target) = setup().await;
    let db = open(&state, &b).unwrap();
    db.set_import_sha(&"a".repeat(64)).unwrap();
    db.mark_initialized().unwrap();
    drop(db);
    let boot = Uuid::new_v4();
    state.store.verify_project_workspace(&b, boot).unwrap();
    {
        let mut vms = state.vms.lock().unwrap();
        let vm = vms.get_mut(&b.vm_id).unwrap();
        vm.state = VmState::Running;
        vm.last_runtime_id = Some(boot);
    }
    let host = Uuid::new_v4();
    let caller: Caller = serde_json::from_value(
        serde_json::json!({"principal":SandboxPrincipal::Host(host),"operator":false}),
    )
    .unwrap();
    let mut policy = SandboxAccessPolicy {
        grants: vec![SandboxResourceGrant {
            resource: SandboxResource::Project { id: b.project_id },
            permissions: vec![SandboxPermission::Read, SandboxPermission::Write],
        }],
    };
    state.store.save_sandbox_access(host, &policy).unwrap();
    let mut ctx = files::Files::open(state.clone(), b.project_id, caller, true, Uuid::new_v4())
        .await
        .unwrap();
    assert!(ctx.permission().is_ok());
    ctx.require_execution = true;
    assert!(ctx.permission().is_err());
    policy.grants[0]
        .permissions
        .push(SandboxPermission::Execute);
    state.store.save_sandbox_access(host, &policy).unwrap();
    assert!(ctx.permission().is_err());
    policy.grants.push(SandboxResourceGrant {
        resource: SandboxResource::Vm { id: b.vm_id },
        permissions: vec![SandboxPermission::Execute],
    });
    state.store.save_sandbox_access(host, &policy).unwrap();
    assert!(ctx.permission().is_ok());
    policy.grants.pop();
    state.store.save_sandbox_access(host, &policy).unwrap();
    assert!(ctx.permission().is_err());
}
