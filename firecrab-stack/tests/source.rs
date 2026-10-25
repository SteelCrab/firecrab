use firecrab_stack::{GitSource, GitSourceStatus, GitSyncMode};
use serde_json::json;

fn source() -> GitSource {
    serde_json::from_value(json!({"repository":"https://github.com/example/infra", "ref":"heads/main", "manifestPath":"hosts/demo/stack.yaml"})).unwrap()
}

#[test]
fn source_accepts_explicit_refs_and_credential_references() {
    let mut config = source();
    assert_eq!(config.poll_interval_seconds, 900);
    assert!(!config.paused);
    assert_eq!(config.sync_mode, GitSyncMode::DetectOnly);
    assert!(!config.allow_stop);
    let wire = serde_json::to_value(&config).unwrap();
    assert!(wire.get("syncMode").is_none());
    assert!(wire.get("allowStop").is_none());
    for reference in [
        "main",
        "heads/main",
        "tags/v1.0.0",
        "feat/config-update",
        "0123456789abcdef0123456789abcdef01234567",
    ] {
        config.git_ref = reference.into();
        config.validate().unwrap();
    }
    let mut json = serde_json::to_value(config).unwrap();
    json["credentialRef"] = "github".into();
    assert!(
        serde_json::from_value::<GitSource>(json.clone())
            .unwrap()
            .validate()
            .is_ok()
    );
    json["token"] = "do-not-print".into();
    assert!(serde_json::from_value::<GitSource>(json).is_err());
}

#[test]
fn automatic_sync_and_stop_permission_require_explicit_configuration() {
    let mut json = serde_json::to_value(source()).unwrap();
    json["syncMode"] = "automatic".into();
    json["allowStop"] = true.into();
    let config: GitSource = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(config.sync_mode, GitSyncMode::Automatic);
    assert!(config.allow_stop);
    json["syncMode"] = "enabled".into();
    assert!(serde_json::from_value::<GitSource>(json).is_err());
}

#[test]
fn legacy_source_status_and_active_checks_remain_detect_only() {
    let config = json!({"repository":"https://github.com/example/infra","ref":"main","manifestPath":"stack.yaml"});
    let legacy = json!({
        "stackId":"11111111-1111-4111-8111-111111111111", "name":"demo", "generation":1,
        "source":config, "nextCheckAt":0, "failures":0,
        "latest":{
            "id":"22222222-2222-4222-8222-222222222222", "generation":1,
            "source":config, "startedAt":0, "finishedAt":null, "state":"checking",
            "commit":null, "blob":null, "desiredHash":null, "plan":null, "error":null
        }
    });
    let source: GitSourceStatus = serde_json::from_value(legacy).unwrap();
    assert_eq!(source.source.sync_mode, GitSyncMode::DetectOnly);
    let check = source.latest.unwrap();
    assert!(!check.sync);
    assert!(check.operation_id.is_none());
    assert!(source.attempted_revision.is_none());
    assert!(source.recovery_operation_id.is_none());
}

#[test]
fn source_rejects_credentials_redirect_hosts_revision_expressions_and_traversal() {
    for repository in [
        "http://github.com/a/b",
        "https://github.com.evil/a/b",
        "https://secret@github.com/a/b",
        "https://github.com/a/b?token=secret",
        "https://github.com/a/b#secret",
        "https://github.com/a/b.git",
        "https://github.com/a/../b",
        "file:///tmp/repo",
        "https://127.0.0.1/a/b",
    ] {
        let mut config = source();
        config.repository = repository.into();
        let error = config.validate().unwrap_err();
        assert!(!error.contains("secret"));
    }
    for reference in [
        "",
        "-main",
        "../main",
        "main~1",
        "main^",
        "@{1}",
        "a//b",
        "a.lock",
        "a?b",
        "a#b",
        "a%2fb",
        "a b",
        "a\nb",
        "refs/heads/.hidden",
    ] {
        let mut config = source();
        config.git_ref = reference.into();
        assert!(config.validate().is_err(), "{reference}");
    }
    for path in [
        "",
        "/stack.yaml",
        "../stack.yaml",
        "a/../../stack.yaml",
        "a//stack.yaml",
        "a/./stack.yaml",
        "stack.yaml?token=secret",
        "stack%2eyaml",
        "a\\stack.yaml",
    ] {
        let mut config = source();
        config.manifest_path = path.into();
        assert!(config.validate().is_err(), "{path}");
    }
    let mut config = source();
    config.manifest_path = "a/".repeat(17) + "stack.yaml";
    assert!(config.validate().is_err());
    for interval in [0, 59, 86401, u32::MAX] {
        let mut config = source();
        config.poll_interval_seconds = interval;
        assert!(config.validate().is_err());
    }
}
