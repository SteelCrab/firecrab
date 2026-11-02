//! Real SSH, descriptor-based files and unprivileged PTYs in a disposable Linux container.
//! The native test build substitutes a temporary directory for an ext4 mount; it is not VM QA.
use super::*;
use crate::ai_sandbox::{self, SavedInstructions, model::Call};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, path::Path, time::Duration};
use tokio::process::Command;
use tower::ServiceExt;

async fn api(state: &AppState, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let router = super::super::routes()
        .with_state(state.clone())
        .layer(Extension(RequestId(Uuid::new_v4())))
        .layer(Extension(crate::ai_sandbox::access::LocalOperator));
    for attempt in 0..30 {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .header("host", "localhost:5523")
                    .header("origin", "http://localhost:5523")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        // A definitive workspace lock rejection is retried as in the dashboard. Unknown input outcomes are never replayed.
        if status == StatusCode::CONFLICT
            && value["error"]["code"] == "workspace_busy"
            && attempt < 29
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        }
        return (status, value);
    }
    unreachable!();
}
async fn ok(state: &AppState, method: &str, path: &str, body: Value) -> Value {
    let (status, result) = api(state, method, path, body).await;
    assert!(status.is_success(), "{method} {path}: {status} {result}");
    result
}

#[tokio::test]
#[ignore = "requires FIRECRAB_WORKSPACE_TRANSPORT_QA=1 in its disposable Linux root container"]
async fn workspace_transport_retains_file_receipts_ptys_skills_and_editor_instances() {
    assert_eq!(
        std::env::var("FIRECRAB_WORKSPACE_TRANSPORT_QA").as_deref(),
        Ok("1")
    );
    assert_eq!(unsafe { libc::geteuid() }, 0);
    assert!(
        Path::new("/.dockerenv").is_file(),
        "never run this fixture on the controller host"
    );
    let (dir, state, seed) = super::super::tests::fixture().await;
    super::super::tests::bind(&state, &seed).await;
    let b = binding(&state, seed.project_id).unwrap();
    let runtime = guest::Binding {
        vm_id: b.vm_id,
        runtime_id: Uuid::new_v4(),
    };
    let db = open(&state, &b).unwrap();
    db.mark_initialized().unwrap();
    db.set_import_sha(&"a".repeat(64)).unwrap();
    drop(db);
    state
        .store
        .verify_project_workspace(&b, runtime.runtime_id)
        .unwrap();
    {
        let mut vms = state.vms.lock().unwrap();
        let vm = vms.get_mut(&b.vm_id).unwrap();
        vm.state = VmState::Running;
        vm.last_runtime_id = Some(runtime.runtime_id);
    }
    rusqlite::Connection::open(dir.path().join("control.db"))
        .unwrap()
        .execute(
            "UPDATE network_leases SET ipv4='127.0.0.1' WHERE vm_id=?1 AND released_at IS NULL",
            [b.vm_id.to_string()],
        )
        .unwrap();
    let vm = state.vms.lock().unwrap().get(&b.vm_id).unwrap().clone();
    let artifacts =
        crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), b.vm_id);
    let keys = crate::guest_ssh::keys::ensure_vm_ssh_keys(&artifacts).unwrap();
    let auth = tempfile::tempdir_in("/run").unwrap();
    std::fs::copy(&keys.operator_public, auth.path().join("authorized_keys")).unwrap();
    std::fs::create_dir_all("/run/sshd").unwrap();
    let config = dir.path().join("sshd.conf");
    std::fs::write(&config,format!("Port 22\nListenAddress 127.0.0.1\nHostKey {}\nAuthorizedKeysFile {}\nPermitRootLogin yes\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\n",keys.host_private.display(),auth.path().join("authorized_keys").display())).unwrap();
    assert!(
        Command::new("/usr/sbin/usermod")
            .args(["-p", "", "root"])
            .status()
            .await
            .unwrap()
            .success()
    );
    let mut sshd = Command::new("/usr/sbin/sshd")
        .args(["-D", "-e", "-f"])
        .arg(&config)
        .stderr(std::fs::File::create(dir.path().join("sshd.log")).unwrap())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if tokio::net::TcpStream::connect("127.0.0.1:22").await.is_ok() {
            break;
        }
        assert!(sshd.try_wait().unwrap().is_none());
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let root = Path::new("/workspace/shared");
    assert!(!root.exists());
    std::fs::create_dir_all(root).unwrap();
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o777)).unwrap();
    let marker = root.join(".firecrab-workspace");
    std::fs::create_dir(&marker).unwrap();
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        marker.join("ready"),
        format!("{}:{}:{}", b.project_id, b.database_id, "a".repeat(64)),
    )
    .unwrap();
    std::fs::set_permissions(marker.join("ready"), std::fs::Permissions::from_mode(0o600)).unwrap();
    let base = format!("/api/ai-sandbox/projects/{}/workspace", b.project_id);
    files_and_receipts(&state, &base, root).await;
    eprintln!("PASS actual SSH file writes, conflicts, immutable history and confined paths");
    lost_write_acknowledgement(&state, &b, runtime, &base).await;
    super::super::start_terminals(state.clone());
    terminals(&state, &base).await;
    eprintln!("PASS native PTY output, fenced control, resize and termination");
    skills_and_folder_tools(&state, &b, runtime, &base).await;
    eprintln!("PASS frozen skill installation and native file/command tools without a provider");
    editor(&state, &base).await;
    eprintln!("PASS pinned OpenVSCode and execution-scoped stop/reopen");
    sshd.kill().await.unwrap();
    sshd.wait().await.unwrap();
}

