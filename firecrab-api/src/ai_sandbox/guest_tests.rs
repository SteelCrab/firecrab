use super::super::files;
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
    local_script(
        root,
        action,
        seconds,
        script(action, id, invocation, payload, seconds),
    )
    .await
}

async fn local_script(
    root: &Path,
    action: &str,
    seconds: u64,
    input: Vec<u8>,
) -> io::Result<Output> {
    let mut command = Command::new("/bin/sh");
    command
        .arg("-s")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let input = String::from_utf8(input)
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

#[test]
fn versioned_shell_parses_for_every_execution_mode_and_operation() {
    assert!(!SCRIPT.contains("ulimit -n 128"));
    assert!(SCRIPT.contains("--ulimit nofile=1024:1024"));
    let modes = [
        None,
        Some(SandboxExecutionMode::PersistentWorkspace),
        Some(SandboxExecutionMode::AgentSandbox {
            runtime: SandboxContainerRuntime::Docker,
            image: format!("example.invalid/image@sha256:{}", "a".repeat(64)),
        }),
        Some(SandboxExecutionMode::AgentSandbox {
            runtime: SandboxContainerRuntime::Nspawn,
            image: format!("sha256:{}", "b".repeat(64)),
        }),
    ];
    use std::io::Write;
    for mode in &modes {
        for action in [
            "hydrate",
            "hydrate_baseline",
            "stage",
            "run",
            "cancel",
            "recover",
            "status",
            "export",
            "cleanup",
            "processes",
            "signal",
        ] {
            let input = script_options(
                action,
                Uuid::new_v4(),
                Some(Uuid::new_v4()),
                "",
                5,
                ScriptOptions {
                    mode: mode.as_ref(),
                    ..Default::default()
                },
            );
            assert!(!String::from_utf8_lossy(&input).contains("__MODE__"));
            let mut child = std::process::Command::new("/bin/sh")
                .arg("-n")
                .stdin(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(&input).unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{action}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[tokio::test]
async fn queries_refuse_missing_state_without_initializing_paths() {
    let directory = tempfile::tempdir().unwrap();
    for (action, expected_code) in [
        ("processes", 77),
        ("status", 78),
        ("recover", 79),
        ("export", 79),
    ] {
        for existing_root in [false, true] {
            let root = directory.path().join(format!("{action}-{existing_root}"));
            if existing_root {
                std::fs::create_dir_all(root.join(".control/locks")).unwrap();
            }
            let input = String::from_utf8(script(action, Uuid::new_v4(), None, "", 0)).unwrap();
            // The root/tool capability probes are unrelated to the missing-state
            // guard. Keep the production filesystem/dispatch code intact so this
            // regression can also run on an unprivileged developer host.
            let (prefix, rest) = input.split_once("[ \"$(id -u)\" = 0 ] || exit 80").unwrap();
            let (_, dispatch) = rest
                .split_once("[ \"$action\" != probe ] || exit 0")
                .unwrap();
            let input = format!("{prefix}\nlock() {{ return 1; }}\n{dispatch}");
            let result = local_script(&root, action, 0, input.into_bytes())
                .await
                .unwrap();
            assert_eq!(
                result.code,
                expected_code,
                "{action}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert!(result.stdout.is_empty());
            if existing_root {
                assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
                assert_eq!(std::fs::read_dir(root.join(".control")).unwrap().count(), 1);
                assert_eq!(
                    std::fs::read_dir(root.join(".control/locks"))
                        .unwrap()
                        .count(),
                    0
                );
            } else {
                assert!(!root.exists(), "{action} initialized missing state");
            }
        }
    }
}

// Run the real generated shell only in the existing disposable Linux fixture.
#[tokio::test]
#[ignore = "requires FIRECRAB_SANDBOX_GUEST_QA=1 in an isolated disposable root container"]
async fn guest_queries_preserve_control_and_do_not_recreate_cleaned_attempt() {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    assert_eq!(
        std::env::var("FIRECRAB_SANDBOX_GUEST_QA").as_deref(),
        Ok("1")
    );
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let root = directory.path().join("guest");
    let source = directory.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("kept.txt"), "retained").unwrap();
    let id = Uuid::new_v4();
    let invocation = Some(Uuid::new_v4());
    let archive = STANDARD.encode(workspace::archive(&source).unwrap());
    assert_eq!(
        local_invocation(&root, "stage", id, invocation, &archive, 0)
            .await
            .unwrap()
            .code,
        0
    );
    assert_eq!(
        local_invocation(
            &root,
            "run",
            id,
            invocation,
            &STANDARD.encode("printf retained"),
            3
        )
        .await
        .unwrap()
        .code,
        0
    );
    let base = root.join(id.simple().to_string());
    let control = base.join("control");
    let lock = root.join(".control/locks").join(id.simple().to_string());
    // Nonempty lock bytes make accidental truncation observable without a clock race.
    std::fs::write(&lock, "existing-lock").unwrap();
    let paths = [
        root.clone(),
        root.join(".control"),
        root.join(".control/locks"),
        base.clone(),
        control.clone(),
        control.join("owner"),
        lock.clone(),
    ];
    let snapshot = || {
        paths
            .iter()
            .map(|path| {
                let info = path.symlink_metadata().unwrap();
                (
                    info.dev(),
                    info.ino(),
                    info.mode(),
                    info.uid(),
                    info.gid(),
                    info.nlink(),
                    info.len(),
                    info.mtime(),
                    info.mtime_nsec(),
                    info.ctime(),
                    info.ctime_nsec(),
                    if info.is_file() {
                        std::fs::read(path).unwrap()
                    } else {
                        vec![]
                    },
                )
            })
            .collect::<Vec<_>>()
    };
    let before = snapshot();
    for action in ["processes", "status", "recover", "export"] {
        let result = local_invocation(&root, action, id, invocation, "", 0)
            .await
            .unwrap();
        assert_eq!(
            result.code,
            0,
            "{action}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            snapshot(),
            before,
            "{action} changed existing control state"
        );
    }

    // A read-only descriptor must still serialize with the lifecycle writer.
    let held = std::fs::File::open(&lock).unwrap();
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
    let reached = directory.path().join("query-reached-lock");
    let input = String::from_utf8(script("processes", id, invocation, "", 0))
        .unwrap()
        .replace(
            "lock -w 10 9 || exit 81",
            &format!("touch '{}'\nlock -w 10 9 || exit 81", reached.display()),
        );
    let pending = {
        let root = root.clone();
        tokio::spawn(async move { local_script(&root, "processes", 0, input.into_bytes()).await })
    };
    for _ in 0..200 {
        if reached.exists() {
            break;
        }
        assert!(
            !pending.is_finished(),
            "query ended before acquiring its lock"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(reached.exists());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !pending.is_finished(),
        "query bypassed the existing lifecycle lock"
    );
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_UN) }, 0);
    assert_eq!(pending.await.unwrap().unwrap().code, 0);
    assert_eq!(snapshot(), before);

    let unknown = Uuid::new_v4();
    assert_eq!(
        local(&root, "processes", unknown, "", 0)
            .await
            .unwrap()
            .code,
        77
    );
    assert!(!root.join(unknown.simple().to_string()).exists());
    assert!(
        !root
            .join(".control/locks")
            .join(unknown.simple().to_string())
            .exists()
    );
    assert_eq!(snapshot(), before);

    let owner = control.join("owner");
    let original_owner = std::fs::read(&owner).unwrap();
    let owner_permissions = owner.metadata().unwrap().permissions();
    std::fs::remove_file(&owner).unwrap();
    let result = local(&root, "processes", id, "", 0).await.unwrap();
    assert_eq!(result.code, 77);
    assert!(!owner.exists(), "query repaired a missing owner record");
    assert!(control.is_dir());
    assert_eq!(std::fs::read(&lock).unwrap(), b"existing-lock");
    std::fs::write(&owner, original_owner).unwrap();
    std::fs::set_permissions(&owner, owner_permissions).unwrap();

    // Pause the actual cleanup while it owns the lifecycle lock. The query
    // must wait, then observe the removed control instead of recreating it.
    let cleanup_ready = directory.path().join("cleanup-holds-lock");
    let cleanup_release = directory.path().join("cleanup-release");
    let input = String::from_utf8(script("cleanup", id, invocation, "", 0))
        .unwrap()
        .replace(
            "rm -rf \"$base\"",
            &format!(
                "touch '{}'\nwhile [ ! -e '{}' ]; do tool sleep 0.01; done\nrm -rf \"$base\"",
                cleanup_ready.display(),
                cleanup_release.display()
            ),
        );
    let cleanup = {
        let root = root.clone();
        tokio::spawn(async move { local_script(&root, "cleanup", 0, input.into_bytes()).await })
    };
    for _ in 0..200 {
        if cleanup_ready.exists() {
            break;
        }
        assert!(
            !cleanup.is_finished(),
            "cleanup stopped before removing its base"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(cleanup_ready.exists());
    let query_ready = directory.path().join("query-waits-for-cleanup");
    let input = String::from_utf8(script("processes", id, invocation, "", 0))
        .unwrap()
        .replace(
            "lock -w 10 9 || exit 81",
            &format!("touch '{}'\nlock -w 10 9 || exit 81", query_ready.display()),
        );
    let query = {
        let root = root.clone();
        tokio::spawn(async move { local_script(&root, "processes", 0, input.into_bytes()).await })
    };
    for _ in 0..200 {
        if query_ready.exists() {
            break;
        }
        assert!(!query.is_finished());
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(query_ready.exists());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!query.is_finished(), "query did not wait for cleanup");
    std::fs::write(&cleanup_release, "release").unwrap();
    assert_eq!(cleanup.await.unwrap().unwrap().code, 0);
    assert_eq!(query.await.unwrap().unwrap().code, 77);
    assert!(!base.exists());
    // Cleanup is a writer. Only subsequent queries must preserve its lock state.
    std::fs::write(&lock, "cleaned-lock").unwrap();
    let cleaned_lock = std::fs::read(&lock).unwrap();
    for (action, expected_code) in [
        ("processes", 77),
        ("status", 78),
        ("recover", 79),
        ("export", 79),
    ] {
        for _ in 0..2 {
            let result = local_invocation(&root, action, id, invocation, "", 0)
                .await
                .unwrap();
            assert_eq!(
                result.code, expected_code,
                "{action} returned an unexpected missing-state code"
            );
            assert!(result.stdout.is_empty());
            assert!(!base.exists(), "{action} recreated the cleaned attempt");
            assert_eq!(std::fs::read(&lock).unwrap(), cleaned_lock);
        }
    }
    let missing_root = directory.path().join("never-staged");
    assert_eq!(
        local(&missing_root, "processes", Uuid::new_v4(), "", 0)
            .await
            .unwrap()
            .code,
        77
    );
    assert!(
        !missing_root.exists(),
        "query initialized an unknown guest root"
    );
}

// Local Linux process fixture. A managed-MicroVM stop/resume still requires separate live evidence.
#[tokio::test]
#[ignore = "requires FIRECRAB_SANDBOX_GUEST_QA=1 in an isolated disposable root container"]
async fn guest_persistent_workspace_survives_attempt_cleanup_and_reuses_code_packages_and_config() {
    assert_eq!(
        std::env::var("FIRECRAB_SANDBOX_GUEST_QA").as_deref(),
        Ok("1")
    );
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let root = dir.path().join("guest");
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("main.txt"), "initial").unwrap();
    let mode = SandboxExecutionMode::PersistentWorkspace;
    let workspace = Uuid::new_v4();
    for turn in 0..2 {
        let id = Uuid::new_v4();
        let invocation = Some(Uuid::new_v4());
        let options = || ScriptOptions {
            mode: Some(&mode),
            workspace: Some(workspace),
            limits: None,
        };
        if turn == 1 {
            let hydrated = local_script(
                &root,
                "hydrate",
                0,
                script_options("hydrate", id, invocation, "", 0, options()),
            )
            .await
            .unwrap();
            assert_eq!(hydrated.code, 0);
            workspace::import(&source, &hydrated.stdout).unwrap();
            let baseline = files::Root::open(&source)
                .unwrap()
                .files()
                .unwrap()
                .join("\n")
                + "\n";
            let acknowledged = local_script(
                &root,
                "hydrate_baseline",
                0,
                script_options(
                    "hydrate_baseline",
                    id,
                    invocation,
                    &STANDARD.encode(baseline),
                    0,
                    options(),
                ),
            )
            .await
            .unwrap();
            assert_eq!(acknowledged.code, 0);
            assert_eq!(
                std::fs::read_to_string(source.join("main.txt")).unwrap(),
                "changed\n"
            );
            std::fs::write(source.join("main.txt"), "host-overlay\n").unwrap();
            std::fs::remove_file(source.join("config/tool.conf")).unwrap();
        }
        let archive = STANDARD.encode(workspace::archive(&source).unwrap());
        let staged = local_script(
            &root,
            "stage",
            0,
            script_options("stage", id, invocation, &archive, 0, options()),
        )
        .await
        .unwrap();
        assert_eq!(
            staged.code,
            0,
            "{}",
            String::from_utf8_lossy(&staged.stderr)
        );
        let command = if turn == 0 {
            "mkdir -p .local/pkg .config config; echo package >.local/pkg/installed; echo setting >.config/tool; echo removable >config/tool.conf; echo changed >main.txt; printf 'out'; printf 'err' >&2; exit 7"
        } else {
            "test \"$(cat main.txt)\" = host-overlay && test \"$(cat .local/pkg/installed)\" = package && test \"$(cat .config/tool)\" = setting && test ! -e config/tool.conf; printf 'retained'"
        };
        let output = local_script(
            &root,
            "run",
            5,
            script_options(
                "run",
                id,
                invocation,
                &STANDARD.encode(command),
                5,
                options(),
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            output.code,
            if turn == 0 { 7 } else { 0 },
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.stdout,
            if turn == 0 {
                b"out".as_slice()
            } else {
                b"retained".as_slice()
            }
        );
        let export = local_script(
            &root,
            "export",
            0,
            script_options("export", id, invocation, "", 0, options()),
        )
        .await
        .unwrap();
        assert_eq!(export.code, 0);
        let cleanup = local_script(
            &root,
            "cleanup",
            0,
            script_options("cleanup", id, invocation, "", 0, options()),
        )
        .await
        .unwrap();
        assert_eq!(
            cleanup.code,
            0,
            "{}",
            String::from_utf8_lossy(&cleanup.stderr)
        );
        assert!(!root.join(id.simple().to_string()).exists());
        assert_eq!(
            std::fs::read_to_string(
                root.join(".workspaces")
                    .join(workspace.simple().to_string())
                    .join("main.txt")
            )
            .unwrap(),
            if turn == 0 {
                "changed\n"
            } else {
                "host-overlay\n"
            }
        );
    }
}
fn files_limit(action: &str) -> usize {
    if action == "export" {
        super::super::files::TREE_LIMIT + 4 * 1024 * 1024
    } else {
        65536
    }
}

// Executes the shipped C helper, not a mock setpriv/flock implementation.
#[tokio::test]
#[ignore = "requires FIRECRAB_SANDBOX_GUEST_QA=1 and the bundled static helper in a disposable Linux root container"]
async fn guest_static_helper_executes_without_distribution_uid_or_timed_lock_utilities() {
    assert_eq!(
        std::env::var("FIRECRAB_SANDBOX_GUEST_QA").as_deref(),
        Ok("1")
    );
    assert_eq!(unsafe { libc::geteuid() }, 0);
    assert!(
        Command::new("/usr/local/sbin/firecrab-console-resize")
            .arg("--sandbox-probe")
            .status()
            .await
            .unwrap()
            .success()
    );
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let root = directory.path().join("guest");
    let source = directory.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("main.txt"), "initial").unwrap();
    let id = Uuid::new_v4();
    let invocation = Some(Uuid::new_v4());
    let generated = |action: &str, payload: &str| {
        String::from_utf8(script(action, id, invocation, payload, 5))
            .unwrap()
            .replace("if command -v setpriv", "if false && command -v setpriv")
            .replace("if command -v flock", "if false && command -v flock")
            .into_bytes()
    };
    let staged = local_script(
        &root,
        "stage",
        5,
        generated(
            "stage",
            &STANDARD.encode(workspace::archive(&source).unwrap()),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        staged.code,
        0,
        "{}",
        String::from_utf8_lossy(&staged.stderr)
    );
    let request=SandboxGuestCommand{version:1,argv:vec!["/bin/sh".into(),"-c".into(),"test \"$(id -u)\" -ne 0; test \"$(awk '/^NoNewPrivs:/ {print $2}' /proc/self/status)\" = 1; cat; printf stdout; printf stderr >&2; exit 7".into()],cwd:".".into(),env:Default::default(),stdin:"stdin-nonce\n".into(),limits:SandboxGuestLimits{wall_seconds:5,cpu_seconds:5,memory_mib:128,output_bytes:4096}};
    let program = super::super::guest_protocol::program(&request).unwrap();
    let result = local_script(&root, "run", 5, generated("run", &STANDARD.encode(program)))
        .await
        .unwrap();
    assert_eq!(
        result.code,
        7,
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"stdin-nonce\nstdout");
    assert_eq!(result.stderr, b"stderr");
    assert_eq!(
        local_script(&root, "export", 5, generated("export", ""))
            .await
            .unwrap()
            .code,
        0
    );
    assert_eq!(
        local_script(&root, "cleanup", 5, generated("cleanup", ""))
            .await
            .unwrap()
            .code,
        0
    );
    assert!(!root.join(id.simple().to_string()).exists());
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
    let invocation =
        std::fs::read_to_string(base.join(id.simple().to_string()).join("control/current"))
            .unwrap();
    let recovered = local_invocation(
        &base,
        "recover",
        id,
        Some(Uuid::parse_str(&invocation).unwrap()),
        "",
        0,
    )
    .await
    .unwrap();
    assert_eq!(recovered.code, 0);
    assert_eq!(recovered.stdout, b"stdout\n");
    assert_eq!(recovered.stderr, b"stderr\n");
    assert_eq!(
        local_invocation(&base, "recover", id, Some(Uuid::new_v4()), "", 0)
            .await
            .unwrap()
            .code,
        79
    );
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
    let mut observed = false;
    for _ in 0..100 {
        let processes = local(&base, "processes", id, "", 0).await.unwrap();
        assert_eq!(processes.code, 0);
        if !processes.stdout.is_empty() {
            let text = String::from_utf8(processes.stdout).unwrap();
            for process in text.lines() {
                let fields = process.split_whitespace().collect::<Vec<_>>();
                assert_eq!(fields.len(), 4);
                assert!(fields[0].parse::<u32>().unwrap() > 1);
                assert!(fields[1].parse::<u64>().unwrap() > 0);
                assert!(!Uuid::parse_str(fields[2]).unwrap().is_nil());
            }
            observed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        observed,
        "active attempt processes disappeared from the query"
    );
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

// Exercise the real shell's boot fence without rebooting the container host.
#[tokio::test]
#[ignore = "requires FIRECRAB_SANDBOX_GUEST_QA=1 in an isolated disposable root container"]
async fn guest_reboot_recovery_preserves_partial_output_and_unrelated_supervisor() {
    assert_eq!(
        std::env::var("FIRECRAB_SANDBOX_GUEST_QA").as_deref(),
        Ok("1")
    );
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("kept.txt"), "retained").unwrap();
    let base = dir.path().join("guest");
    let id = Uuid::new_v4();
    assert_eq!(
        local(
            &base,
            "stage",
            id,
            &STANDARD.encode(workspace::archive(&source).unwrap()),
            0
        )
        .await
        .unwrap()
        .code,
        0
    );
    let invocation = Uuid::new_v4();
    let control = base.join(id.simple().to_string()).join("control");
    let command = control
        .join("commands")
        .join(invocation.simple().to_string());
    std::fs::create_dir_all(&command).unwrap();
    std::fs::write(command.join("stdout"), "partial stdout\n").unwrap();
    std::fs::write(command.join("stderr"), "partial stderr\n").unwrap();
    std::fs::write(control.join("current"), invocation.simple().to_string()).unwrap();
    std::fs::write(control.join("boot"), Uuid::new_v4().to_string()).unwrap();
    let mut unrelated = Command::new("/bin/sleep")
        .arg("60")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pid = unrelated.id().unwrap();
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let start = stat.split_whitespace().nth(21).unwrap();
    std::fs::write(control.join("supervisor"), format!("{pid} {start}\n")).unwrap();
    assert_eq!(
        local_invocation(&base, "cancel", id, Some(invocation), "", 0)
            .await
            .unwrap()
            .code,
        0
    );
    assert!(
        unrelated.try_wait().unwrap().is_none(),
        "Old-boot PID must not be signalled"
    );
    let output = local_invocation(&base, "recover", id, Some(invocation), "", 0)
        .await
        .unwrap();
    assert_eq!(output.code, 0);
    assert_eq!(output.stdout, b"partial stdout\n");
    assert_eq!(output.stderr, b"partial stderr\n");
    assert_eq!(
        local_invocation(&base, "status", id, Some(invocation), "", 0)
            .await
            .unwrap()
            .code,
        78
    );
    let archive = local(&base, "export", id, "", 0).await.unwrap();
    assert_eq!(archive.code, 0);
    workspace::import(&source, &archive.stdout).unwrap();
    assert_eq!(
        std::fs::read_to_string(source.join("kept.txt")).unwrap(),
        "retained"
    );
    assert_eq!(local(&base, "cleanup", id, "", 0).await.unwrap().code, 0);
    assert!(!base.join(id.simple().to_string()).exists());
    assert!(unrelated.try_wait().unwrap().is_none());
    unrelated.kill().await.unwrap();
}

// Real SSH and Unix processes in the disposable container; this is not a MicroVM reboot.
#[tokio::test]
#[ignore = "requires FIRECRAB_SANDBOX_GUEST_QA=1 in an isolated disposable root container"]
async fn guest_transport_recovers_interrupted_output_before_owned_cleanup() {
    assert_eq!(
        std::env::var("FIRECRAB_SANDBOX_GUEST_QA").as_deref(),
        Ok("1")
    );
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let (dir, mut state, mut saved) = super::super::recovery_tests::fixture().await;
    let mut runtime = (*state.runtime).clone();
    runtime.vms_dir = dir.path().join("vms");
    state = state.with_test_runtime(runtime);
    let vm_id = saved.guest.unwrap().vm_id;
    let mut vm = crate::handlers::vms::test_support::record("sandbox-recovery-qa", vm_id);
    vm.state = VmState::Running;
    vm.last_runtime_id = Some(saved.guest.unwrap().runtime_id);
    crate::handlers::vms::test_support::seed_vm(&state, &vm);
    rusqlite::Connection::open(dir.path().join("state.db"))
        .unwrap()
        .execute(
            "UPDATE network_leases SET ipv4='127.0.0.1' WHERE vm_id=?1 AND released_at IS NULL",
            [vm_id.to_string()],
        )
        .unwrap();
    let artifacts = VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm_id);
    let keys = crate::guest_ssh::keys::ensure_vm_ssh_keys(&artifacts).unwrap();
    // Keep authorized_keys under secure ancestors; sshd correctly rejects /tmp.
    let auth = tempfile::tempdir_in("/run").unwrap();
    let authorized_keys = auth.path().join("authorized_keys");
    std::fs::copy(&keys.operator_public, &authorized_keys).unwrap();
    std::fs::set_permissions(&authorized_keys, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = dir.path().join("sshd.conf");
    std::fs::create_dir_all("/run/sshd").unwrap();
    // The container has no published ports or password authentication.
    std::fs::write(&config, format!("Port 22\nListenAddress 127.0.0.1\nHostKey {}\nAuthorizedKeysFile {}\nPermitRootLogin yes\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\n", keys.host_private.display(), authorized_keys.display())).unwrap();
    assert!(
        Command::new("/usr/sbin/usermod")
            .args(["-p", "", "root"])
            .status()
            .await
            .unwrap()
            .success()
    );
    let mut sshd = Command::new("/usr/sbin/sshd")
        .arg("-D")
        .arg("-e")
        .arg("-f")
        .arg(&config)
        .stderr(std::fs::File::create(dir.path().join("sshd.log")).unwrap())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if tokio::net::TcpStream::connect("127.0.0.1:22").await.is_ok() {
            break;
        }
        assert!(sshd.try_wait().unwrap().is_none(), "QA sshd exited");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("main.txt"), "original").unwrap();
    let attempt = dir.path().join("attempt");
    std::fs::create_dir(&attempt).unwrap();
    for name in ["base", "work"] {
        std::fs::create_dir(attempt.join(name)).unwrap();
        files::snapshot(&source, &attempt.join(name)).unwrap();
    }
    std::fs::write(attempt.join("base-modes.json"), b"{\"main.txt\":420}").unwrap();
    saved.workspace = Some(attempt.clone());
    state
        .store
        .save_sandbox_project(&firecrab_api_types::SandboxProject {
            id: saved.task.project_id,
            owner: firecrab_api_types::SandboxPrincipal::Local,
            spec: saved.project.clone(),
            created_at: 0,
        })
        .unwrap();
    let probe = capture(
        ssh(&state, saved.guest.as_ref().unwrap(), &attempt).unwrap(),
        script("probe", saved.task.attempt_id, None, "", 0),
        65536,
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert_eq!(
        probe.code,
        0,
        "SSH fixture: {}; daemon: {}",
        String::from_utf8_lossy(&probe.stderr),
        std::fs::read_to_string(dir.path().join("sshd.log")).unwrap()
    );
    assert!(recover(&state, &saved).await.unwrap().is_none());
    let invocation = saved.guest_command.take();
    assert!(recover(&state, &saved).await.unwrap().is_none());
    saved.guest_command = invocation;
    let output = execute(
        &state,
        &mut saved,
        "echo confirmed; echo diagnostics >&2; exit 255",
        3,
    )
    .await
    .unwrap();
    assert_eq!(output.code, 255);
    let recovered = recover(&state, &saved).await.unwrap().unwrap();
    assert_eq!(recovered.code, Some(255));
    assert_eq!(recovered.stdout, b"confirmed\n");
    assert_eq!(recovered.stderr, b"diagnostics\n");
    let control = PathBuf::from("/var/lib/firecrab-ai")
        .join(saved.task.attempt_id.simple().to_string())
        .join("control");
    let command = control
        .join("commands")
        .join(saved.guest_command.unwrap().simple().to_string());
    std::fs::write(command.join("exit"), "invalid").unwrap();
    assert!(recover(&state, &saved).await.is_err());
    std::fs::write(command.join("exit"), "255").unwrap();
    std::fs::remove_file(command.join("finished")).unwrap();
    assert!(
        recover(&state, &saved)
            .await
            .unwrap()
            .unwrap()
            .code
            .is_none()
    );
    std::fs::write(command.join("finished"), "").unwrap();
    let previous = saved.guest_command;
    let task_id = saved.task.id;
    let worker_state = state.clone();
    let worker = tokio::spawn(async move {
        execute(&worker_state, &mut saved, "echo retained >main.txt; echo partial-stdout; echo partial-stderr >&2; touch started; sleep 90", 120).await
    });
    let started = control.parent().unwrap().join("workspace/started");
    for _ in 0..200 {
        if started.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(started.exists());
    worker.abort();
    assert!(matches!(worker.await, Err(error) if error.is_cancelled()));
    state.store.interrupt_sandbox_tasks().unwrap();
    let mut interrupted = state.store.sandbox_task(task_id).unwrap().unwrap();
    assert_ne!(interrupted.guest_command, previous);
    assert_eq!(
        interrupted.task.state,
        firecrab_api_types::SandboxTaskState::Interrupted
    );
    let db = rusqlite::Connection::open(dir.path().join("state.db")).unwrap();
    db.execute_batch("CREATE TRIGGER retain_guest BEFORE UPDATE ON ai_sandbox_tasks BEGIN SELECT RAISE(ABORT,'retain guest evidence'); END;").unwrap();
    assert!(
        super::super::reconcile_guest(&state, &mut interrupted)
            .await
            .is_err()
    );
    assert!(
        control.parent().unwrap().exists(),
        "Failed checkpoint must retain guest logs and files"
    );
    assert!(
        state
            .store
            .sandbox_task(task_id)
            .unwrap()
            .unwrap()
            .task
            .events[0]
            .output
            .is_empty()
    );
    db.execute_batch("DROP TRIGGER retain_guest;").unwrap();
    super::super::reconcile_guest(&state, &mut interrupted)
        .await
        .unwrap();
    assert_eq!(interrupted.task.events[0].output, "partial-stdout\n");
    // The real shell can append a signal diagnostic (for example, "Killed").
    assert!(
        interrupted.task.events[0]
            .stderr
            .starts_with("partial-stderr\n")
    );
    assert!(
        interrupted.task.events[0]
            .exit_code
            .is_some_and(|code| code != 0)
    );
    let durable = state.store.sandbox_task(task_id).unwrap().unwrap();
    assert_eq!(durable.task.events[0].output, "partial-stdout\n");
    assert_eq!(
        durable.task.events[0].stderr,
        interrupted.task.events[0].stderr
    );
    assert_eq!(
        durable.task.state,
        firecrab_api_types::SandboxTaskState::Interrupted
    );
    assert_eq!(
        std::fs::read_to_string(attempt.join("work/main.txt")).unwrap(),
        "retained\n"
    );
    assert_eq!(
        std::fs::read_to_string(source.join("main.txt")).unwrap(),
        "original"
    );
    assert!(!control.parent().unwrap().exists());
    sshd.kill().await.unwrap();
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
