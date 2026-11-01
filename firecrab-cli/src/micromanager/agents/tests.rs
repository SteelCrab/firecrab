#[cfg(unix)]
use super::*;
#[cfg(unix)]
fn fixture() -> (tempfile::TempDir, Runtime) {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let script = bin.join("herdr");
    fs::write(&script,r##"#!/usr/bin/env python3
import json,sys,pathlib
p=pathlib.Path(__file__).parent.parent
args=sys.argv[3:]
with (p/'calls.jsonl').open('a') as f:f.write(json.dumps(args)+'\n')
pane={'pane_id':'w1:p1','workspace_id':'w1','terminal_id':(p/'generation').read_text() if (p/'generation').exists() else 'term-original','agent_status':'idle','agent':'codex'}
if args[:2]==['workspace','create']: result={'workspace':{'workspace_id':'w1'},'root_pane':pane}
elif args[:2]==['pane','list']:result={'panes':[pane]}
elif args[:2]==['pane','get']:result={'pane':pane}
elif args[:2]==['pane','read']:print('retained terminal output',end='');sys.exit(0)
elif args[:2]==['status','server']:result={'running':True}
else:result={}
print(json.dumps({'result':result}))
"##).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(bin.join("codex"), "").unwrap();
    let mut runtime = Runtime::new(root.path());
    runtime.program = Some(script);
    runtime.bin_dir = Some(bin);
    (root, runtime)
}
#[cfg(unix)]
fn input(root: &Path, id: Uuid) -> Create {
    Create {
        id,
        name: "Reviewer".into(),
        role: "Review".into(),
        kind: "codex".into(),
        cwd: root.to_string_lossy().into(),
        model: "test-model".into(),
        prompt: String::new(),
        source_digest: String::new(),
    }
}
#[cfg(unix)]
#[test]
fn creates_once_and_keeps_sessions_after_runtime_client_reconnect() {
    let (root, runtime) = fixture();
    let id = Uuid::new_v4();
    runtime.create(input(root.path(), id)).unwrap();
    runtime.create(input(root.path(), id)).unwrap();
    let mut changed = input(root.path(), id);
    changed.role = "Different role".into();
    assert!(runtime.create(changed).is_err());
    let calls = fs::read_to_string(root.path().join("calls.jsonl")).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|s| s.contains("\"workspace\", \"create\""))
            .count(),
        1
    );
    assert_eq!(
        calls
            .lines()
            .filter(|s| s.contains("\"agent\", \"start\""))
            .count(),
        1
    );
    assert!(calls.contains("test-model"));
    let mut reconnected = Runtime::new(root.path());
    reconnected.program = runtime.program;
    reconnected.bin_dir = runtime.bin_dir;
    assert_eq!(
        reconnected.status().unwrap()["agents"][0]["id"],
        id.to_string()
    );
    assert_eq!(
        reconnected.inspect(id).unwrap()["text"],
        "retained terminal output"
    );
}
#[cfg(unix)]
#[test]
fn literal_input_is_not_a_shell_command_and_closed_output_is_retained() {
    let (root, runtime) = fixture();
    let id = Uuid::new_v4();
    runtime.create(input(root.path(), id)).unwrap();
    let text = "$(touch /private/tmp/should-never-exist) ; ' \" input";
    runtime
        .input(
            id,
            Input {
                text: Some(text.into()),
                key: None,
            },
        )
        .unwrap();
    let calls: Vec<Value> = fs::read_to_string(root.path().join("calls.jsonl"))
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    let send = calls
        .iter()
        .find(|v| v[0] == "pane" && v[1] == "send-text")
        .unwrap();
    assert_eq!(send[3], text);
    runtime.close(id).unwrap();
    assert_eq!(
        runtime.inspect(id).unwrap()["text"],
        "retained terminal output"
    );
    assert!(
        runtime
            .input(
                id,
                Input {
                    text: None,
                    key: Some("enter".into())
                }
            )
            .is_err()
    );
}
#[cfg(unix)]
#[test]
fn never_sends_input_or_closes_a_recycled_terminal() {
    let (root, runtime) = fixture();
    let id = Uuid::new_v4();
    runtime.create(input(root.path(), id)).unwrap();
    fs::write(root.path().join("generation"), "term-recycled").unwrap();
    assert!(
        runtime
            .input(
                id,
                Input {
                    text: Some("do not send".into()),
                    key: None
                }
            )
            .is_err()
    );
    assert!(runtime.close(id).is_err());
    let calls = fs::read_to_string(root.path().join("calls.jsonl")).unwrap();
    assert!(!calls.contains("send-text"));
    assert!(!calls.contains("\"close\""));
    assert_eq!(
        runtime.status().unwrap()["agents"][0]["state"],
        "disconnected"
    );
}
#[cfg(unix)]
#[test]
fn foreign_sessions_bad_paths_and_unsupported_keys_are_rejected() {
    let (root, runtime) = fixture();
    assert!(runtime.inspect(Uuid::new_v4()).is_err());
    assert!(!root.path().join("calls.jsonl").exists());
    let id = Uuid::new_v4();
    let mut create = input(root.path(), id);
    create.cwd = "relative".into();
    assert!(runtime.create(create).is_err());
    assert!(!root.path().join("calls.jsonl").exists());
    runtime.create(input(root.path(), id)).unwrap();
    assert!(
        runtime
            .input(
                id,
                Input {
                    text: None,
                    key: Some("arbitrary-command".into())
                }
            )
            .is_err()
    );
    let second = Runtime::new(&root.path().join("other"));
    assert_ne!(runtime.session, second.session);
}

