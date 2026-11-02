//! Behavior checks for retained skill admission, isolation and immutable task resources.
use super::super::tests::{bind, fixture};
use super::*;
use crate::ai_sandbox::{SavedInstructions, SavedTask};
use axum::response::IntoResponse;
/// A small package is source data, never an instruction to execute this test host.
fn package() -> (ProjectSkillEntry, Vec<source::Blob>) {
    (ProjectSkillEntry{source:ProjectSkillSource::Openai,kind:ProjectSkillKind::Skill,path:"plugins/example/skills/review".into(),name:"review".into(),revision:"a".repeat(40),instructions:vec!["SKILL.md".into()]},vec![source::Blob{path:"SKILL.md".into(),base64:STANDARD.encode("---\nname: review\ndescription: Review project files\n---\nFollow the saved task and inspect evidence.\n")},source::Blob{path:"references/context.md".into(),base64:STANDARD.encode("Original reference\n")}])
}
/// Complete synthetic versions exercise the real project DB without claiming a workload VM run.
fn complete(db: &super::super::db::ProjectDb, mut record: db::Record) -> db::Record {
    for i in 0..record.public.files.len() {
        let blob = &record.blobs[i];
        let bytes = STANDARD.decode(&blob.base64).unwrap();
        let v = db
            .observe_file(&format!("{}/{}", record.public.root, blob.path), &bytes)
            .unwrap();
        record.public.files[i].version = v.metadata.id;
    }
    record.public.status = ProjectSkillStatus::Installed;
    db.save_skill_record(&record).unwrap();
    record
}
/// Match the existing saved-rule fixture shape and canonical task storage.
fn task(b: &Binding, record: &db::Record) -> SavedTask {
    let runtime = SandboxRuntimeRef {
        vm_id: b.vm_id,
        runtime_id: Uuid::new_v4(),
    };
    let mut task:SavedTask=serde_json::from_value(serde_json::json!({"task":{"id":Uuid::new_v4(),"projectId":b.project_id,"attemptId":Uuid::new_v4(),"runtime":runtime,"request":{"kind":"code","input":"Use the selected revision","projectDirectory":"","selectedSkillIds":[record.public.id]},"selection":{"connectionId":Uuid::new_v4(),"model":"fixture"},"state":"waiting","createdAt":1,"updatedAt":1,"result":"","events":[],"changes":[]},"project":{"name":"fixture","vmId":b.vm_id},"workspace":null,"guest":runtime,"guest_pending":false})).unwrap();
    let snapshot = ProjectInstructionSnapshot {
        id: Uuid::new_v4(),
        directory: "".into(),
        created_at: 1,
        sha256: super::super::instructions::digest("", &[]).unwrap(),
        files: vec![],
    };
    task.task.request.instruction_snapshot_id = Some(snapshot.id);
    task.instructions = Some(SavedInstructions {
        database_id: b.database_id,
        volume_id: b.volume_id,
        generation: b.generation,
        snapshot,
    });
    let p = &record.public;
    task.skills = vec![tasks::SavedSkill {
        revision: ProjectTaskSkill {
            id: p.id,
            source: p.entry.source,
            revision: p.entry.revision.clone(),
            path: p.entry.path.clone(),
            root: p.root.clone(),
            sha256: p.sha256.clone(),
        },
        instructions: vec![(
            "SKILL.md".into(),
            String::from_utf8(STANDARD.decode(&record.blobs[0].base64).unwrap()).unwrap(),
        )],
        files: p.files.clone(),
    }];
    task
}
#[tokio::test]
async fn pending_installation_reopens_with_the_original_bytes_and_operation_ids() {
    let (_dir, state, seed) = fixture().await;
    bind(&state, &seed).await;
    let b = binding(&state, seed.project_id).unwrap();
    let db = open(&state, &b).unwrap();
    let (entry, blobs) = package();
    let saved = record(Uuid::new_v4(), Caller::default(), entry, blobs).unwrap();
    db.save_skill_record(&saved).unwrap();
    drop(db);
    let db = open(&state, &b).unwrap();
    let restored = db.skill_record(saved.public.id).unwrap();
    assert_eq!(restored.public.status, ProjectSkillStatus::Pending);
    assert_eq!(
        restored.public.files[0].operation_id,
        saved.public.files[0].operation_id
    );
    assert_eq!(restored.blobs[0].base64, saved.blobs[0].base64);
    let input = InstallProjectSkill {
        source: saved.public.entry.source,
        kind: saved.public.entry.kind,
        path: saved.public.entry.path.clone(),
        revision: saved.public.entry.revision.clone(),
    };
    assert!(restored.matches(&input));
    let mut changed = input;
    changed.revision = "b".repeat(40);
    assert!(!restored.matches(&changed));
    let mut corrupt = restored;
    corrupt.blobs[0].base64 = STANDARD.encode("changed");
    assert!(db.save_skill_record(&corrupt).is_err());
}
#[tokio::test]
async fn removing_stopped_installations_preserves_accepted_task_bytes_and_resource_versions() {
    let (_dir, state, seed) = fixture().await;
    bind(&state, &seed).await;
    let b = binding(&state, seed.project_id).unwrap();
    let db = open(&state, &b).unwrap();
    let (entry, blobs) = package();
    let saved = complete(
        &db,
        record(Uuid::new_v4(), Caller::default(), entry, blobs).unwrap(),
    );
    let mut accepted = task(&b, &saved);
    let message = tasks::message(&accepted, &[]).unwrap().unwrap();
    db.observe_file(
        &format!("{}/references/context.md", saved.public.root),
        b"Later mutable file",
    )
    .unwrap();
    drop(db);
    let removed = remove(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Caller::default(),
        Path((b.project_id, saved.public.id)),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(removed.status, ProjectSkillStatus::Removed);
    assert_eq!(tasks::message(&accepted, &[]).unwrap().unwrap(), message);
    let resource = tasks::read(
        &state,
        &accepted,
        &serde_json::json!({"skillId":saved.public.id,"path":"references/context.md"}),
    )
    .await
    .unwrap();
    assert_eq!(resource["content"], "Original reference\n");
    assert!(
        tasks::read(
            &state,
            &accepted,
            &serde_json::json!({"skillId":Uuid::new_v4(),"path":"references/context.md"})
        )
        .await
        .is_err()
    );
    assert!(
        tasks::read(
            &state,
            &accepted,
            &serde_json::json!({"skillId":saved.public.id,"path":"../AGENTS.md"})
        )
        .await
        .is_err()
    );
    accepted.skills[0].instructions[0].1 = "tampered".into();
    assert!(tasks::message(&accepted, &[]).is_err());
}
#[tokio::test]
async fn project_readers_cannot_remove_or_discover_another_projects_installation() {
    let (_dir, state, seed) = fixture().await;
    bind(&state, &seed).await;
    let b = binding(&state, seed.project_id).unwrap();
    let db = open(&state, &b).unwrap();
    let (entry, blobs) = package();
    let saved = record(Uuid::new_v4(), Caller::default(), entry, blobs).unwrap();
    db.save_skill_record(&saved).unwrap();
    drop(db);
    let host = Uuid::new_v4();
    let caller: Caller =
        serde_json::from_value(serde_json::json!({"principal":{"host":host},"operator":false}))
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
        list(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            caller,
            Path(b.project_id)
        )
        .await
        .unwrap()
        .0
        .len(),
        1
    );
    let denied = remove(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        caller,
        Path((b.project_id, saved.public.id)),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(
        denied.into_response().status(),
        axum::http::StatusCode::FORBIDDEN
    );
    assert!(
        read(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            caller,
            Path((Uuid::new_v4(), saved.public.id))
        )
        .await
        .is_err()
    );
}
#[tokio::test]
async fn legacy_schema_and_requests_keep_existing_history_and_omit_empty_skill_fields() {
    let (_dir, state, seed) = fixture().await;
    bind(&state, &seed).await;
    let b = binding(&state, seed.project_id).unwrap();
    let db = open(&state, &b).unwrap();
    let file = db.observe_file("AGENTS.md", b"Retained rule").unwrap();
    db.conn
        .execute_batch(
            "DROP TABLE skill_catalogs; DROP TABLE skill_installations; PRAGMA user_version=8;",
        )
        .unwrap();
    drop(db);
    let db = open(&state, &b).unwrap();
    assert!(db.skill_installations().unwrap().is_empty());
    assert_eq!(
        db.file_version("AGENTS.md", file.metadata.id)
            .unwrap()
            .bytes,
        b"Retained rule"
    );
    assert_eq!(db.skill_catalogs().unwrap().len(), 2);
    let input: SandboxTaskInput =
        serde_json::from_value(serde_json::json!({"kind":"code","input":"Legacy"})).unwrap();
    assert!(
        serde_json::to_value(input)
            .unwrap()
            .get("selectedSkillIds")
            .is_none()
    );
}
#[test]
fn source_compatibility_and_missing_or_corrupt_snapshots_cannot_silently_fall_back() {
    assert!(tasks::compatible(
        ProjectSkillSource::Openai,
        AiProvider::Openai
    ));
    assert!(tasks::compatible(
        ProjectSkillSource::Anthropic,
        AiProvider::Anthropic
    ));
    assert!(!tasks::compatible(
        ProjectSkillSource::Openai,
        AiProvider::Anthropic
    ));
    let (entry, blobs) = package();
    let record = record(Uuid::new_v4(), Caller::default(), entry, blobs).unwrap();
    let b = Binding {
        project_id: Uuid::new_v4(),
        database_id: Uuid::new_v4(),
        volume_id: Uuid::new_v4(),
        vm_id: Uuid::new_v4(),
        generation: 1,
        provisioning: false,
        verified_runtime_id: None,
    };
    let mut task = task(&b, &record);
    task.skills.clear();
    assert!(tasks::message(&task, &[]).is_err());
    assert!(serde_json::from_value::<InstallProjectSkill>(serde_json::json!({"source":"https://evil.invalid","kind":"skill","path":"skills/x","revision":"a".repeat(40)})).is_err());
}
#[tokio::test]
async fn retention_limits_preserve_existing_receipts_and_owner_fencing() {
    let (_dir, state, seed) = fixture().await;
    bind(&state, &seed).await;
    let b = binding(&state, seed.project_id).unwrap();
    let db = open(&state, &b).unwrap();
    let (entry, blobs) = package();
    let mut first = None;
    for _ in 0..64 {
        let r = record(
            Uuid::new_v4(),
            Caller::default(),
            entry.clone(),
            blobs.clone(),
        )
        .unwrap();
        db.save_skill_record(&r).unwrap();
        first = Some(r);
    }
    assert!(
        db.save_skill_record(&record(Uuid::new_v4(), Caller::default(), entry, blobs).unwrap())
            .is_err()
    );
    let mut saved = first.unwrap();
    saved.public.status = ProjectSkillStatus::Removed;
    db.save_skill_record(&saved).unwrap();
    db.conn
        .execute("UPDATE identity SET epoch=epoch+1", [])
        .unwrap();
    assert!(db.save_skill_record(&saved).is_err());
}

#[tokio::test]
async fn source_capacity_blocks_new_downloads_without_blocking_retained_installation_retries() {
    let (_dir, state, seed) = fixture().await;
    bind(&state, &seed).await;
    let b = binding(&state, seed.project_id).unwrap();
    let db = open(&state, &b).unwrap();
    let (entry, blobs) = package();
    let saved = complete(
        &db,
        record(Uuid::new_v4(), Caller::default(), entry, blobs).unwrap(),
    );
    drop(db);
    let permit = state
        .workspaces
        .skill_downloads
        .clone()
        .acquire_many_owned(2)
        .await
        .unwrap();
    let busy = refresh(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Caller::default(),
        Path((b.project_id, ProjectSkillSource::Openai)),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(busy.code(), "workspace_busy");
    let entry = saved.public.entry;
    let response = install(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Caller::default(),
        Path((b.project_id, saved.public.id)),
        ValidatedJson(InstallProjectSkill {
            source: entry.source,
            path: entry.path,
            revision: entry.revision,
            kind: entry.kind,
        }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(response.status, ProjectSkillStatus::Installed);
    assert_eq!(response.id, saved.public.id);
    drop(permit);
    assert!(
        state
            .workspaces
            .skill_downloads
            .clone()
            .try_acquire_many_owned(2)
            .is_ok()
    );
}
