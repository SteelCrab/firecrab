use super::*;
use firecrab_api_types::SandboxGuestLimits;
fn command() -> SandboxGuestCommand {
    SandboxGuestCommand {
        version: 1,
        argv: vec![
            "/usr/bin/printf".into(),
            "%s".into(),
            "$(touch /tmp/forbidden)'; echo bad".into(),
        ],
        cwd: ".".into(),
        env: Default::default(),
        stdin: String::new(),
        limits: SandboxGuestLimits {
            wall_seconds: 5,
            cpu_seconds: 5,
            memory_mib: 128,
            output_bytes: 4096,
        },
    }
}
#[test]
fn rejects_versions_path_escape_and_invalid_limits_before_transport() {
    let mut input = command();
    input.version = 2;
    assert!(program(&input).is_err());
    input.version = 1;
    input.cwd = "child/../outside".into();
    assert!(program(&input).is_err());
    input.cwd = ".".into();
    input.limits.output_bytes = 65537;
    assert!(program(&input).is_err());
}
#[test]
fn literal_argv_and_stdin_survive_shell_transport() {
    let dir = tempfile::tempdir().unwrap();
    let mut input = command();
    input.env.insert("LITERAL".into(), "'`$()\nvalue".into());
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(program(&input).unwrap())
        .env_clear()
        .env("HOME", dir.path())
        .env("PATH", "/usr/bin:/bin")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), input.argv[2]);
    input.argv = vec!["/bin/cat".into()];
    input.stdin = "first\n'`$()\nlast\0".into();
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(program(&input).unwrap())
        .env_clear()
        .env("HOME", dir.path())
        .env("PATH", "/usr/bin:/bin")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, input.stdin.as_bytes());
}
#[test]
fn symlink_cwd_cannot_escape_workspace() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), workspace.path().join("escape")).unwrap();
    let mut input = command();
    input.cwd = "escape".into();
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(program(&input).unwrap())
        .env("HOME", workspace.path())
        .current_dir(workspace.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(84));
    assert!(output.stdout.is_empty());
}
#[test]
fn capability_response_requires_version_and_explicit_support() {
    let supported =
        b"version=1\ninit=openrc\nexecution=true\ndocker=false\nnspawn=false\nunsupported=\n";
    assert!(capabilities(supported).unwrap().execution);
    assert!(!capabilities(supported).unwrap().crun);
    assert!(capabilities(&supported[..supported.len() - 1]).is_ok());
    let crun = b"version=1\ninit=openrc\nexecution=true\ndocker=false\ncrun=true\nnspawn=false\nunsupported=\n";
    assert!(capabilities(crun).unwrap().crun);
    let invalid_crun = b"version=1\ninit=openrc\nexecution=true\ndocker=false\ncrun=unknown\nnspawn=false\nunsupported=\n";
    assert!(capabilities(invalid_crun).is_err());
    let unknown = b"version=1\ninit=openrc\nexecution=true\ndocker=false\nother=true\nnspawn=false\nunsupported=\n";
    assert!(capabilities(unknown).is_err());
    let unsupported=b"version=1\ninit=distroless\nexecution=false\ndocker=false\nnspawn=false\nunsupported=missing_setpriv\n";
    assert_eq!(
        capabilities(unsupported).unwrap().unsupported.as_deref(),
        Some("missing_setpriv")
    );
    assert!(capabilities(b"version=2\n").is_err());
}
