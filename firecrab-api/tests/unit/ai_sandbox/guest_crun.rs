use super::*;

fn mode(image: String) -> SandboxExecutionMode {
    SandboxExecutionMode::AgentSandbox {
        runtime: SandboxContainerRuntime::Crun,
        image,
    }
}

#[tokio::test]
async fn crun_image_preflight_rejects_mutable_images_and_restarted_guests() {
    let (directory, state, saved) = crate::ai_sandbox::recovery_tests::fixture().await;
    let bound = saved.guest.unwrap();
    let mut record = crate::handlers::vms::test_support::record("crun-preflight", bound.vm_id);
    record.state = VmState::Running;
    record.last_runtime_id = Some(Uuid::new_v4());
    crate::handlers::vms::test_support::seed_vm(&state, &record);

    for image in ["ubuntu:latest".into(), "image@sha256:short".into()] {
        let error = mode_supported(&state, &bound, directory.path(), &mode(image))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("digest"),
            "Mutable images must fail before opening the guest transport: {error}"
        );
    }

    let pinned = mode(format!("example.invalid/image@sha256:{}", "a".repeat(64)));
    let error = mode_supported(&state, &bound, directory.path(), &pinned)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("MicroVM restarted"), "{error}");
    assert!(!directory.path().join("known_hosts").exists());
}

// Called by the existing SSH contract inside its disposable root container.
pub(super) async fn unavailable_engine_keeps_the_attempt_unmodified(
    state: &AppState,
    saved: &mut SavedTask,
) {
    let bound = saved.guest.unwrap();
    let attempt = saved.workspace.as_ref().unwrap().clone();
    let capabilities = capabilities(state, &bound, &attempt).await.unwrap();
    assert!(capabilities.execution);
    assert!(!capabilities.crun);

    let pinned = mode(format!("example.invalid/image@sha256:{}", "a".repeat(64)));
    assert!(
        !mode_supported(state, &bound, &attempt, &pinned)
            .await
            .unwrap()
    );

    let previous_mode = saved.execution_mode.replace(pinned);
    let previous_command = saved.guest_command;
    let previous_staging = (saved.guest_pending, saved.guest_staged);
    let error = execute(state, saved, "touch crun-must-not-run", 3)
        .await
        .err()
        .expect("An unavailable crun engine must reject execution");
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert!(error.to_string().contains("container engine"), "{error}");
    assert_eq!(saved.guest_command, previous_command);
    assert_eq!((saved.guest_pending, saved.guest_staged), previous_staging);
    assert!(!attempt.join("work/crun-must-not-run").exists());
    saved.execution_mode = previous_mode;
}
