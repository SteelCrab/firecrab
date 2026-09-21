//! Native init detection and registration on the unpacked, untrusted guest tree.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InitSystem {
    Systemd(&'static str),
    OpenRc(&'static str),
    BusyBox,
}

impl InitSystem {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Systemd(_) => "systemd\n",
            Self::OpenRc(_) => "openrc\n",
            Self::BusyBox => "busybox\n",
        }
    }
}

/// Resolve every component, including absolute symlinks, within the guest root.
/// Detection must neither follow host paths nor create directories.
fn guest_file(tree: &Path, guest: &str) -> Result<Option<PathBuf>, ResolveError> {
    let mut pending: Vec<String> = guest.split('/').rev().map(str::to_owned).collect();
    let mut resolved = PathBuf::new();
    let mut hops = 0;
    while let Some(part) = pending.pop() {
        match part.as_str() {
            "" | "." => continue,
            ".." => {
                resolved.pop();
                continue;
            }
            _ => {}
        }
        let candidate = tree.join(&resolved).join(&part);
        let metadata = match fs::symlink_metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(injection_io("detect guest init", candidate, error)),
        };
        if metadata.file_type().is_symlink() {
            hops += 1;
            if hops > SYMLINK_HOP_LIMIT {
                return Ok(None);
            }
            let target = fs::read_link(&candidate)
                .map_err(|error| injection_io("read guest init link", candidate, error))?;
            if target.is_absolute() {
                resolved.clear();
            }
            pending.extend(target.to_string_lossy().split('/').rev().map(str::to_owned));
        } else {
            resolved.push(part);
            if pending.is_empty() {
                return Ok(metadata.is_file().then(|| tree.join(&resolved)));
            }
            if !metadata.is_dir() {
                return Ok(None);
            }
        }
    }
    Ok(None)
}

fn executable(tree: &Path, guest: &str) -> Result<bool, ResolveError> {
    let Some(path) = guest_file(tree, guest)? else {
        return Ok(false);
    };
    let metadata =
        fs::metadata(&path).map_err(|error| injection_io("stat guest init", path, error))?;
    Ok(metadata.permissions().mode() & 0o111 != 0)
}

pub(super) fn detect(tree: &Path) -> Result<InitSystem, ResolveError> {
    for path in ["/usr/lib/systemd/systemd", "/lib/systemd/systemd"] {
        if executable(tree, path)? {
            return Ok(InitSystem::Systemd(path));
        }
    }
    // An init.d directory alone also occurs on SysV and slim Debian images.
    for runner in ["/sbin/openrc-run", "/usr/sbin/openrc-run"] {
        if !executable(tree, runner)? {
            continue;
        }
        if executable(tree, "/sbin/openrc-init")? || executable(tree, "/usr/sbin/openrc-init")? {
            return Ok(InitSystem::OpenRc(runner));
        }
        // Alpine commonly uses BusyBox init to enter the OpenRC runlevels.
        if (executable(tree, "/sbin/openrc")? || executable(tree, "/usr/sbin/openrc")?)
            && executable(tree, GUEST_INIT)?
            && let Some(path) = guest_file(tree, GUEST_INITTAB)?
        {
            let table = fs::read_to_string(&path)
                .map_err(|error| injection_io("read OpenRC inittab", path, error))?;
            if table
                .lines()
                .any(|line| !line.trim_start().starts_with('#') && line.contains("openrc"))
            {
                return Ok(InitSystem::OpenRc(runner));
            }
        }
    }
    Ok(InitSystem::BusyBox)
}

pub(super) fn install(
    tree: &Path,
    init: InitSystem,
    unwind: &mut InjectedPaths,
) -> Result<(), ResolveError> {
    match init {
        InitSystem::BusyBox => install_symlink(tree, GUEST_INIT, GUEST_TOOLBOX, unwind),
        InitSystem::Systemd(binary) => {
            if guest_file(tree, GUEST_INIT)? != guest_file(tree, binary)? {
                install_symlink(tree, GUEST_INIT, binary, unwind)?;
            }
            for (name, body) in [
                ("firecrab-agent", SYSTEMD_AGENT),
                ("firecrab-oci", SYSTEMD_BOOT),
            ] {
                let unit = format!("/etc/systemd/system/{name}.service");
                install_file(tree, &unit, body.as_bytes(), 0o644, unwind)?;
                install_symlink(
                    tree,
                    &format!("/etc/systemd/system/multi-user.target.wants/{name}.service"),
                    &unit,
                    unwind,
                )?;
            }
            Ok(())
        }
        InitSystem::OpenRc(runner) => {
            // openrc-init owns runlevel startup itself. If available, select it
            // even when the container has an unrelated /sbin/init executable.
            for binary in ["/sbin/openrc-init", "/usr/sbin/openrc-init"] {
                if executable(tree, binary)? {
                    if guest_file(tree, GUEST_INIT)? != guest_file(tree, binary)? {
                        install_symlink(tree, GUEST_INIT, binary, unwind)?;
                    }
                    break;
                }
            }
            for (name, body) in [
                ("firecrab-agent", OPENRC_AGENT),
                ("firecrab-oci", OPENRC_BOOT),
            ] {
                let script = format!("#!{runner}\n{body}");
                let path = format!("/etc/init.d/{name}");
                install_file(tree, &path, script.as_bytes(), 0o755, unwind)?;
                install_symlink(
                    tree,
                    &format!("/etc/runlevels/default/{name}"),
                    &path,
                    unwind,
                )?;
            }
            Ok(())
        }
    }
}

