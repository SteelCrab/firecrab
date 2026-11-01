use super::*;
use crate::project_workspace::tests::{bind, fixture};

fn task(project: Uuid, vm: Uuid) -> SavedTask {
    let runtime = SandboxRuntimeRef {
        vm_id: vm,
        runtime_id: Uuid::new_v4(),
    };
    serde_json::from_value(json!({
        "task":{"id":Uuid::new_v4(),"projectId":project,"attemptId":Uuid::new_v4(),
            "runtime":runtime,"request":{"kind":"code","input":"Implement the selected change"},
            "selection":{"connectionId":Uuid::new_v4(),"model":"fixture"},"state":"waiting",
            "createdAt":1,"updatedAt":1,"result":"","errorCode":null,"error":null,"events":[],"changes":[]},
        "project":{"name":"p","vmId":vm},"workspace":null,"guest":runtime,
        "guest_pending":false
    })).unwrap()
}
fn snapshot() -> ProjectInstructionSnapshot {
    let files = ["AGENTS.md", "api/AGENTS.md", "api/auth/AGENTS.md"]
        .into_iter()
        .enumerate()
        .map(|(i, path)| {
            let content = format!("Saved rule {i}: scope {path}\n");
            ProjectInstructionFile {
                path: path.into(),
                version: i as u64 + 1,
                sha256: format!("{:x}", Sha256::digest(content.as_bytes())),
                content,
            }
        })
        .collect::<Vec<_>>();
    ProjectInstructionSnapshot {
        id: Uuid::new_v4(),
        directory: "api/auth".into(),
        created_at: 1,
        sha256: format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&("api/auth", &files)).unwrap())
        ),
        files,
    }
}
fn attach(saved: &mut SavedTask) {
    let snapshot = snapshot();
    saved.task.request.instruction_snapshot_id = Some(snapshot.id);
    saved.instructions = Some(SavedInstructions {
        database_id: Uuid::new_v4(),
        volume_id: Uuid::new_v4(),
        generation: 1,
        snapshot,
    });
}
fn account(secret: &str) -> SavedConnection {
    serde_json::from_value(json!({"api_key":secret,"connection":{"id":Uuid::new_v4(),"name":"fixture",
        "provider":"openai","endpoint":"https://example.invalid","models":[],"streaming":false,"toolCalls":true}})).unwrap()
}

