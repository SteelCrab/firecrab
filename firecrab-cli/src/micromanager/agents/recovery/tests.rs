use super::super::tests::{guest_fixture, shell_replies};
use super::*;

fn record(name: &str, kind: &str, workspace: &str) -> Agent {
    Agent {
        id: Uuid::new_v4(),
        name: name.into(),
        role: String::new(),
        kind: kind.into(),
        cwd: project::WORKSPACE.into(),
        model: String::new(),
        effort: String::new(),
        workspace_id: workspace.into(),
        pane_id: format!("{workspace}:p1"),
        terminal_id: "guest-terminal".into(),
        closed: false,
        launch_error: None,
        last_output: String::new(),
        request_digest: String::new(),
        pending_task: None,
        conversation_id: None,
        recovery: None,
        chosen: None,
    }
}

/// A VM that was stopped and started: the private Herdr server is down, and
/// its saved layout holds `layout` when it starts again.
fn restarted_vm(records: &[Agent], layout: &[&str]) -> (tempfile::TempDir, Runtime) {
    let (root, runtime) = guest_fixture(true);
    runtime.save(records).unwrap();
    fs::write(root.path().join("server-down"), "").unwrap();
    fs::write(
        root.path().join("guest-layout.json"),
        serde_json::to_vec(layout).unwrap(),
    )
    .unwrap();
    (root, runtime)
}

/// Records after the background relaunch has finished.
fn settled(runtime: &Runtime) -> Vec<Agent> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while runtime
        .records()
        .unwrap()
        .iter()
        .any(|a| a.recovery == Some(Recovery::Pending))
    {
        assert!(Instant::now() < deadline, "CLI relaunch did not finish");
        std::thread::sleep(Duration::from_millis(20));
    }
    // The relaunch saves each result before it releases its lock.
    let lock = runtime.lock_file(RELAUNCH_LOCK).unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    runtime.records().unwrap()
}

fn remote(root: &tempfile::TempDir) -> String {
    fs::read_to_string(root.path().join("remote-calls.jsonl")).unwrap_or_default()
}

fn count(calls: &str, needle: &str) -> usize {
    calls.lines().filter(|l| l.contains(needle)).count()
}

fn agent_start<'a>(calls: &'a str, agent: &Agent) -> &'a str {
    calls
        .lines()
        .find(|l| l.contains("'agent' 'start'") && l.contains(&format!("'{}'", agent.pane_id)))
        .expect("agent start call")
}

#[test]
fn a_restarted_vm_starts_herdr_again_and_reconnects_open_sessions() {
    let mut builder = record("Builder", "codex", "w1");
    builder.model = "gpt-test".into();
    let (root, runtime) = restarted_vm(std::slice::from_ref(&builder), &["w1"]);
    let status = runtime.status().unwrap();
    assert_eq!(status["running"], true);
    assert_ne!(status["agents"][0]["state"], "disconnected");
    let records = settled(&runtime);
    let recovered = &records[0];
    assert_eq!(recovered.terminal_id, "guest-terminal-restored");
    assert_eq!(
        (
            recovered.id,
            recovered.name.as_str(),
            recovered.kind.as_str(),
            recovered.cwd.as_str(),
            recovered.model.as_str(),
            recovered.workspace_id.as_str(),
            recovered.pane_id.as_str()
        ),
        (
            builder.id,
            "Builder",
            "codex",
            project::WORKSPACE,
            "gpt-test",
            "w1",
            "w1:p1"
        )
    );
    let calls = remote(&root);
    assert_eq!(count(&calls, "nohup env HERDR_CONFIG_PATH"), 1);
    // Firecrab, not Herdr, relaunches each CLI, so a pane never gets two.
    assert!(calls.contains("resume_agents_on_restore = false"));
    assert_eq!(count(&calls, "'workspace' 'create'"), 0);
    assert_eq!(count(&calls, "'agent' 'start'"), 1);
    assert!(agent_start(&calls, &builder).contains("'--' '-m' 'gpt-test'"));
    assert_eq!(runtime.status().unwrap()["agents"][0]["state"], "idle");
    runtime
        .input(
            builder.id,
            Input {
                text: Some("continue the work".into()),
                key: None,
            },
        )
        .unwrap();
    runtime
        .terminal_command(
            builder.id,
            &terminal::Attach {
                controller: "controller".into(),
                cols: 80,
                rows: 24,
            },
        )
        .unwrap();
    settled(&runtime);
    let calls = remote(&root);
    assert_eq!(count(&calls, "nohup env HERDR_CONFIG_PATH"), 1);
    assert_eq!(count(&calls, "'agent' 'start'"), 1);
    assert!(calls.contains("'send-text' 'w1:p1' 'continue the work'"));
}

