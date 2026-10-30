use super::*;
use crate::{
    ai_sandbox::access::LocalOperator,
    code_runtime::{self, Driver},
    persistence::{
        Store,
        code_runtime::{Admission, Record},
        code_workspaces::{DeleteResult, WriteResult},
    },
    pipe_auth::SavedAuth,
};
use axum::{
    Extension,
    body::{Body, to_bytes},
    http::StatusCode,
};
use firecrab_api_types::*;
use serde_json::{Value, json};
use std::sync::Mutex;
use tower::ServiceExt;

const SHA: &str = "1234567890abcdef1234567890abcdef12345678";
struct Fixture {
    dir: tempfile::TempDir,
    state: AppState,
    app: Router,
    id: Uuid,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let templates =
        crate::templates::TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
    let mut state = AppState::with_db_file(templates, dir.path().join("state.db"))
        .await
        .unwrap();
    state.code_jobs = Arc::new(tokio::sync::Semaphore::new(0)); // Queue API operations without creating host resources.
    state
        .store
        .put_pipe_auth(&SavedAuth::Github {
            username: "octocat".into(),
            token: "code-private-runtime-token".into(),
        })
        .unwrap();
    let id = Uuid::new_v4();
    let spec=serde_json::from_value(json!({"name":"web-app","github":{"account":"octocat","owner":"team","repository":"web-app","branch":"main"},"runtime":"python","resources":{"cpu":2,"ram":4096,"diskGb":16},"agent":null})).unwrap();
    assert!(matches!(
        state
            .store
            .write_code_workspace(
                id,
                CodeWorkspaceWrite {
                    expected_revision: 0,
                    spec
                },
                1
            )
            .unwrap(),
        WriteResult::Written(_)
    ));
    let app = router(&state).layer(Extension(LocalOperator));
    Fixture {
        dir,
        state,
        app,
        id,
    }
}

fn router(state: &AppState) -> Router {
    build_router(
        state.clone(),
        &HttpConfig::from_values("127.0.0.1:5523", "http://localhost:8080").unwrap(),
    )
}

fn placement() -> CodeRuntimePlacement {
    CodeRuntimePlacement {
        template: SandboxTemplatePin {
            name: "dev".into(),
            version: "v1".into(),
            kernel_sha256: "a".repeat(64),
            rootfs_sha256: "b".repeat(64),
            boot_args_sha256: "c".repeat(64),
            initrd_sha256: None,
        },
        micro_network_id: Uuid::new_v4(),
        storage_root: "default".into(),
    }
}

fn admitted(f: &Fixture, action: CodeRuntimeAction, place: Option<CodeRuntimePlacement>) -> Record {
    let Admission::Accepted(record) = f
        .state
        .store
        .admit_code_runtime(f.id, 1, place, action)
        .unwrap()
    else {
        panic!("admission failed");
    };
    *record
}

async fn call(f: &Fixture, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("host", "127.0.0.1:5523")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    if status.is_success() {
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("code-private"));
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}

#[derive(Default)]
struct Machine {
    created: bool,
    running: bool,
    creates: usize,
    prepares: usize,
    fail_prepare: bool,
    fail_sync: bool,
    files: String,
    runtime: Option<Uuid>,
}
#[derive(Default)]
struct Fake(Mutex<Machine>);

impl Driver for Fake {
    async fn create(&self, record: &Record) -> Result<(), CodeRuntimeFailure> {
        let mut vm = self.0.lock().unwrap();
        if !vm.created {
            if record.vm_created {
                return Err(CodeRuntimeFailure::OwnershipChanged);
            }
            vm.created = true;
            vm.creates += 1;
        }
        Ok(())
    }
    async fn start(&self, _: &Record) -> Result<(), CodeRuntimeFailure> {
        let mut vm = self.0.lock().unwrap();
        if !vm.running {
            vm.running = true;
            vm.runtime = Some(Uuid::new_v4());
        }
        Ok(())
    }
    async fn prepare(&self, _: &Record) -> Result<(Uuid, String), CodeRuntimeFailure> {
        let mut vm = self.0.lock().unwrap();
        vm.prepares += 1;
        if vm.fail_prepare {
            return Err(CodeRuntimeFailure::PreparationFailed);
        }
        if vm.files.is_empty() {
            vm.files = "initial checkout".into();
        }
        Ok((vm.runtime.unwrap(), SHA.into()))
    }
    async fn stop(&self, _: &Record) -> Result<(), CodeRuntimeFailure> {
        let mut vm = self.0.lock().unwrap();
        if vm.fail_sync {
            return Err(CodeRuntimeFailure::SyncFailed);
        }
        vm.running = false;
        Ok(())
    }
}

