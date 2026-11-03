//! Inspect and explicitly complete missing legacy boot evidence without starting a VM.

use super::{
    stacks::{database, persistence_error},
    vms,
};
use crate::{
    error::AppError, extract::ValidatedJson, server::RequestId, state::AppState, vm_boot::Review,
};
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use firecrab_stack::NativeOperation;
use uuid::Uuid;

pub async fn inspect(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let id = vms::parse_id(&id, request.0)?;
    let store = state.store.clone();
    let vm = database(request.0, move || store.load_all())
        .await?
        .remove(&id)
        .ok_or_else(|| AppError::not_found(request.0))?;
    let template = state
        .templates
        .resolve_version(&vm.template, &vm.template_version)
        .ok_or_else(|| unavailable(request.0))?;
    Ok(Json(
        serde_json::json!({"vmId":id,"approved":vm.boot_pin,"candidate":template.stack_pin(),"legacyReviewRequired":vm.boot_pin.is_none() && template.initrd.is_some()}),
    ))
}

fn unavailable(id: Uuid) -> AppError {
    AppError::conflict(
        "vm_boot_review_required",
        "inspect and approve the exact original boot inputs of this inactive legacy VM",
        id,
    )
}

pub async fn review(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
    caller: crate::ai_sandbox::access::Caller,
    ValidatedJson(review): ValidatedJson<Review>,
) -> Result<Json<NativeOperation>, AppError> {
    caller.operator().map_err(|_| {
        AppError::new(
            axum::http::StatusCode::FORBIDDEN,
            "vm_boot_review_forbidden",
            "boot-input review requires operator access",
            request.0,
        )
    })?;
    if !review.valid() {
        return Err(unavailable(request.0));
    }
    let id = vms::parse_id(&id, request.0)?;
    let store = state.store.clone();
    let original = review.clone();
    if let Some(result) = database(request.0, move || {
        store.vm_boot_review_receipt(id, &original)
    })
    .await?
    {
        return Ok(Json(result));
    }
    let _guard = state.lock_vm(id).await;
    let store = state.store.clone();
    let vm = database(request.0, move || store.load_all())
        .await?
        .remove(&id)
        .ok_or_else(|| AppError::not_found(request.0))?;
    if !vm.state.can_delete()
        || crate::network_policy::vm_is_active(&state, &vm)
            .map_err(|_| AppError::internal(request.0))?
    {
        return Err(unavailable(request.0));
    }
    let template = state
        .templates
        .resolve_version(&vm.template, &vm.template_version)
        .ok_or_else(|| unavailable(request.0))?;
    if review.pin != template.stack_pin() {
        return Err(unavailable(request.0));
    }
    let mut approved = vm.clone();
    approved.boot_pin = Some(review.pin.clone());
    let templates = state.templates.clone();
    tokio::task::spawn_blocking(move || crate::vm_boot::verify(&approved, &templates, &template))
        .await
        .map_err(|_| AppError::internal(request.0))?
        .map_err(|_| unavailable(request.0))?;
    let result = state
        .store
        .review_vm_boot_pin(&vm, &review)
        .map_err(|error| persistence_error(error, request.0))?;
    if let Some(cached) = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get_mut(&id)
    {
        cached.boot_pin = Some(review.pin);
    }
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        handlers::vms::test_support::record,
        persistence::Store,
        server::{HttpConfig, build_router},
        templates::{TemplateRegistry, TemplateSpec},
    };
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use serde_json::{Value, json};
    use std::{fs, path::Path, time::Duration};
    use tower::ServiceExt;

    async fn fixture(root: &Path) -> (AppState, Router, crate::model::VmRecord, Review) {
        for file in ["kernel", "rootfs", "initrd"] {
            fs::write(root.join(file), file).unwrap();
        }
        let registry = TemplateRegistry::from_specs(
            root,
            [TemplateSpec {
                alias: "ubuntu-rootfs-26.04".into(),
                version: "v1".into(),
                kernel: "kernel".into(),
                rootfs: "rootfs".into(),
                initrd: Some("initrd".into()),
                boot_args: "console=ttyS0".into(),
            }],
        )
        .unwrap();
        let template = registry.resolve_alias("ubuntu-rootfs-26.04").unwrap();
        let state = AppState::with_db_file(registry, root.join("db.sqlite"))
            .await
            .unwrap();
        let mut vm = record("legacy-initrd", Uuid::new_v4());
        vm.template_kernel_sha256 = template.kernel.sha256().into();
        vm.template_rootfs_sha256 = template.rootfs.sha256().into();
        vm.template_boot_args_sha256 = template.boot_args_sha256();
        state.store.insert(&vm).unwrap();
        state.vms.lock().unwrap().insert(vm.id, vm.clone());
        let review = Review {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: state.store.iac_endpoint_id().unwrap(),
            expected_generation: 0,
            pin: template.stack_pin(),
        };
        let router = build_router(
            state.clone(),
            &HttpConfig {
                bind_addr: "127.0.0.1:5523".parse().unwrap(),
                allowed_origins: vec![],
                max_concurrent_requests: 32,
                request_timeout: Duration::from_secs(10),
                static_root: None,
            },
        )
        .layer(Extension(crate::ai_sandbox::access::LocalOperator));
        (state, router, vm, review)
    }

    async fn request(
        router: &Router,
        method: &str,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn iac_vm_boot_review_recovers_original_receipt_after_transfer_file_loss_and_restart() {
        let root = tempfile::tempdir().unwrap();
        let (state, router, vm, review) = fixture(root.path()).await;
        let path = format!("/api/vms/{}/boot-inputs", vm.id);
        let (status, observed) = request(&router, "GET", &path, Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(observed["legacyReviewRequired"], true);
        assert!(observed["approved"].is_null());
        let body = serde_json::to_value(&review).unwrap();
        let (status, original) = request(&router, "PUT", &path, body.clone()).await;
        assert_eq!(status, StatusCode::OK, "{original}");
        assert_eq!(original["state"], "succeeded");
        assert_eq!(
            state.store.load_all().unwrap()[&vm.id].boot_pin,
            Some(review.pin.clone())
        );
        let target = firecrab_stack::ManagedResource {
            kind: "vm".into(),
            id: vm.id.to_string(),
        };
        let owned = state
            .store
            .change_iac_resource_owner(&firecrab_stack::ChangeResourceOwner {
                operation_id: Uuid::new_v4(),
                expected_endpoint_id: review.expected_endpoint_id,
                resource: target.clone(),
                expected_generation: 0,
                expected_owner: None,
                owner: Some(firecrab_stack::ResourceOwner {
                    manager: firecrab_stack::Manager::Terraform,
                    id: Uuid::new_v4(),
                    key: "firecrab_vm.legacy".into(),
                }),
            })
            .unwrap();
        fs::remove_file(root.path().join("initrd")).unwrap();
        let (status, replayed) = request(&router, "PUT", &path, body).await;
        assert_eq!(status, StatusCode::OK, "{replayed}");
        assert_eq!(replayed, original);
        assert_eq!(state.store.iac_resource_owner(&target).unwrap(), owned);
        let reopened = Store::open(&root.path().join("db.sqlite")).unwrap();
        assert_eq!(
            serde_json::to_value(
                reopened
                    .vm_boot_review_receipt(vm.id, &review)
                    .unwrap()
                    .unwrap()
            )
            .unwrap(),
            original
        );
        let (status, receipt) = request(
            &router,
            "GET",
            &format!("/api/iac/native-operations/{}", review.operation_id),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(receipt, original);
        let mut different = review.clone();
        different.pin.initrd_sha256 = None;
        assert!(reopened.vm_boot_review_receipt(vm.id, &different).is_err());
    }

    #[tokio::test]
    async fn iac_vm_boot_review_rejects_wrong_endpoint_stale_owner_and_changed_bytes_without_commit()
     {
        let root = tempfile::tempdir().unwrap();
        let (state, router, vm, review) = fixture(root.path()).await;
        let path = format!("/api/vms/{}/boot-inputs", vm.id);
        let mut wrong = review.clone();
        wrong.expected_endpoint_id = Uuid::new_v4();
        let (status, _) =
            request(&router, "PUT", &path, serde_json::to_value(wrong).unwrap()).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let mut stale = review.clone();
        stale.expected_generation = 1;
        let (status, _) =
            request(&router, "PUT", &path, serde_json::to_value(stale).unwrap()).await;
        assert_eq!(status, StatusCode::CONFLICT);
        fs::write(root.path().join("initrd"), "changed original initrd").unwrap();
        let (status, error) = request(
            &router,
            "PUT",
            &path,
            serde_json::to_value(&review).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert!(
            state
                .store
                .iac_native_operation(review.operation_id)
                .unwrap()
                .is_none()
        );
        assert!(state.store.load_all().unwrap()[&vm.id].boot_pin.is_none());
        assert!(!error.to_string().contains("changed original initrd"));
    }

    #[tokio::test]
    async fn iac_vm_boot_review_rejects_active_generated_and_reserved_vms() {
        for mode in ["active", "generated", "reserved"] {
            let root = tempfile::tempdir().unwrap();
            let (state, router, mut vm, review) = fixture(root.path()).await;
            match mode {
                "active" => {
                    vm.state = firecrab_api_types::VmState::Running;
                    state.store.update(&vm).unwrap();
                }
                "generated" => {
                    vm.purpose = crate::model::VmPurpose::Pool;
                    state.store.update(&vm).unwrap();
                }
                _ => {
                    state
                        .store
                        .begin_native_vm_delete(&vm, Uuid::new_v4(), true)
                        .unwrap();
                }
            }
            let (status, error) = request(
                &router,
                "PUT",
                &format!("/api/vms/{}/boot-inputs", vm.id),
                json!(review),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT, "{mode}: {error}");
            assert!(state.store.load_all().unwrap()[&vm.id].boot_pin.is_none());
            assert!(
                state
                    .store
                    .iac_native_operation(review.operation_id)
                    .unwrap()
                    .is_none()
            );
        }
    }
}
