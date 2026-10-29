use super::*;
use firecrab_api_types::SandboxEvent;

fn saved(directory: &std::path::Path) -> SavedTask {
    serde_json::from_value(serde_json::json!({
        "task":{"id":Uuid::new_v4(),"projectId":Uuid::new_v4(),"attemptId":Uuid::new_v4(),
        "request":{"kind":"code","input":"test","selection":null,"retryOf":null},
        "selection":{"connectionId":Uuid::new_v4(),"model":"fixture"},"state":"result","createdAt":1,"updatedAt":1,
        "result":"done","errorCode":null,"error":null,"events":[],"changes":[]},
        "project":{"name":"p","sourcePath":null,"vmId":null,"selection":null},
        "workspace":directory,"guest":null,"guest_pending":false
    })).unwrap()
}

#[test]
fn artifact_export_survives_work_cleanup_and_rejects_corruption_or_secrets() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("work")).unwrap();
    fs::write(directory.path().join("work/report.txt"), b"actual output").unwrap();
    fs::write(
        directory.path().join("work/leaked.txt"),
        b"private-provider-key",
    )
    .unwrap();
    let mut task = saved(directory.path());
    assert!(retain(&mut task, "leaked.txt", &["private-provider-key".into()]).is_err());
    assert!(retain(&mut task, "../outside", &[]).is_err());
    let artifact = retain(&mut task, "report.txt", &[]).unwrap();
    assert_eq!(
        retain(&mut task, "report.txt", &[]).unwrap().id,
        artifact.id
    );
    assert_eq!(artifact.task_id, task.task.id);
    assert_eq!(artifact.attempt_id, task.task.attempt_id);
    fs::remove_dir_all(directory.path().join("work")).unwrap();
    assert_eq!(read(&task, artifact.id).unwrap(), b"actual output");
    assert!(verify_export(&task).is_err());
    task.results_exported = true;
    verify_export(&task).unwrap();
    fs::write(
        directory
            .path()
            .join("artifacts")
            .join(artifact.id.to_string()),
        b"tampered",
    )
    .unwrap();
    assert!(verify_export(&task).is_err());
    assert!(read(&task, artifact.id).is_err());
}

#[test]
fn test_records_require_actual_command_exit_status() {
    let directory = tempfile::tempdir().unwrap();
    let mut task = saved(directory.path());
    task.task.events.push(SandboxEvent {
        sequence: 0,
        at: 1,
        name: "run_command".into(),
        input: "false".into(),
        output: String::new(),
        stderr: "reproduction failure".into(),
        exit_code: Some(7),
    });
    record_test(&mut task, "regression", 0).unwrap();
    assert!(!task.task.tests[0].passed);
    record_test(&mut task, "regression", 0).unwrap();
    assert_eq!(task.task.tests.len(), 1);
    assert!(record_test(&mut task, "invented", 9).is_err());
    task.task.events[0].name = "write_file".into();
    assert!(record_test(&mut task, "invented", 0).is_err());
}
