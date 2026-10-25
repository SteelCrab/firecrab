use super::*;

async fn local(
    root: &Path,
    action: &str,
    id: Uuid,
    payload: &str,
    seconds: u64,
) -> io::Result<Output> {
    let invocation = if action == "status" {
        std::fs::read_to_string(root.join(id.simple().to_string()).join("control/current"))
            .ok()
            .and_then(|id| Uuid::parse_str(&id).ok())
    } else {
        Some(Uuid::new_v4())
    };
    local_invocation(root, action, id, invocation, payload, seconds).await
}
async fn local_invocation(
    root: &Path,
    action: &str,
    id: Uuid,
    invocation: Option<Uuid>,
    payload: &str,
    seconds: u64,
) -> io::Result<Output> {
    let mut command = Command::new("/bin/sh");
    command
        .arg("-s")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let input = String::from_utf8(script(action, id, invocation, payload, seconds))
        .unwrap()
        .replace("/var/lib/firecrab-ai", root.to_str().unwrap());
    capture(
        command,
        input.into_bytes(),
        files_limit(action),
        Duration::from_secs(seconds + 20),
    )
    .await
}
fn files_limit(action: &str) -> usize {
    if action == "export" {
        super::super::files::TREE_LIMIT + 4 * 1024 * 1024
    } else {
        65536
    }
}

// Run explicitly only inside a disposable root container. This test creates real dedicated users.
#[tokio::test]
#[ignore = "requires FIRECRAB_SANDBOX_GUEST_QA=1 in an isolated disposable root container"]
async fn guest_shell_enforces_identity_limits_retention_export_and_cancel() {
    assert_eq!(
        std::env::var("FIRECRAB_SANDBOX_GUEST_QA").as_deref(),
        Ok("1")
    );
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let base = dir.path().join("guest");
    std::fs::create_dir(&base).unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o711)).unwrap();
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("main.txt"), "original").unwrap();
    let sentinel = dir.path().join("root-secret");
    std::fs::write(&sentinel, "private").unwrap();
    std::fs::set_permissions(&sentinel, std::fs::Permissions::from_mode(0o600)).unwrap();
    let id = Uuid::new_v4();
    let archive = STANDARD.encode(workspace::archive(&source).unwrap());
    assert_eq!(local(&base, "probe", id, "", 0).await.unwrap().code, 0);
    assert!(!base.join(id.simple().to_string()).exists());
    let staged = local(&base, "stage", id, &archive, 0).await.unwrap();
    assert_eq!(
        staged.code,
        0,
        "{}",
        String::from_utf8_lossy(&staged.stderr)
    );
    let command = format!(
        "test \"$(id -u)\" -ne 0 && test \"$(awk '/^NoNewPrivs:/ {{print $2}}' /proc/self/status)\" = 1 && test -z \"${{OPENAI_API_KEY-}}\" && ! cat '{}' 2>/dev/null && mkdir -p node_modules/demo && echo dependency >node_modules/demo/file && echo changed > main.txt && echo stdout && echo stderr >&2; exit 7",
        sentinel.display()
    );
    let result = local(&base, "run", id, &STANDARD.encode(command), 3)
        .await
        .unwrap();
    assert_eq!(
        result.code,
        7,
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"stdout\n");
    assert_eq!(result.stderr, b"stderr\n");
    let exported = local(&base, "export", id, "", 0).await.unwrap();
    assert_eq!(
        exported.code,
        0,
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    workspace::import(&source, &exported.stdout).unwrap();
    assert_eq!(
        std::fs::read_to_string(source.join("main.txt")).unwrap(),
        "changed\n"
    );
    assert!(!source.join("node_modules").exists());
    let staged = local(
        &base,
        "stage",
        id,
        &STANDARD.encode(workspace::archive(&source).unwrap()),
        0,
    )
    .await
    .unwrap();
    assert_eq!(staged.code, 0);
    let result = local(
        &base,
        "run",
        id,
        &STANDARD.encode("test -f node_modules/demo/file && echo retained"),
        3,
    )
    .await
    .unwrap();
    assert_eq!(result.code, 0);
    assert_eq!(result.stdout, b"retained\n");
    let result = local(&base, "run", id, &STANDARD.encode("exit 255"), 3)
        .await
        .unwrap();
    assert_eq!(result.code, 255);
    assert_eq!(
        local(&base, "status", id, "", 0).await.unwrap().stdout,
        b"255"
    );
    let previous =
        std::fs::read_to_string(base.join(id.simple().to_string()).join("control/current"))
            .unwrap();
    let previous = Uuid::parse_str(&previous).unwrap();
    assert_eq!(
        local_invocation(&base, "status", id, Some(Uuid::new_v4()), "", 0)
            .await
            .unwrap()
            .code,
        78
    );
    assert_eq!(
        local_invocation(
            &base,
            "run",
            id,
            Some(previous),
            &STANDARD.encode("echo repeated"),
            3
        )
        .await
        .unwrap()
        .code,
        82
    );
    let result = local(&base, "run", id, &STANDARD.encode("sleep 10"), 1)
        .await
        .unwrap();
    assert_ne!(result.code, 0);
    let result = local(&base, "run", id, &STANDARD.encode("yes huge-output"), 3)
        .await
        .unwrap();
    assert_eq!(result.code, 125);
    assert_eq!(result.stdout.len(), 65536);
    let result = local(&base, "run", id, &STANDARD.encode("echo reset"), 3)
        .await
        .unwrap();
    assert_eq!(result.code, 0);
    assert_eq!(result.stdout, b"reset\n");
    let marker = base
        .join(id.simple().to_string())
        .join("control/supervisor");
    std::fs::remove_file(&marker).ok();
    let pending = {
        let base = base.clone();
        tokio::spawn(async move { local(&base, "run", id, &STANDARD.encode("sleep 30"), 40).await })
    };
    for _ in 0..100 {
        if marker.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(marker.exists());
    let cancelled = local(&base, "cancel", id, "", 0).await.unwrap();
    assert_eq!(
        cancelled.code,
        0,
        "{}",
        String::from_utf8_lossy(&cancelled.stderr)
    );
    let result = pending.await.unwrap().unwrap();
    assert_ne!(result.code, 0);
    assert_eq!(local(&base, "export", id, "", 0).await.unwrap().code, 0);
    let cleanup = local(&base, "cleanup", id, "", 0).await.unwrap();
    assert_eq!(
        cleanup.code,
        0,
        "{}",
        String::from_utf8_lossy(&cleanup.stderr)
    );
    assert!(!base.join(id.simple().to_string()).exists());
    assert_eq!(
        local(&base, "stage", id, &archive, 0).await.unwrap().code,
        71
    );
    assert_eq!(
        local(&base, "run", id, &STANDARD.encode("echo resurrected"), 1)
            .await
            .unwrap()
            .code,
        71
    );
    assert!(!base.join(id.simple().to_string()).exists());
    assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "private");
}