#[cfg(unix)]
#[test]
fn a_task_calls_the_cli_only_after_nonempty_work_is_submitted() {
    let (root, runtime) = fixture();
    let task = |input: &str| TaskStart {
        id: Uuid::new_v4(),
        input: input.into(),
        cwd: root.path().to_string_lossy().into(),
        cli: String::new(),
        repository: String::new(),
        branch: String::new(),
        model: String::new(),
    };
    assert!(runtime.start_task(task("   ")).is_err());
    runtime.status().unwrap();
    assert!(!root.path().join("calls.jsonl").exists());
    fs::write(root.path().join("bin/claude"), "").unwrap();
    runtime
        .start_task(task("Review authentication changes"))
        .unwrap();
    let records = runtime.records().unwrap();
    assert_eq!(records[0].name, "Review authentication changes");
    assert_eq!(records[0].kind, "codex");
    assert!(records[0].role.is_empty());
    let calls: Vec<Value> = fs::read_to_string(root.path().join("calls.jsonl"))
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    let prompt = calls
        .iter()
        .find(|v| v[0] == "agent" && v[1] == "prompt")
        .unwrap();
    assert_eq!(prompt[3], "Review authentication changes");
}

#[cfg(unix)]
fn guest_fixture(authenticated: bool) -> (tempfile::TempDir, Runtime) {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let ssh = root.path().join("ssh");
    fs::write(&ssh,r##"#!/usr/bin/env python3
import json,sys,pathlib,shlex
root=pathlib.Path(__file__).parent
script=shlex.split(sys.argv[-1])[-1]
with (root/'remote-calls.jsonl').open('a') as f:f.write(json.dumps(script)+'\n')
args=shlex.split(script)
if script.startswith('if [ -f /root/.local/share/firecrab/vm-repository.json') and (root/'guest-project.json').exists():
 import base64
 print(base64.b64encode((root/'guest-project.json').read_bytes()).decode());sys.exit(0)
if 'root=/root/.local/share/firecrab/github-auth' in script and 'nohup' not in script:
 print((root/'github-status').read_text() if (root/'github-status').exists() else '');sys.exit(0)
if 'herdr' not in args:sys.exit(0)
args=args[args.index('herdr')+3:]
pane={'pane_id':'w1:p1','workspace_id':'w1','terminal_id':'guest-terminal','agent_status':'blocked' if (root/'startup-blocked').exists() else 'idle','agent':'codex'}
if args[:2]==['agent','start'] and (root/'startup-blocked').exists():print(json.dumps({'error':{'message':'agent_not_ready'}}));sys.exit(0)
if args[:2]==['agent','prompt'] and (root/'lost-ack').exists():print(json.dumps({'error':{'message':'connection interrupted'}}));sys.exit(0)
if args[:2]==['status','server']:result={'running':True}
elif args[:2]==['workspace','create']:result={'workspace':{'workspace_id':'w1'},'root_pane':pane}
elif args[:2]==['pane','list']:result={'panes':[pane]}
elif args[:2]==['pane','get']:result={'pane':pane}
elif args[:2]==['pane','read']:print('guest terminal');sys.exit(0)
else:result={}
print(json.dumps({'result':result}))
"##).unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700)).unwrap();
    let local = root.path().join("local-herdr");
    fs::write(
        &local,
        "#!/bin/sh\necho 'A local agent must never be launched' >&2\nexit 99\n",
    )
    .unwrap();
    fs::set_permissions(&local, fs::Permissions::from_mode(0o700)).unwrap();
    let guest = guest::Guest {
        id: Uuid::new_v4(),
        home: root.path().to_path_buf(),
        address: "192.0.2.10".parse().unwrap(),
        key: root.path().join("operator key"),
        known: root.path().join("known hosts"),
        proxy: "unused-test-proxy".into(),
        program: Some(ssh),
        available: ["herdr", "codex", "gh"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        authenticated: if authenticated {
            ["codex", "github"].into_iter().map(str::to_owned).collect()
        } else {
            Default::default()
        },
    };
    let runtime = Runtime {
        home: root.path().join("metadata"),
        program: Some(local),
        session: "guest-test".into(),
        bin_dir: None,
        guest: Some(guest),
    };
    (root, runtime)
}

#[cfg(unix)]
#[test]
fn microvm_tasks_never_use_host_directories_or_implicit_cli_selection() {
    let (root, runtime) = guest_fixture(true);
    let id = Uuid::new_v4();
    let request = |repo: &str, cli: &str| TaskStart {
        id,
        input: "Review the repository $(do-not-execute)".into(),
        cwd: "/missing-host-path".into(),
        repository: repo.into(),
        branch: "main".into(),
        cli: cli.into(),
        model: String::new(),
    };
    assert!(runtime.start_task(request("example/project", "")).is_err());
    assert!(!root.path().join("remote-calls.jsonl").exists());
    runtime
        .start_task(request("example/project", "codex"))
        .unwrap();
    runtime
        .start_task(request("https://github.com/example/project.git", "codex"))
        .unwrap();
    assert!(
        runtime
            .start_task(request("example/another", "codex"))
            .is_err()
    );
    let records = runtime.records().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].cwd, project::WORKSPACE);
    assert!(records[0].role.is_empty());
    let calls = fs::read_to_string(root.path().join("remote-calls.jsonl")).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|l| l.contains("'agent' 'start'"))
            .count(),
        1
    );
    assert!(calls.contains("https://github.com/example/project.git"));
    assert!(calls.contains("Review the repository $(do-not-execute)"));
    assert!(!calls.contains("Role: "));
}

