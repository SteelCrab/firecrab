use super::*;
use crate::persistence::PersistenceError;
use firecrab_api_types::MicroServiceResponse;
use firecrab_stack::{ApplyState, ApplyStatus, StepState};

fn service(state: &AppState) -> MicroServiceResponse {
    let record=MicroServiceResponse{id:Uuid::new_v4(),spec:serde_json::from_value(json!({"name":"transactional","image":"ghcr.io/team/app:1","port":8080,"cpu":1,"ram":512})).unwrap()};
    state.store.insert_micro_service(&record).unwrap();
    state.store.register_stack("writer").unwrap();
    state
        .store
        .bind_stack_resource(
            "writer",
            &Binding {
                kind: ResourceKind::Service,
                key: "web".into(),
                resource_id: record.id,
            },
        )
        .unwrap();
    record
}
fn running_step(state: &AppState, id: Uuid) -> ApplyStatus {
    let current = state.store.stack("writer").unwrap().unwrap();
    let status=serde_json::from_value(json!({"operationId":Uuid::new_v4(),"stackId":current.id,"name":"writer","state":"running","changes":[{"kind":"service","key":"web","resourceId":id,"action":"update","fields":["ram"],"reason":null,"state":"running"}],"error":null})).unwrap();
    state
        .store
        .begin_stack_apply(&current, "reviewed", true, &status, None)
        .unwrap();
    status
}

#[tokio::test]
async fn iac_service_commit_rechecks_a_stack_step_accepted_after_native_preflight() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let record = service(&state);
    let authority = state.store.service_policy_authority(&record).unwrap();
    assert!(
        state
            .store
            .stack_service_mutation_allowed(record.id, None)
            .unwrap()
    );
    let status = running_step(&state, record.id);
    let mut change = record.clone();
    change.spec.ram = 1024;
    assert!(matches!(
        state
            .store
            .update_micro_service_checked(&change, &authority, None),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    assert!(matches!(
        state.store.delete_micro_service_checked(record.id, None),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    assert_eq!(
        state.store.micro_service(record.id).unwrap().unwrap(),
        record
    );
    assert!(
        state
            .store
            .update_micro_service_checked(&change, &authority, Some(status.operation_id))
            .unwrap()
    );
    assert_eq!(
        state.store.micro_service(record.id).unwrap().unwrap(),
        change
    );
}

#[tokio::test]
async fn iac_service_old_stack_witness_cannot_become_an_idle_native_writer() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let record = service(&state);
    let authority = state.store.service_policy_authority(&record).unwrap();
    let mut status = running_step(&state, record.id);
    status.state = ApplyState::Succeeded;
    status.changes[0].state = StepState::Succeeded;
    assert!(state.store.save_stack_apply(&status).unwrap());
    let mut newer = record.clone();
    newer.spec.ram = 2048;
    assert!(
        state
            .store
            .update_micro_service_checked(&newer, &authority, None)
            .unwrap()
    );
    assert!(matches!(
        state
            .store
            .update_micro_service_checked(&record, &authority, Some(status.operation_id)),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    assert!(matches!(
        state
            .store
            .delete_micro_service_checked(record.id, Some(status.operation_id)),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    assert_eq!(
        state.store.micro_service(record.id).unwrap().unwrap(),
        newer
    );
}