#[test]
fn a_session_missing_from_the_saved_layout_gets_one_new_workspace() {
    let kept = record("Kept", "codex", "w1");
    // Created just before the stop, so Herdr had not saved it yet.
    let lost = record("Lost", "codex", "w7");
    let (root, runtime) = restarted_vm(&[kept.clone(), lost.clone()], &["w1", "w3"]);
    runtime.status().unwrap();
    let records = settled(&runtime);
    assert_eq!(records[0].workspace_id, "w1");
    assert_eq!(
        (
            records[1].workspace_id.as_str(),
            records[1].pane_id.as_str(),
            records[1].terminal_id.as_str()
        ),
        ("w4", "w4:p1", "guest-terminal-restored")
    );
    runtime.status().unwrap();
    settled(&runtime);
    let calls = remote(&root);
    assert_eq!(count(&calls, "'workspace' 'create'"), 1);
    assert!(calls.contains(&format!(
        "'workspace' 'create' '--cwd' '{}' '--label' 'Lost' '--no-focus'",
        project::WORKSPACE
    )));
    assert_eq!(count(&calls, "'agent' 'start'"), 2);
    assert!(agent_start(&calls, &records[1]).contains("'--pane' 'w4:p1'"));
}

#[test]
fn closed_and_sign_in_sessions_are_never_relaunched() {
    let open = record("Open", "codex", "w1");
    let mut closed = record("Closed", "codex", "w2");
    closed.closed = true;
    let mut login = record("Codex sign in", "codex", "w3");
    login.role = "authentication".into();
    let (root, runtime) = restarted_vm(&[open, closed, login], &["w1", "w2", "w3"]);
    runtime.status().unwrap();
    let records = settled(&runtime);
    assert_eq!(records[0].terminal_id, "guest-terminal-restored");
    for skipped in &records[1..] {
        assert_eq!(skipped.terminal_id, "guest-terminal");
        assert_eq!(skipped.recovery, None);
    }
    let calls = remote(&root);
    assert_eq!(count(&calls, "'agent' 'start'"), 1);
    assert_eq!(count(&calls, "'pane' 'run'"), 0);
    assert!(!calls.contains("login"));
}

#[test]
fn a_waiting_task_is_never_delivered_again_after_recovery() {
    let mut waiting = record("Waiting", "codex", "w1");
    waiting.pending_task = Some(PendingTask {
        text: "Not sent yet".into(),
        uncertain: false,
    });
    let mut uncertain = record("Uncertain", "codex", "w2");
    uncertain.pending_task = Some(PendingTask {
        text: "Maybe sent".into(),
        uncertain: true,
    });
    let (root, runtime) = restarted_vm(&[waiting.clone(), uncertain.clone()], &["w1", "w2"]);
    runtime.status().unwrap();
    settled(&runtime);
    assert_eq!(count(&remote(&root), "'agent' 'prompt'"), 0);
    let status = runtime.status().unwrap();
    assert_eq!(status["agents"][0]["hasPendingTask"], true);
    assert_eq!(status["agents"][1]["taskDeliveryUncertain"], true);
    assert!(runtime.deliver_task(uncertain.id).is_err());
    runtime.deliver_task(waiting.id).unwrap();
    runtime.deliver_task(waiting.id).unwrap();
    let calls = remote(&root);
    assert_eq!(count(&calls, "'agent' 'prompt'"), 1);
    assert!(!calls.contains("Maybe sent"));
}

