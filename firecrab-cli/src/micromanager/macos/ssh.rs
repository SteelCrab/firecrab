//! One policy for command sessions and the resident management tunnel.

use std::ffi::OsString;
use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::micromanager::{
    manifest,
    runtime::{Runtime, Ssh},
};

pub fn load(home: &Path) -> Result<Ssh, manifest::Error> {
    let settings = Runtime::load(home)?.ssh;
    prepare(&home.join("runtime"), &settings)?;
    Ok(settings)
}

pub fn prepare(runtime: &Path, settings: &Ssh) -> Result<(), manifest::Error> {
    if !settings.reuse_connections {
        return Ok(());
    }
    crate::micromanager::managed_home::reject_symlink_components(runtime)?;
    let directory = socket_directory(runtime);
    let result = (|| {
        let owner = fs::metadata(runtime)?.uid();
        match DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let metadata = fs::symlink_metadata(&directory)?;
        if !metadata.is_dir()
            || metadata.uid() != owner
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "SSH control directory must be a private directory owned by the managed home's user",
            ));
        }
        Ok(())
    })();
    result.map_err(|source| manifest::Error::Io {
        action: "prepare SSH control directory",
        path: directory,
        source,
    })
}

fn socket_directory(runtime: &Path) -> PathBuf {
    // Unix-domain sockets have a short path limit. Managed homes can be much
    // longer, so use a private, deterministic directory outside the VM share.
    let hash = format!("{:x}", Sha256::digest(runtime.as_os_str().as_bytes()));
    Path::new("/private/tmp").join(format!("fc-ssh-{}", &hash[..16]))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Session {
    Command,
    Tunnel,
    Forward,
}

pub fn arguments(runtime: &Path, settings: &Ssh, session: Session) -> Vec<OsString> {
    let mut arguments = Vec::new();
    for option in [
        format!("ConnectTimeout={}", settings.connect_timeout_secs),
        format!("ServerAliveInterval={}", settings.keepalive_interval_secs),
        format!("ServerAliveCountMax={}", settings.keepalive_count_max),
    ] {
        arguments.extend(["-o".into(), option.into()]);
    }
    if settings.reuse_connections && session != Session::Forward {
        for option in [
            format!(
                "ControlMaster={}",
                if session == Session::Tunnel {
                    "yes"
                } else {
                    "auto"
                }
            ),
            format!("ControlPath={}/%C", socket_directory(runtime).display()),
            // An attached `ssh -N` exits immediately. The API tunnel must own
            // its transport; command sessions may share that master.
            format!(
                "ControlPersist={}",
                if session == Session::Tunnel {
                    "no".into()
                } else {
                    settings.persist_secs.to_string()
                }
            ),
        ] {
            arguments.extend(["-o".into(), option.into()]);
        }
    } else {
        // Mux forwards outlive their requesting client. A TCP relay must own
        // its transport so stopping it releases the listening port.
        // A per-user ssh_config must not override an explicit opt-out either.
        arguments.extend([
            "-o".into(),
            "ControlPath=none".into(),
            "-o".into(),
            "ControlMaster=no".into(),
            "-o".into(),
            "ControlPersist=no".into(),
        ]);
    }
    arguments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openssh_applies_configured_policy_and_can_disable_reuse() {
        let runtime = Path::new("/a very long managed home/runtime");
        for reuse in [true, false] {
            let settings = Ssh {
                connect_timeout_secs: 23,
                keepalive_interval_secs: 31,
                keepalive_count_max: 7,
                reuse_connections: reuse,
                ..Ssh::default()
            };
            let output = std::process::Command::new("/usr/bin/ssh")
                .args(["-G", "-F", "/dev/null"])
                .args(arguments(runtime, &settings, Session::Command))
                .arg("root@192.0.2.7")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let config = String::from_utf8(output.stdout).unwrap();
            for expected in [
                "connecttimeout 23",
                "serveraliveinterval 31",
                "serveralivecountmax 7",
            ] {
                assert!(config.lines().any(|line| line == expected), "{config}");
            }
            assert_eq!(config.contains("controlmaster auto"), reuse);
            assert_eq!(config.contains("/private/tmp/fc-ssh-"), reuse);
        }
    }

    #[test]
    fn resident_tunnels_own_their_transport_and_forwards_do_not_use_a_master() {
        for (session, expected) in [
            (Session::Tunnel, "controlmaster true"),
            (Session::Forward, "controlmaster false"),
        ] {
            let output = std::process::Command::new("/usr/bin/ssh")
                .args(["-G", "-F", "/dev/null"])
                .args(arguments(Path::new("/runtime"), &Ssh::default(), session))
                .arg("root@192.0.2.7")
                .output()
                .unwrap();
            assert!(output.status.success());
            let config = String::from_utf8(output.stdout).unwrap();
            assert!(config.lines().any(|line| line == expected), "{config}");
            assert!(config.contains("controlpersist no"), "{config}");
        }
    }

    #[test]
    fn control_sockets_are_short_private_and_distinct_for_each_home() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root
            .path()
            .join("very long managed home with spaces")
            .join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        let settings = Ssh::default();
        prepare(&runtime, &settings).unwrap();
        let directory = socket_directory(&runtime);
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(directory.as_os_str().len() + 41 + 17 < 104);
        assert_ne!(directory, socket_directory(&runtime.join("other")));
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn an_existing_shared_or_symlinked_control_directory_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let directory = socket_directory(root.path());
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(prepare(root.path(), &Ssh::default()).is_err());
        fs::remove_dir(&directory).unwrap();
        std::os::unix::fs::symlink(root.path(), &directory).unwrap();
        assert!(prepare(root.path(), &Ssh::default()).is_err());
        fs::remove_file(directory).unwrap();
    }
}