#[tokio::test]
async fn code_runtime_reserves_one_vm_across_concurrent_database_connections_and_rejects_conflicting_placement()
 {
    let f = fixture().await;
    let place = placement();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let tasks: Vec<_> = (0..2)
        .map(|_| {
            let store = Store::open(&f.dir.path().join("state.db")).unwrap();
            let barrier = barrier.clone();
            let place = place.clone();
            let id = f.id;
            std::thread::spawn(move || {
                barrier.wait();
                let Admission::Accepted(record) = store
                    .admit_code_runtime(id, 1, Some(place), CodeRuntimeAction::Start)
                    .unwrap()
                else {
                    panic!()
                };
                record.runtime
            })
        })
        .collect();
    let results: Vec<_> = tasks.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results[0], results[1]);
    assert!(matches!(
        f.state
            .store
            .admit_code_runtime(f.id, 1, Some(placement()), CodeRuntimeAction::Start)
            .unwrap(),
        Admission::PlacementChanged
    ));
    assert!(matches!(
        f.state
            .store
            .admit_code_runtime(f.id, 1, None, CodeRuntimeAction::Stop)
            .unwrap(),
        Admission::Busy
    ));
    assert_eq!(f.state.store.code_runtime_pending().unwrap().len(), 1);
}

#[tokio::test]
async fn code_runtime_start_stop_resume_keeps_the_vm_identity_commit_and_working_files() {
    let f = fixture().await;
    let vm = Fake::default();
    let record = admitted(&f, CodeRuntimeAction::Start, Some(placement()));
    let vm_id = record.runtime.vm_id;
    code_runtime::execute(&f.state.store, record, &vm).await;
    let ready = f.state.store.code_runtime(f.id).unwrap().unwrap();
    assert_eq!(ready.runtime.phase, CodeRuntimePhase::Ready);
    assert_eq!(ready.runtime.initial_commit.as_deref(), Some(SHA));
    vm.0.lock().unwrap().files = "uncommitted changes and local commits".into();
    code_runtime::execute(
        &f.state.store,
        admitted(&f, CodeRuntimeAction::Stop, None),
        &vm,
    )
    .await;
    assert_eq!(
        f.state
            .store
            .code_runtime(f.id)
            .unwrap()
            .unwrap()
            .runtime
            .phase,
        CodeRuntimePhase::Stopped
    );
    code_runtime::execute(
        &f.state.store,
        admitted(&f, CodeRuntimeAction::Start, None),
        &vm,
    )
    .await;
    let resumed = f.state.store.code_runtime(f.id).unwrap().unwrap();
    assert_eq!(resumed.runtime.vm_id, vm_id);
    assert_eq!(resumed.runtime.initial_commit, ready.runtime.initial_commit);
    assert_ne!(resumed.runtime.runtime_id, ready.runtime.runtime_id);
    let machine = vm.0.lock().unwrap();
    assert_eq!(machine.creates, 1);
    assert_eq!(machine.files, "uncommitted changes and local commits");
}

#[tokio::test]
async fn code_runtime_recovery_reuses_a_vm_created_before_the_api_crashed() {
    let f = fixture().await;
    let mut record = admitted(&f, CodeRuntimeAction::Start, Some(placement()));
    record.runtime.phase = CodeRuntimePhase::Provisioning;
    f.state.store.save_code_runtime(&record).unwrap();
    let vm = Fake(Mutex::new(Machine {
        created: true,
        creates: 1,
        ..Machine::default()
    }));
    let reopened = Store::open(&f.dir.path().join("state.db")).unwrap();
    let pending = reopened.code_runtime_pending().unwrap().pop().unwrap();
    code_runtime::execute(&reopened, pending, &vm).await;
    assert_eq!(
        reopened.code_runtime(f.id).unwrap().unwrap().runtime.phase,
        CodeRuntimePhase::Ready
    );
    assert_eq!(vm.0.lock().unwrap().creates, 1);
}

