use super::*;
use firecrab_stack::{
    ChangeResourceOwner, ManagedResource, Manager, NativeAction, NativeOperation,
    NativeOperationState, ResourceOwner,
};

fn record() -> ScheduleResponse {
    ScheduleResponse {
        id: Uuid::new_v4(),
        spec: serde_json::from_value(serde_json::json!({"name":"reviewed schedule","vmId":Uuid::new_v4(),
            "action":"start","timing":{"kind":"cron","expression":"0 * * * *","timeZone":"UTC"},"enabled":false})).unwrap(),
        next_run_at_ms:None,created_at_ms:100,updated_at_ms:100,last_run:None,
    }
}

#[test]
fn iac_schedule_late_public_writes_cannot_cross_another_process_adoption() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let original = record();
    store.insert_schedule(&original).unwrap();
    let target = ManagedResource {
        kind: "schedule".into(),
        id: original.id.to_string(),
    };
    assert!(store.iac_public_write_allowed(&target).unwrap());
    let second = Store::open(&path).unwrap();
    let before = second.iac_resource_owner(&target).unwrap();
    let owned = second
        .change_iac_resource_owner(&ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: before.endpoint_id,
            resource: target,
            expected_generation: before.generation,
            expected_owner: before.owner,
            owner: Some(ResourceOwner {
                manager: Manager::Terraform,
                id: Uuid::new_v4(),
                key: "firecrab_schedule.hourly".into(),
            }),
        })
        .unwrap();
    let mut changed = original.clone();
    changed.spec.name = "late unreviewed edit".into();
    assert!(matches!(
        store.insert_schedule(&changed),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    for (replacement, enabled) in [
        (Some(&changed.spec), None),
        (None, Some(false)),
        (None, Some(true)),
        (None, None),
    ] {
        assert!(matches!(
            store.mutate_schedule(original.id, replacement, enabled, 200),
            Err(PersistenceError::IacOwnershipConflict)
        ));
        assert_eq!(store.schedule(original.id).unwrap(), Some(original.clone()));
    }
    second
        .change_iac_resource_owner(&ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: owned.endpoint_id,
            resource: owned.resource,
            expected_generation: owned.generation,
            expected_owner: owned.owner,
            owner: None,
        })
        .unwrap();
    assert_eq!(
        store
            .mutate_schedule(original.id, Some(&changed.spec), None, 201)
            .unwrap(),
        ScheduleMutation::Done
    );
    assert_eq!(
        store.schedule(original.id).unwrap().unwrap().spec,
        changed.spec
    );
}

#[test]
fn iac_schedule_public_writes_keep_uncertain_native_work_reserved_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let original = record();
    store.insert_schedule(&original).unwrap();
    let ownership = store
        .iac_resource_owner(&ManagedResource {
            kind: "schedule".into(),
            id: original.id.to_string(),
        })
        .unwrap();
    let operation = NativeOperation {
        operation_id: Uuid::new_v4(),
        ownership,
        action: NativeAction::ApplyResourcePlan,
        state: NativeOperationState::Running,
        error_code: None,
        execution_generation: 0,
        configuration_generation: None,
        retain_disks: None,
    };
    super::super::iac_native::save(&store.lock(), &operation).unwrap();
    let reopened = Store::open(&path).unwrap();
    reopened.interrupt_native_iac_operations().unwrap();
    assert_eq!(
        reopened
            .iac_native_operation(operation.operation_id)
            .unwrap()
            .unwrap()
            .state,
        NativeOperationState::NeedsRecovery
    );
    assert!(matches!(
        reopened.insert_schedule(&original),
        Err(PersistenceError::IacResourceBusy)
    ));
    for enabled in [None, Some(false), Some(true)] {
        assert!(matches!(
            reopened.mutate_schedule(original.id, None, enabled, 200),
            Err(PersistenceError::IacResourceBusy)
        ));
    }
    assert_eq!(reopened.schedule(original.id).unwrap(), Some(original));
}

#[cfg(target_os = "macos")]
#[test]
fn iac_schedule_lock_uses_one_private_regular_file_for_database_aliases() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let alias = root.path().join("alias.db");
    symlink(&path, &alias).unwrap();
    let second = Store::open(&alias).unwrap();
    let lock = store.scheduler_lock().unwrap();
    let lock_path = root.path().join("state.db.scheduler.lock");
    let metadata = lock_path.metadata().unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert!(second.scheduler_lock().is_err());
    drop(lock);
    drop(second.scheduler_lock().unwrap());
    std::fs::remove_file(&lock_path).unwrap();
    symlink(&path, &lock_path).unwrap();
    assert!(store.scheduler_lock().is_err());
    // A rejected lock symlink cannot truncate or change the database.
    assert!(store.list_schedules().unwrap().is_empty());
}
