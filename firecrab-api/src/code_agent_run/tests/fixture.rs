use super::*;
use crate::pipe_ai::SavedConnection;
use crate::{
    ai_sandbox::access::LocalOperator,
    persistence::{code_runtime::Admission, code_workspaces::WriteResult},
    server::{HttpConfig, build_router},
};
use serde_json::json;
use std::sync::Arc;

pub(super) struct Fixture {
    pub(super) _dir: tempfile::TempDir,
    pub(super) state: AppState,
    pub(super) app: Router,
    pub(super) id: Uuid,
    pub(super) record: Record,
}
pub(super) async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let templates =
        crate::templates::TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
    let mut state = AppState::with_db_file(templates, dir.path().join("state.db"))
        .await
        .unwrap();
    state.code_jobs = Arc::new(tokio::sync::Semaphore::new(0));
    let id = Uuid::new_v4();
    state
        .store
        .put_pipe_auth(&crate::pipe_auth::SavedAuth::Github {
            username: "octocat".into(),
            token: "not-an-editor-credential".into(),
        })
        .unwrap();
    let account = Uuid::new_v4();
    state
        .store
        .save_ai_connection(&SavedConnection {
            connection: AiConnection {
                id: account,
                name: "Assigned account".into(),
                provider: AiProvider::Openai,
                authentication: AiAuthentication::ApiKey,
                endpoint: crate::pipe_ai::ENDPOINT.into(),
                models: vec![],
                streaming: false,
                tool_calls: false,
            },
            api_key: "host-only-synthetic-key".into(),
            oauth: None,
        })
        .unwrap();
    let spec=serde_json::from_value(json!({"name":"web-app","github":{"account":"octocat","owner":"team","repository":"web-app","branch":"main"},"runtime":"python","resources":{"cpu":2,"ram":4096,"diskGb":16},"agent":{"agent":"codex","connectionId":account}})).unwrap();
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
    let network =
        crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);
    let placement = CodeRuntimePlacement {
        template: SandboxTemplatePin {
            name: "dev".into(),
            version: "v1".into(),
            kernel_sha256: "a".repeat(64),
            rootfs_sha256: "b".repeat(64),
            initrd_sha256: None,
            boot_args_sha256: "c".repeat(64),
        },
        micro_network_id: network,
        storage_root: state.storage.default_id().into(),
    };
    let Admission::Accepted(mut r) = state
        .store
        .admit_code_runtime(id, 1, Some(placement), CodeRuntimeAction::Start)
        .unwrap()
    else {
        panic!()
    };
    r.vm_created = true;
    r.runtime.phase = CodeRuntimePhase::Ready;
    r.runtime.runtime_id = Some(Uuid::new_v4());
    r.runtime.initial_commit = Some("a".repeat(40));
    assert!(state.store.save_code_runtime(&r).unwrap());
    let mut vm = crate::handlers::vms::test_support::record(
        &format!("code-{}", id.simple()),
        r.runtime.vm_id,
    );
    vm.template = "dev".into();
    vm.template_version = "v1".into();
    vm.template_kernel_sha256 = "a".repeat(64);
    vm.template_rootfs_sha256 = "b".repeat(64);
    vm.template_boot_args_sha256 = "c".repeat(64);
    vm.cpu = 2;
    vm.ram = 4096;
    vm.disk_gb = 16;
    vm.micro_network_id = network;
    vm.storage_root = r.runtime.placement.storage_root.clone();
    vm.state = VmState::Running;
    vm.last_runtime_id = r.runtime.runtime_id;
    vm.egress_policy = EgressPolicy::Internet;
    state
        .store
        .allocate_lease(
            vm.id,
            crate::ipam::SubnetSpec {
                micro_network_id: network,
                network: "172.31.0.0".parse().unwrap(),
                prefix: 24,
                ipv6: None,
            },
        )
        .unwrap();
    state.vms.lock().unwrap().insert(vm.id, vm);
    let app = build_router(
        state.clone(),
        &HttpConfig {
            bind_addr: "127.0.0.1:5523".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 32,
            request_timeout: Duration::from_secs(3),
            static_root: None,
        },
    )
    .layer(Extension(LocalOperator));
    Fixture {
        _dir: dir,
        state,
        app,
        id,
        record: *r,
    }
}