#[test]
fn legacy_requests_and_records_retain_their_serialized_shape() {
    let saved = task(Uuid::new_v4(), Uuid::new_v4());
    let value = serde_json::to_value(&saved).unwrap();
    assert!(value.get("instructions").is_none());
    assert!(
        value["task"]["request"]
            .get("instructionSnapshotId")
            .is_none()
    );
    assert!(message(&saved, &[]).unwrap().is_none());
    let mut chosen = saved.clone();
    attach(&mut chosen);
    assert_ne!(saved.task.request, chosen.task.request);
}
#[test]
fn accepted_rules_keep_order_and_bytes_across_task_replay_without_public_content() {
    let mut saved = task(Uuid::new_v4(), Uuid::new_v4());
    attach(&mut saved);
    let original = message(&saved, &[]).unwrap().unwrap();
    let reloaded: SavedTask = serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
    assert_eq!(message(&reloaded, &[]).unwrap().unwrap(), original);
    let content = original["content"].as_str().unwrap();
    let context: Value = serde_json::from_str(content.split_once('\n').unwrap().1).unwrap();
    assert_eq!(context["directory"], "api/auth");
    assert_eq!(context["files"][0]["path"], "AGENTS.md");
    assert_eq!(context["files"][2]["path"], "api/auth/AGENTS.md");
    assert!(
        context["files"][2]["content"]
            .as_str()
            .unwrap()
            .contains("Saved rule 2")
    );
    let audit: Value = serde_json::from_str(&audit(&saved).unwrap()).unwrap();
    assert!(
        audit["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|file| file.get("content").is_none())
    );
    assert!(
        !serde_json::to_string(&saved.task)
            .unwrap()
            .contains("Saved rule 0")
    );
}
#[test]
fn missing_replaced_corrupt_or_out_of_scope_rules_fail_instead_of_falling_back() {
    let mut saved = task(Uuid::new_v4(), Uuid::new_v4());
    attach(&mut saved);
    let mut altered = saved.clone();
    altered.instructions = None;
    assert!(message(&altered, &[]).is_err());
    let mut altered = saved.clone();
    altered.task.request.instruction_snapshot_id = None;
    assert!(message(&altered, &[]).is_err());
    let mut altered = saved.clone();
    altered.task.request.instruction_snapshot_id = Some(Uuid::new_v4());
    assert!(message(&altered, &[]).is_err());
    let mut altered = saved.clone();
    altered.instructions.as_mut().unwrap().snapshot.files[1]
        .content
        .push_str(" changed");
    assert!(message(&altered, &[]).is_err());
    let mut altered = saved.clone();
    altered
        .instructions
        .as_mut()
        .unwrap()
        .snapshot
        .files
        .reverse();
    assert!(message(&altered, &[]).is_err());
    let mut altered = saved.clone();
    altered.instructions.as_mut().unwrap().snapshot.directory = "../api".into();
    assert!(message(&altered, &[]).is_err());
    let mut altered = saved.clone();
    altered.task.request.kind = SandboxTaskKind::Chat;
    assert!(message(&altered, &[]).is_err());
}
#[test]
fn raw_credential_checks_and_refreshed_context_redaction_handle_json_escaping() {
    let secret = "fake-quoted-\"credential\\value";
    let mut saved = task(Uuid::new_v4(), Uuid::new_v4());
    let mut context = snapshot();
    context.files[0].content = secret.into();
    context.files[0].sha256 = format!("{:x}", Sha256::digest(secret.as_bytes()));
    context.sha256 = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(&context.directory, &context.files)).unwrap())
    );
    saved.task.request.instruction_snapshot_id = Some(context.id);
    saved.instructions = Some(SavedInstructions {
        database_id: Uuid::new_v4(),
        volume_id: Uuid::new_v4(),
        generation: 1,
        snapshot: context,
    });
    assert!(
        !saved
            .instructions
            .as_ref()
            .unwrap()
            .contains_credentials(&[])
    );
    assert!(
        saved
            .instructions
            .as_ref()
            .unwrap()
            .contains_credentials(&[account(secret)])
    );
    let message = message(&saved, &[account(secret)]).unwrap().unwrap();
    let document: Value = serde_json::from_str(
        message["content"]
            .as_str()
            .unwrap()
            .split_once('\n')
            .unwrap()
            .1,
    )
    .unwrap();
    assert_eq!(document["files"][0]["content"], "[REDACTED]");
    assert_eq!(
        saved.instructions.as_ref().unwrap().snapshot.files[0].content,
        secret
    );
}
#[tokio::test]
async fn binding_changes_fence_context_before_another_effect() {
    let (_dir, state, b) = fixture().await;
    bind(&state, &b).await;
    let binding = state
        .store
        .project_workspace(b.project_id)
        .unwrap()
        .unwrap();
    let mut saved = task(b.project_id, b.vm_id);
    attach(&mut saved);
    let rules = saved.instructions.as_mut().unwrap();
    rules.database_id = binding.database_id;
    rules.volume_id = binding.volume_id;
    rules.generation = binding.generation;
    verify(&state, &saved).unwrap();
    for field in ["database", "volume", "generation", "runtime"] {
        let mut altered = saved.clone();
        let context = altered.instructions.as_mut().unwrap();
        match field {
            "database" => context.database_id = Uuid::new_v4(),
            "volume" => context.volume_id = Uuid::new_v4(),
            "generation" => context.generation += 1,
            _ => altered.task.runtime.as_mut().unwrap().vm_id = Uuid::new_v4(),
        }
        assert!(verify(&state, &altered).is_err(), "{field}");
    }
}