#[cfg(unix)]
#[test]
fn microvm_login_never_submits_the_waiting_task_to_a_model() {
    let (root, runtime) = guest_fixture(false);
    let task = TaskStart {
        id: Uuid::new_v4(),
        input: "Do not run before signing in".into(),
        cwd: String::new(),
        repository: String::new(),
        branch: String::new(),
        cli: "codex".into(),
        model: String::new(),
    };
    assert!(runtime.start_task(task).is_err());
    assert!(!root.path().join("remote-calls.jsonl").exists());
    runtime.login(Uuid::new_v4(), "codex").unwrap();
    let calls = fs::read_to_string(root.path().join("remote-calls.jsonl")).unwrap();
    assert!(calls.contains("codex login --device-auth"));
    assert!(!calls.contains("'agent' 'prompt'"));
    assert!(!calls.contains("'agent' 'start'"));
}

#[cfg(unix)]
#[test]
fn github_addresses_and_remote_arguments_cannot_inject_shell_commands() {
    assert_eq!(
        guest::repository_url("https://github.com/example/project.git").unwrap(),
        Some("https://github.com/example/project.git".into())
    );
    for value in [
        "https://other.invalid/a/b",
        "https://token@github.com/a/b",
        "a/../b",
        "a/b?x=y",
        "a/b\nmalicious",
    ] {
        assert!(guest::repository_url(value).is_err(), "{value}");
    }
    let input = "literal ' $(printf injected) ; command\n";
    let output = Command::new("sh")
        .args([
            "-c",
            &format!("set -- {}; printf '%s' \"$1\"", guest::quote(input)),
        ])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(output.stdout).unwrap(), input);
}

