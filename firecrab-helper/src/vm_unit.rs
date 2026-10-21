//! Runs a VM's shim (`firecrab-api vm-shim`) in its own transient systemd
//! unit, so the VM's lifetime belongs to PID 1 rather than to the API process.
//!
//! This is the one place the helper starts a long-lived process on the API's
//! behalf, so the API supplies as little as possible and nothing it supplies
//! gains privilege:
//! * the program is the `firecrab-api` installed beside this helper, never a
//!   path from the request (the helper cannot read another user's
//!   `/proc/<pid>/exe` without `CAP_SYS_PTRACE`, and should not need to);
//! * the program and the requested Firecracker binary must be files only root
//!   can change, so the API cannot make the unit run a file it wrote;
//! * the unit runs as the peer's uid and gid from `SO_PEERCRED`, sandboxed no
//!   weaker than `firecrab-api.service`, so it can do nothing the API process
//!   could not already do;
//! * the unit name is derived from the VM UUID;
//! * the runtime directory must be a plain absolute path; the shim, as the
//!   API's user, refuses one that is not the API's own private directory.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use firecrab_helper_protocol::network::{UNIT_CPU_QUOTA_PERCENT, UNIT_MEMORY_MAX_MIB};
use thiserror::Error;
use tokio::process::Command;
use uuid::Uuid;

/// Longest stop grace the helper accepts. Generous for a slow guest, while
/// keeping a unit's stop timeout (and so `systemctl stop`) bounded.
const MAX_STOP_GRACE_MS: u64 = 10 * 60 * 1000;
/// Added to the stop grace for the unit's own stop timeout, so systemd only
/// escalates after the shim's own SIGTERM-then-SIGKILL has had its chance.
const STOP_TIMEOUT_SLACK_SECS: u64 = 10;
/// Owner of every file a VM unit may execute.
const ROOT_UID: u32 = 0;
/// The API's unit, whose private `/tmp` every VM unit shares.
const API_UNIT: &str = "firecrab-api.service";

/// The connecting process, as `SO_PEERCRED` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Peer {
    /// The peer's uid; the unit runs as this user.
    pub uid: u32,
    /// The peer's gid; the unit runs with this group.
    pub gid: u32,
    /// The peer's pid, to find the systemd unit (and so the sandbox) it runs in.
    pub pid: Option<i32>,
}

/// What the API asks for in `StartVmUnit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitRequest {
    /// The VM; also names the unit.
    pub vm_id: Uuid,
    /// The start's runtime directory (config, sockets, console log).
    pub runtime_dir: PathBuf,
    /// Firecracker binary the shim runs.
    pub firecracker: PathBuf,
    /// Whether Firecracker gets `--enable-pci`.
    pub enable_pci: bool,
    /// The shim's SIGTERM-to-SIGKILL grace.
    pub stop_grace_ms: u64,
    /// Ceiling on the unit's memory in MiB (`MemoryMax`); `None` is unlimited.
    pub memory_max_mib: Option<u32>,
    /// Ceiling on the unit's CPU time as a percentage of one core
    /// (`CPUQuota`); `None` is unlimited.
    pub cpu_quota_percent: Option<u32>,
}

/// Why a VM unit request failed.
#[derive(Debug, Error)]
pub enum VmUnitError {
    /// The request itself is unacceptable.
    #[error("{0}")]
    Invalid(String),
    /// `systemd-run`/`systemctl` could not be run.
    #[error("failed to run {program}: {source}")]
    Spawn {
        /// The command.
        program: &'static str,
        #[source]
        source: io::Error,
    },
    /// The command ran and failed.
    #[error("{program} failed ({status}): {stderr}")]
    Failed {
        /// The command.
        program: &'static str,
        /// Its exit status.
        status: std::process::ExitStatus,
        /// What it printed on stderr.
        stderr: String,
    },
}

/// `firecrab-vm-<uuid>.service`.
pub fn unit_name(vm_id: Uuid) -> String {
    format!("firecrab-vm-{}.service", vm_id.as_simple())
}