#[tokio::test]
async fn code_runtime_failure_retains_resources_and_stale_workers_cannot_overwrite_a_new_attempt() {
    let f = fixture().await;
    let original = admitted(&f, CodeRuntimeAction::Start, Some(placement()));
    let vm = Fake(Mutex::new(Machine {
        fail_prepare: true,
        ..Machine::default()
    }));
    code_runtime::execute(&f.state.store, original.clone(), &vm).await;
    let failed = f.state.store.code_runtime(f.id).unwrap().unwrap();
    assert_eq!(failed.runtime.phase, CodeRuntimePhase::Failed);
    assert_eq!(
        failed.runtime.error,
        Some(CodeRuntimeFailure::PreparationFailed)
    );
    assert!(failed.vm_created);
    let retry = admitted(&f, CodeRuntimeAction::Start, None);
    assert_ne!(retry.runtime.operation_id, original.runtime.operation_id);
    assert!(!f.state.store.save_code_runtime(&original).unwrap());
    vm.0.lock().unwrap().fail_prepare = false;
    code_runtime::execute(&f.state.store, retry, &vm).await;
    assert_eq!(vm.0.lock().unwrap().creates, 1);
    assert_eq!(
        f.state
            .store
            .code_runtime(f.id)
            .unwrap()
            .unwrap()
            .runtime
            .phase,
        CodeRuntimePhase::Ready
    );
}

#[tokio::test]
async fn code_runtime_failed_sync_retains_the_running_guest_and_data() {
    let f = fixture().await;
    let vm = Fake::default();
    code_runtime::execute(
        &f.state.store,
        admitted(&f, CodeRuntimeAction::Start, Some(placement())),
        &vm,
    )
    .await;
    vm.0.lock().unwrap().fail_sync = true;
    code_runtime::execute(
        &f.state.store,
        admitted(&f, CodeRuntimeAction::Stop, None),
        &vm,
    )
    .await;
    let failed = f.state.store.code_runtime(f.id).unwrap().unwrap();
    assert_eq!(failed.runtime.error, Some(CodeRuntimeFailure::SyncFailed));
    assert!(vm.0.lock().unwrap().running);
    assert_eq!(vm.0.lock().unwrap().files, "initial checkout");
}

#[tokio::test]
async fn code_runtime_bound_configuration_rejects_file_affecting_edits_and_allows_account_selection_when_idle()
 {
    let f = fixture().await;
    let record = admitted(&f, CodeRuntimeAction::Start, Some(placement()));
    let mut spec = f.state.store.code_workspace(f.id).unwrap().unwrap().spec;
    spec.name = "renamed".into();
    assert!(matches!(
        f.state
            .store
            .write_code_workspace(
                f.id,
                CodeWorkspaceWrite {
                    expected_revision: 1,
                    spec: spec.clone()
                },
                2
            )
            .unwrap(),
        WriteResult::RuntimeBound
    ));
    code_runtime::execute(&f.state.store, record, &Fake::default()).await;
    let mut changed = spec.clone();
    changed.github.branch = "another-branch".into();
    assert!(matches!(
        f.state
            .store
            .write_code_workspace(
                f.id,
                CodeWorkspaceWrite {
                    expected_revision: 1,
                    spec: changed
                },
                2
            )
            .unwrap(),
        WriteResult::RuntimeBound
    ));
    assert!(matches!(
        f.state
            .store
            .write_code_workspace(
                f.id,
                CodeWorkspaceWrite {
                    expected_revision: 1,
                    spec
                },
                2
            )
            .unwrap(),
        WriteResult::Written(_)
    ));
    assert!(matches!(
        f.state.store.delete_code_workspace(f.id, 2).unwrap(),
        DeleteResult::RuntimeBound
    ));
}

