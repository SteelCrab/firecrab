use super::super::tests::{guest_fixture, shell_replies};
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};

const READ: &str = "head -c 65537 /workspace/firecrab/project/AGENTS.md";
const CLAUDE: &str = "head -c 65537 /root/.claude/CLAUDE.md";
const GEMINI: &str = "head -c 65537 /root/.gemini/settings.json";
const DOC: &str = "# Rules\n\n- Run tests.\n- Write English PR titles.\n";

fn ready() -> (tempfile::TempDir, Runtime) {
    let (root, mut runtime) = guest_fixture(true);
    let guest = runtime.guest.as_mut().unwrap();
    guest.available.insert("claude".into());
    guest.authenticated.insert("claude".into());
    let manifest = project::VmProject {
        vm_id: guest.id,
        repository: Some("https://github.com/example/project.git".into()),
        branch: "main".into(),
        path: project::WORKSPACE.into(),
    };
    fs::create_dir_all(&runtime.home).unwrap();
    fs::write(
        root.path().join("guest-project.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    (root, runtime)
}

fn sha(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn replies(root: &Path, extra: Value) {
    let mut all = json!({
        READ: {"out": STANDARD.encode(DOC)},
        CLAUDE: {"out": STANDARD.encode("@/workspace/firecrab/project/AGENTS.md\n")},
        GEMINI: {"out": STANDARD.encode(r#"{"context":{"fileName":["AGENTS.md","GEMINI.md"]}}"#)},
    });
    for (k, v) in extra.as_object().unwrap() {
        all[k] = v.clone();
    }
    shell_replies(root, all);
}

fn calls(root: &Path) -> Vec<String> {
    fs::read_to_string(root.join("remote-calls.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn stdin(root: &Path, n: usize) -> String {
    fs::read_to_string(root.join(format!("remote-stdin-{n}.bin"))).unwrap()
}

fn refine(cli: &str, input: &str) -> Refine {
    Refine {
        mode: Mode::Edit,
        block: Some(1),
        input: input.into(),
        cli: cli.into(),
        model: String::new(),
        digest: sha(DOC),
        previous: String::new(),
    }
}

#[test]
fn rules_are_read_only_until_the_project_is_ready() {
    let (_root, runtime) = guest_fixture(true);
    let value = runtime.rules().unwrap();
    assert_eq!(value["state"], "unavailable");
    assert_eq!(
        value["reason"],
        "Prepare the repository in Workspace first."
    );
}

#[test]
fn rules_read_existing_file_with_digest_and_blocks() {
    let (root, runtime) = ready();
    replies(root.path(), json!({}));
    let value = runtime.rules().unwrap();
    assert_eq!(value["state"], "ready");
    assert_eq!(value["exists"], true);
    assert_eq!(value["text"], DOC);
    assert_eq!(value["digest"], sha(DOC));
    assert_eq!(value["blocks"].as_array().unwrap().len(), 3);
}

#[test]
fn missing_file_is_an_empty_document() {
    let (root, runtime) = ready();
    replies(root.path(), json!({READ: {"out": "missing"}}));
    let value = runtime.rules().unwrap();
    assert_eq!(value["exists"], false);
    assert_eq!(value["text"], "");
    assert_eq!(value["digest"], sha(""));
}

#[test]
fn non_utf8_or_oversized_file_is_unavailable() {
    for bytes in [vec![0xff, 0xfe], vec![b'a'; 65537]] {
        let (root, runtime) = ready();
        replies(root.path(), json!({READ: {"out": STANDARD.encode(&bytes)}}));
        assert_eq!(runtime.rules().unwrap()["state"], "unavailable");
    }
}

#[test]
fn refine_requires_a_logged_in_codex_or_claude() {
    let (root, mut runtime) = ready();
    replies(root.path(), json!({}));
    assert!(runtime.refine_rules(refine("gemini", "x")).is_err());
    runtime
        .guest
        .as_mut()
        .unwrap()
        .authenticated
        .remove("claude");
    assert!(runtime.refine_rules(refine("claude", "x")).is_err());
    assert!(
        !calls(root.path())
            .iter()
            .any(|c| c.contains("firecrab-refine"))
    );
}

#[test]
fn refine_sends_prompt_on_stdin_and_returns_one_block() {
    let (root, runtime) = ready();
    replies(
        root.path(),
        json!({"firecrab-refine": {"out": "```markdown\n- Always run tests before commit.\n```\n"}}),
    );
    let value = runtime
        .refine_rules(refine("claude", "never commit without tests"))
        .unwrap();
    assert_eq!(value["after"], "- Always run tests before commit.");
    assert_eq!(value["before"], "- Run tests.");
    assert_eq!(value["mode"], "edit");
    assert_eq!(value["block"], 1);
    assert_eq!(value["cli"], "claude");
    assert_eq!(value["name"], "Claude Code");
    assert_eq!(value["digest"], sha(DOC));
    let script = calls(root.path())
        .into_iter()
        .find(|c| c.contains("firecrab-refine"))
        .unwrap();
    assert!(script.contains("mktemp -d"));
    assert!(!script.contains("cd /workspace"));
    let prompt = stdin(root.path(), 0);
    assert!(prompt.contains("never commit without tests"));
    assert!(prompt.contains("- Write English PR titles."));
    assert!(prompt.contains("Current rule:\n- Run tests."));
}

#[test]
fn refine_rejects_empty_or_oversized_output() {
    for out in [String::from("  \n"), "x".repeat(8193)] {
        let (root, runtime) = ready();
        replies(root.path(), json!({"firecrab-refine": {"out": out}}));
        assert!(runtime.refine_rules(refine("claude", "x")).is_err());
    }
}

#[test]
fn refine_rejects_input_over_16_kib() {
    let (root, runtime) = ready();
    replies(root.path(), json!({}));
    assert!(
        runtime
            .refine_rules(refine("claude", &"x".repeat(16385)))
            .is_err()
    );
    assert!(
        !calls(root.path())
            .iter()
            .any(|c| c.contains("firecrab-refine"))
    );
}

#[test]
fn apply_refuses_when_digest_changed() {
    let (root, runtime) = ready();
    replies(root.path(), json!({}));
    let error = runtime
        .apply_rules(Apply {
            mode: Mode::Add,
            block: None,
            text: "- x".into(),
            digest: sha("old"),
        })
        .err()
        .unwrap();
    assert!(
        error
            .to_string()
            .contains("The file changed. Reload it and refine again."),
        "{error}"
    );
    assert!(!calls(root.path()).iter().any(|c| c.contains("cat >")));
}

#[test]
fn apply_edit_writes_only_the_target_block() {
    let (root, runtime) = ready();
    replies(root.path(), json!({}));
    let value = runtime
        .apply_rules(Apply {
            mode: Mode::Edit,
            block: Some(1),
            text: "- Always run tests.".into(),
            digest: sha(DOC),
        })
        .unwrap();
    assert_eq!(
        stdin(root.path(), 0),
        "# Rules\n\n- Always run tests.\n- Write English PR titles.\n"
    );
    assert_eq!(value["state"], "ready");
    assert!(
        calls(root.path())
            .iter()
            .any(|c| c.contains("/workspace/firecrab/project/AGENTS.md.firecrab-tmp"))
    );
}

#[test]
fn apply_add_appends_and_creates_missing_file() {
    let (root, runtime) = ready();
    replies(root.path(), json!({READ: {"out": "missing"}}));
    runtime
        .apply_rules(Apply {
            mode: Mode::Add,
            block: None,
            text: "- b".into(),
            digest: sha(""),
        })
        .unwrap();
    assert_eq!(stdin(root.path(), 0), "- b\n");
}

#[test]
fn wiring_adds_claude_import_and_gemini_context_once() {
    let (root, runtime) = ready();
    shell_replies(
        root.path(),
        json!({
            CLAUDE: {"out": "missing"},
            GEMINI: {"out": STANDARD.encode(r#"{"theme":"x"}"#)},
        }),
    );
    runtime.ensure_rule_wiring().unwrap();
    let claude = stdin(root.path(), 0);
    assert!(
        claude.contains("<!-- firecrab:agents-md -->\n@/workspace/firecrab/project/AGENTS.md\n")
    );
    let settings: Value = serde_json::from_str(&stdin(root.path(), 1)).unwrap();
    assert_eq!(settings["theme"], "x");
    assert_eq!(
        settings["context"]["fileName"],
        json!(["AGENTS.md", "GEMINI.md"])
    );
    let (root, runtime) = ready();
    replies(root.path(), json!({}));
    runtime.ensure_rule_wiring().unwrap();
    assert!(!root.path().join("remote-stdin-0.bin").exists());
}

#[test]
fn broken_gemini_settings_do_not_block_an_approved_rule() {
    let (root, runtime) = ready();
    replies(
        root.path(),
        json!({GEMINI: {"out": STANDARD.encode("{not json")}}),
    );
    let value = runtime
        .apply_rules(Apply {
            mode: Mode::Add,
            block: None,
            text: "- c".into(),
            digest: sha(DOC),
        })
        .unwrap();
    assert!(value["warning"].as_str().unwrap().contains("settings.json"));
    assert_eq!(stdin(root.path(), 0), format!("{DOC}\n- c\n"));
}

#[test]
fn successful_preparation_wires_rules_for_claude_and_gemini() {
    let (root, runtime) = ready();
    shell_replies(
        root.path(),
        json!({
            "timeout 900": {"out": "FIRECRAB_STEP=4\n", "status": 0},
            CLAUDE: {"out": "missing"},
            GEMINI: {"out": "missing"},
        }),
    );
    runtime.prepare("claude").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let log = loop {
        let log = fs::read_to_string(runtime.home.join("preparation.log")).unwrap_or_default();
        if log.contains("FIRECRAB_RULES=") || Instant::now() > deadline {
            break log;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(log.contains("FIRECRAB_RULES=ready"), "{log}");
    assert!(
        calls(root.path())
            .iter()
            .any(|c| c.contains("/root/.claude/CLAUDE.md.firecrab-tmp"))
    );
}
