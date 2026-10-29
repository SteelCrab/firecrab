//! Validation and literal argv encoding for the authenticated V1 guest channel.
use firecrab_api_types::{SandboxGuestCapabilities, SandboxGuestCommand};
use std::{io, path::Component};

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// Quote one shell transport value. The resulting program still receives an exact argv.
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Validate before staging, saving an invocation, or contacting the guest.
pub fn validate(command: &SandboxGuestCommand) -> io::Result<()> {
    if command.version != 1 {
        return Err(invalid("Unsupported guest protocol version"));
    }
    if command.argv.is_empty()
        || command.argv.len() > 128
        || command.argv[0].is_empty()
        || command.argv[0].starts_with('-')
        || command.argv[0].contains('=')
        || command.argv.iter().any(|arg| arg.contains('\0'))
        || command.argv.iter().map(String::len).sum::<usize>() > 8192
    {
        return Err(invalid(
            "Guest argv requires 1–128 arguments and at most 8192 bytes without NUL",
        ));
    }
    if command.cwd.len() > 1024
        || command.cwd.contains('\0')
        || command.cwd.starts_with('/')
        || std::path::Path::new(&command.cwd).components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(invalid(
            "Guest cwd must remain inside the admitted workspace",
        ));
    }
    if command.env.len() > 64
        || command.env.iter().any(|(key, value)| {
            key.is_empty()
                || key.len() > 128
                || !key.bytes().enumerate().all(|(index, b)| {
                    b.is_ascii_alphabetic() || b == b'_' || (index > 0 && b.is_ascii_digit())
                })
                || value.contains('\0')
        })
        || command
            .env
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>()
            > 16384
    {
        return Err(invalid("Invalid or excessive explicit guest environment"));
    }
    if command.stdin.len() > 65536 {
        return Err(invalid("Guest stdin exceeds 64 KiB"));
    }
    let limits = &command.limits;
    if !(1..=120).contains(&limits.wall_seconds)
        || !(1..=120).contains(&limits.cpu_seconds)
        || !(16..=65536).contains(&limits.memory_mib)
        || !(1..=65536).contains(&limits.output_bytes)
    {
        return Err(invalid("Unsupported guest execution limits"));
    }
    Ok(())
}

/// Transport is shell-based; no untrusted value becomes shell syntax or a host command.
pub fn program(command: &SandboxGuestCommand) -> io::Result<String> {
    validate(command)?;
    use base64::{Engine, engine::general_purpose::STANDARD};
    let mut text = format!(
        "set -eu\nworkspace=$(pwd -P)\nCDPATH= cd -- {}\nresolved=$(pwd -P)\ncase \"$resolved/\" in \"$workspace/\"*) ;; *) printf 'Guest cwd escaped the workspace\\n' >&2; exit 84;; esac\n",
        quote(if command.cwd.is_empty() {
            "."
        } else {
            &command.cwd
        })
    );
    // A protected per-invocation file supplies stdin; neither argv nor shell command substitution does.
    text.push_str(&format!("stdin_path=\"$HOME/.firecrab-stdin-$$\"\numask 077\nset -C\nprintf '%s' {} | base64 -d >\"$stdin_path\"\nset +C\nexec 8<\"$stdin_path\"\nrm -f -- \"$stdin_path\"\nexec env -i -- HOME=\"$HOME\" PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",quote(&STANDARD.encode(&command.stdin))));
    for (key, value) in &command.env {
        text.push(' ');
        text.push_str(&quote(&format!("{key}={value}")));
    }
    for arg in &command.argv {
        text.push(' ');
        text.push_str(&quote(arg));
    }
    text.push_str(" <&8\n");
    Ok(text)
}

/// Reject malformed/mismatched replies instead of treating a successful SSH connection as readiness.
pub fn capabilities(bytes: &[u8]) -> io::Result<SandboxGuestCapabilities> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| invalid("Invalid guest capability response"))?;
    let mut values = std::collections::BTreeMap::new();
    for line in text.lines() {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| invalid("Invalid guest capability response"))?;
        if values.insert(key, value).is_some() {
            return Err(invalid("Duplicate guest capability field"));
        }
    }
    if !(6..=7).contains(&values.len())
        || values.get("version") != Some(&"1")
        || !values.contains_key("unsupported")
        || values.keys().any(|key| {
            !matches!(
                *key,
                "version" | "init" | "execution" | "docker" | "crun" | "nspawn" | "unsupported"
            )
        })
    {
        return Err(invalid("Unsupported guest protocol response"));
    }
    let init = *values
        .get("init")
        .ok_or_else(|| invalid("Guest init capability is missing"))?;
    if !matches!(init, "systemd" | "openrc" | "distroless") {
        return Err(invalid("Unsupported guest init capability"));
    }
    let flag = |key| match values.get(key) {
        Some(&"true") => Ok(true),
        Some(&"false") => Ok(false),
        _ => Err(invalid("Invalid guest capability flag")),
    };
    let execution = flag("execution")?;
    let reason = values
        .get("unsupported")
        .filter(|value| !value.is_empty())
        .map(|value| (*value).to_string());
    if execution == reason.is_some() {
        return Err(invalid("Inconsistent guest execution capability"));
    }
    Ok(SandboxGuestCapabilities {
        version: 1,
        init: init.into(),
        execution,
        docker: flag("docker")?,
        crun: if values.contains_key("crun") {
            flag("crun")?
        } else {
            false
        },
        nspawn: flag("nspawn")?,
        unsupported: reason,
    })
}

#[cfg(test)]
mod tests {
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
}
