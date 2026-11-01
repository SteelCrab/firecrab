use super::*;
use crate::templates::TemplateRegistry;

async fn fixture() -> (tempfile::TempDir, AppState) {
    let temp = tempfile::tempdir().unwrap();
    let templates = TemplateRegistry::from_specs(temp.path(), []).unwrap();
    let state = AppState::with_db_file(templates, temp.path().join("workspaces.sqlite"))
        .await
        .unwrap();
    (temp, state)
}
fn input() -> CreateWorkspaceRequest {
    serde_json::from_value(serde_json::json!({"id":Uuid::new_v4()})).unwrap()
}
async fn create_without_worker(
    state: &AppState,
    input: CreateWorkspaceRequest,
) -> Result<WorkspaceSetup, AppError> {
    // Admission and persistence run normally, but no host image/network actions run in this fixture.
    state
        .workspace_setups
        .workers
        .lock()
        .unwrap()
        .insert(input.id);
    create(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        ValidatedJson(input),
    )
    .await
    .map(|(_, Json(job))| job)
}
#[tokio::test]
async fn retries_and_restart_keep_the_same_reserved_vm_and_network() {
    let (temp, state) = fixture().await;
    let input = input();
    assert_eq!((input.cpu, input.ram, input.disk_gb), (2, 4096, 32));
    let first = create_without_worker(&state, input.clone()).await.unwrap();
    let second = create_without_worker(&state, input.clone()).await.unwrap();
    assert_eq!(first.vm_id, second.vm_id);
    let saved = state.store.workspace_setup(input.id).unwrap().unwrap();
    let reopened = AppState::with_db_file(
        TemplateRegistry::from_specs(temp.path(), []).unwrap(),
        temp.path().join("workspaces.sqlite"),
    )
    .await
    .unwrap();
    let third = create_without_worker(&reopened, input.clone())
        .await
        .unwrap();
    assert_eq!(third.vm_id, first.vm_id);
    assert_eq!(
        reopened
            .store
            .workspace_setup(input.id)
            .unwrap()
            .unwrap()
            .reserved_network,
        saved.reserved_network
    );
    assert!(state.vms.lock().unwrap().is_empty());
    let mut changed = input;
    changed.cpu = 4;
    assert_eq!(
        create_without_worker(&state, changed)
            .await
            .unwrap_err()
            .code(),
        "workspace_request_changed"
    );
}
#[tokio::test]
async fn inspect_is_read_only_and_failed_jobs_require_an_explicit_retry() {
    let (_temp, state) = fixture().await;
    let input = input();
    let first = create_without_worker(&state, input.clone()).await.unwrap();
    let mut saved = read(&state, input.id).await.unwrap();
    saved.progress.state = Status::Failed;
    saved.progress.error = Some("image download failed".into());
    save(&state, &saved).await.unwrap();
    state.workspace_setups.workers.lock().unwrap().clear();
    recover(&state).await;
    let Json(inspected) = get(State(state.clone()), Path(input.id)).await.unwrap();
    assert_eq!(inspected.state, Status::Failed);
    assert!(state.workspace_setups.workers.lock().unwrap().is_empty());
    let retried = create_without_worker(&state, input).await.unwrap();
    assert_eq!(retried.vm_id, first.vm_id);
    assert_eq!(retried.state, Status::Queued);
    assert_eq!(retried.error, None);
}
#[tokio::test]
async fn queue_limits_and_invalid_resources_do_not_create_records() {
    let (_temp, state) = fixture().await;
    let mut invalid = input();
    invalid.ram = 1536;
    invalid.cpu = 0;
    invalid.disk_gb = 1;
    let id = invalid.id;
    let error = create_without_worker(&state, invalid).await.unwrap_err();
    assert_eq!(error.code(), "validation_failed");
    assert_eq!(error.field_names().len(), 3);
    assert!(state.store.workspace_setup(id).unwrap().is_none());
    for _ in 0..8 {
        create_without_worker(&state, input()).await.unwrap();
    }
    assert_eq!(
        create_without_worker(&state, input())
            .await
            .unwrap_err()
            .code(),
        "workspace_queue_full"
    );
    assert_eq!(state.store.workspace_setups().unwrap().len(), 8);
}
#[tokio::test]
async fn persistence_rejects_replacing_a_reserved_vm_identity() {
    let (_temp, state) = fixture().await;
    let input = input();
    create_without_worker(&state, input.clone()).await.unwrap();
    let mut saved = read(&state, input.id).await.unwrap();
    let vm = saved.progress.vm_id;
    saved.progress.vm_id = Uuid::new_v4();
    assert!(state.store.save_workspace_setup(&saved).is_err());
    assert_eq!(read(&state, input.id).await.unwrap().progress.vm_id, vm);
}

#[tokio::test]
async fn restart_resumes_a_queued_job_and_image_failure_cannot_create_a_vm() {
    let (temp, state) = fixture().await;
    let input = input();
    let first = create_without_worker(&state, input.clone()).await.unwrap();
    let mut reopened = AppState::with_db_file(
        TemplateRegistry::from_specs(temp.path(), []).unwrap(),
        temp.path().join("workspaces.sqlite"),
    )
    .await
    .unwrap();
    reopened.image_packages = crate::image_install::ImageInstallTracker::disabled();
    reopened.image_installs = crate::image_install::ImageInstallTracker::disabled();
    recover(&reopened).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let job = read(&reopened, input.id).await.unwrap();
        if job.progress.state == Status::Failed {
            assert_eq!(job.progress.vm_id, first.vm_id);
            assert!(job.progress.error.is_some());
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "recovered worker did not finish"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(reopened.vms.lock().unwrap().is_empty());
    assert!(reopened.store.list_micro_networks().unwrap().is_empty());
}