#[cfg(unix)]
#[test]
fn first_run_approval_keeps_the_task_until_the_user_sends_it() {
    let (root, runtime) = guest_fixture(true);
    fs::write(root.path().join("startup-blocked"), "").unwrap();
    let id = Uuid::new_v4();
    let task = TaskStart {
        id,
        input: "Keep this task while the CLI asks about project trust".into(),
        cwd: String::new(),
        repository: String::new(),
        branch: String::new(),
        cli: "codex".into(),
        model: String::new(),
    };
    let pending = runtime.start_task(task).unwrap();
    assert_eq!(pending["hasPendingTask"], true);
    assert!(pending.get("pendingTask").is_none());
    assert!(runtime.deliver_task(id).is_err());
    fs::remove_file(root.path().join("startup-blocked")).unwrap();
    assert_eq!(runtime.deliver_task(id).unwrap()["hasPendingTask"], false);
    runtime.deliver_task(id).unwrap();
    let calls = fs::read_to_string(root.path().join("remote-calls.jsonl")).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|l| l.contains("'agent' 'prompt'"))
            .count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn an_uncertain_prompt_delivery_is_never_automatically_replayed() {
    let (root, runtime) = guest_fixture(true);
    fs::write(root.path().join("lost-ack"), "").unwrap();
    let id = Uuid::new_v4();
    let task = TaskStart {
        id,
        input: "Send only once".into(),
        cwd: String::new(),
        repository: String::new(),
        branch: String::new(),
        cli: "codex".into(),
        model: String::new(),
    };
    let result = runtime.start_task(task).unwrap();
    assert_eq!(result["hasPendingTask"], false);
    assert_eq!(result["taskDeliveryUncertain"], true);
    assert!(runtime.deliver_task(id).is_err());
    let calls = fs::read_to_string(root.path().join("remote-calls.jsonl")).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|l| l.contains("'agent' 'prompt'"))
            .count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn vm_project_is_cloned_once_and_recovered_after_native_metadata_loss() {
    let (root, runtime) = guest_fixture(true);
    let id = Uuid::new_v4();
    let task = |id| TaskStart {
        id,
        input: "Inspect files".into(),
        cwd: String::new(),
        repository: String::new(),
        branch: String::new(),
        cli: "codex".into(),
        model: String::new(),
    };
    let configured = runtime
        .setup_project(project::Setup {
            repository: "example/project".into(),
            branch: "main".into(),
        })
        .unwrap();
    runtime.start_task(task(id)).unwrap();
    runtime.start_task(task(Uuid::new_v4())).unwrap();
    assert!(
        runtime
            .records()
            .unwrap()
            .iter()
            .all(|a| a.cwd == project::WORKSPACE)
    );
    assert!(
        runtime
            .setup_project(project::Setup {
                repository: "example/other".into(),
                branch: String::new()
            })
            .is_err()
    );
    let remote = fs::read_to_string(root.path().join("remote-calls.jsonl")).unwrap();
    assert_eq!(
        remote
            .lines()
            .filter(|s| s.contains(" clone --single-branch"))
            .count(),
        1
    );
    assert!(!remote.contains(" checkout "));
    assert!(!remote.contains(" pull "));
    let manifest = project::VmProject {
        vm_id: runtime.guest.as_ref().unwrap().id,
        repository: Some("https://github.com/example/project.git".into()),
        branch: "main".into(),
        path: project::WORKSPACE.into(),
    };
    fs::write(
        root.path().join("guest-project.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    fs::remove_file(runtime.home.join("vm-repository.json")).unwrap();
    assert_eq!(
        runtime.vm_project().unwrap()["repository"],
        configured["repository"]
    );
    assert_eq!(runtime.vm_project().unwrap()["state"], "ready");
    let mut wrong = manifest;
    wrong.vm_id = Uuid::new_v4();
    fs::write(
        root.path().join("guest-project.json"),
        serde_json::to_vec(&wrong).unwrap(),
    )
    .unwrap();
    fs::remove_file(runtime.home.join("vm-repository.json")).unwrap();
    assert!(runtime.vm_project().is_err());
}

#[cfg(unix)]
#[test]
fn github_auth_reuses_pending_requests_and_never_runs_a_model() {
    let (root, runtime) = guest_fixture(false);
    fs::write(
        root.path().join("github-status"),
        "PENDING\nCODE:AB12-CD34\n",
    )
    .unwrap();
    let first = runtime.connect_github().unwrap();
    let reopened = runtime.connect_github().unwrap();
    assert_eq!(first, reopened);
    assert_eq!(first["userCode"], "AB12-CD34");
    assert!(runtime.records().unwrap().is_empty());
    let remote = fs::read_to_string(root.path().join("remote-calls.jsonl")).unwrap();
    assert!(!remote.contains("nohup"));
    assert!(!remote.contains("'agent' 'start'"));
    let (_, connected) = guest_fixture(true);
    assert_eq!(connected.connect_github().unwrap()["state"], "connected");
}