async fn files_and_receipts(state: &AppState, base: &str, root: &Path) {
    let path = format!("{base}/files?path=src/evidence.txt");
    let first = json!({"operationId":Uuid::new_v4(),"expectedVersion":0,"contentBase64":STANDARD.encode(b"first\0bytes")});
    let created = ok(state, "PUT", &path, first.clone()).await;
    let version = created["version"]["id"].as_u64().unwrap();
    assert_eq!(
        ok(state, "GET", &path, json!({})).await["contentBase64"],
        first["contentBase64"]
    );
    assert_eq!(
        api(
            state,
            "PUT",
            &path,
            json!({"operationId":Uuid::new_v4(),"expectedVersion":0,"contentBase64":"eA=="})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let changed=ok(state,"PUT",&path,json!({"operationId":Uuid::new_v4(),"expectedVersion":version,"contentBase64":STANDARD.encode(b"second")})).await;
    let latest = changed["version"]["id"].as_u64().unwrap();
    let diff = ok(
        state,
        "GET",
        &format!("{base}/files/diff?path=src/evidence.txt&from={version}&to={latest}"),
        json!({}),
    )
    .await;
    assert_eq!(diff["before"]["contentBase64"], first["contentBase64"]);
    let restored = ok(
        state,
        "POST",
        &format!("{base}/files/restore?path=src/evidence.txt"),
        json!({"operationId":Uuid::new_v4(),"expectedVersion":latest,"version":version}),
    )
    .await;
    assert_eq!(restored["version"]["restoredFrom"], version);
    assert_eq!(
        ok(state, "PUT", &path, first).await["version"]["id"],
        version
    );
    assert_eq!(
        std::fs::read(root.join("src/evidence.txt")).unwrap(),
        b"first\0bytes"
    );
    let tree = ok(
        state,
        "GET",
        &format!("{base}/files/tree?path=src"),
        json!({}),
    )
    .await;
    assert_eq!(tree[0]["name"], "evidence.txt");
    assert_eq!(
        ok(
            state,
            "GET",
            &format!("{base}/files/history?path=src/evidence.txt"),
            json!({})
        )
        .await
        .as_array()
        .unwrap()
        .len(),
        3
    );
    let operations = ok(state, "GET", &format!("{base}/file-operations"), json!({})).await;
    assert!(!operations.as_array().unwrap().is_empty());
    for op in operations.as_array().unwrap() {
        if op["state"] == "committed" {
            ok(
                state,
                "POST",
                &format!(
                    "{base}/file-operations/{}/reconcile",
                    op["id"].as_str().unwrap()
                ),
                json!({}),
            )
            .await;
        }
    }
    assert_eq!(
        api(
            state,
            "GET",
            &format!("{base}/files?path=.firecrab-workspace/ready"),
            json!({})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    std::os::unix::fs::symlink("/etc/passwd", root.join("outside-link")).unwrap();
    assert_eq!(
        api(
            state,
            "GET",
            &format!("{base}/files?path=outside-link"),
            json!({})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    ok(state,"PUT",&format!("{base}/files?path=AGENTS.md"),json!({"operationId":Uuid::new_v4(),"expectedVersion":0,"contentBase64":STANDARD.encode(b"Use retained file versions.\n")})).await;
}
async fn terminals(state: &AppState, base: &str) {
    let id = Uuid::new_v4();
    let path = format!("{base}/terminals/{id}");
    let body = json!({"label":"Transport QA","workingDirectory":"","rows":24,"columns":80});
    let created = ok(state, "PUT", &path, body.clone()).await;
    assert_eq!(created["status"], "running");
    assert_eq!(ok(state, "PUT", &path, body).await["id"], id.to_string());
    let client = Uuid::new_v4();
    let lease = ok(
        state,
        "POST",
        &format!("{path}/control"),
        json!({"clientId":client,"expectedGeneration":created["controlGeneration"]}),
    )
    .await;
    let control = json!({"clientId":lease["clientId"],"generation":lease["generation"],"token":lease["token"]});
    ok(state, "PUT", &format!("{path}/control"), control.clone()).await;
    ok(state,"POST",&format!("{path}/input"),json!({"type":"input","control":control,"contentBase64":STANDARD.encode(b"printf 'transport-once\\n'; printf once >> terminal-once\n")})).await;
    let mut observed = false;
    let mut transcript = String::new();
    for _ in 0..60 {
        let output = ok(state, "GET", &format!("{path}/output?cursor=0"), json!({})).await;
        let bytes = STANDARD
            .decode(output["contentBase64"].as_str().unwrap())
            .unwrap();
        transcript = String::from_utf8_lossy(&bytes).replace('\r', "");
        if transcript.lines().any(|line| line == "transport-once") {
            observed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(observed, "native PTY transcript: {transcript:?}");
    ok(
        state,
        "POST",
        &format!("{path}/input"),
        json!({"type":"resize","control":control,"rows":42,"columns":111}),
    )
    .await;
    let metadata = ok(state, "GET", &path, json!({})).await;
    assert_eq!(metadata["rows"], 42);
    assert_eq!(metadata["columns"], 111);
    ok(state, "DELETE", &format!("{path}/control"), control.clone()).await;
    assert_eq!(
        api(
            state,
            "POST",
            &format!("{path}/input"),
            json!({"type":"input","control":control,"contentBase64":"eAo="})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let control = ok(
        state,
        "POST",
        &format!("{path}/control"),
        json!({"clientId":Uuid::new_v4(),"expectedGeneration":metadata["controlGeneration"]}),
    )
    .await;
    assert!(!control["token"].as_str().unwrap().is_empty());
    let stopped = ok(state, "POST", &format!("{path}/stop"), json!({"clientId":control["clientId"],"generation":control["generation"],"token":control["token"]})).await;
    assert_eq!(stopped["id"], id.to_string());
    let mut ended = false;
    for _ in 0..100 {
        let current = ok(state, "GET", &path, json!({})).await;
        if matches!(current["status"].as_str(), Some("exited" | "interrupted"))
            && current["cleanupRequired"] != true
        {
            ended = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        ended,
        "native UID termination must be confirmed before accepting stop"
    );
    assert_eq!(
        std::fs::read("/workspace/shared/terminal-once").unwrap(),
        b"once"
    );
    assert!(
        !ok(state, "GET", &format!("{base}/terminals"), json!({}))
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
}
async fn skills_and_folder_tools(
    state: &AppState,
    b: &Binding,
    runtime: guest::Binding,
    base: &str,
) {
    let (entry, blobs) = super::tests::package();
    let saved = record(Uuid::new_v4(), Caller::default(), entry.clone(), blobs).unwrap();
    let id = saved.public.id;
    open(state, b).unwrap().save_skill_record(&saved).unwrap();
    let path = format!("{base}/skills/installations/{id}");
    let input = json!({"source":entry.source,"kind":entry.kind,"path":entry.path,"revision":entry.revision});
    let installed = ok(state, "PUT", &path, input.clone()).await;
    assert_eq!(installed["status"], "installed");
    assert_eq!(ok(state, "PUT", &path, input).await, installed);
    let snapshot_id = Uuid::new_v4();
    let snapshot = ok(
        state,
        "PUT",
        &format!("{base}/instructions/snapshots/{snapshot_id}"),
        json!({"directory":""}),
    )
    .await;
    let captured = tasks::capture(
        state.clone(),
        Caller::default(),
        b.project_id,
        runtime,
        vec![id],
        AiProvider::Openai,
        Uuid::new_v4(),
    )
    .await
    .unwrap();
    let mut task = super::tests::task(b, &saved);
    task.skills = captured;
    task.guest = Some(runtime);
    task.task.runtime = Some(runtime);
    task.task.request.project_shell = true;
    task.instructions = Some(SavedInstructions {
        database_id: b.database_id,
        volume_id: b.volume_id,
        generation: b.generation,
        snapshot: serde_json::from_value(snapshot).unwrap(),
    });
    task.task.request.instruction_snapshot_id = Some(snapshot_id);
    let mut call = Call {
        id: "write".into(),
        name: "write_file".into(),
        arguments: json!({"path":"agent.txt","content":"saved through native files","expectedVersion":0}),
    };
    ai_sandbox::folder::prepare(&mut task, &mut call);
    assert!(task.guest_pending);
    let output = ai_sandbox::folder::tool(state, &mut task, &call)
        .await
        .unwrap();
    assert!(!output.3);
    assert!(!task.guest_pending);
    assert_eq!(task.task.folder_mutations.len(), 1);
    for (name, path) in [
        ("read_file", "agent.txt"),
        ("read_file", "not-created.txt"),
        ("list_files", ""),
    ] {
        let result = ai_sandbox::folder::tool(
            state,
            &mut task,
            &Call {
                id: name.into(),
                name: name.into(),
                arguments: json!({"path":path}),
            },
        )
        .await
        .unwrap();
        assert!(!result.3);
    }
    let mut command = Call {
        id: "command".into(),
        name: "run_argv".into(),
        arguments: json!({"version":1,"argv":["/bin/sh","-c","printf command-proof; printf once >> command-once"],"cwd":"","env":[],"stdin":"","limits":{"wallSeconds":5,"cpuSeconds":3,"memoryMib":64,"outputBytes":8192}}),
    };
    ai_sandbox::folder::prepare(&mut task, &mut command);
    assert!(task.guest_pending);
    let result = ai_sandbox::folder::tool(state, &mut task, &command)
        .await
        .unwrap();
    assert_eq!(result.0, "command-proof");
    assert_eq!(result.2, Some(0));
    assert!(!task.guest_pending);
    task.guest_pending = true;
    let second = ai_sandbox::folder::tool(state, &mut task, &command)
        .await
        .unwrap();
    assert_eq!(second.0, "command-proof");
    assert_eq!(
        std::fs::read("/workspace/shared/command-once").unwrap(),
        b"once"
    );
    ok(state, "DELETE", &path, json!({})).await;
    assert!(
        tasks::capture(
            state.clone(),
            Caller::default(),
            b.project_id,
            runtime,
            vec![id],
            AiProvider::Openai,
            Uuid::new_v4()
        )
        .await
        .is_err()
    );
    let retained = tasks::read(
        state,
        &task,
        &json!({"skillId":id,"path":"references/context.md"}),
    )
    .await
    .unwrap();
    assert_eq!(retained["content"], "Original reference\n");
    assert!(
        tasks::message(&task, &[]).unwrap().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("Follow the saved task")
    );
}
async fn editor(state: &AppState, base: &str) {
    let view = Uuid::new_v4();
    ok(
        state,
        "PUT",
        &format!("{base}/views/{view}"),
        json!({"operationId":Uuid::new_v4(),"expectedRevision":0,"document":{}}),
    )
    .await;
    let path = format!("{base}/editors/{view}");
    let body =
        json!({"viewId":view,"workingDirectory":"","file":"agent.txt","requestId":Uuid::new_v4()});
    let first = ok(state, "PUT", &path, body.clone()).await;
    assert_eq!(first["status"], "ready");
    let second = ok(state, "PUT", &path, body).await;
    assert_eq!(second["instance"], first["instance"]);
    proxy_workbench(state, &path, &first).await;
    assert_eq!(ok(state, "GET", &path, json!({})).await["status"], "ready");
    assert_eq!(
        api(
            state,
            "POST",
            &format!("{path}/stop"),
            json!({"expectedInstance":"0".repeat(64)})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    stop_editor(state, &path, &first["instance"]).await;
    assert_eq!(
        ok(
            state,
            "POST",
            &format!("{path}/stop"),
            json!({"expectedInstance":first["instance"]})
        )
        .await["instance"],
        first["instance"]
    );
    let replacement = ok(
        state,
        "PUT",
        &path,
        json!({"viewId":view,"workingDirectory":"","file":"agent.txt","requestId":Uuid::new_v4()}),
    )
    .await;
    assert_ne!(replacement["instance"], first["instance"]);
    assert_eq!(
        api(
            state,
            "POST",
            &format!("{path}/stop"),
            json!({"expectedInstance":first["instance"]})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    stop_editor(state, &path, &replacement["instance"]).await;
}

async fn proxy_workbench(state: &AppState, editor_path: &str, editor: &Value) {
    let client = Uuid::new_v4();
    ok(
        state,
        "PUT",
        &format!("{editor_path}/clients/{client}"),
        json!({"instance":editor["instance"]}),
    )
    .await;
    async fn read(
        state: &AppState,
        host: &str,
        path: &str,
    ) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let router = Router::new()
            .fallback(|| async { "controller-fallback" })
            .with_state(state.clone())
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                super::super::editors::dispatch,
            ))
            .layer(Extension(crate::ai_sandbox::access::LocalOperator))
            .layer(Extension(RequestId(Uuid::new_v4())));
        let response = router
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("host", host)
                    .header("accept-encoding", "identity")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap();
        (status, headers, body.to_vec())
    }
    let host = editor["host"].as_str().unwrap();
    let (status, headers, bytes) = read(state, host, &format!("/?firecrabClient={client}")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let html = String::from_utf8(bytes).unwrap();
    assert!(html.contains("vscode-workbench-web-configuration"));
    assert!(html.contains("/_firecrab/browser-bridge.js"));
    assert!(!html.contains("controller-fallback"));
    assert!(!headers.contains_key("set-cookie"));
    let (status, _, file) = read(state, host, "/_firecrab/files?path=agent.txt").await;
    assert_eq!(status, StatusCode::OK);
    let file: Value = serde_json::from_slice(&file).unwrap();
    assert_eq!(
        STANDARD
            .decode(file["contentBase64"].as_str().unwrap())
            .unwrap(),
        b"saved through native files"
    );
    let icon = html
        .split("href=\"")
        .skip(1)
        .filter_map(|s| s.split('"').next())
        .find(|p| p.starts_with('/') && p.ends_with("/favicon.ico"))
        .expect("pinned workbench icon");
    let (status, _, icon) = read(state, host, icon).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!icon.is_empty());
}

async fn stop_editor(state: &AppState, path: &str, instance: &Value) {
    let (status, result) = api(
        state,
        "POST",
        &format!("{path}/stop"),
        json!({"expectedInstance":instance}),
    )
    .await;
    if !status.is_success() {
        assert_eq!(status, StatusCode::CONFLICT, "{result}");
        assert!(
            result["error"]["message"]
                .as_str()
                .unwrap()
                .contains("termination is not yet confirmed"),
            "{result}"
        );
    }
    let mut ended = false;
    for _ in 0..100 {
        let current = ok(state, "GET", path, json!({})).await;
        if current["status"] == "stopped" {
            ended = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        ended,
        "private editor native UID must finish before stop acceptance"
    );
}
async fn lost_write_acknowledgement(
    state: &AppState,
    b: &Binding,
    runtime: guest::Binding,
    base: &str,
) {
    use super::super::{files_db::Operation, files_guest as wire};
    let id = Uuid::new_v4();
    let db = open(state, b).unwrap();
    let op = db
        .stage_file(Operation {
            id,
            path: "receipt.txt".into(),
            expected: 0,
            content: b"written before lost acknowledgement".to_vec(),
            author: Caller::default().principal,
            restored_from: None,
            created_at: 0,
            version: None,
            state: "pending".into(),
        })
        .unwrap();
    let reply = wire::request(
        state,
        b,
        &runtime,
        &db.directory,
        &"a".repeat(64),
        wire::operation_frame(&db, &op).unwrap(),
    )
    .await
    .unwrap();
    assert!(matches!(reply,wire::Reply::Data(ref bytes) if bytes.is_empty()));
    assert!(db.file_operation(id).unwrap().unwrap().version.is_none());
    drop(db);
    let result = ok(
        state,
        "POST",
        &format!("{base}/file-operations/{id}/reconcile"),
        json!({}),
    )
    .await;
    assert_eq!(
        STANDARD
            .decode(result["contentBase64"].as_str().unwrap())
            .unwrap(),
        op.content
    );
    std::fs::write("/workspace/shared/receipt.txt", b"later external edit").unwrap();
    let retry = ok(
        state,
        "PUT",
        &format!("{base}/files?path=receipt.txt"),
        json!({"operationId":id,"expectedVersion":0,"contentBase64":STANDARD.encode(op.content)}),
    )
    .await;
    assert_eq!(retry["version"], result["version"]);
    assert_eq!(
        std::fs::read("/workspace/shared/receipt.txt").unwrap(),
        b"later external edit"
    );
}
