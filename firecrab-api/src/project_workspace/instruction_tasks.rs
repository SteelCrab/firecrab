//! Read saved rules for task admission without reopening any private editor buffer.
use super::*;
use crate::ai_sandbox::SavedInstructions;

/// Reuse saved, caller-private context only after task storage has moved to this project.
pub(crate) async fn task_instructions(
    state: AppState,
    caller: Caller,
    project: Uuid,
    vm: Uuid,
    snapshot: Uuid,
    request: Uuid,
) -> Result<SavedInstructions, AppError> {
    authorize(&state, caller, project, SandboxPermission::Read, request)?;
    blocking(request, move || {
        if snapshot.is_nil() {
            return Err(Error::Invalid("Use a saved instruction snapshot UUID"));
        }
        let binding = binding(&state, project)?;
        if binding.vm_id != vm || binding.provisioning {
            return Err(Error::Conflict(
                "Saved rules require the current project runtime binding",
            ));
        }
        if state.store.sandbox_storage_status(project)?["storage"] != "project" {
            return Err(Error::Conflict(
                "Complete the Sandbox project database migration before admitting saved rules",
            ));
        }
        let db = open(&state, &binding)?;
        let snapshot = db.instruction_snapshot(caller, snapshot)?;
        caller
            .project(&state, project, SandboxPermission::Read)
            .map_err(|_| Error::Denied)?;
        Ok(SavedInstructions {
            database_id: binding.database_id,
            volume_id: binding.volume_id,
            generation: binding.generation,
            snapshot,
        })
    })
    .await
}

/// Verify the same root-to-leaf saved context during replay and before model dispatch.
pub(crate) fn validate_task_instructions(
    snapshot: &ProjectInstructionSnapshot,
) -> Result<(), String> {
    instructions::validate(snapshot).map_err(|_| "Saved instruction provenance is invalid".into())
}

#[cfg(test)]
mod tests {
    use super::super::tests::{bind, fixture};
    use super::*;
    use axum::{http::StatusCode, response::IntoResponse};

