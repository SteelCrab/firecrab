use super::super::tests::{shell_replies, workspace_fixture};
use super::*;

/// A Codex 0.166 rollout line, with its fields in the order Codex writes them.
fn codex_item(kind: &str, text: &str) -> String {
    let part = if kind == "UserMessage" {
        "text"
    } else {
        "Text"
    };
    format!(
        r#"{{"timestamp":"2026-11-03T08:25:28.942Z","ordinal":9,"type":"event_msg","payload":{{"type":"item_completed","thread_id":"t","turn_id":"u","item":{{"type":"{kind}","id":"i","content":[{{"type":"{part}","text":{}}}]}}}},"started_at_ms":1,"completed_at_ms":2}}"#,
        serde_json::to_string(text).unwrap()
    )
}
fn claude(kind: &str, content: Value, extra: Value) -> String {
    let mut record =
        json!({"type":kind,"isSidechain":false,"message":{"role":kind,"content":content}});
    record
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    record.to_string()
}

#[test]
fn codex_history_pairs_the_latest_input_with_the_reply_after_it() {
    let first = codex_item("UserMessage", "Fix the login bug");
    let reply = codex_item("AgentMessage", "Fixed the session check.");
    let typed = codex_item("UserMessage", "Now add a test typed in the terminal");
    assert_eq!(
        latest(&[&first, &reply]),
        (
            Some("Fix the login bug".into()),
            Some("Fixed the session check.".into())
        )
    );
    // A new input has no reply yet: the older reply belongs to the older input.
    assert_eq!(
        latest(&[&first, &reply, &typed]),
        (Some("Now add a test typed in the terminal".into()), None)
    );
    let progress = codex_item("AgentMessage", "Adding the test now.");
    assert_eq!(
        latest(&[&first, &reply, &typed, &progress]).1,
        Some("Adding the test now.".into())
    );
    // Codex threads in the legacy history mode store the same messages as events.
    let legacy_input = json!({"type":"event_msg","payload":{"type":"user_message","message":"Legacy input","images":[]}}).to_string();
    let legacy_reply =
        json!({"type":"event_msg","payload":{"type":"agent_message","message":"Legacy reply"}})
            .to_string();
    assert_eq!(
        latest(&[&legacy_input, &legacy_reply]),
        (Some("Legacy input".into()), Some("Legacy reply".into()))
    );
}

