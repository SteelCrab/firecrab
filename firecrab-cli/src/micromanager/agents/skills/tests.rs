use super::super::tests::{guest_fixture, shell_replies};
use super::*;
use crate::micromanager::skills::source::tests::{Fixture, SHA, standard};
use crate::micromanager::skills::store::Store;
use base64::{Engine, engine::general_purpose::STANDARD};

fn calls(root: &Path) -> Vec<String> {
    fs::read_to_string(root.join("remote-calls.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn enable(runtime: &Runtime, names: &[&str]) {
    fs::create_dir_all(&runtime.home).unwrap();
    fs::write(
        runtime.home.join("skills.json"),
        json!({"enabled": names}).to_string(),
    )
    .unwrap();
}

#[test]
fn set_skill_saves_target_and_reports_pending_when_vm_is_unreachable() {
    let fixture = Fixture::start();
    standard(&fixture, SHA, "x");
    let home = tempfile::tempdir().unwrap();
    Store::new(home.path())
        .import(&fixture.source(), "webapp-testing")
        .unwrap();
    let vm = Uuid::new_v4();
    let value = set_vm_skill(home.path(), vm, "webapp-testing", true).unwrap();
    assert_eq!(value["state"], "pending");
    assert!(value["reason"].is_string());
    assert_eq!(value["enabled"], json!(["webapp-testing"]));
    let saved: Value = serde_json::from_slice(
        &fs::read(
            home.path()
                .join(format!("runtime/agents/microvms/{vm}/skills.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(saved["enabled"], json!(["webapp-testing"]));
    let off = set_vm_skill(home.path(), vm, "webapp-testing", false).unwrap();
    assert_eq!(off["enabled"], json!([]));
}

#[test]
fn set_skill_rejects_names_that_were_never_imported() {
    let home = tempfile::tempdir().unwrap();
    assert!(set_vm_skill(home.path(), Uuid::new_v4(), "webapp-testing", true).is_err());
}

#[test]
fn sync_sends_archive_once_and_reports_applied() {
    let (root, runtime) = guest_fixture(true);
    let fixture = Fixture::start();
    standard(&fixture, SHA, "print('run')\n");
    Store::new(&runtime.skills_home)
        .import(&fixture.source(), "webapp-testing")
        .unwrap();
    enable(&runtime, &["webapp-testing"]);
    let value = runtime.sync_skills().unwrap();
    assert_eq!(value["state"], "applied", "{value}");
    let transfers: Vec<_> = calls(root.path())
        .into_iter()
        .filter(|c| c.contains("tar -x"))
        .collect();
    assert_eq!(transfers.len(), 1);
    let bytes = fs::read(root.path().join("remote-stdin-0.bin")).unwrap();
    let mut archive = tar::Archive::new(bytes.as_slice());
    let paths: Vec<String> = archive
        .entries()
        .unwrap()
        .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(paths.contains(&"webapp-testing/scripts/run.py".to_owned()));
    assert!(paths.contains(&"webapp-testing/.firecrab-skill".to_owned()));
}

#[test]
fn sync_skips_transfer_when_vm_record_matches() {
    let (root, runtime) = guest_fixture(true);
    let fixture = Fixture::start();
    standard(&fixture, SHA, "x");
    let imported = Store::new(&runtime.skills_home)
        .import(&fixture.source(), "webapp-testing")
        .unwrap();
    enable(&runtime, &["webapp-testing"]);
    let record = json!({"skills":[{"name":"webapp-testing","digest":imported.digest}]});
    shell_replies(
        root.path(),
        json!({"head -c 65536 /root/.local/share/firecrab/skills.json": {
            "out": STANDARD.encode(record.to_string())
        }}),
    );
    let value = runtime.sync_skills().unwrap();
    assert_eq!(value["state"], "applied");
    assert!(!calls(root.path()).iter().any(|c| c.contains("tar -x")));
}

#[test]
fn sync_reports_conflicts_without_failing() {
    let (root, runtime) = guest_fixture(true);
    let fixture = Fixture::start();
    standard(&fixture, SHA, "x");
    Store::new(&runtime.skills_home)
        .import(&fixture.source(), "webapp-testing")
        .unwrap();
    enable(&runtime, &["webapp-testing"]);
    shell_replies(
        root.path(),
        json!({"tar -x": {"out": "conflict:/root/.claude/skills/webapp-testing\n"}}),
    );
    let value = runtime.sync_skills().unwrap();
    assert_eq!(value["state"], "applied");
    assert_eq!(
        value["conflicts"],
        json!(["/root/.claude/skills/webapp-testing"])
    );
}

#[test]
fn sync_failure_is_reported_as_failed_state() {
    let (root, runtime) = guest_fixture(true);
    let fixture = Fixture::start();
    standard(&fixture, SHA, "x");
    Store::new(&runtime.skills_home)
        .import(&fixture.source(), "webapp-testing")
        .unwrap();
    enable(&runtime, &["webapp-testing"]);
    shell_replies(
        root.path(),
        json!({"tar -x": {"err": "tar: short read", "status": 2}}),
    );
    let value = runtime.sync_skills().unwrap();
    assert_eq!(value["state"], "failed");
    assert!(value["reason"].as_str().unwrap().contains("short read"));
}

#[test]
fn clis_list_installed_coding_clis() {
    let (_root, mut runtime) = guest_fixture(true);
    runtime
        .guest
        .as_mut()
        .unwrap()
        .available
        .insert("claude".into());
    let value = runtime.sync_skills().unwrap();
    let clis = value["clis"].as_array().unwrap();
    let find = |id: &str| clis.iter().find(|c| c["id"] == id).cloned();
    assert_eq!(
        find("codex").unwrap(),
        json!({"id":"codex","name":"Codex","installed":true})
    );
    assert_eq!(find("claude").unwrap()["installed"], true);
    assert_eq!(find("gemini").unwrap()["installed"], false);
    assert!(find("github").is_none());
}

#[test]
fn shell_with_input_delivers_stdin() {
    let (root, runtime) = guest_fixture(true);
    runtime
        .guest
        .as_ref()
        .unwrap()
        .shell_with_input("cat", b"abc", Duration::from_secs(5))
        .unwrap();
    assert_eq!(
        fs::read(root.path().join("remote-stdin-0.bin")).unwrap(),
        b"abc"
    );
}

#[test]
fn task_start_syncs_skills_before_creating_the_session() {
    let (root, runtime) = guest_fixture(true);
    let fixture = Fixture::start();
    standard(&fixture, SHA, "x");
    Store::new(&runtime.skills_home)
        .import(&fixture.source(), "webapp-testing")
        .unwrap();
    enable(&runtime, &["webapp-testing"]);
    runtime
        .start_task(TaskStart {
            id: Uuid::new_v4(),
            input: "Add tests".into(),
            cwd: String::new(),
            repository: "example/project".into(),
            branch: "main".into(),
            cli: "codex".into(),
            model: String::new(),
        })
        .unwrap();
    let calls = calls(root.path());
    let sync = calls.iter().position(|c| c.contains("tar -x")).unwrap();
    let create = calls
        .iter()
        .position(|c| c.contains("'workspace' 'create'"))
        .unwrap();
    assert!(sync < create);
}

#[test]
fn successful_preparation_syncs_skills_into_new_clis() {
    let (root, runtime) = guest_fixture(true);
    fs::create_dir_all(&runtime.home).unwrap();
    let fixture = Fixture::start();
    standard(&fixture, SHA, "x");
    Store::new(&runtime.skills_home)
        .import(&fixture.source(), "webapp-testing")
        .unwrap();
    enable(&runtime, &["webapp-testing"]);
    shell_replies(
        root.path(),
        json!({"timeout 900": {"out": "FIRECRAB_STEP=4\n", "status": 0}}),
    );
    runtime.prepare("codex").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let log = loop {
        let log = fs::read_to_string(runtime.home.join("preparation.log")).unwrap_or_default();
        if log.contains("FIRECRAB_SKILLS=") || Instant::now() > deadline {
            break log;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(log.contains("FIRECRAB_SKILLS=applied"), "{log}");
    assert!(calls(root.path()).iter().any(|c| c.contains("tar -x")));
}