/// Starts the VM's shim in its unit. Returns once systemd has started it;
/// the API then connects to the shim's socket as usual.
pub async fn start(request: &UnitRequest, peer: &Peer) -> Result<(), VmUnitError> {
    validate_request(request)?;
    let helper = std::env::current_exe().map_err(|error| {
        VmUnitError::Invalid(format!("cannot resolve the helper's executable: {error}"))
    })?;
    let program = trusted_executable(&shim_program_beside(&helper), ROOT_UID)?;
    let request = UnitRequest {
        firecracker: trusted_executable(&request.firecracker, ROOT_UID)?,
        ..request.clone()
    };
    // An unreadable cgroup leaves the sandbox on: the stricter guess.
    let sandboxed = peer
        .pid
        .and_then(|pid| fs::read_to_string(format!("/proc/{pid}/cgroup")).ok())
        .is_none_or(|cgroup| sandboxed_by_api_unit(&cgroup));
    // A unit of the same name left over from this VM's previous run (still
    // stopping, or failed and not yet collected) would make `systemd-run`
    // refuse the name; the VM is not running as far as the API knows.
    stop(request.vm_id).await?;
    let _ = run(
        "systemctl",
        vec!["reset-failed".into(), unit_name(request.vm_id).into()],
    )
    .await;
    run(
        "systemd-run",
        start_args(&program, &request, peer, sandboxed),
    )
    .await
}

/// Whether the process with this `/proc/<pid>/cgroup` runs in
/// `firecrab-api.service`, and so in that unit's sandbox.
pub fn sandboxed_by_api_unit(cgroup: &str) -> bool {
    cgroup.lines().any(|line| {
        line.rsplit_once(':')
            .is_some_and(|(_, path)| path.rsplit('/').next() == Some(API_UNIT))
    })
}

/// A command-line argument as systemd reads it: `$NAME` would be replaced
/// from the unit's environment, so every `$` is doubled.
fn literal(arg: impl Into<OsString>) -> OsString {
    let arg: OsString = arg.into();
    arg.to_string_lossy().replace('$', "$$").into()
}

/// Stops the VM's unit; a unit that does not exist is already stopped.
pub async fn stop(vm_id: Uuid) -> Result<(), VmUnitError> {
    match run("systemctl", vec!["stop".into(), unit_name(vm_id).into()]).await {
        // 5: "Unit … not loaded" — nothing to stop.
        Err(VmUnitError::Failed { status, .. }) if status.code() == Some(5) => Ok(()),
        other => other,
    }
}

