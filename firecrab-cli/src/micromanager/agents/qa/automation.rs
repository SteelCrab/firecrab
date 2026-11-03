//! Opt-in QA against explicitly owned resources. Not an automated coverage input.
use super::*;
#[test]
#[ignore = "Explicit disposable qa-automation MicroVM; never calls an AI model"]
fn real_disposable_microvm_pipeline() {
    let home = std::env::var("FIRECRAB_AUTOMATION_QA_HOME").expect("Private QA managed home");
    let vm: Uuid = std::env::var("FIRECRAB_AUTOMATION_QA_VM")
        .expect("Disposable QA VM")
        .parse()
        .unwrap();
    let api_port = super::super::super::dev::instance::api_port(Path::new(&home)).unwrap();
    let metadata: Value =
        reqwest::blocking::get(format!("http://127.0.0.1:{api_port}/api/vms/{vm}"))
            .unwrap()
            .json()
            .unwrap();
    assert!(
        metadata["name"]
            .as_str()
            .unwrap()
            .starts_with("qa-automation-")
    );
    let runtime = Runtime::for_vm(Path::new(&home), vm, false).unwrap();
    for previous in runtime.records().unwrap() {
        assert!(
            previous.closed && previous.role == "qa" && previous.name == "Disposable automation QA",
            "Never borrow a user's agent workspace"
        );
        if runtime.automation_status(previous.id).unwrap()["run"]["appRunning"] == true {
            runtime
                .automate(
                    previous.id,
                    Request {
                        action: "stop_app".into(),
                        id: Uuid::new_v4(),
                        config: Config::default(),
                    },
                )
                .unwrap();
        }
    }
    runtime.save(&[]).unwrap();
    let guest = runtime.guest.as_ref().unwrap();
    runtime.ensure_preview_connector().unwrap();
    let help = guest
        .shell("cloudflared tunnel --help", Duration::from_secs(10))
        .unwrap();
    assert!(help.contains("grace-period"));
    guest.shell("export DEBIAN_FRONTEND=noninteractive; apt-get update >/dev/null && apt-get install -y --no-install-recommends curl git coreutils ca-certificates util-linux >/dev/null",Duration::from_secs(300)).unwrap();
    let id = Uuid::new_v4();
    let cwd = format!("/workspace/firecrab/{id}");
    let agent = Agent {
        id,
        name: "Disposable automation QA".into(),
        role: "qa".into(),
        kind: "codex".into(),
        cwd: cwd.clone(),
        model: String::new(),
        effort: String::new(),
        workspace_id: String::new(),
        pane_id: String::new(),
        terminal_id: String::new(),
        closed: true,
        launch_error: None,
        last_output: String::new(),
        request_digest: String::new(),
        pending_task: None,
        conversation_id: None,
        recovery: None,
        chosen: None,
    };
    runtime.save(std::slice::from_ref(&agent)).unwrap();
    let nginx = format!(
        "pid {cwd}/nginx.pid; error_log stderr; events {{}} http {{ access_log off; server {{ listen 0.0.0.0:8765; location / {{ return 200 'FIRECRAB_AUTOMATION_QA_OK'; }} }} }}"
    );
    guest.shell(&format!("set -eu; mkdir -p {cwd}; cd {cwd}; git init -q; printf %s {} | base64 -d > nginx.conf",guest::quote(&STANDARD.encode(nginx))),Duration::from_secs(10)).unwrap();
    let config = Config {
        test_command: "printf tests-ok > tests.flag".into(),
        build_command: "printf build-ok > build.flag".into(),
        start_command: format!("/usr/sbin/nginx -c {cwd}/nginx.conf -g 'daemon off;'"),
        port: 8765,
    };
    let request = |action: &str, run: Uuid, config: &Config| Request {
        action: action.into(),
        id: run,
        config: config.clone(),
    };
    let wait = || {
        for _ in 0..240 {
            let state = runtime.automation_status(id).unwrap();
            if state["run"]["state"] != "running" {
                return state;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        panic!("Disposable QA verification did not finish");
    };
    let run = Uuid::new_v4();
    runtime.automate(id, request("run", run, &config)).unwrap();
    let state = wait();
    assert_eq!(state["run"]["state"], "passed", "{state}");
    assert_eq!(state["run"]["appReady"], true, "{state}");
    assert!(
        guest
            .shell(
                "curl --noproxy '*' -fsS http://127.0.0.1:8765",
                Duration::from_secs(10)
            )
            .unwrap()
            .contains("FIRECRAB_AUTOMATION_QA_OK")
    );
    assert_eq!(
        runtime.automate(id, request("run", run, &config)).unwrap()["run"]["id"],
        run.to_string()
    );
    let preview_root = format!("/root/.local/share/firecrab/automation/{id}");
    guest
        .shell(
            &format!(
                r#"set -eu
job={preview_root}/{run}
nohup setsid sleep 900 </dev/null >/dev/null 2>&1 &
pid=$!
printf '%s' "$pid" > "$job/preview.pid"
sed 's/^.*) //' /proc/$pid/stat | awk '{{print $20}}' > "$job/preview.start"
printf 'https://fixture-only-not-published.trycloudflare.com' > "$job/preview.log"
"#
            ),
            Duration::from_secs(10),
        )
        .unwrap();
    assert_eq!(
        runtime.automation_status(id).unwrap()["preview"]["running"],
        true
    );
    runtime
        .automate(id, request("stop_preview", Uuid::new_v4(), &config))
        .unwrap();
    assert!(runtime.automation_status(id).unwrap()["preview"].is_null());
    runtime
        .automate(id, request("stop_app", Uuid::new_v4(), &config))
        .unwrap();
    let bad = Config {
        test_command: "exit 7".into(),
        start_command: String::new(),
        ..config.clone()
    };
    runtime
        .automate(id, request("run", Uuid::new_v4(), &bad))
        .unwrap();
    assert_eq!(wait()["run"]["state"], "failed");
    let ci = Config {
        test_command: "true".into(),
        start_command: String::new(),
        ..config
    };
    runtime
        .automate(id, request("configure_ci", Uuid::new_v4(), &ci))
        .unwrap();
    assert!(
        runtime
            .automate(id, request("configure_ci", Uuid::new_v4(), &ci))
            .is_err(),
        "Never overwrite an existing workflow"
    );
    println!(
        "Disposable VM verified: test/build/app, idempotent run, owned app stop, failed test gate, non-overwriting CI generation. No AI model calls or public publication."
    );
}
