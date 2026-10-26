use super::super::shutdown;
use super::lifecycle::helper;
use super::*;

fn request() -> SandboxTunnelVerify {
    SandboxTunnelVerify {
        mode: SandboxTunnelVerifyMode::Shutdown,
        path: "/health".into(),
        body_sha256: None,
        web_socket: false,
    }
}

fn inactive(row: &SandboxTunnelBinding) -> Snapshot {
    let mut observed = snapshot(row);
    observed.firecrab_tunnels[0].connected = false;
    observed.quick_tunnels[0].unit.active_state = "inactive".into();
    observed.quick_tunnels[0].unit.sub_state = "dead".into();
    observed.quick_tunnels[0].unit.main_pid = 0;
    observed
}

fn removed(row: &SandboxTunnelBinding) -> Snapshot {
    let mut observed = inactive(row);
    observed.firecrab_tunnels.clear();
    observed.quick_tunnels.clear();
    observed
}

#[tokio::test]
async fn stopped_and_deleted_shutdown_proof_survives_restart_without_remote_retirement() {
    for deleted in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut state = state(dir.path()).await;
        let mut row = binding();
        task(&state, &row);
        row.state = if deleted {
            SandboxTunnelState::Deleted
        } else {
            SandboxTunnelState::Stopped
        };
        row.local_removed = deleted;
        row.remote_retirement = SandboxTunnelRetirement::OperatorRequired;
        state.store.save_sandbox_tunnel(&row).unwrap();
        if deleted {
            state.store.delete_sandbox_project(row.project_id).unwrap();
        }
        let observed = if deleted {
            removed(&row)
        } else {
            inactive(&row)
        };
        let worker = helper(
            &mut state,
            dir.path(),
            vec![
                (Operation::Inspect, Some(observed.clone())),
                (Operation::Inspect, Some(observed)),
            ],
        )
        .await;
        let Json(verified) = shutdown::verify(
            &state,
            RequestId(Uuid::new_v4()),
            Caller::default(),
            row.id,
            &request(),
            |url| async move {
                assert_eq!(url, "https://sandbox-preview.firecrab.dev/health");
                if deleted {
                    shutdown::classify(404, br#"{"error":"unknown_address"}"#)
                } else {
                    shutdown::classify(503, br#"{"error":"target_offline"}"#)
                }
            },
        )
        .await
        .unwrap();
        worker.await.unwrap();
        assert_eq!(
            verified.public_shutdown,
            SandboxTunnelVerification::Verified
        );
        assert_eq!(
            verified.remote_retirement,
            SandboxTunnelRetirement::OperatorRequired
        );
        assert_eq!(
            (verified.task_id, verified.attempt_id, verified.runtime),
            (row.task_id, row.attempt_id, row.runtime)
        );
        let proof = verified.shutdown_proof.as_ref().unwrap();
        assert_eq!(proof.path, "/health");
        assert_eq!(proof.status, Some(if deleted { 404 } else { 503 }));
        assert!(proof.checked_at > 0);
        drop(state);
        let reopened = super::state(dir.path()).await;
        assert_eq!(
            reopened.store.sandbox_tunnel(row.id).unwrap(),
            Some(verified.clone())
        );
        assert_eq!(
            inventory(
                State(reopened.clone()),
                Extension(RequestId(Uuid::new_v4())),
                Caller::default()
            )
            .await
            .unwrap()
            .0,
            vec![verified]
        );
        let delegate: Caller =
            serde_json::from_value(json!({"principal":{"host":Uuid::new_v4()},"operator":false}))
                .unwrap();
        assert_eq!(
            shutdown::verify(
                &reopened,
                RequestId(Uuid::new_v4()),
                delegate,
                row.id,
                &request(),
                |_| async { panic!("delegate cannot probe") }
            )
            .await
            .unwrap_err()
            .code(),
            "sandbox_permission_denied"
        );
    }
}

#[tokio::test]
async fn active_or_uncertain_local_connector_cannot_be_mistaken_for_public_shutdown() {
    for (status, disconnected, removed_flag) in [
        (SandboxTunnelState::Connected, false, false),
        (SandboxTunnelState::Stopping, true, false),
        (SandboxTunnelState::NeedsReconciliation, true, false),
        (SandboxTunnelState::Stopped, false, false),
        (SandboxTunnelState::Deleted, true, false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut state = state(dir.path()).await;
        let mut row = binding();
        task(&state, &row);
        row.state = status;
        row.local_removed = removed_flag;
        state.store.save_sandbox_tunnel(&row).unwrap();
        let observed = if disconnected {
            inactive(&row)
        } else {
            snapshot(&row)
        };
        let worker = helper(
            &mut state,
            dir.path(),
            vec![(Operation::Inspect, Some(observed))],
        )
        .await;
        assert!(
            shutdown::verify(
                &state,
                RequestId(Uuid::new_v4()),
                Caller::default(),
                row.id,
                &request(),
                |_| async { panic!("unconfirmed connector cannot probe") }
            )
            .await
            .is_err()
        );
        worker.await.unwrap();
        assert_eq!(state.store.sandbox_tunnel(row.id).unwrap(), Some(row));
    }
}

#[tokio::test]
async fn reclaimed_address_is_rejected_before_and_after_the_public_observation() {
    for reclaim_before in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut state = state(dir.path()).await;
        let mut old = binding();
        task(&state, &old);
        old.state = SandboxTunnelState::Deleted;
        old.local_removed = true;
        state.store.save_sandbox_tunnel(&old).unwrap();
        let mut next = old.clone();
        next.id = Uuid::new_v4();
        next.state = SandboxTunnelState::Connecting;
        next.local_removed = false;
        if reclaim_before {
            state.store.save_sandbox_tunnel(&next).unwrap();
        }
        let observations = if reclaim_before { 1 } else { 2 };
        let worker = helper(
            &mut state,
            dir.path(),
            vec![(Operation::Inspect, Some(removed(&old))); observations],
        )
        .await;
        let error = shutdown::verify(
            &state,
            RequestId(Uuid::new_v4()),
            Caller::default(),
            old.id,
            &request(),
            |_| async {
                assert!(
                    !reclaim_before,
                    "reclaimed address must fail before the probe"
                );
                state.store.save_sandbox_tunnel(&next).unwrap();
                shutdown::classify(503, br#"{"error":"target_offline"}"#)
            },
        )
        .await
        .unwrap_err();
        worker.await.unwrap();
        assert_eq!(error.code(), "sandbox_preview_shutdown_ownership_changed");
        assert_eq!(
            state.store.sandbox_tunnel(old.id).unwrap(),
            Some(old.clone())
        );
        assert_eq!(
            state
                .store
                .sandbox_tunnel_claim(&old.spec.subdomain)
                .unwrap(),
            Some(next.id)
        );
    }
}

#[tokio::test]
async fn local_connector_restart_after_probe_prevents_verified_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = state(dir.path()).await;
    let mut row = binding();
    task(&state, &row);
    row.state = SandboxTunnelState::Stopped;
    state.store.save_sandbox_tunnel(&row).unwrap();
    let worker = helper(
        &mut state,
        dir.path(),
        vec![
            (Operation::Inspect, Some(inactive(&row))),
            (Operation::Inspect, Some(snapshot(&row))),
        ],
    )
    .await;
    assert!(
        shutdown::verify(
            &state,
            RequestId(Uuid::new_v4()),
            Caller::default(),
            row.id,
            &request(),
            |_| async { shutdown::classify(503, br#"{"error":"target_offline"}"#) }
        )
        .await
        .is_err()
    );
    worker.await.unwrap();
    assert_eq!(state.store.sandbox_tunnel(row.id).unwrap(), Some(row));
}

#[tokio::test]
async fn failed_public_probe_clears_earlier_shutdown_success_without_retaining_response_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = state(dir.path()).await;
    let mut row = binding();
    task(&state, &row);
    row.state = SandboxTunnelState::Stopped;
    row.public_shutdown = SandboxTunnelVerification::Verified;
    state.store.save_sandbox_tunnel(&row).unwrap();
    let worker = helper(
        &mut state,
        dir.path(),
        vec![(Operation::Inspect, Some(inactive(&row))); 4],
    )
    .await;
    for network_failure in [false, true] {
        let Json(observed) = shutdown::verify(
            &state,
            RequestId(Uuid::new_v4()),
            Caller::default(),
            row.id,
            &request(),
            |_| async move {
                if network_failure {
                    shutdown::Observation::default()
                } else {
                    shutdown::classify(503, br#"{"error":"private-token-value"}"#)
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(
            observed.public_shutdown,
            SandboxTunnelVerification::Unverified
        );
        assert_eq!(
            observed.error_code.as_deref(),
            Some("sandbox_preview_shutdown_unverified")
        );
        assert!(
            observed
                .shutdown_proof
                .as_ref()
                .unwrap()
                .error_code
                .is_none()
        );
        assert_eq!(
            observed.shutdown_proof.as_ref().unwrap().status,
            if network_failure { None } else { Some(503) }
        );
        assert!(
            !serde_json::to_string(&observed)
                .unwrap()
                .contains("private-token-value")
        );
    }
    worker.await.unwrap();
}

#[tokio::test]
async fn legacy_application_default_and_shutdown_input_fences_do_not_make_public_requests() {
    let legacy: SandboxTunnelVerify = serde_json::from_value(json!({"path":"/health"})).unwrap();
    assert_eq!(legacy.mode, SandboxTunnelVerifyMode::Application);
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path()).await;
    let mut row = binding();
    task(&state, &row);
    row.state = SandboxTunnelState::Stopped;
    state.store.save_sandbox_tunnel(&row).unwrap();
    for (path, digest, websocket) in [
        ("https://foreign.example", None, false),
        ("//foreign.example", None, false),
        ("/bad#fragment", None, false),
        ("/", Some("0".repeat(64)), false),
        ("/", None, true),
    ] {
        let mut request = request();
        request.path = path.into();
        request.body_sha256 = digest;
        request.web_socket = websocket;
        assert_eq!(
            verify(
                State(state.clone()),
                Path(row.id),
                Extension(RequestId(Uuid::new_v4())),
                Caller::default(),
                ValidatedJson(request)
            )
            .await
            .unwrap_err()
            .code(),
            "sandbox_preview_invalid_verification"
        );
    }
    let mut old = serde_json::to_value(&row).unwrap();
    old.as_object_mut().unwrap().remove("shutdownProof");
    assert!(
        serde_json::from_value::<SandboxTunnelBinding>(old)
            .unwrap()
            .shutdown_proof
            .is_none()
    );
}

#[tokio::test]
async fn shutdown_proof_update_cannot_reopen_or_retarget_a_tombstone_or_replace_a_newer_observation()
 {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path()).await;
    let mut row = binding();
    row.state = SandboxTunnelState::Deleted;
    row.local_removed = true;
    row.remote_retirement = SandboxTunnelRetirement::OperatorRequired;
    state.store.save_sandbox_tunnel(&row).unwrap();
    let mut observed = row.clone();
    observed.updated_at = row.updated_at + 1;
    observed.public_shutdown = SandboxTunnelVerification::Verified;
    observed.shutdown_proof = Some(SandboxTunnelShutdownProof {
        path: "/".into(),
        status: Some(503),
        error_code: Some("target_offline".into()),
        checked_at: 123,
    });
    for changed in ["state", "taskId", "target", "remoteRetirement"] {
        let mut invalid = observed.clone();
        match changed {
            "state" => invalid.state = SandboxTunnelState::Connected,
            "taskId" => invalid.task_id = Uuid::new_v4(),
            "target" => {
                invalid.spec.target = SandboxTunnelTarget::Nginx {
                    host: "foreign.example".into(),
                }
            }
            "remoteRetirement" => invalid.remote_retirement = SandboxTunnelRetirement::NotRequested,
            _ => unreachable!(),
        }
        assert!(
            state
                .store
                .save_sandbox_tunnel_shutdown(&row, &invalid)
                .is_err(),
            "{changed}"
        );
        assert_eq!(
            state.store.sandbox_tunnel(row.id).unwrap(),
            Some(row.clone())
        );
    }
    // The general tombstone lifecycle remains immutable.
    assert!(state.store.save_sandbox_tunnel(&observed).is_err());
    state
        .store
        .save_sandbox_tunnel_shutdown(&row, &observed)
        .unwrap();
    assert!(
        state
            .store
            .save_sandbox_tunnel_shutdown(&row, &observed)
            .is_err()
    );
    assert_eq!(state.store.sandbox_tunnel(row.id).unwrap(), Some(observed));
    assert!(
        state
            .store
            .sandbox_tunnel_claim(&row.spec.subdomain)
            .unwrap()
            .is_none()
    );
}
