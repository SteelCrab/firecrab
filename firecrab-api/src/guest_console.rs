//! Installs the small static resize helper beside the existing metrics agent.
use std::path::Path;

use crate::rootfs::{RootfsError, set_guest_file_mode, write_into_image};

/// Fixed path invoked by the guest metrics service, never a caller-selected command.
pub(crate) const GUEST_PATH: &str = "/usr/local/sbin/firecrab-console-resize";
/// Private key used only to authenticate this VM runtime's application log stream.
const APP_KEY: &str = "/etc/firecrab/app-log.key";
const APP_SCRIPT: &str = "/etc/firecrab/services.d/app";
const WRAPPER_BEGIN: &str = "# >>> firecrab application log";
const WRAPPER_END: &str = "# <<< firecrab application log";

/// Install the app-only supervisor without changing native or fallback init.
/// Returns false when this image does not have an OCI application script.
pub(crate) fn configure_app_log(rootfs: &Path, key: Option<&str>) -> Result<bool, RootfsError> {
    use crate::rootfs::{guest_path_exists, remove_from_image, run_debugfs};
    if guest_path_exists(rootfs, APP_KEY) {
        remove_from_image(rootfs, APP_KEY);
    }
    let Some(key) = key else {
        return Ok(false);
    };
    if key.len() != crate::vm_shim::app_log::KEY_LEN || !key.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(RootfsError::Specialize {
            path: rootfs.to_owned(),
            detail: "invalid app log key".into(),
        });
    }
    if !guest_path_exists(rootfs, APP_SCRIPT) {
        return Ok(false);
    }
    // Older installed helpers cannot supervise output. Report unavailable
    // instead of leaving the shim indefinitely waiting for a connection.
    if !guest_path_exists(rootfs, GUEST_PATH) {
        return Ok(false);
    }
    let helper = run_debugfs(rootfs, &format!("cat {GUEST_PATH}"))?;
    if !supports_app_log(&helper.stdout) {
        return Ok(false);
    }
    let output = run_debugfs(rootfs, &format!("cat {APP_SCRIPT}"))?;
    let script = std::str::from_utf8(&output.stdout).map_err(|_| RootfsError::Specialize {
        path: rootfs.to_owned(),
        detail: "app script is not UTF-8".into(),
    })?;
    let script = instrument_app_script(script).ok_or_else(|| RootfsError::Specialize {
        path: rootfs.to_owned(),
        detail: "app script has no valid interpreter line".into(),
    })?;
    write_into_image(rootfs, APP_SCRIPT, script.as_bytes())?;
    set_guest_file_mode(rootfs, APP_SCRIPT, "0100755");
    write_into_image(rootfs, APP_KEY, key.as_bytes())?;
    set_guest_file_mode(rootfs, APP_KEY, "0100600");
    let stat = run_debugfs(rootfs, &format!("stat {APP_KEY}"))?;
    if !String::from_utf8_lossy(&stat.stdout).contains("Mode:  0600") {
        remove_from_image(rootfs, APP_KEY);
        return Err(RootfsError::Specialize {
            path: rootfs.to_owned(),
            detail: "could not protect app log key".into(),
        });
    }
    Ok(true)
}

fn supports_app_log(binary: &[u8]) -> bool {
    binary
        .windows(b"--app-log-check".len())
        .any(|bytes| bytes == b"--app-log-check")
}

/// Re-exec the original app through the collector exactly once; old helpers fall back.
fn instrument_app_script(script: &str) -> Option<String> {
    if script.contains(WRAPPER_BEGIN) {
        return script.contains(WRAPPER_END).then(|| script.to_owned());
    }
    let (first, rest) = script.split_once('\n')?;
    if !first.starts_with("#!") {
        return None;
    }
    Some(format!(
        "{first}\n{WRAPPER_BEGIN}\nif [ \"${{1-}}\" = --firecrab-app-log-child ]; then\n  shift\nelif [ -r {APP_KEY} ] && {GUEST_PATH} --app-log-check >/dev/null 2>&1; then\n  exec {GUEST_PATH} --app-run \"$0\" --firecrab-app-log-child\nfi\n{WRAPPER_END}\n{rest}"
    ))
}

/// Find the host-bundled helper or an explicit local development build.
pub(crate) fn install(rootfs: &Path) -> Result<(), RootfsError> {
    let path = std::env::var_os("FIRECRAB_GUEST_CONSOLE_BIN")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.join("firecrab-guest-console")))
        });
    let Some(path) = path else {
        return Ok(());
    };
    install_binary(rootfs, &path)
}

/// Refresh the executable on the offline disk; old payloads without it still boot.
fn install_binary(rootfs: &Path, path: &Path) -> Result<(), RootfsError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(RootfsError::Specialize {
                path: rootfs.to_owned(),
                detail: format!("read guest console helper: {error}"),
            });
        }
    };
    if !bytes.starts_with(b"\x7fELF") || bytes.len() > 1024 * 1024 {
        return Err(RootfsError::Specialize {
            path: rootfs.to_owned(),
            detail: "guest console helper must be a small static Linux executable".into(),
        });
    }
    write_into_image(rootfs, GUEST_PATH, &bytes)?;
    set_guest_file_mode(rootfs, GUEST_PATH, "0100755");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn app_wrapper_is_idempotent_and_preserves_the_original_script() {
        let script = "#!/bin/sh\nexport APP_VALUE=42\nexec /bin/app\n";
        let wrapped = instrument_app_script(script).unwrap();
        assert!(wrapped.starts_with("#!/bin/sh\n"));
        assert!(wrapped.contains("--app-log-check"));
        assert!(wrapped.ends_with("export APP_VALUE=42\nexec /bin/app\n"));
        assert_eq!(instrument_app_script(&wrapped).unwrap(), wrapped);
        assert!(instrument_app_script("echo no-interpreter\n").is_none());
        assert!(instrument_app_script("#!/bin/sh\n# >>> firecrab application log\n").is_none());
    }

    #[test]
    fn detects_only_helpers_with_app_log_support() {
        assert!(!supports_app_log(b"\x7fELF old-console-helper"));
        assert!(supports_app_log(b"\x7fELF --app-log-check\0"));
    }

    #[test]
    fn installs_and_refreshes_the_helper_inside_the_guest_with_executable_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("guest.ext4");
        assert!(
            std::process::Command::new("mkfs.ext4")
                .args(["-q", "-F"])
                .arg(&image)
                .arg("8M")
                .status()
                .unwrap()
                .success()
        );
        for path in ["/usr", "/usr/local", "/usr/local/sbin"] {
            crate::rootfs::run_debugfs(&image, &format!("mkdir {path}")).unwrap();
        }
        let payload = dir.path().join("helper");
        for bytes in [b"\x7fELFfirst".as_slice(), b"\x7fELFsecond".as_slice()] {
            std::fs::write(&payload, bytes).unwrap();
            install_binary(&image, &payload).unwrap();
            let output = crate::rootfs::run_debugfs(&image, &format!("cat {GUEST_PATH}")).unwrap();
            assert_eq!(output.stdout, bytes);
            let output = crate::rootfs::run_debugfs(&image, &format!("stat {GUEST_PATH}")).unwrap();
            assert!(String::from_utf8_lossy(&output.stdout).contains("0755"));
        }
        std::fs::write(&payload, b"not an ELF").unwrap();
        assert!(install_binary(&image, &payload).is_err());
    }
}