#[test]
fn tool_output_and_injected_context_never_become_a_preview() {
    let secret = "TOKEN=do-not-show";
    let command = json!({"type":"event_msg","payload":{"type":"item_completed","item":{
        "type":"CommandExecution","command":"env","aggregated_output":secret}}})
    .to_string();
    let context = json!({"type":"response_item","payload":{"type":"message","role":"user",
        "content":[{"type":"input_text","text":secret}]}})
    .to_string();
    let tool_result = claude(
        "user",
        json!([{"type":"tool_result","tool_use_id":"x","content":secret}]),
        json!({}),
    );
    let tool_use = claude(
        "assistant",
        json!([{"type":"tool_use","name":"Bash","input":{"command":secret}}]),
        json!({}),
    );
    let meta = claude("user", json!(secret), json!({"isMeta":true}));
    let sidechain = claude("user", json!(secret), json!({"isSidechain":true}));
    let notification = claude(
        "user",
        json!(secret),
        json!({"origin":{"kind":"task-notification"}}),
    );
    let injected = claude(
        "user",
        json!(format!(
            "<side-sessions-event>{secret}</side-sessions-event>"
        )),
        json!({"turnOrigin":"sdk"}),
    );
    let stdout = claude(
        "user",
        json!(format!(
            "<local-command-stdout>{secret}</local-command-stdout>"
        )),
        json!({}),
    );
    // Claude Code stores a `!` shell command with its output in one typed record.
    let shell = claude(
        "user",
        json!(format!(
            "<bash-input>env</bash-input><bash-stdout>{secret}</bash-stdout><bash-stderr></bash-stderr>"
        )),
        json!({"origin":{"kind":"human"}}),
    );
    let lines = [
        &command,
        &context,
        &tool_result,
        &tool_use,
        &meta,
        &sidechain,
        &notification,
        &injected,
        &stdout,
        &shell,
    ];
    assert_eq!(latest(&lines.map(String::as_str)), (None, None));
    let reply = claude(
        "assistant",
        json!([{"type":"text","text":"Noted."}]),
        json!({}),
    );
    let (input, _) = latest(&[&shell, &reply]);
    assert_eq!(input, Some("!env".into()));
    // A long shell output line is cut in the guest. Its start must not leak either.
    let cut = format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"user","message":{{"role":"user","content":"<bash-input>env</bash-input><bash-stdout>{secret} {}"#,
        "x".repeat(100)
    );
    assert_eq!(latest(&[&cut, &reply]).0, Some("!env".into()));
    assert_eq!(latest(&[&cut]), (None, None));
}

#[test]
fn claude_transcripts_show_typed_prompts_commands_and_text_replies() {
    let prompt = claude(
        "user",
        json!("Review the API changes"),
        json!({"origin":{"kind":"human"}}),
    );
    let thinking = claude(
        "assistant",
        json!([{"type":"thinking","thinking":"private"}]),
        json!({}),
    );
    let reply = claude(
        "assistant",
        json!([{"type":"text","text":"Two handlers miss input checks."}]),
        json!({}),
    );
    assert_eq!(
        latest(&[&prompt, &thinking, &reply]),
        (
            Some("Review the API changes".into()),
            Some("Two handlers miss input checks.".into())
        )
    );
    let parts = claude(
        "user",
        json!([{"type":"image","source":{"type":"base64","data":"AAAA"}},{"type":"text","text":"What is in this screenshot?"}]),
        json!({}),
    );
    assert_eq!(
        latest(&[&parts]).0,
        Some("What is in this screenshot?".into())
    );
    // Context the CLI adds to a typed prompt is not part of the input.
    let reminded = claude(
        "user",
        json!([{"type":"text","text":"<system-reminder>\nLinked sessions now\n</system-reminder>"},{"type":"text","text":"Ship it"}]),
        json!({"origin":{"kind":"human"}}),
    );
    assert_eq!(latest(&[&reminded]).0, Some("Ship it".into()));
    // A local slash command gets no reply, so the last prompt and its reply stay.
    let local = claude(
        "user",
        json!(
            "<command-name>/model</command-name>\n<command-message>model</command-message>\n<command-args>opus</command-args>"
        ),
        json!({}),
    );
    assert_eq!(
        latest(&[&prompt, &reply, &local]),
        (
            Some("Review the API changes".into()),
            Some("Two handlers miss input checks.".into())
        )
    );
    // A slash command that runs a prompt becomes the input once the CLI replies to it.
    let review = claude(
        "user",
        json!("<command-message>review</command-message>\n<command-name>/review</command-name>"),
        json!({}),
    );
    let answer = claude(
        "assistant",
        json!([{"type":"text","text":"Reviewed three files."}]),
        json!({}),
    );
    assert_eq!(
        latest(&[&prompt, &reply, &review, &answer]),
        (Some("/review".into()), Some("Reviewed three files.".into()))
    );
}

#[test]
fn a_compaction_summary_is_not_the_latest_input() {
    let prompt = claude(
        "user",
        json!("Review the API changes"),
        json!({"origin":{"kind":"human"}}),
    );
    let reply = claude(
        "assistant",
        json!([{"type":"text","text":"Two handlers miss input checks."}]),
        json!({}),
    );
    // Claude Code stores the summary of a compacted conversation as a user record.
    let summary = claude(
        "user",
        json!("This session continues an earlier conversation. Summary of invented work."),
        json!({"isCompactSummary":true,"isVisibleInTranscriptOnly":true}),
    );
    assert_eq!(
        latest(&[&prompt, &reply, &summary]),
        (
            Some("Review the API changes".into()),
            Some("Two handlers miss input checks.".into())
        )
    );
}

#[test]
fn previews_drop_terminal_controls_and_stay_short() {
    let noisy = codex_item(
        "AgentMessage",
        "\u{1b}[31mDone\u{1b}[0m\r\n\n\t\u{1b}]0;title\u{7}tests\u{7} pass",
    );
    assert_eq!(latest(&[&noisy]).1, Some("Done tests pass".into()));
    let long = codex_item("UserMessage", &"가".repeat(2000));
    let input = latest(&[&long]).0.unwrap();
    assert_eq!(input.chars().count(), PREVIEW_CHARS + 1);
    assert!(input.ends_with('…'));
}

#[test]
fn a_line_cut_at_the_guest_limit_keeps_the_start_of_its_text() {
    let text = format!("Start of a long \\\"reply\\\" {}", "x".repeat(400));
    let line = codex_item("AgentMessage", &text);
    for cut in [line.len() - 40, line.find("reply").unwrap() + 2] {
        let (_, response) = latest(&[&line[..cut]]);
        assert!(
            response
                .as_deref()
                .is_some_and(|r| r.starts_with("Start of a long \\\"re")),
            "{response:?}"
        );
    }
    // A cut may split an escape sequence, including a UTF-16 surrogate pair.
    let escape = |hex: &str| format!("{}u{hex}", char::from(b'\\'));
    let escaped = format!(
        r#"{{"type":"event_msg","payload":{{"type":"agent_message","message":"Quote \" then {}{} and {} end"}}}}"#,
        escape("d83d"),
        escape("de00"),
        escape("00e9")
    );
    assert!(escaped.is_ascii());
    let start = escaped.find("Quote").unwrap();
    for end in start + 5..escaped.len() {
        let (_, response) = latest(&[&escaped[..end]]);
        let response = response.unwrap_or_default();
        assert!(
            "Quote \" then \u{1F600} and \u{e9} end".starts_with(&response),
            "{end}: {response:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn previews_read_open_cli_sessions_in_one_read_only_guest_call() {
    let (root, runtime, codex) = workspace_fixture();
    let session = |name: &str, kind: &str| Agent {
        id: Uuid::new_v4(),
        name: name.into(),
        kind: kind.into(),
        pane_id: format!("w1:{name}"),
        ..codex.clone()
    };
    let claude_session = session("claude", "claude");
    let closed = Agent {
        closed: true,
        ..session("closed", "codex")
    };
    let login = Agent {
        role: "authentication".into(),
        ..session("login", "claude")
    };
    let other = session("other", "copilot");
    runtime
        .save(&[codex.clone(), claude_session.clone(), closed, login, other])
        .unwrap();
    let output = format!(
        "FIRECRAB_PREVIEW {}\n{}\n{}\nFIRECRAB_PREVIEW {}\n{}\n",
        codex.id,
        codex_item("UserMessage", "Typed in the terminal"),
        codex_item("AgentMessage", "Answered in the terminal"),
        claude_session.id,
        claude("user", json!("Waiting on Claude"), json!({})),
    );
    shell_replies(root.path(), json!({"FIRECRAB_PREVIEW": {"out": output}}));
    let previews = runtime.previews().unwrap();
    assert_eq!(
        previews,
        json!({"previews":[
            {"id":codex.id,"input":"Typed in the terminal","response":"Answered in the terminal"},
            {"id":claude_session.id,"input":"Waiting on Claude","response":null},
        ]})
    );
    let calls: Vec<String> = fs::read_to_string(root.path().join("remote-calls.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(calls.len(), 1);
    let script = &calls[0];
    assert!(script.contains(&format!("preview '{}' 'codex' 'w1:p1'", codex.id)));
    assert!(script.contains(&format!(
        "preview '{}' 'claude' 'w1:claude'",
        claude_session.id
    )));
    assert!(
        !script.contains("w1:closed")
            && !script.contains("w1:login")
            && !script.contains("w1:other")
    );
    assert!(script.contains("herdr --session 'guest-test' pane process-info"));
    for write in ["send-text", "send-keys", "'agent' 'prompt'", "rm "] {
        assert!(!script.contains(write), "{write}");
    }
}

/// When the fake MicroVM booted, in seconds since the Unix epoch.
#[cfg(unix)]
const BOOT: u64 = 1_793_000_000;
#[cfg(unix)]
const PROJECT: &str = "/workspace/firecrab/project";
/// The latest input and the reply to it.
#[cfg(unix)]
type Latest = (Option<String>, Option<String>);

/// A MicroVM on this host: a fake `/proc` and home that the guest script reads like the guest's.
/// Times are milliseconds after boot.
#[cfg(unix)]
struct Vm {
    root: tempfile::TempDir,
}
#[cfg(unix)]
impl Vm {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("proc")).unwrap();
        Self { root }
    }
    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }
    /// A process that keeps `open` files open.
    fn process(&self, pid: u32, open: &[&PathBuf]) {
        let dir = self.root.path().join(format!("proc/{pid}/fd"));
        fs::create_dir_all(&dir).unwrap();
        for (n, path) in open.iter().enumerate() {
            std::os::unix::fs::symlink(path, dir.join(format!("{}", n + 3))).unwrap();
        }
    }
    /// A Codex rollout of a thread created at `created`. Its id is a UUIDv7, as in Codex.
    fn rollout(&self, created: u64, cwd: &str, subagent: bool, lines: &[String]) -> PathBuf {
        let at = format!("{:012x}", BOOT * 1000 + created);
        let id = format!("{}-{}-7000-8000-{created:012x}", &at[..8], &at[8..]);
        let parent = if subagent {
            r#""parent_thread_id":"0","#
        } else {
            ""
        };
        let meta = format!(
            r#"{{"timestamp":"2026-11-03T10:00:00.000Z","ordinal":0,"type":"session_meta","payload":{{"session_id":"{id}","id":"{id}",{parent}"timestamp":"2026-11-03T10:00:00.000Z","cwd":"{cwd}","originator":"codex-tui","cli_version":"0.166.0","source":"cli"}}}}"#
        );
        let path = self.home().join(format!(
            ".codex/sessions/2026/11/03/rollout-2026-11-03T10-00-00-{id}.jsonl"
        ));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("{meta}\n{}\n", lines.join("\n"))).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(
                std::time::UNIX_EPOCH + Duration::from_millis(BOOT * 1000 + created + 1000),
            )
            .unwrap();
        path
    }
    /// Run the guest script for `(agent id, kind, pane processes)` and parse each latest message.
    fn latest(&self, panes: &[(&str, &str, &[u32])]) -> (String, Vec<Latest>) {
        let mut processes = String::from("processes() {\n  case $1 in\n");
        let mut calls = String::new();
        for (id, kind, pids) in panes {
            let found = pids
                .iter()
                .map(|pid| format!(r#"{{"argv":["{kind}"],"name":"{kind}","pid":{pid}}}"#))
                .collect::<Vec<_>>()
                .join(",");
            processes.push_str(&format!(
                "    pane-{id}) printf '%s\\n' '{{\"id\":\"cli:pane:process_info\",\"result\":{{\"process_info\":{{\"foreground_processes\":[{found}],\"pane_id\":\"pane-{id}\",\"shell_pid\":1}},\"type\":\"pane_process_info\"}}}}' ;;\n"
            ));
            calls.push_str(&format!("preview '{id}' '{kind}' 'pane-{id}'\n"));
        }
        processes.push_str("  esac\n}\n");
        let script = format!(
            "proc={}\n{processes}{}\n{calls}",
            guest::quote(self.root.path().join("proc").to_str().unwrap()),
            include_str!("../preview.sh")
        );
        let output = Command::new("sh")
            .args(["-c", &script])
            .env("HOME", self.home())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let mut sections: Vec<(&str, Vec<&str>)> = Vec::new();
        for line in text.lines() {
            match line.strip_prefix(SECTION) {
                Some(id) => sections.push((id, Vec::new())),
                None => sections.last_mut().unwrap().1.push(line),
            }
        }
        let latest = panes
            .iter()
            .map(|(id, _, _)| {
                let (_, lines) = sections.iter().find(|(section, _)| section == id).unwrap();
                latest(lines)
            })
            .collect();
        (text, latest)
    }
}
#[cfg(unix)]
fn turn(input: &str, reply: &str) -> Vec<String> {
    vec![
        codex_item("UserMessage", input),
        codex_item("AgentMessage", reply),
    ]
}
#[cfg(unix)]
fn pair(input: &str, reply: &str) -> Latest {
    (Some(input.into()), Some(reply.into()))
}

#[cfg(unix)]
#[test]
fn a_codex_process_that_keeps_its_rollouts_open_is_matched_to_them() {
    let vm = Vm::new();
    // Without the shared daemon, Codex keeps its rollouts open itself. `/new` opens a newer one.
    let older = vm.rollout(61_000, PROJECT, false, &turn("Before /new", "Old reply"));
    let newer = vm.rollout(90_000, PROJECT, false, &turn("After /new", "New reply"));
    let helper = vm.rollout(
        95_000,
        PROJECT,
        true,
        &turn("Subagent task", "Subagent reply"),
    );
    let shared = vm.rollout(62_000, PROJECT, false, &turn("Shared", "Shared reply"));
    vm.process(200, &[&older, &newer, &helper]);
    vm.process(300, &[&shared]);
    let (_, latest) = vm.latest(&[("a", "codex", &[200])]);
    assert_eq!(latest, [pair("After /new", "New reply")]);
}

#[cfg(unix)]
#[test]
fn codex_cards_stay_empty_when_the_shared_daemon_holds_the_threads() {
    let vm = Vm::new();
    // Codex 0.166 runs its threads on a shared daemon by default. Nothing there ties a thread
    // to a pane, so these cards stay empty instead of risking another session's messages.
    let thread = |created, input: &str| {
        vm.rollout(
            created,
            PROJECT,
            false,
            &turn(input, &format!("Reply to {input}")),
        )
    };
    let open = [
        // Terminal A starts at 60 s and starts its thread at once.
        thread(61_000, "First thread of A"),
        // Terminal B runs `codex resume` at 90 s. Its thread is older than both terminals.
        thread(5_000, "Resumed thread of B"),
        // Terminal A runs /new at 100 s.
        thread(100_000, "New thread of A"),
        // Terminal C starts at 120 s and starts its thread at once.
        thread(121_000, "First thread of C"),
    ];
    vm.process(300, &open.iter().collect::<Vec<_>>());
    vm.process(200, &[]);
    vm.process(210, &[]);
    vm.process(230, &[]);
    // A terminal whose own Codex keeps its rollout open gets its exact preview.
    let own = thread(131_000, "Own rollout");
    vm.process(220, &[&own]);
    let (output, latest) = vm.latest(&[
        ("a", "codex", &[200]),
        ("b", "codex", &[210]),
        ("c", "codex", &[230]),
        ("d", "codex", &[220]),
    ]);
    assert_eq!(
        latest,
        [
            (None, None),
            (None, None),
            (None, None),
            pair("Own rollout", "Reply to Own rollout"),
        ]
    );
    for text in [
        "First thread of A",
        "Resumed thread of B",
        "New thread of A",
        "First thread of C",
    ] {
        assert!(!output.contains(text), "{text}");
    }
}

#[cfg(unix)]
#[test]
fn claude_sessions_are_found_through_the_session_registry() {
    let vm = Vm::new();
    let secret = "TOKEN=do-not-show";
    // Claude Code registers each running process under ~/.claude/sessions/<pid>.json.
    for (pid, session, prompt) in [
        (
            400,
            "6f1c1d2e-1111-4222-8333-944455556666",
            "Review the API changes",
        ),
        (
            410,
            "7a2d2e3f-2222-4333-9444-a55566667777",
            "Another terminal",
        ),
    ] {
        let registry = vm.home().join(format!(".claude/sessions/{pid}.json"));
        fs::create_dir_all(registry.parent().unwrap()).unwrap();
        fs::write(
            &registry,
            format!(
                r#"{{"pid":{pid},"sessionId":"{session}","cwd":"{PROJECT}","kind":"interactive"}}"#
            ),
        )
        .unwrap();
        let transcript = vm.home().join(format!(
            ".claude/projects/-workspace-firecrab-project/{session}.jsonl"
        ));
        fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        let lines = [
            claude("user", json!(prompt), json!({"origin":{"kind":"human"}})),
            claude(
                "assistant",
                json!([{"type":"tool_use","id":"t","name":"Bash","input":{"command":secret}}]),
                json!({}),
            ),
            claude(
                "user",
                json!([{"tool_use_id":"t","type":"tool_result","content":secret}]),
                json!({}),
            ),
            claude(
                "assistant",
                json!([{"type":"text","text":format!("Answer to {prompt}")}]),
                json!({}),
            ),
        ];
        fs::write(&transcript, lines.join("\n") + "\n").unwrap();
    }
    let (output, latest) = vm.latest(&[("a", "claude", &[400])]);
    assert_eq!(
        latest,
        [pair(
            "Review the API changes",
            "Answer to Review the API changes"
        )]
    );
    assert!(!output.contains(secret) && !output.contains("Another terminal"));
}

#[cfg(unix)]
#[test]
fn a_long_compaction_summary_never_leaves_the_vm() {
    let vm = Vm::new();
    let session = "6f1c1d2e-1111-4222-8333-944455556666";
    fs::create_dir_all(vm.home().join(".claude/sessions")).unwrap();
    fs::write(
        vm.home().join(".claude/sessions/400.json"),
        format!(r#"{{"pid":400,"sessionId":"{session}"}}"#),
    )
    .unwrap();
    // A summary is often longer than the 16 KiB the guest keeps of a line, and its marker
    // comes after the text, so only the guest can still see it.
    let summary = format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"user","message":{{"role":"user","content":"Summary of invented work. {}"}},"isCompactSummary":true,"isVisibleInTranscriptOnly":true,"uuid":"u"}}"#,
        "detail ".repeat(4000)
    );
    let lines = [
        claude("user", json!("Ship it"), json!({"origin":{"kind":"human"}})),
        claude(
            "assistant",
            json!([{"type":"text","text":"Shipped."}]),
            json!({}),
        ),
        summary,
    ];
    let transcript = vm.home().join(format!(
        ".claude/projects/-workspace-firecrab-project/{session}.jsonl"
    ));
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::write(&transcript, lines.join("\n") + "\n").unwrap();
    let (output, latest) = vm.latest(&[("a", "claude", &[400])]);
    assert_eq!(latest, [pair("Ship it", "Shipped.")]);
    assert!(!output.contains("Summary of invented work"));
}

#[cfg(unix)]
#[test]
fn a_tail_that_starts_inside_a_record_leaves_that_record_out() {
    let vm = Vm::new();
    let session = "6f1c1d2e-1111-4222-8333-944455556666";
    fs::create_dir_all(vm.home().join(".claude/sessions")).unwrap();
    fs::write(
        vm.home().join(".claude/sessions/400.json"),
        format!(r#"{{"pid":400,"sessionId":"{session}"}}"#),
    )
    .unwrap();
    // Claude Code writes a record's type after its message. The guest reads only the last
    // 2 MiB, which can start inside a tool call, after its tool_use marker.
    let secret = "TOKEN=do-not-show";
    let tool = format!(
        r#"{{"parentUuid":"p","isSidechain":false,"message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t","name":"Write","input":{{"description":"{}","content":"{secret}"}}}}]}},"type":"assistant","uuid":"u"}}"#,
        "x".repeat(500)
    );
    let prompt = claude("user", json!("Ship it"), json!({"origin":{"kind":"human"}}));
    let reply = claude(
        "assistant",
        json!([{"type":"text","text":"Shipped."}]),
        json!({}),
    );
    let result = |output: &str| {
        claude(
            "user",
            json!([{"tool_use_id":"t","type":"tool_result","content":output}]),
            json!({}),
        )
    };
    // A large tool result fills the rest, so the read starts 100 bytes before the tool input.
    let after_start = tool.len() - tool.find(r#"","content":""#).unwrap() + 100;
    let rest = 2 * 1024 * 1024 - after_start - 1 - prompt.len() - 1 - reply.len() - 1;
    let filler = result(&"z".repeat(rest - 1 - result("").len()));
    let text = format!("{tool}\n{filler}\n{prompt}\n{reply}\n");
    assert_eq!(
        text.len() - text.find(r#"","content":""#).unwrap() + 100,
        2 * 1024 * 1024
    );
    let transcript = vm.home().join(format!(
        ".claude/projects/-workspace-firecrab-project/{session}.jsonl"
    ));
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::write(&transcript, text).unwrap();
    let (output, latest) = vm.latest(&[("a", "claude", &[400])]);
    assert_eq!(latest, [pair("Ship it", "Shipped.")]);
    assert!(!output.contains(secret), "{output}");
}

#[cfg(unix)]
#[test]
fn the_latest_input_stays_through_a_long_run_of_replies() {
    let vm = Vm::new();
    let session = "6f1c1d2e-1111-4222-8333-944455556666";
    fs::create_dir_all(vm.home().join(".claude/sessions")).unwrap();
    fs::write(
        vm.home().join(".claude/sessions/400.json"),
        format!(r#"{{"pid":400,"sessionId":"{session}"}}"#),
    )
    .unwrap();
    // Agents report progress as separate replies, often more than ten in one turn.
    let mut lines = vec![claude(
        "user",
        json!("Refactor the parser"),
        json!({"origin":{"kind":"human"}}),
    )];
    for step in 1..=20 {
        lines.push(claude(
            "assistant",
            json!([{"type":"text","text":format!("Step {step} done.")}]),
            json!({}),
        ));
    }
    let transcript = vm.home().join(format!(
        ".claude/projects/-workspace-firecrab-project/{session}.jsonl"
    ));
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::write(&transcript, lines.join("\n") + "\n").unwrap();
    let (_, latest) = vm.latest(&[("a", "claude", &[400])]);
    assert_eq!(latest, [pair("Refactor the parser", "Step 20 done.")]);
}