    #[tokio::test]
    async fn task_context_requires_canonical_storage_and_caller_private_saved_versions() {
        let (_directory, state, seed) = fixture().await;
        bind(&state, &seed).await;
        let b = binding(&state, seed.project_id).unwrap();
        let db = open(&state, &b).unwrap();
        db.mark_initialized().unwrap();
        let version = db
            .observe_file("AGENTS.md", b"Retained saved rule\n")
            .unwrap();
        let files = vec![ProjectInstructionFile {
            path: "AGENTS.md".into(),
            version: version.metadata.id,
            sha256: version.metadata.sha256,
            content: "Retained saved rule\n".into(),
        }];
        let snapshot = ProjectInstructionSnapshot {
            id: Uuid::new_v4(),
            directory: "".into(),
            created_at: 1,
            sha256: instructions::digest("", &files).unwrap(),
            files,
        };
        db.save_instruction_snapshot(Caller::default(), &snapshot)
            .unwrap();
        drop(db);
        let load = || {
            task_instructions(
                state.clone(),
                Caller::default(),
                b.project_id,
                b.vm_id,
                snapshot.id,
                Uuid::new_v4(),
            )
        };
        assert_eq!(
            load().await.err().unwrap().into_response().status(),
            StatusCode::CONFLICT
        );
        let db = open(&state, &b).unwrap();
        state
            .store
            .migrate_sandbox_tasks(&b, &db, Uuid::new_v4(), Uuid::new_v4())
            .unwrap();
        db.observe_file("AGENTS.md", b"Later saved rule\n").unwrap();
        drop(db);
        let accepted = load().await.unwrap();
        assert_eq!(accepted.snapshot, snapshot);
        assert_eq!(accepted.database_id, b.database_id);
        assert_eq!(accepted.volume_id, b.volume_id);
        assert_eq!(accepted.generation, b.generation);
        assert_eq!(
            task_instructions(
                state.clone(),
                Caller::default(),
                b.project_id,
                Uuid::new_v4(),
                snapshot.id,
                Uuid::new_v4()
            )
            .await
            .err()
            .unwrap()
            .into_response()
            .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            task_instructions(
                state.clone(),
                Caller::default(),
                b.project_id,
                b.vm_id,
                Uuid::nil(),
                Uuid::new_v4()
            )
            .await
            .err()
            .unwrap()
            .into_response()
            .status(),
            StatusCode::BAD_REQUEST
        );
        let host = Uuid::new_v4();
        let caller: Caller = serde_json::from_value(
            serde_json::json!({"principal": SandboxPrincipal::Host(host), "operator":false}),
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
        assert_eq!(
            task_instructions(
                state.clone(),
                caller,
                b.project_id,
                b.vm_id,
                snapshot.id,
                Uuid::new_v4()
            )
            .await
            .err()
            .unwrap()
            .into_response()
            .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            task_instructions(
                state.clone(),
                caller,
                b.project_id,
                b.vm_id,
                Uuid::new_v4(),
                Uuid::new_v4()
            )
            .await
            .err()
            .unwrap()
            .into_response()
            .status(),
            StatusCode::NOT_FOUND
        );
        let database = state
            .workspaces
            .root
            .join(b.project_id.to_string())
            .join("project.db");
        std::fs::rename(&database, database.with_extension("kept")).unwrap();
        assert!(load().await.is_err());
        assert!(!database.exists());
    }
    #[tokio::test]
    async fn idempotent_admission_keeps_private_rules_without_recapture_or_dispatch() {
        use crate::{
            ai_sandbox::{ModelProof, fingerprint},
            extract::ValidatedJson,
            server::RequestId,
        };
        use axum::{
            Extension, Json,
            extract::{Path, State},
        };
        use serde_json::json;
        let (directory, state, seed) = fixture().await;
        bind(&state, &seed).await;
        let b = binding(&state, seed.project_id).unwrap();
        let source = directory.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let account: crate::pipe_ai::SavedConnection = serde_json::from_value(json!({
            "api_key":"fake-instruction-admission-secret","connection":{"id":Uuid::new_v4(),
            "name":"fixture","provider":"openai","endpoint":"https://example.invalid",
            "models":["fixture"],"streaming":false,"toolCalls":true}
        }))
        .unwrap();
        state.store.save_ai_connection(&account).unwrap();
        let selection = AiModelSelection {
            connection_id: account.connection.id,
            model: "fixture".into(),
        };
        state
            .store
            .save_sandbox_model(&ModelProof {
                selection: selection.clone(),
                fingerprint: fingerprint(&account),
                verified: true,
                tools_verified: true,
            })
            .unwrap();
        let mut project = state.store.sandbox_project(b.project_id).unwrap().unwrap();
        project.spec.source_path = Some(source.to_string_lossy().into());
        project.spec.selection = Some(selection);
        state.store.save_sandbox_project(&project).unwrap();
        let db = open(&state, &b).unwrap();
        db.mark_initialized().unwrap();
        let version = db
            .observe_file("AGENTS.md", b"Admitted original rule")
            .unwrap();
        let files = vec![ProjectInstructionFile {
            path: "AGENTS.md".into(),
            version: version.metadata.id,
            sha256: version.metadata.sha256,
            content: "Admitted original rule".into(),
        }];
        let snapshot = ProjectInstructionSnapshot {
            id: Uuid::new_v4(),
            directory: "".into(),
            created_at: 1,
            sha256: instructions::digest("", &files).unwrap(),
            files,
        };
        db.save_instruction_snapshot(Caller::default(), &snapshot)
            .unwrap();
        state
            .store
            .migrate_sandbox_tasks(&b, &db, Uuid::new_v4(), Uuid::new_v4())
            .unwrap();
        drop(db);
        {
            let mut vms = state.vms.lock().unwrap();
            let vm = vms.get_mut(&b.vm_id).unwrap();
            vm.state = firecrab_api_types::VmState::Running;
            vm.last_runtime_id = Some(Uuid::new_v4());
        }
        assert!(state.store.active_lease(b.vm_id).unwrap().is_some());
        // Keep execution blocked until cancellation is durable. This test never contacts a VM or model.
        let permits = state.sandbox.permits.acquire_many(2).await.unwrap();
        let key = Uuid::new_v4();
        let input: SandboxTaskInput =
            serde_json::from_value(json!({"kind":"code","input":"Update tests",
            "instructionSnapshotId":snapshot.id}))
            .unwrap();
        let submit = |input| {
            crate::handlers::ai_sandbox::create_task(
                State(state.clone()),
                Extension(RequestId(Uuid::new_v4())),
                Caller::default(),
                Path((b.project_id, key)),
                ValidatedJson(input),
            )
        };
        let (status, Json(first)) = submit(input.clone()).await.unwrap();
        assert_eq!(status, StatusCode::ACCEPTED);
        let retained = state.store.sandbox_task(key).unwrap().unwrap();
        assert_eq!(retained.instructions.as_ref().unwrap().snapshot, snapshot);
        let db = open(&state, &b).unwrap();
        db.observe_file("AGENTS.md", b"Later source rule").unwrap();
        // The task's canonical saved copy remains sufficient even if the selector row is unavailable.
        db.conn
            .execute(
                "DELETE FROM instruction_snapshots WHERE id=?1",
                [snapshot.id.to_string()],
            )
            .unwrap();
        drop(db);
        let (status, Json(replayed)) = submit(input.clone()).await.unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(first.attempt_id, replayed.attempt_id);
        let mut changed = input.clone();
        changed.instruction_snapshot_id = Some(Uuid::new_v4());
        assert_eq!(
            submit(changed).await.err().unwrap().code(),
            "sandbox_task_conflict"
        );
        let signal = state
            .sandbox
            .cancellations
            .lock()
            .unwrap()
            .get(&key)
            .unwrap()
            .clone();
        signal.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if state.store.sandbox_task(key).unwrap().unwrap().task.state
                    == SandboxTaskState::Cancelled
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let retained = state.store.sandbox_task(key).unwrap().unwrap();
        assert!(retained.workspace.is_none());
        assert!(!retained.guest_staged);
        assert_eq!(retained.instructions.unwrap().snapshot, snapshot);
        drop(permits);
    }
}