#[tokio::test]
async fn code_runtime_lease_excludes_duplicate_workers_and_detach_does_not_recreate_a_binding() {
    let f = fixture().await;
    let mut record = admitted(&f, CodeRuntimeAction::Start, Some(placement()));
    let first = code_runtime::lock(&f.state, f.id).unwrap();
    assert!(code_runtime::lock(&f.state, f.id).is_err());
    drop(first);
    let _next = code_runtime::lock(&f.state, f.id).unwrap();
    record.runtime.phase = CodeRuntimePhase::Stopped;
    f.state.store.save_code_runtime(&record).unwrap();
    assert!(f.state.store.detach_code_runtime(f.id, 1).unwrap());
    assert!(!f.state.store.save_code_runtime(&record).unwrap());
    assert!(f.state.store.code_runtime(f.id).unwrap().is_none());
    assert!(matches!(
        f.state.store.delete_code_workspace(f.id, 1).unwrap(),
        DeleteResult::Deleted
    ));
}

#[tokio::test]
async fn code_runtime_routes_enforce_operator_revision_and_input_contracts_without_creating_vms() {
    let f = fixture().await;
    let path = format!("/api/code/workspaces/{}/runtime", f.id);
    let response = router(&f.state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("{path}/start"))
                .header("host", "127.0.0.1:5523")
                .header("content-type", "application/json")
                .body(Body::from("{\"expectedRevision\":1}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    for body in [
        json!({"expectedRevision":0}),
        json!({"expectedRevision":1,"vmId":Uuid::new_v4()}),
        json!({"expectedRevision":1,"token":"code-private-submitted"}),
        json!({"expectedRevision":1,"projectPath":"/root"}),
    ] {
        assert_eq!(
            call(&f, "POST", &format!("{path}/start"), body).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(call(&f,"POST",&path,json!({"expectedRevision":1,"template":"uninstalled","microNetworkId":Uuid::new_v4(),"storageRoot":null})).await.0,StatusCode::BAD_REQUEST);
    assert!(f.state.store.code_runtime(f.id).unwrap().is_none());
    assert!(f.state.store.load_all().unwrap().is_empty());
    let record = admitted(&f, CodeRuntimeAction::Start, Some(placement()));
    let (status, queued) = call(
        &f,
        "POST",
        &format!("{path}/start"),
        json!({"expectedRevision":1}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(queued["vmId"], record.runtime.vm_id.to_string());
    assert_eq!(
        queued["operationId"],
        record.runtime.operation_id.to_string()
    );
    assert_eq!(
        call(
            &f,
            "POST",
            &format!("{path}/stop"),
            json!({"expectedRevision":1})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn code_runtime_owned_vm_mutation_routes_are_fenced_until_detach() {
    let f = fixture().await;
    let mut record = admitted(&f, CodeRuntimeAction::Start, Some(placement()));
    let path = format!("/api/vms/{}", record.runtime.vm_id);
    for (method, suffix, body) in [
        ("POST", "/start", Value::Null),
        ("POST", "/stop", Value::Null),
        ("POST", "/pause", Value::Null),
        ("POST", "/resume", Value::Null),
        ("PUT", "/storage", json!({"storageRoot":"default"})),
        ("DELETE", "", Value::Null),
        ("PUT", "", json!({"ram":8192,"cpu":2,"diskGb":16})),
        ("PUT", "/shells", json!({"shellIds":[]})),
        ("PUT", "/port-forwards", json!({"portForwards":[]})),
    ] {
        let (status, result) = call(&f, method, &format!("{path}{suffix}"), body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{result}");
        assert_eq!(result["error"]["code"], "code_vm_owned");
    }
    record.runtime.phase = CodeRuntimePhase::Stopped;
    f.state.store.save_code_runtime(&record).unwrap();
    let (status, result) = call(
        &f,
        "POST",
        &format!("/api/code/workspaces/{}/runtime/detach", f.id),
        json!({"expectedRevision":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["vmId"], record.runtime.vm_id.to_string());
    assert!(
        f.state
            .store
            .code_runtime_owner(record.runtime.vm_id)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn code_runtime_read_does_not_claim_ready_when_the_live_vm_disappears() {
    let f = fixture().await;
    let vm = Fake::default();
    code_runtime::execute(
        &f.state.store,
        admitted(&f, CodeRuntimeAction::Start, Some(placement())),
        &vm,
    )
    .await;
    let (status, result) = call(
        &f,
        "GET",
        &format!("/api/code/workspaces/{}/runtime", f.id),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["phase"], "reconcile_required");
    assert_eq!(result["vmState"], Value::Null);
}
