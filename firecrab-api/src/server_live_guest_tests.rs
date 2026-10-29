//! Explicit opt-in operator-transport check against a dedicated real user MicroVM.
//! This test uses no provider credential or real model; model protocol has separate tests.
use crate::{
    ai_sandbox::{SavedTask, files, guest, workspace},
    state::AppState,
};
use firecrab_api_types::*;
use serde::Deserialize;
use std::{collections::BTreeMap, path::PathBuf, time::Duration};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Target {
    vm_id: Uuid,
    runtime_id: Uuid,
    ipv4: std::net::Ipv4Addr,
    vms_dir: PathBuf,
}

#[tokio::test]
#[ignore = "requires FIRECRAB_LIVE_GUEST_TARGET: an explicitly selected disposable user VM fixture"]
async fn real_guest_operator_transport_runs_exports_cancels_and_cleans_owned_data() {
    let target: Target = serde_json::from_slice(
        &std::fs::read(
            std::env::var_os("FIRECRAB_LIVE_GUEST_TARGET")
                .expect("Supply a private non-credential target JSON file"),
        )
        .unwrap(),
    )
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let db = directory.path().join("state.db");
    let templates =
        crate::templates::TemplateRegistry::from_specs(directory.path(), std::iter::empty())
            .unwrap();
    let mut state = AppState::with_db_file(templates, db.clone()).await.unwrap();
    let mut runtime = (*state.runtime).clone();
    runtime.vms_dir = target.vms_dir;
    state = state.with_test_runtime(runtime);
    let mut vm = crate::handlers::vms::test_support::record("live-guest-qa", target.vm_id);
    vm.storage_root = "explicit-live-guest-fixture".into();
    vm.state = VmState::Running;
    vm.last_runtime_id = Some(target.runtime_id);
    crate::handlers::vms::test_support::seed_vm(&state, &vm);
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute(
            "UPDATE network_leases SET ipv4=?1 WHERE vm_id=?2 AND released_at IS NULL",
            rusqlite::params![target.ipv4.to_string(), target.vm_id.to_string()],
        )
        .unwrap();
    let source = directory.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("main.txt"), "original").unwrap();
    let attempt = directory.path().join("attempt");
    std::fs::create_dir(&attempt).unwrap();
    for name in ["base", "work"] {
        std::fs::create_dir(attempt.join(name)).unwrap();
        files::snapshot(&source, &attempt.join(name)).unwrap();
    }
    std::fs::write(
        attempt.join("base-modes.json"),
        serde_json::to_vec(&BTreeMap::from([("main.txt", 0o644)])).unwrap(),
    )
    .unwrap();
    let id = Uuid::new_v4();
    let selection = AiModelSelection {
        connection_id: Uuid::nil(),
        model: "no-provider-used".into(),
    };
    let mut saved = SavedTask {
        caller: Default::default(),
        task: SandboxTask {
            git_commit: None,
            retry_root: None,
            retry_number: 0,
            dependency_results: vec![],
            tests: vec![],
            artifacts: vec![],
            environment: None,
            id,
            project_id: Uuid::new_v4(),
            attempt_id: Uuid::new_v4(),
            runtime: Some(guest::binding(&state, target.vm_id).unwrap()),
            request: SandboxTaskInput {
                coordination: Default::default(),
                environment_role: None,
                kind: SandboxTaskKind::Code,
                input: "Explicit guest transport QA".into(),
                selection: None,
                retry_of: None,
            },
            selection,
            state: SandboxTaskState::InProgress,
            created_at: 0,
            updated_at: 0,
            result: String::new(),
            error_code: None,
            error: None,
            events: vec![],
            changes: vec![],
        },
        project: SandboxProjectSpec {
            context: Default::default(),
            environment_id: None,
            name: "Live guest QA".into(),
            source_path: Some(source.to_str().unwrap().into()),
            vm_id: Some(target.vm_id),
            selection: None,
            instructions: String::new(),
        },
        workspace: Some(attempt.clone()),
        guest: Some(guest::binding(&state, target.vm_id).unwrap()),
        guest_pending: false,
        guest_command: None,
        guest_staged: false,
        results_exported: false,
        execution_mode: None,
    };
    state
        .store
        .save_sandbox_project(&SandboxProject {
            id: saved.task.project_id,
            owner: SandboxPrincipal::Local,
            spec: saved.project.clone(),
            created_at: 0,
        })
        .unwrap();
    eprintln!(
        "Real guest QA vm={} runtime={} attempt={}",
        target.vm_id, target.runtime_id, saved.task.attempt_id
    );
    let result = guest::execute(&state, &mut saved,
        "test \"$(id -u)\" -ne 0 && test \"$(awk '/^NoNewPrivs:/ {print $2}' /proc/self/status)\" = 1 && test -z \"${OPENAI_API_KEY-}\" && echo changed >main.txt && echo guest-stdout && echo guest-stderr >&2; exit 7", 10).await.unwrap();
    assert_eq!(result.code, 7);
    assert_eq!(result.stdout, b"guest-stdout\n");
    assert_eq!(result.stderr, b"guest-stderr\n");
    assert_eq!(
        std::fs::read_to_string(source.join("main.txt")).unwrap(),
        "original"
    );
    let mut changes = workspace::changes(&attempt).unwrap();
    workspace::apply(&source, &mut changes, &["main.txt".into()]).unwrap();
    assert_eq!(
        std::fs::read_to_string(source.join("main.txt")).unwrap(),
        "changed\n"
    );
    let result = guest::execute(&state, &mut saved, "exit 255", 5)
        .await
        .unwrap();
    assert_eq!(result.code, 255);
    let previous_invocation = saved.guest_command;
    let worker_state = state.clone();
    let worker = tokio::spawn(async move {
        let result = guest::execute(&worker_state, &mut saved, "sleep 30", 40).await;
        (saved, result)
    });
    let mut running = None;
    for _ in 0..200 {
        if let Some(record) = state.store.sandbox_task(id).unwrap()
            && record.guest_command.is_some()
            && record.guest_command != previous_invocation
        {
            running = Some(record);
        }
        if running.as_ref().is_some_and(|v| v.guest_command.is_some()) {
            tokio::time::sleep(Duration::from_secs(2)).await;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    guest::cancel(&state, &running.expect("Guest invocation was checkpointed"))
        .await
        .unwrap();
    let (saved, outcome) = worker.await.unwrap();
    assert!(!outcome.is_ok_and(|v| v.code == 0));
    guest::cancel(&state, &saved).await.unwrap();
    guest::export(&state, &saved).await.unwrap();
    guest::cleanup(&state, &saved).await.unwrap();
    eprintln!(
        "Real guest operator SSH, unprivileged execution, exit 7/255, export, source apply, cancellation and owned cleanup passed. No provider was used."
    );
}