#[tokio::test]
async fn guest_transport_pins_the_selected_vm_and_refuses_a_changed_boot_or_stopped_vm() {
    let dir = tempfile::tempdir().unwrap();
    let templates =
        crate::templates::TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
    let mut state = AppState::with_db_file(templates, dir.path().join("state.db"))
        .await
        .unwrap();
    let mut runtime = (*state.runtime).clone();
    runtime.vms_dir = dir.path().join("vms");
    state = state.with_test_runtime(runtime);
    let id = Uuid::new_v4();
    let mut vm = crate::handlers::vms::test_support::record("sandbox", id);
    vm.storage_root = "sandbox-fixture".into();
    vm.state = VmState::Running;
    vm.last_runtime_id = Some(Uuid::new_v4());
    crate::handlers::vms::test_support::seed_vm(&state, &vm);
    let bound = binding(&state, id).unwrap();
    let paths = VmSshPaths::from_artifacts(&VmArtifactPaths::for_vm(
        &state.vms_dir_for(&vm.storage_root),
        id,
    ));
    std::fs::create_dir_all(&paths.dir).unwrap();
    std::fs::write(&paths.operator_private, "fixture-key").unwrap();
    std::fs::set_permissions(
        &paths.operator_private,
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    std::fs::write(&paths.host_public,"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA fixture\n").unwrap();
    let attempt = dir.path().join("attempt");
    std::fs::create_dir(&attempt).unwrap();
    let command = ssh(&state, &bound, &attempt).unwrap();
    let args = command
        .as_std()
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(args.iter().any(|a| a == "StrictHostKeyChecking=yes"));
    assert!(args.iter().any(|a| a == "IdentityAgent=none"));
    assert_eq!(args.last().unwrap(), "/bin/sh -s");
    let lease = state.store.active_lease(id).unwrap().unwrap();
    assert!(args.iter().any(|a| a == &format!("root@{}", lease.ipv4)));
    assert!(
        std::fs::read_to_string(attempt.join("known_hosts"))
            .unwrap()
            .starts_with(&format!("{} ssh-ed25519 ", lease.ipv4))
    );
    state
        .vms
        .lock()
        .unwrap()
        .get_mut(&id)
        .unwrap()
        .last_runtime_id = Some(Uuid::new_v4());
    assert!(ssh(&state, &bound, &attempt).is_err());
    state.vms.lock().unwrap().get_mut(&id).unwrap().state = VmState::Stopped;
    assert!(binding(&state, id).is_err());
}