async fn run(program: &'static str, args: Vec<OsString>) -> Result<(), VmUnitError> {
    let output = Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|source| VmUnitError::Spawn { program, source })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(VmUnitError::Failed {
            program,
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

/// Checks everything in the request that is not derived by the helper.
pub fn validate_request(request: &UnitRequest) -> Result<(), VmUnitError> {
    validate_runtime_dir(&request.runtime_dir)?;
    if !is_plain_absolute(&request.firecracker) {
        return Err(VmUnitError::Invalid(format!(
            "firecracker path {} must be absolute without '.' or '..'",
            request.firecracker.display()
        )));
    }
    if request.stop_grace_ms > MAX_STOP_GRACE_MS {
        return Err(VmUnitError::Invalid(format!(
            "stop grace {} ms exceeds {MAX_STOP_GRACE_MS} ms",
            request.stop_grace_ms
        )));
    }
    if let Some(memory) = request.memory_max_mib
        && !UNIT_MEMORY_MAX_MIB.contains(&memory)
    {
        return Err(VmUnitError::Invalid(format!(
            "memory limit {memory} MiB is outside {}..={} MiB",
            UNIT_MEMORY_MAX_MIB.start(),
            UNIT_MEMORY_MAX_MIB.end()
        )));
    }
    if let Some(percent) = request.cpu_quota_percent
        && !UNIT_CPU_QUOTA_PERCENT.contains(&percent)
    {
        return Err(VmUnitError::Invalid(format!(
            "CPU quota {percent}% is outside {}..={}%",
            UNIT_CPU_QUOTA_PERCENT.start(),
            UNIT_CPU_QUOTA_PERCENT.end()
        )));
    }
    Ok(())
}

/// The runtime directory must be a plain absolute path. Whether it is the
/// API's own private directory is checked by the shim itself, which runs as
/// the API's user: the API keeps its directories closed (`0700`), and this
/// helper deliberately lacks the capability to look inside them. The unit
/// runs as the API's user, so a wrong directory gains it nothing.
pub fn validate_runtime_dir(dir: &Path) -> Result<(), VmUnitError> {
    if !is_plain_absolute(dir) {
        return Err(VmUnitError::Invalid(format!(
            "runtime directory {}: must be absolute without '.' or '..'",
            dir.display()
        )));
    }
    Ok(())
}

/// The API binary installed beside the helper at `helper_exe`, which runs
/// the shim as `firecrab-api vm-shim`. Both come from one install (or one dev
/// build), so the shim speaks the protocol of the API that install runs. A
/// helper whose file an update replaced reads as `<path> (deleted)`; the API
/// beside that path is the updated one, which the restarted API will speak.
pub fn shim_program_beside(helper_exe: &Path) -> PathBuf {
    let helper = helper_exe.to_string_lossy();
    let helper = Path::new(helper.strip_suffix(" (deleted)").unwrap_or(&helper));
    helper.with_file_name("firecrab-api")
}

/// `path` resolved through any symlinks, provided neither the file nor any
/// directory above it can be changed by anyone but root or `owner_uid`: the
/// file and every ancestor are owned by one of them and writable by no group
/// or other user, except sticky directories such as `/tmp`, where nobody can
/// replace another user's entry.
pub fn trusted_executable(path: &Path, owner_uid: u32) -> Result<PathBuf, VmUnitError> {
    let untrusted =
        |reason: String| VmUnitError::Invalid(format!("executable {}: {reason}", path.display()));
    let resolved = fs::canonicalize(path).map_err(|error| untrusted(error.to_string()))?;
    for (depth, component) in resolved.ancestors().enumerate() {
        let metadata = fs::metadata(component).map_err(|error| untrusted(error.to_string()))?;
        let file = depth == 0;
        if file && !metadata.is_file() {
            return Err(untrusted("is not a regular file".to_owned()));
        }
        if metadata.uid() != ROOT_UID && metadata.uid() != owner_uid {
            return Err(untrusted(format!(
                "{} is owned by uid {}",
                component.display(),
                metadata.uid()
            )));
        }
        let sticky = !file && metadata.mode() & 0o1000 != 0;
        if metadata.mode() & 0o022 != 0 && !sticky {
            return Err(untrusted(format!(
                "{} is writable by group or others",
                component.display()
            )));
        }
    }
    Ok(resolved)
}

/// `systemd-run` arguments that start `program vm-shim …` as the peer in the
/// VM's unit, inside the API's own sandbox when the API runs in one.
pub fn start_args(
    program: &Path,
    request: &UnitRequest,
    peer: &Peer,
    sandboxed: bool,
) -> Vec<OsString> {
    let stop_timeout = request.stop_grace_ms.div_ceil(1000) + STOP_TIMEOUT_SLACK_SECS;
    let mut args: Vec<OsString> = vec![
        format!("--unit={}", unit_name(request.vm_id)).into(),
        format!("--description=firecrab MicroVM {}", request.vm_id).into(),
        "--collect".into(),
        "--quiet".into(),
        format!("--uid={}", peer.uid).into(),
        format!("--gid={}", peer.gid).into(),
        // SIGTERM reaches only the shim, which stops Firecracker itself;
        // systemd's SIGKILL to the whole unit is the backstop.
        "--property=KillMode=mixed".into(),
        format!("--property=TimeoutStopSec={stop_timeout}").into(),
        "--property=NoNewPrivileges=yes".into(),
    ];
    // The API derives these from the VM's RAM and vCPUs. They only lower what
    // the unit may use, so within sane bounds the helper applies them as given.
    if let Some(memory) = request.memory_max_mib {
        args.push(format!("--property=MemoryMax={memory}M").into());
    }
    if let Some(percent) = request.cpu_quota_percent {
        args.push(format!("--property=CPUQuota={percent}%").into());
    }
    if sandboxed {
        // The sandbox of `firecrab-api.service`; a unit must not be a way out
        // of it. Its `/tmp` is the API's own private one, not a third: paths
        // the API resolved there (a MicroStorage root, say) must name the
        // same files for the shim.
        args.extend([
            "--property=PrivateTmp=yes".into(),
            format!("--property=JoinsNamespaceOf={API_UNIT}").into(),
            "--property=ProtectHome=yes".into(),
            "--property=ProtectSystem=full".into(),
        ]);
    }
    let mut working_directory = OsString::from("--working-directory=");
    working_directory.push(&request.runtime_dir);
    args.extend([working_directory, "--".into()]);
    let command: [OsString; 10] = [
        program.into(),
        "vm-shim".into(),
        "--vm-id".into(),
        request.vm_id.to_string().into(),
        "--runtime-dir".into(),
        request.runtime_dir.clone().into(),
        "--firecracker".into(),
        request.firecracker.clone().into(),
        "--stop-grace-ms".into(),
        request.stop_grace_ms.to_string().into(),
    ];
    args.extend(command.into_iter().map(literal));
    if request.enable_pci {
        args.push("--enable-pci".into());
    }
    args
}

fn is_plain_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use uuid::Uuid;

    use super::*;

    fn own_uid() -> u32 {
        // SAFETY: getuid has no failure mode.
        unsafe { libc::getuid() }
    }

    fn private_dir() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        directory
    }

    fn request(runtime_dir: &Path) -> UnitRequest {
        UnitRequest {
            vm_id: Uuid::from_u128(0x1234),
            runtime_dir: runtime_dir.to_owned(),
            firecracker: PathBuf::from("/usr/local/bin/firecracker"),
            enable_pci: false,
            stop_grace_ms: 5000,
            memory_max_mib: None,
            cpu_quota_percent: None,
        }
    }

    fn peer() -> Peer {
        Peer {
            uid: 991,
            gid: 992,
            pid: Some(4242),
        }
    }

    #[test]
    fn the_api_unit_is_recognised_from_its_cgroup() {
        assert!(sandboxed_by_api_unit(
            "0::/system.slice/firecrab-api.service\n"
        ));
        assert!(sandboxed_by_api_unit(
            "12:pids:/system.slice/firecrab-api.service\n1:name=systemd:/system.slice/firecrab-api.service\n"
        ));
        assert!(!sandboxed_by_api_unit(
            "0::/user.slice/user-1000.slice/session-3.scope\n"
        ));
        assert!(!sandboxed_by_api_unit(
            "0::/system.slice/firecrab-api.service.d\n"
        ));
        assert!(!sandboxed_by_api_unit(""));
    }

    fn sandbox_properties(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .filter(|arg| {
                [
                    "--property=PrivateTmp=",
                    "--property=JoinsNamespaceOf=",
                    "--property=ProtectHome=",
                    "--property=ProtectSystem=",
                ]
                .iter()
                .any(|prefix| arg.starts_with(prefix))
            })
            .collect()
    }

    #[test]
    fn a_unit_gets_the_sandbox_of_the_api_that_asked_for_it() {
        let directory = private_dir();
        let program = Path::new("/usr/local/lib/firecrab/firecrab-api");
        let unit_request = request(directory.path());

        let installed = start_args(program, &unit_request, &peer(), true);
        assert_eq!(sandbox_properties(&installed).len(), 4, "{installed:?}");

        // An API run from a checkout has no sandbox, so its VMs need none to
        // reach the same files; the unit still never gains privileges.
        let from_source = start_args(program, &unit_request, &peer(), false);
        assert!(
            sandbox_properties(&from_source).is_empty(),
            "{from_source:?}"
        );
        assert!(from_source.contains(&OsString::from("--property=NoNewPrivileges=yes")));
    }

    #[test]
    fn dollar_signs_reach_the_shim_unexpanded() {
        let program = Path::new("/opt/fire$crab/firecrab-api");
        let mut unit_request = request(Path::new("/srv/vm$HOME/r/one"));
        unit_request.firecracker = PathBuf::from("/opt/fc$1/firecracker");

        let args = start_args(program, &unit_request, &peer(), true);
        let text: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let command = &text[text.iter().position(|arg| arg == "--").unwrap() + 1..];
        // systemd expands `$NAME` in a unit's command line; `$$` is a literal `$`.
        assert_eq!(command[0], "/opt/fire$$crab/firecrab-api");
        assert!(
            command.contains(&"/srv/vm$$HOME/r/one".to_owned()),
            "{command:?}"
        );
        assert!(
            command.contains(&"/opt/fc$$1/firecracker".to_owned()),
            "{command:?}"
        );
    }

    #[test]
    fn the_unit_name_comes_from_the_vm_id_only() {
        assert_eq!(
            unit_name(Uuid::from_u128(0x1234)),
            "firecrab-vm-00000000000000000000000000001234.service"
        );
    }

    #[test]
    fn a_runtime_directory_the_helper_cannot_look_into_is_left_to_the_shim() {
        // The API's runtime directories are 0700 under its own 0700 tree,
        // closed to this helper; the shim checks them as the API's user.
        validate_runtime_dir(Path::new("/var/lib/firecrab/data/vms/x/r/y")).unwrap();
    }

    #[test]
    fn a_relative_runtime_directory_is_rejected() {
        assert!(validate_runtime_dir(Path::new("data/vms/x/r/y")).is_err());
    }

    #[test]
    fn a_runtime_directory_with_parent_components_is_rejected() {
        assert!(validate_runtime_dir(Path::new("/var/lib/firecrab/data/vms/../../etc")).is_err());
    }

    fn executable(directory: &Path, mode: u32) -> PathBuf {
        let path = directory.join("firecracker");
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn the_shim_program_is_the_api_installed_beside_this_helper() {
        assert_eq!(
            shim_program_beside(Path::new("/usr/local/lib/firecrab/firecrab-helper")),
            PathBuf::from("/usr/local/lib/firecrab/firecrab-api")
        );
        // A dev build runs both binaries from the same build directory.
        assert_eq!(
            shim_program_beside(Path::new(
                "/usr/local/lib/firecrab-dev/build.7a8hfC/firecrab-helper"
            )),
            PathBuf::from("/usr/local/lib/firecrab-dev/build.7a8hfC/firecrab-api")
        );
    }

    #[test]
    fn a_helper_replaced_by_an_update_still_finds_the_api_beside_it() {
        assert_eq!(
            shim_program_beside(Path::new(
                "/usr/local/lib/firecrab/firecrab-helper (deleted)"
            )),
            PathBuf::from("/usr/local/lib/firecrab/firecrab-api")
        );
    }

    #[test]
    fn an_executable_only_trusted_owners_can_change_is_trusted() {
        let directory = private_dir();
        let path = executable(directory.path(), 0o755);
        assert_eq!(
            trusted_executable(&path, own_uid()).unwrap(),
            fs::canonicalize(&path).unwrap()
        );
    }

    #[test]
    fn an_executable_others_can_write_is_not_trusted() {
        let directory = private_dir();
        let path = executable(directory.path(), 0o775);
        assert!(trusted_executable(&path, own_uid()).is_err());
    }

    #[test]
    fn an_executable_owned_by_an_untrusted_user_is_not_trusted() {
        let directory = private_dir();
        let path = executable(directory.path(), 0o755);
        assert!(trusted_executable(&path, own_uid().wrapping_add(1)).is_err());
    }

    #[test]
    fn an_executable_in_a_directory_others_can_write_is_not_trusted() {
        let directory = private_dir();
        let open = directory.path().join("open");
        fs::create_dir(&open).unwrap();
        fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).unwrap();
        let path = executable(&open, 0o755);
        assert!(trusted_executable(&path, own_uid()).is_err());
    }

    #[test]
    fn a_symlinked_executable_is_trusted_as_its_target() {
        let directory = private_dir();
        let path = executable(directory.path(), 0o755);
        let link = directory.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(
            trusted_executable(&link, own_uid()).unwrap(),
            fs::canonicalize(&path).unwrap()
        );
    }

    #[test]
    fn the_start_command_runs_the_shim_as_the_peer_in_a_named_unit() {
        let directory = private_dir();
        let program = Path::new("/usr/local/lib/firecrab/firecrab-api");
        let mut unit_request = request(directory.path());
        unit_request.enable_pci = true;

        let args = start_args(program, &unit_request, &peer(), true);

        let text: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let separator = text.iter().position(|arg| arg == "--").unwrap();
        let (options, command) = text.split_at(separator);
        assert!(
            options.contains(
                &"--unit=firecrab-vm-00000000000000000000000000001234.service".to_owned()
            )
        );
        assert!(options.contains(&"--uid=991".to_owned()));
        assert!(options.contains(&"--gid=992".to_owned()));
        assert!(options.contains(&"--collect".to_owned()));
        assert!(options.contains(&"--property=KillMode=mixed".to_owned()));
        assert!(options.contains(&"--property=NoNewPrivileges=yes".to_owned()));
        // No weaker than firecrab-api.service, whose children shims used to be.
        assert!(options.contains(&"--property=PrivateTmp=yes".to_owned()));
        // The API's `/tmp`, not one of its own: paths the API resolved there
        // (a MicroStorage root, say) must mean the same file to the shim.
        assert!(options.contains(&"--property=JoinsNamespaceOf=firecrab-api.service".to_owned()));
        assert!(options.contains(&"--property=ProtectHome=yes".to_owned()));
        assert!(options.contains(&"--property=ProtectSystem=full".to_owned()));
        assert!(options.contains(&"--property=TimeoutStopSec=15".to_owned()));
        assert!(options.contains(&format!(
            "--working-directory={}",
            directory.path().display()
        )));
        assert_eq!(
            &command[1..],
            [
                "/usr/local/lib/firecrab/firecrab-api".to_owned(),
                "vm-shim".to_owned(),
                "--vm-id".to_owned(),
                "00000000-0000-0000-0000-000000001234".to_owned(),
                "--runtime-dir".to_owned(),
                directory.path().display().to_string(),
                "--firecracker".to_owned(),
                "/usr/local/bin/firecracker".to_owned(),
                "--stop-grace-ms".to_owned(),
                "5000".to_owned(),
                "--enable-pci".to_owned(),
            ]
        );
        assert!(args.iter().all(|arg: &OsString| !arg.is_empty()));
    }

    /// The `--property=` options `start_args` gives systemd-run, which come
    /// before the `--` that starts the shim's own command line.
    fn unit_options(unit_request: &UnitRequest) -> Vec<String> {
        let program = Path::new("/usr/local/lib/firecrab/firecrab-api");
        let text: Vec<String> = start_args(program, unit_request, &peer(), true)
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let separator = text.iter().position(|arg| arg == "--").unwrap();
        text[..separator].to_vec()
    }

    #[test]
    fn the_start_command_limits_the_unit_when_the_request_carries_limits() {
        let directory = private_dir();
        let mut unit_request = request(directory.path());
        unit_request.memory_max_mib = Some(768);
        unit_request.cpu_quota_percent = Some(200);

        let options = unit_options(&unit_request);

        assert!(options.contains(&"--property=MemoryMax=768M".to_owned()));
        assert!(options.contains(&"--property=CPUQuota=200%".to_owned()));
    }

    #[test]
    fn the_start_command_leaves_the_unit_unlimited_without_limits() {
        let directory = private_dir();

        let options = unit_options(&request(directory.path()));

        assert!(
            !options
                .iter()
                .any(|option| option.contains("MemoryMax") || option.contains("CPUQuota")),
            "{options:?}"
        );
    }

    #[test]
    fn limits_outside_the_accepted_range_are_rejected_before_systemd_sees_them() {
        let directory = private_dir();
        for memory in [0, 127, 1_048_577] {
            let mut unit_request = request(directory.path());
            unit_request.memory_max_mib = Some(memory);
            assert!(validate_request(&unit_request).is_err(), "{memory} MiB");
        }
        for percent in [0, 99, 6_401] {
            let mut unit_request = request(directory.path());
            unit_request.cpu_quota_percent = Some(percent);
            assert!(validate_request(&unit_request).is_err(), "{percent}%");
        }

        for (memory, percent) in [(128, 100), (768, 200), (1_048_576, 6_400)] {
            let mut unit_request = request(directory.path());
            unit_request.memory_max_mib = Some(memory);
            unit_request.cpu_quota_percent = Some(percent);
            validate_request(&unit_request).unwrap();
        }
    }

    #[test]
    fn requests_with_unsafe_values_are_rejected_before_systemd_sees_them() {
        let directory = private_dir();
        let mut relative_firecracker = request(directory.path());
        relative_firecracker.firecracker = PathBuf::from("firecracker");
        assert!(validate_request(&relative_firecracker).is_err());

        let mut endless_grace = request(directory.path());
        endless_grace.stop_grace_ms = 24 * 60 * 60 * 1000;
        assert!(validate_request(&endless_grace).is_err());

        validate_request(&request(directory.path())).unwrap();
    }
}
