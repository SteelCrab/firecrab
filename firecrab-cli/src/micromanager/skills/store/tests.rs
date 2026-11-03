use super::super::source::tests::{APACHE, Fixture, SHA, skill_md, standard};
use super::*;
use std::io::Read;

const SHA2: &str = "1111111111111111111111111111111111111111";

#[test]
fn import_copies_every_file_and_records_digest() {
    let fixture = Fixture::start();
    standard(&fixture, SHA, "print('run')\n");
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path());
    let imported = store.import(&fixture.source(), "webapp-testing").unwrap();
    assert_eq!(imported.commit, SHA);
    assert_eq!(imported.digest.len(), 64);
    assert!(imported.digest.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(
        fs::read_to_string(
            home.path()
                .join("runtime/skills/store/webapp-testing/scripts/run.py")
        )
        .unwrap(),
        "print('run')\n"
    );
    let all = store.imported().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].description, "The webapp-testing skill.");
}

#[test]
fn import_refuses_non_apache_and_problem_skills() {
    let fixture = Fixture::start();
    standard(&fixture, SHA, "x");
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path());
    let error = store.import(&fixture.source(), "pdf").err().unwrap();
    assert!(error.to_string().contains("license"), "{error}");
    assert!(store.import(&fixture.source(), "missing").is_err());
    let unsupported = Fixture::start();
    let md = skill_md("x", None);
    unsupported.publish(
        SHA,
        &[("skills/x/SKILL.md", &md), ("skills/x/LICENSE.txt", APACHE)],
        &[json!({"path":"skills/x/link","mode":"120000","type":"blob","size":4,"sha":"s"})],
    );
    let other = tempfile::tempdir().unwrap();
    assert!(
        Store::new(other.path())
            .import(&unsupported.source(), "x")
            .is_err()
    );
    assert!(Store::new(other.path()).imported().unwrap().is_empty());
}

#[test]
fn import_replaces_previous_version_atomically() {
    let fixture = Fixture::start();
    let md = skill_md("webapp-testing", None);
    fixture.publish(
        SHA,
        &[
            ("skills/webapp-testing/SKILL.md", &md),
            ("skills/webapp-testing/LICENSE.txt", APACHE),
            ("skills/webapp-testing/scripts/old.py", "old"),
        ],
        &[],
    );
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path());
    store.import(&fixture.source(), "webapp-testing").unwrap();
    fixture.publish(
        SHA2,
        &[
            ("skills/webapp-testing/SKILL.md", &md),
            ("skills/webapp-testing/LICENSE.txt", APACHE),
            ("skills/webapp-testing/scripts/new.py", "new"),
        ],
        &[],
    );
    store.import(&fixture.source(), "webapp-testing").unwrap();
    let all = store.imported().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].commit, SHA2);
    let root = home.path().join("runtime/skills/store");
    assert!(!root.join("webapp-testing/scripts/old.py").exists());
    assert!(root.join("webapp-testing/scripts/new.py").exists());
    let leftovers: Vec<_> = fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with('.'))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn view_marks_reasons_and_updates() {
    let fixture = Fixture::start();
    let webapp = skill_md("webapp-testing", None);
    let pdf = skill_md("pdf", Some("Proprietary. LICENSE.txt has complete terms"));
    let doc = skill_md("doc-coauthoring", None);
    let publish = |sha: &str, script: &str| {
        fixture.publish(
            sha,
            &[
                ("skills/webapp-testing/SKILL.md", &webapp),
                ("skills/webapp-testing/LICENSE.txt", APACHE),
                ("skills/webapp-testing/scripts/run.py", script),
                ("skills/pdf/SKILL.md", &pdf),
                ("skills/doc-coauthoring/SKILL.md", &doc),
            ],
            &[],
        )
    };
    publish(SHA, "one");
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path());
    store.import(&fixture.source(), "webapp-testing").unwrap();
    let view = store.view(&fixture.source()).unwrap();
    let by = |name: &str| {
        view["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(view["repository"], "anthropics/skills");
    assert_eq!(view["commit"], SHA);
    assert_eq!(view["error"], Value::Null);
    assert_eq!(by("pdf")["reason"], "proprietary");
    assert_eq!(by("pdf")["importable"], false);
    assert_eq!(by("doc-coauthoring")["reason"], "unknown-license");
    assert_eq!(by("doc-coauthoring")["license"], "unknown");
    assert_eq!(by("webapp-testing")["license"], "apache2");
    assert_eq!(by("webapp-testing")["importable"], true);
    assert_eq!(by("webapp-testing")["imported"]["commit"], SHA);
    assert_eq!(by("webapp-testing")["updateAvailable"], false);
    publish(SHA2, "two");
    let view = store.view(&fixture.source()).unwrap();
    let webapp = view["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "webapp-testing")
        .unwrap();
    assert_eq!(webapp["updateAvailable"], true);
}

#[test]
fn view_keeps_imported_skills_when_github_fails() {
    let fixture = Fixture::start();
    standard(&fixture, SHA, "x");
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path());
    store.import(&fixture.source(), "webapp-testing").unwrap();
    fixture.set(
        "/repos/anthropics/skills/commits/main",
        403,
        &[("x-ratelimit-remaining", "0")],
        b"{}",
    );
    let view = store.view(&fixture.source()).unwrap();
    assert!(
        view["error"]
            .as_str()
            .unwrap()
            .contains("GitHub API rate limit reached. Try again later.")
    );
    assert_eq!(view["commit"], Value::Null);
    let skills = view["skills"].as_array().unwrap();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0]["name"], "webapp-testing");
    assert_eq!(skills[0]["description"], "The webapp-testing skill.");
    assert_eq!(skills[0]["imported"]["commit"], SHA);
}

#[test]
fn archive_contains_marker_and_files() {
    let fixture = Fixture::start();
    standard(&fixture, SHA, "print('run')\n");
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path());
    let imported = store.import(&fixture.source(), "webapp-testing").unwrap();
    let bytes = store
        .archive(&["webapp-testing".into(), "never-imported".into()])
        .unwrap();
    let mut archive = tar::Archive::new(bytes.as_slice());
    let mut entries = std::collections::BTreeMap::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        let mut text = String::new();
        entry.read_to_string(&mut text).unwrap();
        entries.insert(path, text);
    }
    assert_eq!(
        entries.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "webapp-testing/.firecrab-skill",
            "webapp-testing/LICENSE.txt",
            "webapp-testing/SKILL.md",
            "webapp-testing/scripts/run.py"
        ]
    );
    assert_eq!(entries["webapp-testing/scripts/run.py"], "print('run')\n");
    let marker: Value = serde_json::from_str(&entries["webapp-testing/.firecrab-skill"]).unwrap();
    assert_eq!(
        marker,
        json!({"name":"webapp-testing","commit":SHA,"digest":imported.digest})
    );
}