#[test]
fn concurrent_refreshes_restart_and_relaunch_only_once() {
    let first = record("First", "codex", "w1");
    let missing = record("Missing", "codex", "w5");
    let (root, runtime) = restarted_vm(&[first, missing], &["w1"]);
    let refreshes = (0..4)
        .map(|_| {
            let runtime = runtime.clone();
            std::thread::spawn(move || runtime.status().unwrap())
        })
        .collect::<Vec<_>>();
    for refresh in refreshes {
        refresh.join().unwrap();
    }
    runtime.status().unwrap();
    let records = settled(&runtime);
    assert!(records.iter().all(|a| a.recovery == Some(Recovery::Fresh)));
    let calls = remote(&root);
    assert_eq!(count(&calls, "nohup env HERDR_CONFIG_PATH"), 1);
    assert_eq!(count(&calls, "'workspace' 'create'"), 1);
    assert_eq!(count(&calls, "'agent' 'start'"), 2);
}

#[test]
fn claude_continues_only_the_conversation_saved_under_its_own_id() {
    let conversation = Uuid::new_v4();
    let mut claude = record("Claude", "claude", "w1");
    claude.model = "sonnet".into();
    claude.conversation_id = Some(conversation);
    let (root, runtime) = restarted_vm(std::slice::from_ref(&claude), &["w1"]);
    shell_replies(
        root.path(),
        json!({format!("/{conversation}.jsonl"): {"out": "found"}}),
    );
    runtime.status().unwrap();
    let records = settled(&runtime);
    assert_eq!(records[0].recovery, Some(Recovery::Resumed));
    assert_eq!(records[0].conversation_id, Some(conversation));
    let calls = remote(&root);
    assert!(
        agent_start(&calls, &claude).contains(&format!(
            "'--kind' 'claude' '--pane' 'w1:p1' '--timeout' '30000' '--' '--resume' '{conversation}' '--model' 'sonnet'"
        )),
        "{calls}"
    );
    assert_eq!(
        runtime.status().unwrap()["agents"][0]["recovery"],
        "resumed"
    );
    assert!(
        runtime.status().unwrap()["agents"][0]
            .get("conversationId")
            .is_none()
    );
}

#[test]
fn an_unknown_conversation_starts_fresh_in_the_same_folder() {
    // Created before Firecrab kept Claude conversation ids.
    let old = record("Old Claude", "claude", "w1");
    // Claude Code saved no messages under this id.
    let mut empty = record("Empty Claude", "claude", "w2");
    let unused = Uuid::new_v4();
    empty.conversation_id = Some(unused);
    // Codex cannot be started with a known conversation id.
    let mut codex = record("Codex", "codex", "w3");
    codex.model = "gpt-test".into();
    let (root, runtime) = restarted_vm(
        &[old.clone(), empty.clone(), codex.clone()],
        &["w1", "w2", "w3"],
    );
    runtime.status().unwrap();
    let records = settled(&runtime);
    assert!(records.iter().all(|a| a.recovery == Some(Recovery::Fresh)));
    assert!(records.iter().all(|a| a.cwd == project::WORKSPACE));
    let calls = remote(&root);
    assert!(!calls.contains("--resume"));
    assert!(!calls.contains("'resume'"));
    for (before, after) in [(&old, &records[0]), (&empty, &records[1])] {
        let id = after.conversation_id.expect("new Claude conversation id");
        assert_ne!(Some(id), before.conversation_id);
        assert_ne!(id, unused);
        assert!(agent_start(&calls, before).contains(&format!("'--' '--session-id' '{id}'")));
    }
    assert_eq!(records[2].conversation_id, None);
    assert!(agent_start(&calls, &codex).ends_with("'--' '-m' 'gpt-test' '--no-daemon'\""));
    assert_eq!(runtime.status().unwrap()["agents"][2]["recovery"], "fresh");
}

#[test]
fn a_new_claude_session_keeps_its_conversation_id_for_a_later_resume() {
    let (root, mut runtime) = guest_fixture(true);
    runtime
        .guest
        .as_mut()
        .unwrap()
        .available
        .insert("claude".into());
    let id = Uuid::new_v4();
    runtime
        .create(Create {
            id,
            name: "Claude review".into(),
            role: String::new(),
            kind: "claude".into(),
            cwd: project::WORKSPACE.into(),
            model: "opus".into(),
            effort: String::new(),
            prompt: String::new(),
            source_digest: String::new(),
        })
        .unwrap();
    let agent = runtime.agent(id).unwrap();
    let conversation = agent.conversation_id.expect("Claude conversation id");
    assert_ne!(conversation, id);
    assert!(agent_start(&remote(&root), &agent).ends_with(&format!(
        "'--' '--session-id' '{conversation}' '--model' 'opus'\""
    )));
    assert!(
        cli_args(&agent, Some(&Saved::default()))
            .starts_with(&["--resume".into(), conversation.to_string()])
    );
}

