//! Actual Windows CLI -> WSL2 service shell E2E. Requires installed microManager.
//!
//! cargo test -p firecrab-cli --test windows_service_shell -- --ignored --test-threads=1
//! FIRECRAB_WINDOWS_SHELL_CLI can select the Windows executable under test.
//! Hosted Windows CI compiles these tests; a nested-KVM Windows host runs them.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

struct ShellOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

fn cli(args: &[&str], input: &str) -> ShellOutput {
    if !cfg!(target_os = "windows") {
        panic!("service shell runtime E2E requires a native Windows host");
    }
    let executable = std::env::var_os("FIRECRAB_WINDOWS_SHELL_CLI")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_firecrab")));
    let mut child = Command::new(executable)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the native Windows CLI");
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let read_stdout = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let read_stderr = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    // Close stdin even for a no-command shell, so EOF cannot hang the test.
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(input.as_bytes())
        .expect("write shell input");
    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll CLI exit") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("native service shell timed out after 30 seconds: {args:?}");
        }
        thread::sleep(Duration::from_millis(50));
    };
    ShellOutput {
        status,
        stdout: String::from_utf8(read_stdout.join().unwrap())
            .expect("UTF-8 stdout")
            .replace('\r', ""),
        stderr: String::from_utf8(read_stderr.join().unwrap())
            .expect("UTF-8 stderr")
            .replace('\r', ""),
    }
}

#[test]
#[ignore = "requires Windows WSL2 microManager; run with --ignored"]
fn commands_run_as_root_in_the_root_home() {
    for (command, expected) in [("id", "root\n"), ("pwd", "/root\n")] {
        let args = if command == "id" {
            vec!["service", "shell", "--", "id", "-un"]
        } else {
            vec!["service", "shell", "pwd"]
        };
        let output = cli(&args, "");
        assert!(output.status.success(), "{}", output.stderr);
        assert_eq!(output.stdout, expected);
    }
}

fn default_shell(verb: &str) {
    let output = cli(
        &["service", verb],
        "printf '__USER__=%s\\n' \"$(id -un)\"\nprintf '__HOME__=%s\\n' \"$PWD\"\nexit 19\n",
    );
    assert_eq!(output.status.code(), Some(19), "{}", output.stderr);
    assert!(
        output.stdout.contains("__USER__=root\n"),
        "{}",
        output.stdout
    );
    assert!(
        output.stdout.contains("__HOME__=/root\n"),
        "{}",
        output.stdout
    );
}

#[test]
#[ignore = "requires Windows WSL2 microManager; run with --ignored"]
fn the_default_shell_reads_stdin_and_preserves_exit_status() {
    default_shell("shell");
}

#[test]
#[ignore = "requires Windows WSL2 microManager; run with --ignored"]
fn the_run_alias_still_opens_the_default_shell() {
    default_shell("run");
}

#[test]
#[ignore = "requires Windows WSL2 microManager; run with --ignored"]
fn literal_arguments_survive_windows_and_wsl_without_shell_expansion() {
    let literals = [
        "\"double\"",
        "",
        "it's here",
        "a b",
        "$HOME",
        "\\path\\",
        "$(printf unsafe)",
        "a;b",
        "한글 소스",
    ];
    let mut args = vec!["service", "shell", "--", "printf", "[%s]\\n"];
    args.extend(literals);
    let output = cli(&args, "");
    assert!(output.status.success(), "{}", output.stderr);
    let expected: String = literals
        .iter()
        .map(|value| format!("[{value}]\n"))
        .collect();
    assert_eq!(output.stdout, expected);
}

#[test]
#[ignore = "requires Windows WSL2 microManager; run with --ignored"]
fn stdout_stderr_and_failure_status_reach_the_host_separately() {
    let output = cli(
        &[
            "service",
            "shell",
            "sh",
            "-c",
            "printf __OUT__; printf __ERR__ >&2; exit 23",
        ],
        "",
    );
    assert_eq!(output.status.code(), Some(23));
    assert_eq!(output.stdout, "__OUT__");
    assert_eq!(output.stderr, "__ERR__");
}

#[test]
#[ignore = "requires Windows WSL2 microManager; run with --ignored"]
fn command_stdin_reaches_the_guest_and_eof_terminates_it() {
    let input = "first line\n한글 입력\nlast line\n";
    let output = cli(&["service", "shell", "--", "cat"], input);
    assert!(output.status.success(), "{}", output.stderr);
    assert_eq!(output.stdout, input);
}

#[test]
#[ignore = "requires Windows WSL2 microManager; run with --ignored"]
fn a_missing_command_fails_and_the_next_shell_still_works() {
    let output = cli(
        &["service", "shell", "firecrab-qa-command-does-not-exist"],
        "",
    );
    assert!(!output.status.success());
    assert!(
        !output.stderr.is_empty(),
        "missing command must report an error"
    );
    let output = cli(&["service", "shell", "true"], "");
    assert!(output.status.success(), "{}", output.stderr);
}

#[test]
#[ignore = "requires Windows WSL2 microManager; run with --ignored"]
fn exiting_a_shell_keeps_the_management_services_and_api_alive() {
    let output = cli(&["service", "shell", "sh", "-c", "exit 7"], "");
    assert_eq!(output.status.code(), Some(7));
    let output = cli(
        &[
            "service",
            "shell",
            "systemctl",
            "is-active",
            "firecrab-api",
            "firecrab-helper",
        ],
        "",
    );
    assert!(output.status.success(), "{}", output.stderr);
    assert_eq!(output.stdout, "active\nactive\n");
    let output = cli(
        &[
            "service",
            "shell",
            "curl",
            "-fsS",
            "http://127.0.0.1:5523/api/host",
        ],
        "",
    );
    assert!(output.status.success(), "{}", output.stderr);
    let host: serde_json::Value = serde_json::from_str(&output.stdout).expect("host API JSON");
    assert!(host.is_object(), "host API must return an object");
}
