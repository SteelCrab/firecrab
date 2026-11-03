use super::super::tests::{shell_replies, workspace_fixture};
use super::*;

// Invented entries in the shape `codex debug models` prints.
fn catalog(instructions: &str) -> String {
    json!({"models":[
        {"slug":"sample-mini","display_name":"Sample Mini","visibility":"list","priority":4,"base_instructions":instructions},
        {"slug":"sample-pro","display_name":"Sample Pro","visibility":"list","priority":1},
        {"slug":"sample-hidden","display_name":"Sample Hidden","visibility":"hide","priority":0},
        {"slug":"sample-plain","visibility":"list","priority":2},
        {"slug":"sample-unranked","display_name":"Sample Unranked","visibility":"list"},
        {"slug":"bad\nslug","display_name":"Bad","visibility":"list","priority":3},
        {"slug":"x".repeat(129),"visibility":"list","priority":3},
        {"slug":"","visibility":"list","priority":3},
        {"slug":"sample-pro","display_name":"Duplicate","visibility":"list","priority":5}
    ]})
    .to_string()
}

#[test]
fn only_models_codex_lists_in_its_picker_are_offered_in_its_order() {
    assert_eq!(
        parse_codex_models(&catalog("")).unwrap(),
        json!([
            {"id":"sample-pro","name":"Sample Pro"},
            {"id":"sample-plain","name":"sample-plain"},
            {"id":"sample-mini","name":"Sample Mini"},
            {"id":"sample-unranked","name":"Sample Unranked"},
        ])
    );
    for text in ["", "not JSON", r#"{"models":{}}"#, r#"{"other":[]}"#] {
        assert!(parse_codex_models(text).is_err(), "{text}");
    }
}

#[test]
fn the_catalog_comes_from_the_vms_codex_once_per_vm_and_failures_are_not_kept() {
    let (root, runtime, _) = workspace_fixture();
    // `timeout` in the guest exits 124 when Codex does not answer in time.
    shell_replies(
        root.path(),
        json!({"codex debug models": {"status": 124, "err": "timed out"}}),
    );
    assert!(runtime.codex_models().is_err());
    // The full catalog is far larger than a plain shell reply.
    shell_replies(
        root.path(),
        json!({"codex debug models": {"out": catalog(&"x".repeat(200_000))}}),
    );
    assert_eq!(
        runtime.codex_models().unwrap()["models"][0],
        json!({"id":"sample-pro","name":"Sample Pro"})
    );
    shell_replies(
        root.path(),
        json!({"codex debug models": {"out": "not JSON"}}),
    );
    assert_eq!(
        runtime.codex_models().unwrap()["models"][0]["id"],
        "sample-pro"
    );
    let calls = fs::read_to_string(root.path().join("remote-calls.jsonl")).unwrap();
    assert_eq!(calls.matches("codex debug models").count(), 2);
    assert!(calls.contains("timeout 10 codex debug models"));
    assert!(!calls.contains("--bundled"));

    let (root, mut other, _) = workspace_fixture();
    other.guest.as_mut().unwrap().available.remove("codex");
    assert!(other.codex_models().is_err());
    assert!(!root.path().join("remote-calls.jsonl").exists());
}