const SYSTEMD_AGENT: &str = r#"[Unit]
Description=Firecrab Metrics Agent
After=local-fs.target

[Service]
Type=simple
ExecStart=/etc/firecrab/busybox sh /usr/local/sbin/firecrab-guest-agent
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
"#;

// RemainAfterExit retains the bootstrap worker and application in this cgroup.
const SYSTEMD_BOOT: &str = r#"[Unit]
Description=Firecrab OCI bootstrap
Wants=firecrab-agent.service network.target
After=local-fs.target network.target

[Service]
Type=oneshot
ExecStart=/etc/firecrab/busybox sh /etc/firecrab/rc.boot
RemainAfterExit=yes
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=multi-user.target
"#;

const OPENRC_AGENT: &str = r#"description="Firecrab Metrics Agent"
command="/etc/firecrab/busybox"
command_args="sh /usr/local/sbin/firecrab-guest-agent"
command_background=true
pidfile="/run/firecrab-agent.openrc.pid"
depend() {
    need localmount
}
"#;

const OPENRC_BOOT: &str = r#"description="Firecrab OCI bootstrap"
depend() {
    need localmount
    want firecrab-agent
    after net
}
start() {
    /etc/firecrab/busybox sh /etc/firecrab/rc.boot
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn program(tree: &Path, path: &str) {
        let path = tree.join(path.trim_start_matches('/'));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"native program").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn detection_follows_usr_merge_and_guest_absolute_links() {
        let tree = tempfile::tempdir().unwrap();
        program(tree.path(), "/usr/lib/systemd/systemd");
        std::os::unix::fs::symlink("/usr/lib", tree.path().join("lib")).unwrap();
        assert_eq!(
            guest_file(tree.path(), "/lib/systemd/systemd").unwrap(),
            Some(tree.path().join("usr/lib/systemd/systemd"))
        );
        assert!(matches!(
            detect(tree.path()).unwrap(),
            InitSystem::Systemd(_)
        ));
    }

    #[test]
    fn detection_does_not_follow_links_to_host_programs() {
        let tree = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        program(outside.path(), "/systemd/systemd");
        fs::create_dir_all(tree.path().join("usr/lib")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("systemd"),
            tree.path().join("usr/lib/systemd"),
        )
        .unwrap();
        let before = fs::read_dir(tree.path()).unwrap().count();
        assert_eq!(detect(tree.path()).unwrap(), InitSystem::BusyBox);
        assert_eq!(fs::read_dir(tree.path()).unwrap().count(), before);
    }

    #[test]
    fn openrc_init_is_selected_without_an_inittab_and_registration_unwinds() {
        let tree = tempfile::tempdir().unwrap();
        program(tree.path(), "/usr/sbin/openrc-run");
        program(tree.path(), "/usr/sbin/openrc-init");
        let init = detect(tree.path()).unwrap();
        assert_eq!(init, InitSystem::OpenRc("/usr/sbin/openrc-run"));
        {
            let mut unwind = InjectedPaths::default();
            install(tree.path(), init, &mut unwind).unwrap();
            assert_eq!(
                fs::read_link(tree.path().join("sbin/init")).unwrap(),
                Path::new("/usr/sbin/openrc-init")
            );
            assert!(
                fs::read_to_string(tree.path().join("etc/init.d/firecrab-agent"))
                    .unwrap()
                    .starts_with("#!/usr/sbin/openrc-run\n")
            );
            // A later injection failure drops the journal without keep().
        }
        assert!(!tree.path().join("etc").exists());
        assert!(!tree.path().join("sbin").exists());
        assert!(tree.path().join("usr/sbin/openrc-init").is_file());
    }

    #[test]
    fn non_executable_systemd_is_not_an_init() {
        let tree = tempfile::tempdir().unwrap();
        program(tree.path(), "/usr/lib/systemd/systemd");
        fs::set_permissions(
            tree.path().join("usr/lib/systemd/systemd"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert_eq!(detect(tree.path()).unwrap(), InitSystem::BusyBox);
    }

    #[test]
    fn boot_and_openrc_scripts_have_valid_shell_syntax() {
        use std::io::Write;
        for script in [
            boot_script_for(true),
            boot_script_for(false),
            OPENRC_AGENT.into(),
            OPENRC_BOOT.into(),
        ] {
            let mut child = std::process::Command::new("sh")
                .arg("-n")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(script.as_bytes())
                .unwrap();
            assert!(child.wait().unwrap().success());
        }
    }
}
