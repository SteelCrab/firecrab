use super::*;
use firecrab_stack::{ApplyState, GitSyncMode};

async fn automatic(
    root: &std::path::Path,
    fail: Option<&'static str>,
) -> (AppState, Arc<Mutex<Vec<&'static str>>>) {
    let (state, mut source) = fixture(root).await;
    source.source.sync_mode = GitSyncMode::Automatic;
    source.source.paused = false;
    state
        .store
        .configure_stack_source(
            "demo",
            &ConfigureGitSource {
                expected_generation: source.generation,
                source: source.source,
            },
            0,
        )
        .unwrap();
    let socket = root.join("helper.sock");
    let (_, calls) = crate::network::test_support::spawn_recording_helper(&socket, fail);
    (
        state.with_test_network(crate::network::NetworkClient::with_socket_path(socket)),
        calls,
    )
}

async fn run(state: &AppState, github: &Github, trigger: SourceTrigger) -> GitSourceStatus {
    let generation = state.store.stack_source("demo").unwrap().generation;
    let (source, fresh) = state
        .store
        .begin_stack_check("demo", generation, now() + 86400, trigger)
        .unwrap()
        .unwrap();
    assert!(fresh);
    reconcile(state, &source, source.latest.clone().unwrap(), github).await;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let source = state.store.refresh_stack_sync("demo", now()).unwrap();
            if !source.latest.as_ref().unwrap().state.is_active() {
                return source;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn git_vm_checks_require_an_explicit_pin_and_never_follow_a_repointed_alias() {
    let root = tempfile::tempdir().unwrap();
    let (state, calls) = automatic(root.path(), None).await;
    std::fs::write(root.path().join("kernel"), "kernel").unwrap();
    std::fs::write(root.path().join("rootfs"), "rootfs").unwrap();
    let spec = crate::templates::TemplateSpec {
        alias: "worker-image".into(),
        version: "v1".into(),
        kernel: "kernel".into(),
        rootfs: "rootfs".into(),
        initrd: None,
        boot_args: "console=ttyS0".into(),
    };
    let template = state.templates.register_spec(spec.clone()).unwrap();
    let mut stack: serde_json::Value =
        serde_json::to_value(firecrab_stack::parse(YAML).unwrap()).unwrap();
    stack["microVMs"] = json!({"worker":{"name":"worker","template":"worker-image","network":"app","ram":128,"cpu":1,"diskGb":1}});
    let remote = Arc::new(Mutex::new(Remote {
        first: serde_json::to_vec(&stack).unwrap(),
        ..Default::default()
    }));
    let (github, task) = github(remote.clone()).await;
    let missing = run(&state, &github, SourceTrigger::Poll).await;
    assert_eq!(
        missing.latest.as_ref().unwrap().state,
        GitCheckState::Blocked
    );
    assert!(missing.last_apply.is_none());
    assert!(calls.lock().unwrap().is_empty());
    stack["microVMs"]["worker"]["templatePin"] =
        serde_json::to_value(template.stack_pin()).unwrap();
    remote.lock().unwrap().first = serde_json::to_vec(&stack).unwrap();
    let pinned = run(&state, &github, SourceTrigger::Check).await;
    let first_plan = pinned.latest.as_ref().unwrap().plan.as_ref().unwrap();
    assert!(!first_plan.blocked);
    assert_eq!(first_plan.inputs.templates["worker"], template.stack_pin());
    state
        .templates
        .register_spec(crate::templates::TemplateSpec {
            version: "v2".into(),
            boot_args: "console=ttyS0 quiet".into(),
            ..spec.clone()
        })
        .unwrap();
    let unchanged = run(&state, &github, SourceTrigger::Check).await;
    assert_eq!(
        unchanged.latest.as_ref().unwrap().plan.as_ref().unwrap(),
        first_plan
    );
    state
        .templates
        .register_spec(crate::templates::TemplateSpec {
            boot_args: "changed".into(),
            ..spec
        })
        .unwrap();
    let replaced = run(&state, &github, SourceTrigger::Poll).await;
    assert_eq!(
        replaced.latest.as_ref().unwrap().state,
        GitCheckState::Blocked
    );
    assert!(replaced.last_apply.is_none());
    assert!(calls.lock().unwrap().is_empty());
    assert!(state.store.load_all().unwrap().is_empty());
    task.abort();
}

#[tokio::test]
async fn automatic_sync_creates_repairs_unchanged_commit_updates_and_reverts_without_noop_effects()
{
    let root = tempfile::tempdir().unwrap();
    let (state, calls) = automatic(root.path(), None).await;
    let unrelated: firecrab_api_types::MicroNetworkResponse = serde_json::from_value(json!({"id":uuid::Uuid::new_v4(),"name":"unmanaged","subnetCidr":"172.31.30.0/24","gateway":"172.31.30.1","internetEnabled":false})).unwrap();
    state.store.insert_micro_network(&unrelated).unwrap();
    let remote = Arc::new(Mutex::new(Remote::default()));
    let (github, task) = github(remote.clone()).await;
    let first = run(&state, &github, SourceTrigger::Poll).await;
    assert_eq!(first.latest.as_ref().unwrap().state, GitCheckState::Applied);
    assert_eq!(first.successful_revision.as_ref().unwrap().commit, COMMIT_A);
    assert_eq!(first.attempted_revision, first.successful_revision);
    let operation = first.last_apply.as_ref().unwrap();
    assert_eq!(operation.state, ApplyState::Succeeded);
    let id = operation.changes[0].change.resource_id.unwrap();
    let before = calls.lock().unwrap().len();
    let noop = run(&state, &github, SourceTrigger::Poll).await;
    assert_eq!(noop.latest.as_ref().unwrap().state, GitCheckState::InSync);
    assert!(noop.latest.as_ref().unwrap().operation_id.is_none());
    assert_eq!(noop.last_apply, first.last_apply);
    assert_eq!(calls.lock().unwrap().len(), before);

    state.store.set_micro_network_internet(id, false).unwrap();
    let repaired = run(&state, &github, SourceTrigger::Poll).await;
    assert_eq!(
        repaired.latest.as_ref().unwrap().state,
        GitCheckState::Applied
    );
    assert_eq!(
        repaired.latest.as_ref().unwrap().commit.as_deref(),
        Some(COMMIT_A)
    );
    assert!(
        state
            .store
            .micro_network(id)
            .unwrap()
            .unwrap()
            .internet_enabled
    );
    remote.lock().unwrap().head_b = true;
    let updated = run(&state, &github, SourceTrigger::Poll).await;
    assert_eq!(
        updated.successful_revision.as_ref().unwrap().commit,
        COMMIT_B
    );
    assert!(
        !state
            .store
            .micro_network(id)
            .unwrap()
            .unwrap()
            .internet_enabled
    );
    remote.lock().unwrap().head_b = false;
    let reverted = run(&state, &github, SourceTrigger::Poll).await;
    assert_eq!(
        reverted.successful_revision.as_ref().unwrap().commit,
        COMMIT_A
    );
    assert!(
        state
            .store
            .micro_network(id)
            .unwrap()
            .unwrap()
            .internet_enabled
    );
    assert_eq!(state.store.list_micro_networks().unwrap().len(), 2);
    assert_eq!(
        state.store.micro_network(unrelated.id).unwrap().unwrap(),
        unrelated
    );
    assert_eq!(
        state.store.stack("demo").unwrap().unwrap().bindings.len(),
        1
    );
    assert_eq!(state.store.stack_check_history("demo").unwrap().len(), 5);
    task.abort();
}

#[tokio::test]
async fn checks_stay_read_only_in_automatic_mode_and_manual_sync_works_while_paused() {
    let root = tempfile::tempdir().unwrap();
    let (state, calls) = automatic(root.path(), None).await;
    let mut source = state.store.stack_source("demo").unwrap();
    source.source.paused = true;
    let source = state
        .store
        .configure_stack_source(
            "demo",
            &ConfigureGitSource {
                expected_generation: source.generation,
                source: source.source,
            },
            0,
        )
        .unwrap();
    let (github, task) = github(Arc::new(Mutex::new(Remote::default()))).await;
    assert!(
        state
            .store
            .begin_stack_check("demo", source.generation, now(), SourceTrigger::Poll)
            .unwrap()
            .is_none()
    );
    let checked = run(&state, &github, SourceTrigger::Check).await;
    assert_eq!(checked.latest.unwrap().state, GitCheckState::Drifted);
    assert!(checked.last_apply.is_none());
    assert!(calls.lock().unwrap().is_empty());
    assert!(
        state
            .store
            .stack("demo")
            .unwrap()
            .unwrap()
            .bindings
            .is_empty()
    );
    let synced = run(&state, &github, SourceTrigger::Sync(None)).await;
    assert_eq!(synced.latest.unwrap().state, GitCheckState::Applied);
    assert!(synced.source.paused);
    task.abort();
}

#[tokio::test]
async fn failed_git_reads_never_apply_or_turn_into_empty_stacks() {
    for remote in [
        Remote {
            code: Some(StatusCode::UNAUTHORIZED),
            ..Default::default()
        },
        Remote {
            first: b"private-secret: [".to_vec(),
            ..Default::default()
        },
        Remote {
            mode: "120000",
            ..Default::default()
        },
    ] {
        let root = tempfile::tempdir().unwrap();
        let (state, calls) = automatic(root.path(), None).await;
        let (github, task) = github(Arc::new(Mutex::new(remote))).await;
        let failed = run(&state, &github, SourceTrigger::Poll).await;
        assert_eq!(failed.latest.as_ref().unwrap().state, GitCheckState::Failed);
        assert!(failed.last_apply.is_none());
        assert!(calls.lock().unwrap().is_empty());
        assert!(
            state
                .store
                .stack("demo")
                .unwrap()
                .unwrap()
                .bindings
                .is_empty()
        );
        assert!(
            !serde_json::to_string(&failed)
                .unwrap()
                .contains("private-secret")
        );
        task.abort();
    }
}

#[tokio::test]
async fn automatic_sync_blocks_replacement_and_retains_removed_owned_resources() {
    let kept =
        "  kept:\n    name: kept\n    subnetCidr: 172.31.50.0/24\n    internetEnabled: true\n";
    for remove in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (state, calls) = automatic(root.path(), None).await;
        let mut spec = Remote::default();
        if remove {
            spec.first = (YAML.to_owned() + kept).into_bytes();
        }
        let remote = Arc::new(Mutex::new(spec));
        let (github, task) = github(remote.clone()).await;
        let first = run(&state, &github, SourceTrigger::Poll).await;
        let id = first.last_apply.as_ref().unwrap().changes[0]
            .change
            .resource_id
            .unwrap();
        let before = calls.lock().unwrap().len();
        {
            let mut remote = remote.lock().unwrap();
            remote.head_b = true;
            remote.second = if remove {
                ("apiVersion: firecrab.dev/v1alpha1\nkind: Stack\nname: demo\nmicroNetworks:\n"
                    .to_owned()
                    + kept)
                    .into_bytes()
            } else {
                YAML.replace("172.31.20.0/24", "172.31.40.0/24")
                    .into_bytes()
            };
        }
        let observed = run(&state, &github, SourceTrigger::Poll).await;
        assert_eq!(
            observed.latest.as_ref().unwrap().state,
            if remove {
                GitCheckState::InSync
            } else {
                GitCheckState::Blocked
            }
        );
        assert_eq!(observed.desired_revision.as_ref().unwrap().commit, COMMIT_B);
        assert_eq!(observed.last_apply, first.last_apply);
        assert_eq!(calls.lock().unwrap().len(), before);
        assert!(state.store.micro_network(id).unwrap().is_some());
        assert_eq!(
            state.store.stack("demo").unwrap().unwrap().bindings.len(),
            if remove { 2 } else { 1 }
        );
        task.abort();
    }
}

#[tokio::test]
async fn failed_native_apply_blocks_polling_and_configuration_cannot_bypass_recovery() {
    let root = tempfile::tempdir().unwrap();
    let (state, calls) = automatic(root.path(), Some("ensure_micro_network_bridge")).await;
    let (github, task) = github(Arc::new(Mutex::new(Remote::default()))).await;
    let failed = run(&state, &github, SourceTrigger::Poll).await;
    assert_eq!(
        failed.latest.as_ref().unwrap().state,
        GitCheckState::RecoveryRequired
    );
    assert_eq!(
        failed.last_apply.as_ref().unwrap().state,
        ApplyState::Failed
    );
    let id = failed.recovery_operation_id.unwrap();
    let count = calls.lock().unwrap().len();
    assert!(
        state
            .store
            .begin_stack_check(
                "demo",
                failed.generation,
                now() + 86400,
                SourceTrigger::Poll
            )
            .unwrap()
            .is_none()
    );
    let checked = run(&state, &github, SourceTrigger::Check).await;
    assert_eq!(checked.recovery_operation_id, Some(id));
    let mut config = checked.source;
    config.poll_interval_seconds = 60;
    let configured = state
        .store
        .configure_stack_source(
            "demo",
            &ConfigureGitSource {
                expected_generation: checked.generation,
                source: config,
            },
            0,
        )
        .unwrap();
    assert_eq!(configured.recovery_operation_id, Some(id));
    for recover in [None, Some(uuid::Uuid::new_v4())] {
        assert!(matches!(
            state.store.begin_stack_check(
                "demo",
                configured.generation,
                now(),
                SourceTrigger::Sync(recover)
            ),
            Err(PersistenceError::StackSyncRecoveryRequired)
        ));
    }
    // Explicit recovery replans the reserved UUID. It cannot silently adopt or recreate it.
    let recovered = run(&state, &github, SourceTrigger::Sync(Some(id))).await;
    assert_eq!(recovered.latest.unwrap().state, GitCheckState::Blocked);
    assert_eq!(recovered.recovery_operation_id, Some(id));
    assert_eq!(recovered.last_apply.unwrap().operation_id, id);
    assert_eq!(calls.lock().unwrap().len(), count);
    let binding = state.store.stack("demo").unwrap().unwrap().bindings[0].clone();
    state.store.unbind_stack_resource("demo", &binding).unwrap();
    let socket = root.path().join("recovered-helper.sock");
    crate::network::test_support::spawn_recording_helper(&socket, None);
    let state = state.with_test_network(crate::network::NetworkClient::with_socket_path(socket));
    let recovered = run(&state, &github, SourceTrigger::Sync(Some(id))).await;
    assert_eq!(recovered.latest.unwrap().state, GitCheckState::Applied);
    assert!(recovered.recovery_operation_id.is_none());
    assert_ne!(recovered.last_apply.unwrap().operation_id, id);
    assert_eq!(state.store.list_micro_networks().unwrap().len(), 1);
    task.abort();
}
