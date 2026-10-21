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
            install_serial_console(tree, unwind)?;
            // Firecrab owns ttyS0 on every path. An image getty there would
            // ask for a root password a container image never set, and two
            // programs reading one terminal split its input between them.
            for getty in ["serial-getty@ttyS0.service", "console-getty.service"] {
                install_symlink(
                    tree,
                    &format!("/etc/systemd/system/{getty}"),
                    "/dev/null",
                    unwind,
                )?;
            }
            for (name, body) in [
                ("firecrab-agent", SYSTEMD_AGENT),
                ("firecrab-oci", SYSTEMD_BOOT),
                ("firecrab-console", SYSTEMD_CONSOLE),
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
            let mut services = vec![
                ("firecrab-agent", OPENRC_AGENT),
                ("firecrab-oci", OPENRC_BOOT),
            ];
            // An inittab line cannot be masked without rewriting the native
            // inittab, so an image that already runs something on ttyS0
            // keeps it.
            if !openrc_serves_serial(tree)? {
                install_serial_console(tree, unwind)?;
                services.push(("firecrab-console", OPENRC_CONSOLE));
            }
            for (name, body) in services {
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

fn install_serial_console(tree: &Path, unwind: &mut InjectedPaths) -> Result<(), ResolveError> {
    install_file(
        tree,
        GUEST_SERIAL_SCRIPT,
        serial_console_script().as_bytes(),
        0o755,
        unwind,
    )
}

/// True when the image's inittab or default runlevel already runs something
/// on the serial console.
fn openrc_serves_serial(tree: &Path) -> Result<bool, ResolveError> {
    if let Some(path) = guest_file(tree, GUEST_INITTAB)? {
        let table = fs::read_to_string(&path)
            .map_err(|error| injection_io("read OpenRC inittab", path, error))?;
        if table
            .lines()
            .any(|line| !line.trim_start().starts_with('#') && line.contains("ttyS0"))
        {
            return Ok(true);
        }
    }
    Ok(guest_file(tree, "/etc/runlevels/default/agetty.ttyS0")?.is_some())
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

// Restart=always brings the console back after `exit`, like BusyBox respawn.
const SYSTEMD_CONSOLE: &str = r#"[Unit]
Description=Firecrab serial console
After=systemd-user-sessions.service

[Service]
ExecStart=/etc/firecrab/busybox sh /etc/firecrab/rc.serial
Restart=always
RestartSec=0

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

// supervise-daemon respawns it after `exit`; 0 lifts its restart limit.
const OPENRC_CONSOLE: &str = r#"description="Firecrab serial console"
supervisor=supervise-daemon
command="/etc/firecrab/busybox"
command_args="sh /etc/firecrab/rc.serial"
respawn_delay=1
respawn_max=0
depend() {
    need localmount
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
    fn systemd_serves_the_serial_console_through_firecrab() {
        let tree = tempfile::tempdir().unwrap();
        program(tree.path(), "/usr/lib/systemd/systemd");
        let init = detect(tree.path()).unwrap();
        let mut unwind = InjectedPaths::default();
        install(tree.path(), init, &mut unwind).unwrap();
        unwind.keep();
        let units = tree.path().join("etc/systemd/system");
        assert_eq!(
            fs::read_link(units.join("multi-user.target.wants/firecrab-console.service")).unwrap(),
            Path::new("/etc/systemd/system/firecrab-console.service")
        );
        for getty in ["serial-getty@ttyS0.service", "console-getty.service"] {
            assert_eq!(
                fs::read_link(units.join(getty)).unwrap(),
                Path::new("/dev/null")
            );
        }
        let script = fs::read_to_string(tree.path().join("etc/firecrab/rc.serial")).unwrap();
        assert!(script.contains("exec </dev/ttyS0 >/dev/ttyS0 2>&1"));
        assert!(script.contains("--autologin root"));
    }

    #[test]
    fn openrc_gets_a_serial_console_unless_the_image_runs_one() {
        let commented =
            "::wait:/sbin/openrc default\n#ttyS0::respawn:/sbin/getty -L 115200 ttyS0\n";
        let active = "::wait:/sbin/openrc default\nttyS0::respawn:/sbin/getty -L 115200 ttyS0\n";
        let plain = "::wait:/sbin/openrc default\n";
        for (inittab, runlevel_getty, console) in [
            (commented, false, true),
            (active, false, false),
            (plain, true, false),
        ] {
            let tree = tempfile::tempdir().unwrap();
            for path in ["/sbin/openrc-run", "/sbin/openrc", "/bin/busybox"] {
                program(tree.path(), path);
            }
            std::os::unix::fs::symlink("/bin/busybox", tree.path().join("sbin/init")).unwrap();
            fs::create_dir_all(tree.path().join("etc")).unwrap();
            fs::write(tree.path().join("etc/inittab"), inittab).unwrap();
            if runlevel_getty {
                program(tree.path(), "/etc/init.d/agetty.ttyS0");
                fs::create_dir_all(tree.path().join("etc/runlevels/default")).unwrap();
                std::os::unix::fs::symlink(
                    "/etc/init.d/agetty.ttyS0",
                    tree.path().join("etc/runlevels/default/agetty.ttyS0"),
                )
                .unwrap();
            }
            let init = detect(tree.path()).unwrap();
            assert_eq!(init, InitSystem::OpenRc("/sbin/openrc-run"));
            let mut unwind = InjectedPaths::default();
            install(tree.path(), init, &mut unwind).unwrap();
            unwind.keep();
            let link = tree.path().join("etc/runlevels/default/firecrab-console");
            assert_eq!(link.is_symlink(), console, "{inittab:?}");
            assert_eq!(tree.path().join("etc/firecrab/rc.serial").exists(), console);
            assert_eq!(
                fs::read_to_string(tree.path().join("etc/inittab")).unwrap(),
                inittab,
                "the native inittab is never rewritten"
            );
        }
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
            serial_console_script(),
            OPENRC_AGENT.into(),
            OPENRC_BOOT.into(),
            OPENRC_CONSOLE.into(),
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
