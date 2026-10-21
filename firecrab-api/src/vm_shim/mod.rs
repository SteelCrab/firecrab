//! The per-VM shim: a small process that owns one Firecracker child and
//! serves its console, control, and exit status on a Unix socket, so the
//! API controls a VM without being the Firecracker process's parent.
//!
//! It runs as a subcommand of this binary (`firecrab-api vm-shim …`) in the
//! VM's own systemd unit, rather than as a separate executable, so it is
//! always the same version as the installed API and needs nothing new from
//! packaging or self-update.

pub(crate) mod client;
pub(crate) mod protocol;
pub(crate) mod server;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use thiserror::Error;
use uuid::Uuid;

use crate::artifacts::HostRuntimePaths;
use server::ShimConfig;

/// `argv[1]` that turns this binary into a shim.
pub(crate) const SUBCOMMAND: &str = "vm-shim";

/// A shim command line that cannot be run.
#[derive(Debug, Error)]
pub(crate) enum ShimArgsError {
    /// A required flag was absent.
    #[error("missing {0}")]
    Missing(&'static str),
    /// A flag had no value after it.
    #[error("{0} needs a value")]
    NoValue(&'static str),
    /// A flag this shim does not know.
    #[error("unknown argument {0:?}")]
    Unknown(OsString),
    /// A value that does not parse.
    #[error("invalid {flag}: {reason}")]
    Invalid {
        /// The flag whose value was rejected.
        flag: &'static str,
        /// Why.
        reason: String,
    },
}

/// The arguments after `vm-shim` that make the shim run `config`: the
/// command line the network helper builds for a VM unit, kept here so tests
/// can round-trip it through [`parse_args`].
///
/// The runtime directory is made absolute against this process's working
/// directory: the default storage root is relative (`data/vms`), and the
/// shim must not depend on being started from the same place.
#[cfg(test)]
pub(crate) fn command_args(config: &ShimConfig) -> Vec<OsString> {
    let runtime_dir =
        std::path::absolute(&config.runtime.dir).unwrap_or_else(|_| config.runtime.dir.clone());
    let mut args = vec![
        OsString::from("--vm-id"),
        OsString::from(config.vm_id.to_string()),
        OsString::from("--runtime-dir"),
        runtime_dir.into_os_string(),
        OsString::from("--firecracker"),
        config.firecracker.clone().into_os_string(),
        OsString::from("--stop-grace-ms"),
        OsString::from(config.stop_grace.as_millis().to_string()),
    ];
    if config.enable_pci {
        args.push(OsString::from("--enable-pci"));
    }
    args
}

/// Parses the arguments after `vm-shim`. Every runtime path is derived from
/// `--runtime-dir`, the same way the API derives them, so the two can never
/// disagree on where a socket lives.
pub(crate) fn parse_args(
    args: impl IntoIterator<Item = OsString>,
) -> Result<ShimConfig, ShimArgsError> {
    let mut vm_id = None;
    let mut runtime_dir = None;
    let mut firecracker = None;
    let mut stop_grace = None;
    let mut enable_pci = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |flag: &'static str| args.next().ok_or(ShimArgsError::NoValue(flag));
        match arg.to_str() {
            Some("--vm-id") => {
                let raw = value("--vm-id")?;
                let parsed = raw
                    .to_str()
                    .and_then(|text| Uuid::parse_str(text).ok())
                    .ok_or_else(|| ShimArgsError::Invalid {
                        flag: "--vm-id",
                        reason: format!("{raw:?} is not a UUID"),
                    })?;
                vm_id = Some(parsed);
            }
            Some("--runtime-dir") => runtime_dir = Some(PathBuf::from(value("--runtime-dir")?)),
            Some("--firecracker") => firecracker = Some(PathBuf::from(value("--firecracker")?)),
            Some("--stop-grace-ms") => {
                let raw = value("--stop-grace-ms")?;
                let millis = raw
                    .to_str()
                    .and_then(|text| text.parse::<u64>().ok())
                    .ok_or_else(|| ShimArgsError::Invalid {
                        flag: "--stop-grace-ms",
                        reason: format!("{raw:?} is not a number of milliseconds"),
                    })?;
                stop_grace = Some(Duration::from_millis(millis));
            }
            Some("--enable-pci") => enable_pci = true,
            _ => return Err(ShimArgsError::Unknown(arg)),
        }
    }
    let runtime_dir = runtime_dir.ok_or(ShimArgsError::Missing("--runtime-dir"))?;
    if !runtime_dir.is_absolute() {
        return Err(ShimArgsError::Invalid {
            flag: "--runtime-dir",
            reason: format!("{} is not absolute", runtime_dir.display()),
        });
    }
    Ok(ShimConfig {
        vm_id: vm_id.ok_or(ShimArgsError::Missing("--vm-id"))?,
        runtime: HostRuntimePaths::in_dir(runtime_dir),
        firecracker: firecracker.ok_or(ShimArgsError::Missing("--firecracker"))?,
        enable_pci,
        stop_grace: stop_grace.ok_or(ShimArgsError::Missing("--stop-grace-ms"))?,
    })
}

/// Entry point for `firecrab-api vm-shim …`: runs one VM until Firecracker
/// exits. Exits 0 only when the VM ended cleanly, so a service manager sees
/// a crashed VM as a failed unit.
pub(crate) fn run(args: Vec<OsString>) -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let config = match parse_args(args) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("[ERROR] vm-shim: {error}");
            return ExitCode::from(2);
        }
    };
    if let Err(reason) = check_private_dir(&config.runtime.dir) {
        // Where the API looks for why a start failed.
        let _ = std::fs::write(&config.runtime.shim_error, &reason);
        eprintln!("[ERROR] vm-shim: {reason}");
        return ExitCode::FAILURE;
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("[ERROR] vm-shim: failed to start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    // SIGTERM is watched from the first poll, before Firecracker exists, so
    // a service manager stopping the shim early still gets a recorded exit.
    let outcome = runtime.block_on(async {
        let terminate = terminate_requested();
        server::serve(config, terminate).await
    });
    match outcome {
        // A stop that was asked for is a success even though Firecracker died
        // of the SIGTERM; otherwise every normal stop would fail its unit.
        Ok(exit) if exit.status.clean() || exit.stop_requested => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("[ERROR] vm-shim: failed to run Firecracker: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The runtime directory must be this user's own private directory, not
/// reached through a symlink. The privileged helper that starts a systemd
/// unit cannot look inside the API's directories, so the shim, running as
/// the API's user, checks the directory it was handed before using it.
fn check_private_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    let refuse = |reason: &str| format!("runtime directory {}: {reason}", dir.display());
    let metadata = std::fs::symlink_metadata(dir).map_err(|error| refuse(&error.to_string()))?;
    if !metadata.file_type().is_dir() {
        return Err(refuse("is not a directory (or is a symlink)"));
    }
    // SAFETY: geteuid has no failure mode.
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(refuse("is not owned by this user"));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(refuse("must not be accessible to group or others"));
    }
    Ok(())
}

/// Starts watching for SIGTERM (a service manager stopping the shim, or the
/// API's parent-death signal) immediately, and returns a future that
/// resolves when one arrives.
fn terminate_requested() -> impl std::future::Future<Output = ()> {
    let watcher = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
    async move {
        match watcher {
            Ok(mut terminate) => {
                terminate.recv().await;
            }
            Err(error) => {
                tracing::warn!(%error, "cannot watch for SIGTERM; only a client can stop this VM");
                std::future::pending::<()>().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::time::Duration;

    use uuid::Uuid;

    use super::*;
    use crate::artifacts::HostRuntimePaths;
    use crate::vm_shim::server::ShimConfig;

    fn config() -> ShimConfig {
        ShimConfig {
            vm_id: Uuid::new_v4(),
            runtime: HostRuntimePaths::in_dir(PathBuf::from("/var/lib/firecrab/vms/x/r/y")),
            firecracker: PathBuf::from("/usr/local/bin/firecracker"),
            enable_pci: true,
            stop_grace: Duration::from_millis(5000),
        }
    }

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn a_config_round_trips_through_its_command_line() {
        for enable_pci in [true, false] {
            let mut original = config();
            original.enable_pci = enable_pci;
            assert_eq!(parse_args(command_args(&original)).unwrap(), original);
        }
    }

    /// The default storage root is relative to the API's working directory
    /// (`data/vms`); the shim must still get an absolute directory, since a
    /// service manager may start it somewhere else.
    #[test]
    fn a_relative_runtime_directory_reaches_the_shim_as_an_absolute_one() {
        let mut original = config();
        original.runtime = HostRuntimePaths::in_dir(PathBuf::from("data/vms/a/r/b"));

        let parsed = parse_args(command_args(&original)).unwrap();

        let expected = std::env::current_dir().unwrap().join("data/vms/a/r/b");
        assert_eq!(parsed.runtime, HostRuntimePaths::in_dir(expected));
    }

    #[test]
    fn a_missing_flag_is_rejected() {
        let error = parse_args(args(&["--vm-id", &Uuid::new_v4().to_string()])).unwrap_err();
        assert!(error.to_string().contains("--runtime-dir"), "{error}");
    }

    #[test]
    fn a_malformed_vm_id_is_rejected() {
        let mut line = command_args(&config());
        let position = line.iter().position(|arg| arg == "--vm-id").unwrap();
        line[position + 1] = OsString::from("not-a-uuid");
        assert!(parse_args(line).is_err());
    }

    #[test]
    fn a_relative_runtime_directory_is_rejected() {
        let mut line = command_args(&config());
        let position = line.iter().position(|arg| arg == "--runtime-dir").unwrap();
        line[position + 1] = OsString::from("relative/dir");
        assert!(parse_args(line).is_err());
    }

    #[test]
    fn an_unknown_flag_is_rejected() {
        let mut line = command_args(&config());
        line.push(OsString::from("--surprise"));
        assert!(parse_args(line).is_err());
    }

    fn private_dir() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        directory
    }

    #[test]
    fn a_private_runtime_directory_of_this_user_is_accepted() {
        let directory = private_dir();
        check_private_dir(directory.path()).unwrap();
    }

    #[test]
    fn a_runtime_directory_others_can_read_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let directory = private_dir();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o750)).unwrap();
        assert!(check_private_dir(directory.path()).is_err());
    }

    #[test]
    fn a_symlinked_runtime_directory_is_refused() {
        let directory = private_dir();
        let real = directory.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = directory.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(check_private_dir(&link).is_err());
    }
}
