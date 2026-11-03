//! An operator moves only quiesced task records after a verified matched checkpoint.
use super::*;
pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/sandbox-storage",
            get(status),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/sandbox-migrations/{operation}",
            axum::routing::put(migrate),
        )
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    checkpoint_id: Uuid,
}
async fn status(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    authorize(&state, caller, project, SandboxPermission::Read, id.0)?;
    blocking(id.0, move || {
        Ok(state.store.sandbox_storage_status(project)?)
    })
    .await
    .map(Json)
}
async fn migrate(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path((project, operation)): Path<(Uuid, Uuid)>,
    ValidatedJson(input): ValidatedJson<Input>,
) -> Result<Json<serde_json::Value>, AppError> {
    caller.operator().map_err(|_| denied(id.0))?;
    authorize(&state, caller, project, SandboxPermission::Write, id.0)?;
    if operation.is_nil() || input.checkpoint_id.is_nil() {
        return Err(
            Error::Invalid("Use non-empty migration and checkpoint identities").response(id.0),
        );
    }
    let receipt = state
        .store
        .sandbox_storage_status(project)
        .map_err(|_| AppError::internal(id.0))?;
    if receipt["storage"] == "project" {
        if receipt["operationId"] == operation.to_string()
            && receipt["checkpointId"] == input.checkpoint_id.to_string()
        {
            return Ok(Json(receipt));
        }
        return Err(
            Error::Conflict("This project already completed another Sandbox migration")
                .response(id.0),
        );
    }
    let claim = state.sandbox.admission.clone().lock_owned().await;
    let b = binding(&state, project).map_err(|e| e.response(id.0))?;
    let guard = state.lock_vm(b.vm_id).await;
    if state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains_key(&b.vm_id)
    {
        handlers::block_storage::require_inactive_user_vm(&state, b.vm_id, id.0)?;
    }
    blocking(id.0, move || {
        let (_claim, _guard) = (claim, guard);
        let db = open(&state, &b)?;
        if binding(&state, project)? != b {
            return Err(Error::Conflict("Workspace binding changed"));
        }
        let directory = db
            .directory
            .join("checkpoints")
            .join(input.checkpoint_id.to_string());
        let manifest=checkpoint::completed(&directory, &b, input.checkpoint_id, true)?;
        let volume=state.store.block_storage(b.volume_id)?;
        let path=block_storage::verify_disk(&state,&volume).map_err(|_|Error::Conflict("Retained project volume is unavailable"))?;
        if manifest.generation!=b.generation||checkpoint::digest(&path)?!=manifest.volume_sha256{return Err(Error::Conflict("Create a matched checkpoint of the current stopped project volume before migration"));}
        state
            .store
            .migrate_sandbox_tasks(&b, &db, operation, input.checkpoint_id)?;
        Ok(state.store.sandbox_storage_status(project)?)
    })
    .await
    .map(Json)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{bind, call, fixture};
    use super::*;
    use axum::http::StatusCode;
    use serde_json::json;
    #[tokio::test]
    async fn migration_api_requires_operator_current_matched_checkpoint_and_keeps_receipt_offline()
    {
        let (_dir, state, b) = fixture().await;
        bind(&state, &b).await;
        let b = binding(&state, b.project_id).unwrap();
        let db = open(&state, &b).unwrap();
        db.mark_initialized().unwrap();
        drop(db);
        let base = format!("/api/ai-sandbox/projects/{}/workspace", b.project_id);
        let operation = Uuid::new_v4();
        let cp = Uuid::new_v4();
        let path = format!("{base}/sandbox-migrations/{operation}");
        let host = Uuid::new_v4();
        assert_eq!(
            call(
                &state,
                "GET",
                &(base.clone() + "/sandbox-storage"),
                json!({}),
                Some(host)
            )
            .await
            .0,
            StatusCode::FORBIDDEN
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
        assert_eq!(
            call(
                &state,
                "GET",
                &(base.clone() + "/sandbox-storage"),
                json!({}),
                Some(host)
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(
            call(&state, "PUT", &path, json!({"checkpointId":cp}), Some(host))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_ne!(
            call(
                &state,
                "PUT",
                &path,
                json!({"checkpointId":cp,"path":"/tmp/forged"}),
                None
            )
            .await
            .0,
            StatusCode::OK
        );
        let (status, body) = call(
            &state,
            "PUT",
            &format!("{base}/checkpoints/{cp}"),
            json!({}),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, receipt) = call(&state, "PUT", &path, json!({"checkpointId":cp}), None).await;
        assert_eq!(status, StatusCode::OK, "{receipt}");
        assert_eq!(receipt["storage"], "project");
        assert_eq!(
            call(&state, "PUT", &path, json!({"checkpointId":cp}), None)
                .await
                .1,
            receipt
        );
        assert_eq!(
            call(
                &state,
                "PUT",
                &format!("{base}/sandbox-migrations/{}", Uuid::new_v4()),
                json!({"checkpointId":cp}),
                None
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            call(
                &state,
                "GET",
                &(base + "/sandbox-storage"),
                json!({}),
                Some(host)
            )
            .await
            .1,
            receipt
        );
    }
    #[tokio::test]
    async fn prepared_migration_blocks_vm_start_until_the_same_handoff_completes() {
        let (_dir, state, b) = fixture().await;
        bind(&state, &b).await;
        let b = binding(&state, b.project_id).unwrap();
        let db = open(&state, &b).unwrap();
        db.mark_initialized().unwrap();
        drop(db);
        let base = format!("/api/ai-sandbox/projects/{}/workspace", b.project_id);
        let checkpoint = Uuid::new_v4();
        let operation = Uuid::new_v4();
        let (status, body) = call(
            &state,
            "PUT",
            &format!("{base}/checkpoints/{checkpoint}"),
            json!({}),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let db = open(&state, &b).unwrap();
        db.conn.execute_batch("CREATE TRIGGER pause_migration BEFORE INSERT ON sandbox_migration BEGIN SELECT RAISE(ABORT,'pause'); END;").unwrap();
        drop(db);
        let path = format!("{base}/sandbox-migrations/{operation}");
        assert_ne!(
            call(
                &state,
                "PUT",
                &path,
                json!({"checkpointId":checkpoint}),
                None
            )
            .await
            .0,
            StatusCode::OK
        );
        assert!(state.store.sandbox_vm_migration_pending(b.vm_id).unwrap());
        assert!(
            !state
                .store
                .sandbox_vm_migration_pending(Uuid::new_v4())
                .unwrap()
        );
        let previous = state.vms.lock().unwrap().get(&b.vm_id).unwrap().state;
        let result = handlers::vms::start_vm_request(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(b.vm_id.to_string()),
        )
        .await;
        assert_eq!(result.unwrap_err().code(), "sandbox_migration_pending");
        assert_eq!(
            state.vms.lock().unwrap().get(&b.vm_id).unwrap().state,
            previous
        );
        let db = open(&state, &b).unwrap();
        db.conn
            .execute_batch("DROP TRIGGER pause_migration")
            .unwrap();
        drop(db);
        let (status, receipt) = call(
            &state,
            "PUT",
            &path,
            json!({"checkpointId":checkpoint}),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{receipt}");
        assert_eq!(receipt["storage"], "project");
        assert!(!state.store.sandbox_vm_migration_pending(b.vm_id).unwrap());
    }
}