#[test]
fn typed_input_waits_until_the_restored_cli_runs() {
    let mut restoring = record("Restoring", "codex", "w1");
    restoring.recovery = Some(Recovery::Pending);
    let (root, runtime) = guest_fixture(true);
    runtime.save(std::slice::from_ref(&restoring)).unwrap();
    let text = Input {
        text: Some("ls -la".into()),
        key: None,
    };
    assert!(runtime.input(restoring.id, text).is_err());
    assert!(!remote(&root).contains("send-text"));
}

#[test]
fn only_a_transcript_with_messages_under_the_exact_id_is_resumed() {
    let home = tempfile::tempdir().unwrap();
    let project = home.path().join("projects/-workspace-firecrab-project");
    fs::create_dir_all(&project).unwrap();
    let saved = Uuid::new_v4();
    let titled = Uuid::new_v4();
    fs::write(
        project.join(format!("{saved}.jsonl")),
        "{\"type\":\"summary\"}\n{\"parentUuid\":null,\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
    )
    .unwrap();
    // Claude Code can keep only metadata under an id. `--resume` cannot open it.
    fs::write(
        project.join(format!("{titled}.jsonl")),
        "{\"type\":\"agent-name\"}\n{\"type\":\"ai-title\"}\n",
    )
    .unwrap();
    let found = |id: Uuid| {
        let output = Command::new("sh")
            .env("CLAUDE_CONFIG_DIR", home.path())
            .args(["-c", &transcript_check(id)])
            .output()
            .unwrap();
        assert!(output.status.success());
        output.stdout == b"found"
    };
    assert!(found(saved));
    assert!(!found(titled));
    assert!(!found(Uuid::new_v4()));
}

#[test]
fn a_new_task_during_a_relaunch_keeps_the_relaunch_result() {
    let mut earlier = record("Earlier Claude", "claude", "w1");
    earlier.conversation_id = Some(Uuid::new_v4());
    let (root, runtime) = restarted_vm(std::slice::from_ref(&earlier), &["w1"]);
    let id = Uuid::new_v4();
    // The new task's CLI is slow to start, so the relaunch finishes first.
    shell_replies(
        root.path(),
        json!({format!("fc-{}", &id.simple().to_string()[..12]): {"delayMs": 2000}}),
    );
    let creating = {
        let runtime = runtime.clone();
        std::thread::spawn(move || {
            runtime.create(Create {
                id,
                name: "New task".into(),
                role: String::new(),
                kind: "codex".into(),
                cwd: project::WORKSPACE.into(),
                model: String::new(),
                effort: String::new(),
                prompt: String::new(),
                source_digest: String::new(),
            })
        })
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while runtime.agent(earlier.id).unwrap().recovery != Some(Recovery::Pending) {
        assert!(
            Instant::now() < deadline,
            "the new task did not restart Herdr"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    runtime.status().unwrap();
    while runtime.agent(earlier.id).unwrap().recovery != Some(Recovery::Fresh) {
        assert!(Instant::now() < deadline, "the relaunch did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
    let relaunched = runtime.agent(earlier.id).unwrap().conversation_id;
    creating.join().unwrap().unwrap();
    runtime.status().unwrap();
    let records = settled(&runtime);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].recovery, Some(Recovery::Fresh));
    assert_eq!(records[0].conversation_id, relaunched);
    assert_eq!(records[1].id, id);
    assert!(records[1].launch_error.is_none());
    let calls = remote(&root);
    assert_eq!(count(&calls, "nohup env HERDR_CONFIG_PATH"), 1);
    let starts = calls
        .lines()
        .filter(|l| l.contains("'agent' 'start'") && l.contains("'--pane' 'w1:p1'"))
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 1, "{calls}");
    assert!(starts[0].contains(&format!("'--session-id' '{}'", relaunched.unwrap())));
}
