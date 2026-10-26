//! Authenticated SSH transport using each VM's existing operator key and pinned host key.
//! Only a task-owned unprivileged guest user runs model commands. No host shell is exposed.
use super::{SavedTask, workspace};
use crate::{artifacts::VmArtifactPaths, guest_ssh::VmSshPaths, model::VmPurpose, state::AppState};
use base64::{Engine, engine::general_purpose::STANDARD};
use firecrab_api_types::{
    SandboxContainerRuntime, SandboxExecutionMode, SandboxGuestCapabilities, SandboxGuestCommand,
    SandboxGuestLimits, SandboxGuestProcess, SandboxGuestSignal, SandboxGuestSignalRequest,
    VmState,
};
use std::{
    io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use uuid::Uuid;

/// Captured guest identity shared with durable task records.
pub type Binding = super::GuestBinding;

/// Bounded guest process result, with stdout and stderr kept separate.
pub struct Output {
    /// Bytes received on stdout.
    pub stdout: Vec<u8>,
    /// Bytes received on stderr.
    pub stderr: Vec<u8>,
    /// SSH or guest exit status.
    pub code: i32,
}

/// Retained command evidence; a reboot can leave output without a confirmed exit.
pub struct RecoveredOutput {
    /// Bounded bytes written before interruption.
    pub stdout: Vec<u8>,
    /// Bounded diagnostic bytes written before interruption.
    pub stderr: Vec<u8>,
    /// Confirmed guest exit, absent when the supervisor did not finish.
    pub code: Option<i32>,
}

/// Refuse internal, service-owned, stopped or unaddressed VMs before using any SSH key.
pub fn binding(state: &AppState, id: Uuid) -> io::Result<Binding> {
    let vm = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&id)
        .cloned()
        .ok_or_else(|| io::Error::other("MicroVM does not exist"))?;
    if vm.state != VmState::Running
        || vm.purpose != VmPurpose::Instance
        || state
            .store
            .cluster_worker(id)
            .map_err(io::Error::other)?
            .is_some()
        || state
            .store
            .vm_has_service_owner(id)
            .map_err(io::Error::other)?
    {
        return Err(io::Error::other(
            "Select a running user MicroVM without a MicroService owner",
        ));
    }
    if state
        .store
        .active_lease(id)
        .map_err(io::Error::other)?
        .is_none()
    {
        return Err(io::Error::other("MicroVM has no network address"));
    }
    Ok(Binding {
        vm_id: id,
        runtime_id: vm
            .last_runtime_id
            .ok_or_else(|| io::Error::other("MicroVM runtime is not ready"))?,
    })
}

fn ssh(state: &AppState, binding: &Binding, attempt: &Path) -> io::Result<Command> {
    let current = self::binding(state, binding.vm_id)?;
    if current.runtime_id != binding.runtime_id {
        return Err(io::Error::other(
            "MicroVM restarted; the previous execution cannot be reused",
        ));
    }
    let vm = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&binding.vm_id)
        .cloned()
        .ok_or_else(|| io::Error::other("MicroVM disappeared"))?;
    let lease = state
        .store
        .active_lease(vm.id)
        .map_err(io::Error::other)?
        .ok_or_else(|| io::Error::other("MicroVM address disappeared"))?;
    let keys = VmSshPaths::from_artifacts(&VmArtifactPaths::for_vm(
        &state.vms_dir_for(&vm.storage_root),
        vm.id,
    ));
    let executable = std::fs::metadata("/usr/bin/ssh")?;
    let private = std::fs::symlink_metadata(&keys.operator_private)?;
    if !executable.is_file()
        || executable.permissions().mode() & 0o111 == 0
        || !private.is_file()
        || private.permissions().mode() & 0o077 != 0
    {
        return Err(io::Error::other(
            "Sandbox requires /usr/bin/ssh and a private owner-only VM operator key",
        ));
    }
    let key = std::fs::read_to_string(&keys.host_public)?;
    let fields: Vec<_> = key.split_whitespace().take(2).collect();
    if fields.len() != 2
        || fields[0] != "ssh-ed25519"
        || !fields[1]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
    {
        return Err(io::Error::other("Expected VM host key is invalid"));
    }
    let known = attempt.join("known_hosts");
    std::fs::write(
        &known,
        format!("{} {} {}\n", lease.ipv4, fields[0], fields[1]),
    )?;
    std::fs::set_permissions(&known, std::fs::Permissions::from_mode(0o600))?;
    let mut cmd = Command::new("/usr/bin/ssh");
    cmd.env_clear().env("PATH", "/usr/bin:/bin");
    cmd.args([
        "-F",
        "/dev/null",
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "IdentitiesOnly=yes",
        "-o",
        "IdentityAgent=none",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "GlobalKnownHostsFile=/dev/null",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ConnectTimeout=5",
        "-o",
        "ServerAliveInterval=5",
        "-o",
        "ServerAliveCountMax=2",
        "-o",
        "LogLevel=ERROR",
    ])
    .arg("-o")
    .arg(format!("UserKnownHostsFile={}", known.display()))
    .arg("-i")
    .arg(&keys.operator_private)
    .arg(format!("root@{}", lease.ipv4))
    .arg("/bin/sh -s");
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    Ok(cmd)
}

async fn read_limited(mut reader: impl AsyncRead + Unpin, limit: usize) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        if output.len() + n > limit {
            return Err(io::Error::other("Guest output limit exceeded"));
        }
        output.extend_from_slice(&chunk[..n]);
    }
    Ok(output)
}

async fn capture(
    mut command: Command,
    input: Vec<u8>,
    limit: usize,
    timeout: Duration,
) -> io::Result<Output> {
    let mut child = command.spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("SSH stdin unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("SSH stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("SSH stderr unavailable"))?;
    let work = async {
        let write = async move {
            stdin.write_all(&input).await?;
            stdin.shutdown().await?;
            drop(stdin);
            Ok::<_, io::Error>(())
        };
        let (_, stdout, stderr, status) = tokio::try_join!(
            write,
            read_limited(stdout, limit),
            read_limited(stderr, 65536),
            child.wait()
        )?;
        Ok::<_, io::Error>(Output {
            stdout,
            stderr,
            code: status.code().unwrap_or(255),
        })
    };
    match tokio::time::timeout(timeout, work).await {
        Ok(result) => result,
        Err(_) => {
            let _ = child.kill().await;
            Err(io::Error::other("Guest transport timed out"))
        }
    }
}

#[cfg(test)]
fn script(
    action: &str,
    id: Uuid,
    invocation: Option<Uuid>,
    payload: &str,
    seconds: u64,
) -> Vec<u8> {
    script_options(
        action,
        id,
        invocation,
        payload,
        seconds,
        ScriptOptions::default(),
    )
}

#[derive(Default)]
struct ScriptOptions<'a> {
    mode: Option<&'a SandboxExecutionMode>,
    workspace: Option<Uuid>,
    limits: Option<&'a SandboxGuestLimits>,
}

fn script_options(
    action: &str,
    id: Uuid,
    invocation: Option<Uuid>,
    payload: &str,
    seconds: u64,
    options: ScriptOptions<'_>,
) -> Vec<u8> {
    let ScriptOptions {
        mode,
        workspace,
        limits,
    } = options;
    let (runtime, image, persistent) = match mode {
        Some(SandboxExecutionMode::AgentSandbox {
            runtime: SandboxContainerRuntime::Docker,
            image,
        }) => ("docker", image.as_str(), false),
        Some(SandboxExecutionMode::AgentSandbox {
            runtime: SandboxContainerRuntime::Nspawn,
            image,
        }) => ("nspawn", image.as_str(), false),
        Some(SandboxExecutionMode::PersistentWorkspace) => ("direct", "", true),
        None => ("direct", "", false),
    };
    SCRIPT
        .replace("__MODE__", runtime)
        .replace("__IMAGE__", image)
        .replace("__PERSISTENT__", if persistent { "true" } else { "false" })
        .replace(
            "__WORKSPACE__",
            &workspace.unwrap_or(id).simple().to_string(),
        )
        .replace(
            "__CPU_SECONDS__",
            &limits.map_or(120, |l| l.cpu_seconds).to_string(),
        )
        .replace(
            "__MEMORY_KIB__",
            &(u64::from(limits.map_or(1024, |l| l.memory_mib)) * 1024).to_string(),
        )
        .replace(
            "__OUTPUT_BYTES__",
            &limits.map_or(65536, |l| l.output_bytes).to_string(),
        )
        .replace("__ID__", &id.simple().to_string())
        .replace("__ACTION__", action)
        .replace(
            "__COMMAND__",
            &invocation.unwrap_or_default().simple().to_string(),
        )
        .replace("__PAYLOAD__", payload)
        .replace("__SECONDS__", &seconds.to_string())
        .into_bytes()
}

fn task_script(
    saved: &SavedTask,
    action: &str,
    invocation: Option<Uuid>,
    payload: &str,
    seconds: u64,
    limits: Option<&SandboxGuestLimits>,
) -> io::Result<Vec<u8>> {
    // Older persisted attempts omit execution_mode and keep the borrowed-VM contract.
    if let Some(mode) = &saved.execution_mode {
        super::environment::validate_mode(mode).map_err(io::Error::other)?;
    }
    Ok(script_options(
        action,
        saved.task.attempt_id,
        invocation,
        payload,
        seconds,
        ScriptOptions {
            mode: saved.execution_mode.as_ref(),
            workspace: saved.guest.map(|r| r.vm_id),
            limits,
        },
    ))
}

/// Authenticate the VM and negotiate execution support without creating users or workspaces.
pub async fn capabilities(
    state: &AppState,
    binding: &Binding,
    directory: &Path,
) -> io::Result<SandboxGuestCapabilities> {
    let output = capture(
        ssh(state, binding, directory)?,
        CAPABILITIES.as_bytes().to_vec(),
        4096,
        Duration::from_secs(12),
    )
    .await?;
    if output.code != 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Guest protocol requires an authenticated shell; this guest does not provide the V1 channel",
        ));
    }
    super::guest_protocol::capabilities(&output.stdout)
}

/// Confirm the selected immutable image without pulling or mutating a shared base.
pub async fn mode_supported(
    state: &AppState,
    binding: &Binding,
    directory: &Path,
    mode: &SandboxExecutionMode,
) -> io::Result<bool> {
    super::environment::validate_mode(mode).map_err(io::Error::other)?;
    let command = match mode {
        SandboxExecutionMode::PersistentWorkspace => return Ok(true),
        SandboxExecutionMode::AgentSandbox {
            runtime: SandboxContainerRuntime::Docker,
            image,
        } => format!(
            "set -eu\nPATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\nexport PATH\ndocker image inspect {} >/dev/null 2>&1\n",
            super::guest_protocol::quote(image)
        ),
        SandboxExecutionMode::AgentSandbox {
            runtime: SandboxContainerRuntime::Nspawn,
            image,
        } => {
            let digest = image
                .strip_prefix("sha256:")
                .ok_or_else(|| io::Error::other("Invalid nspawn image digest"))?;
            format!(
                "set -eu\nPATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\nexport PATH\nsource='/var/lib/firecrab-sandbox-images/{digest}.tar'\n[ -f \"$source\" ] && [ ! -L \"$source\" ] || exit 86\nif [ -x /etc/firecrab/busybox ]; then actual=$(/etc/firecrab/busybox sha256sum \"$source\"); else actual=$(sha256sum \"$source\"); fi\n[ \"${{actual%% *}}\" = '{digest}' ]\n"
            )
        }
    };
    let output = capture(
        ssh(state, binding, directory)?,
        command.into_bytes(),
        4096,
        Duration::from_secs(15),
    )
    .await?;
    Ok(output.code == 0)
}
fn attempt(saved: &SavedTask) -> io::Result<PathBuf> {
    saved
        .workspace
        .clone()
        .ok_or_else(|| io::Error::other("Task workspace is unavailable"))
}

/// Upload a sanitized snapshot, run a bounded command, then export changes before cleanup.
pub async fn execute(
    state: &AppState,
    saved: &mut SavedTask,
    command: &str,
    seconds: u64,
) -> io::Result<Output> {
    execute_bounded(state, saved, command, seconds, None).await
}

/// Execute a versioned literal argv/cwd/env/stdin request with explicit hard limits.
pub async fn execute_request(
    state: &AppState,
    saved: &mut SavedTask,
    request: &SandboxGuestCommand,
) -> io::Result<Output> {
    super::guest_protocol::validate(request)?;
    let mut request = request.clone();
    if let Some(reference) = &saved.task.environment {
        for (key, value) in
            super::environment::service_environment(state, reference).map_err(io::Error::other)?
        {
            if request.env.contains_key(&key) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Guest request cannot override environment service discovery",
                ));
            }
            request.env.insert(key, value);
        }
    }
    let command = super::guest_protocol::program(&request)?;
    execute_bounded(
        state,
        saved,
        &command,
        request.limits.wall_seconds,
        Some(&request.limits),
    )
    .await
}

async fn execute_bounded(
    state: &AppState,
    saved: &mut SavedTask,
    command: &str,
    seconds: u64,
    limits: Option<&SandboxGuestLimits>,
) -> io::Result<Output> {
    super::access::attempt(state, saved).map_err(io::Error::other)?;
    if command.trim().is_empty()
        || command.len() > if limits.is_some() { 131072 } else { 8192 }
        || command.contains('\0')
        || !(1..=120).contains(&seconds)
    {
        return Err(io::Error::other("Invalid guest command or time limit"));
    }
    let binding = saved
        .guest
        .ok_or_else(|| io::Error::other("Select a MicroVM before running commands"))?;
    let attempt = attempt(saved)?;
    let probe = capabilities(state, &binding, &attempt).await?;
    if !probe.execution {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            probe
                .unsupported
                .unwrap_or_else(|| "Guest execution is unsupported".into()),
        ));
    }
    if let Some(mode) = &saved.execution_mode {
        let engine = match mode {
            SandboxExecutionMode::PersistentWorkspace => true,
            SandboxExecutionMode::AgentSandbox {
                runtime: SandboxContainerRuntime::Docker,
                ..
            } => probe.docker,
            SandboxExecutionMode::AgentSandbox {
                runtime: SandboxContainerRuntime::Nspawn,
                ..
            } => probe.nspawn,
        };
        if !engine || !mode_supported(state, &binding, &attempt, mode).await? {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Selected guest container engine or exact pinned image is unavailable; pre-install it inside the managed MicroVM",
            ));
        }
    }
    let work = attempt.join("work");
    let archive = tokio::task::spawn_blocking(move || workspace::archive(&work))
        .await
        .map_err(io::Error::other)??;
    let stage_command = ssh(state, &binding, &attempt)?;
    super::access::attempt(state, saved).map_err(io::Error::other)?;
    saved.guest_pending = true;
    saved.guest_staged = true;
    super::save(state, saved).await.map_err(io::Error::other)?;
    let staged = capture(
        stage_command,
        task_script(saved, "stage", None, &STANDARD.encode(archive), 0, limits)?,
        65536,
        Duration::from_secs(60),
    )
    .await?;
    if staged.code != 0 {
        return Err(io::Error::other(
            "MicroVM project staging failed; verify SSH, tar, an unprivileged-user utility and the injected toolbox",
        ));
    }
    let invocation = Uuid::new_v4();
    super::access::attempt(state, saved).map_err(io::Error::other)?;
    saved.guest_command = Some(invocation);
    super::save(state, saved).await.map_err(io::Error::other)?;
    let output = capture(
        ssh(state, &binding, &attempt)?,
        task_script(
            saved,
            "run",
            Some(invocation),
            &STANDARD.encode(command),
            seconds,
            limits,
        )?,
        65536,
        Duration::from_secs(seconds + 15),
    )
    .await?;
    let status = capture(
        ssh(state, &binding, &attempt)?,
        task_script(saved, "status", Some(invocation), "", 0, limits)?,
        1024,
        Duration::from_secs(30),
    )
    .await?;
    if status.code != 0 || String::from_utf8_lossy(&status.stdout).trim() != output.code.to_string()
    {
        return Err(io::Error::other(
            "Guest invocation completion could not be confirmed; reconcile before retrying",
        ));
    }
    export(state, saved).await?;
    Ok(output)
}

/// Stop only this attempt's root supervisor and dedicated guest UID, then verify termination.
pub async fn cancel(state: &AppState, saved: &SavedTask) -> io::Result<()> {
    let Some(binding) = &saved.guest else {
        return Ok(());
    };
    let attempt = attempt(saved)?;
    let output = capture(
        ssh(state, binding, &attempt)?,
        task_script(saved, "cancel", saved.guest_command, "", 0, None)?,
        65536,
        Duration::from_secs(30),
    )
    .await?;
    if output.code != 0 {
        return Err(io::Error::other("Guest termination could not be confirmed"));
    }
    Ok(())
}

/// Read the recorded invocation after confirmed termination, before deleting its logs.
pub async fn recover(state: &AppState, saved: &SavedTask) -> io::Result<Option<RecoveredOutput>> {
    let Some(invocation) = saved.guest_command else {
        return Ok(None);
    };
    let binding = saved
        .guest
        .as_ref()
        .ok_or_else(|| io::Error::other("No guest binding"))?;
    let attempt = attempt(saved)?;
    let output = capture(
        ssh(state, binding, &attempt)?,
        task_script(saved, "recover", Some(invocation), "", 0, None)?,
        65536,
        Duration::from_secs(30),
    )
    .await?;
    if output.code == 79 {
        return Ok(None);
    }
    if output.code != 0 {
        return Err(io::Error::other("Cannot recover guest command output"));
    }
    let status = capture(
        ssh(state, binding, &attempt)?,
        task_script(saved, "status", Some(invocation), "", 0, None)?,
        1024,
        Duration::from_secs(30),
    )
    .await?;
    let code = match status.code {
        0 => Some(
            String::from_utf8_lossy(&status.stdout)
                .trim()
                .parse::<i32>()
                .ok()
                .filter(|code| (0..=255).contains(code))
                .ok_or_else(|| io::Error::other("Invalid retained guest exit status"))?,
        ),
        78 => None,
        _ => {
            return Err(io::Error::other(
                "Cannot confirm retained guest exit status",
            ));
        }
    };
    Ok(Some(RecoveredOutput {
        stdout: output.stdout,
        stderr: output.stderr,
        code,
    }))
}

/// Recover a complete bounded workspace from the authenticated guest.
pub async fn export(state: &AppState, saved: &SavedTask) -> io::Result<()> {
    let binding = saved
        .guest
        .as_ref()
        .ok_or_else(|| io::Error::other("No guest binding"))?;
    let attempt = attempt(saved)?;
    let result = capture(
        ssh(state, binding, &attempt)?,
        task_script(saved, "export", saved.guest_command, "", 0, None)?,
        super::files::TREE_LIMIT + 4 * 1024 * 1024,
        Duration::from_secs(60),
    )
    .await?;
    if result.code == 79 {
        return Ok(());
    }
    if result.code != 0 {
        return Err(io::Error::other(
            "Guest export failed; guest workspace was retained",
        ));
    }
    let work = attempt.join("work");
    tokio::task::spawn_blocking(move || workspace::import(&work, &result.stdout))
        .await
        .map_err(io::Error::other)?
}

/// Remove only attempt-owned guest data after a successful export and termination check.
pub async fn cleanup(state: &AppState, saved: &SavedTask) -> io::Result<()> {
    let Some(binding) = &saved.guest else {
        return Ok(());
    };
    let attempt = attempt(saved)?;
    let output = capture(
        ssh(state, binding, &attempt)?,
        task_script(saved, "cleanup", saved.guest_command, "", 0, None)?,
        65536,
        Duration::from_secs(30),
    )
    .await?;
    if output.code != 0 {
        return Err(io::Error::other(
            "Guest workspace cleanup requires reconciliation",
        ));
    }
    Ok(())
}

/// Inspect only the dedicated UID of this exact attempt and boot.
pub async fn processes(
    state: &AppState,
    saved: &SavedTask,
) -> io::Result<Vec<SandboxGuestProcess>> {
    super::access::attempt(state, saved).map_err(io::Error::other)?;
    let binding = saved
        .guest
        .as_ref()
        .ok_or_else(|| io::Error::other("Attempt has no guest runtime"))?;
    let output = capture(
        ssh(state, binding, &attempt(saved)?)?,
        task_script(saved, "processes", saved.guest_command, "", 0, None)?,
        16384,
        Duration::from_secs(10),
    )
    .await?;
    if output.code != 0 {
        return Err(io::Error::other(
            "Cannot inspect the admitted guest process generation",
        ));
    }
    let text = std::str::from_utf8(&output.stdout).map_err(io::Error::other)?;
    let mut processes = Vec::new();
    for line in text.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 4 || processes.len() >= 128 {
            return Err(io::Error::other(
                "Invalid or excessive scoped guest process response",
            ));
        }
        let process = SandboxGuestProcess {
            pid: fields[0].parse().map_err(io::Error::other)?,
            start_ticks: fields[1].parse().map_err(io::Error::other)?,
            boot_id: fields[2].into(),
            state: fields[3].into(),
        };
        Uuid::parse_str(&process.boot_id).map_err(io::Error::other)?;
        if process.pid <= 1 || process.start_ticks == 0 || process.state.len() != 1 {
            return Err(io::Error::other("Invalid scoped guest process identity"));
        }
        processes.push(process);
    }
    Ok(processes)
}

/// Signal only a currently owned UID/boot/PID/start-time tuple.
pub async fn signal(
    state: &AppState,
    saved: &SavedTask,
    request: &SandboxGuestSignalRequest,
) -> io::Result<()> {
    super::access::attempt(state, saved).map_err(io::Error::other)?;
    let binding = saved
        .guest
        .as_ref()
        .ok_or_else(|| io::Error::other("Attempt has no guest runtime"))?;
    let boot = Uuid::parse_str(&request.process.boot_id).map_err(io::Error::other)?;
    if boot.to_string() != request.process.boot_id
        || request.process.pid <= 1
        || request.process.pid > i32::MAX as u32
        || request.process.start_ticks == 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid guest process identity",
        ));
    }
    let signal = match request.signal {
        SandboxGuestSignal::Terminate => "TERM",
        SandboxGuestSignal::Kill => "KILL",
    };
    let payload = format!(
        "{}:{}:{}:{signal}",
        request.process.pid, request.process.start_ticks, boot
    );
    let output = capture(
        ssh(state, binding, &attempt(saved)?)?,
        task_script(saved, "signal", saved.guest_command, &payload, 0, None)?,
        4096,
        Duration::from_secs(10),
    )
    .await?;
    if output.code != 0 {
        return Err(io::Error::other(
            "Guest process ownership or boot/start identity changed; signal was refused",
        ));
    }
    Ok(())
}

const CAPABILITIES: &str = r#"set -u
PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export PATH
tool() { if [ -x /etc/firecrab/busybox ]; then /etc/firecrab/busybox "$@"; else "$@"; fi; }
[ "$(id -u)" = 0 ] || exit 80
init=distroless
if [ -d /run/systemd/system ]; then init=systemd
elif [ -x /sbin/openrc-run ] || [ -d /run/openrc ]; then init=openrc; fi
unsupported=
if ! command -v setpriv >/dev/null 2>&1 || ! setpriv --reuid 0 --regid 0 --clear-groups --no-new-privs /bin/true 2>/dev/null; then unsupported=missing_util_linux_setpriv
elif ! command -v flock >/dev/null 2>&1 || ! flock -w 0 --help >/dev/null 2>&1; then unsupported=missing_timed_flock
elif ! { tar --exclude=__firecrab_probe__ --help >/dev/null 2>&1 || { [ -x /etc/firecrab/busybox ] && /etc/firecrab/busybox tar --exclude=__firecrab_probe__ --help >/dev/null 2>&1; }; }; then unsupported=missing_tar_exclude
elif ! (ulimit -v 1048576 && ulimit -t 1 && ulimit -n 128) 2>/dev/null; then unsupported=missing_hard_resource_limits
elif ! tool timeout --help >/dev/null 2>&1; then unsupported=missing_timeout; fi
execution=true
[ -z "$unsupported" ] || execution=false
docker=false
if command -v docker >/dev/null 2>&1 && [ -f /sys/fs/cgroup/cgroup.controllers ] && tool grep -qw memory /sys/fs/cgroup/cgroup.controllers && tool grep -qw pids /sys/fs/cgroup/cgroup.controllers && tool timeout -s KILL 3 docker info >/dev/null 2>&1; then docker=true; fi
nspawn=false
if [ "$init" = systemd ] && command -v systemd-nspawn >/dev/null 2>&1 && [ -f /sys/fs/cgroup/cgroup.controllers ]; then nspawn=true; fi
printf 'version=1\ninit=%s\nexecution=%s\ndocker=%s\nnspawn=%s\nunsupported=%s\n' "$init" "$execution" "$docker" "$nspawn" "$unsupported"
"#;

// All substitutions except the base64 payload are generated UUIDs, fixed actions or bounded integers.
// The root-owned control directory is outside the command user's writable workspace.
const SCRIPT: &str = r#"set -eu
umask 077
PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export PATH
id='__ID__'
invocation='__COMMAND__'
action='__ACTION__'
mode='__MODE__'
image='__IMAGE__'
persistent='__PERSISTENT__'
workspace_id='__WORKSPACE__'
output_limit='__OUTPUT_BYTES__'
protocol=1
[ "$protocol" = 1 ] || exit 85
user="a$(printf '%s' "$id" | cut -c1-30)"
root=/var/lib/firecrab-ai
meta="$root/.control"
fence="$meta/fences/$id"
base="$root/$id"
control="$base/control"
work="$base/workspace"
if [ "$persistent" = true ]; then work="$root/.workspaces/$workspace_id"; fi
cmd="$control/commands/$invocation"
tool() { if [ -x /etc/firecrab/busybox ]; then /etc/firecrab/busybox "$@"; else "$@"; fi; }
[ "$(id -u)" = 0 ] || exit 80
if command -v setpriv >/dev/null 2>&1 && setpriv --reuid 0 --regid 0 --clear-groups --no-new-privs /bin/true 2>/dev/null; then
  drop_program=$(command -v setpriv)
else printf 'Sandbox requires util-linux setpriv with direct UID switching and no-new-privileges. Install util-linux in the selected development VM.\n' >&2; exit 80; fi
if command -v flock >/dev/null 2>&1; then lock_program=$(command -v flock)
else lock_program=/etc/firecrab/busybox; fi
lock() { if [ "$lock_program" = /etc/firecrab/busybox ]; then "$lock_program" flock "$@"; else "$lock_program" "$@"; fi; }
lock -w 0 --help >/dev/null 2>&1 || exit 80
if command -v tar >/dev/null 2>&1 && tar --exclude=__firecrab_probe__ --help >/dev/null 2>&1; then tar_program=$(command -v tar)
elif [ -x /etc/firecrab/busybox ] && /etc/firecrab/busybox tar --exclude=__firecrab_probe__ --help >/dev/null 2>&1; then tar_program=/etc/firecrab/busybox
else printf 'Sandbox requires tar with --exclude support. Install GNU tar in the selected development VM.\n' >&2; exit 80; fi
archive_tar() { if [ "$tar_program" = /etc/firecrab/busybox ]; then "$tar_program" tar "$@"; else "$tar_program" "$@"; fi; }
[ "$action" != probe ] || exit 0
mkdir -p "$meta/locks" "$meta/fences"
chmod 711 "$root" "$meta" "$meta/fences"
chmod 700 "$meta/locks"
exec 9>"$meta/locks/$id"
lock -w 10 9 || exit 81
case "$action" in stage|run) [ ! -e "$fence" ] || exit 71;; esac
mkdir -p "$control"
chmod 711 "$base"
if [ -f "$control/owner" ]; then [ "$(cat "$control/owner")" = "$id" ] || exit 70; fi
printf '%s' "$id" >"$control/owner"
boot=$(cat /proc/sys/kernel/random/boot_id)
if [ -f "$control/uid" ]; then
  [ "$(id -u "$user" 2>/dev/null || true)" = "$(cat "$control/uid")" ] || exit 72
elif id -u "$user" >/dev/null 2>&1; then exit 72; fi
terminate_user() {
  uid=$(id -u "$user" 2>/dev/null) || return 0
  for status in /proc/[0-9]*/status; do
    [ -r "$status" ] || continue
    actual=$(tool awk '/^Uid:/ {print $2; exit}' "$status" 2>/dev/null) || continue
    [ "$actual" = "$uid" ] || continue
    pid=${status#/proc/}; pid=${pid%/status}
    kill -KILL "$pid" 2>/dev/null || true
  done
}
stop_container() {
  [ -f "$control/current" ] || return 0
  current=$(cat "$control/current")
  case "$current" in *[!0-9a-f]*|'') return 1;; esac
  [ "${#current}" = 32 ] || return 1
  container="firecrab-ai-$id-$current"
  if [ "$mode" = docker ] && command -v docker >/dev/null 2>&1; then
    owner=$(docker inspect --format '{{index .Config.Labels "io.firecrab.attempt"}}' "$container" 2>/dev/null || true)
    if [ -n "$owner" ]; then
      [ "$owner" = "$id" ] || return 1
      docker kill "$container" >/dev/null 2>&1 || true
      docker rm -f "$container" >/dev/null 2>&1 || true
      ! docker inspect "$container" >/dev/null 2>&1 || return 1
    fi
  fi
}
user_alive() {
  uid=$(id -u "$user" 2>/dev/null) || return 1
  for status in /proc/[0-9]*/status; do
    [ -r "$status" ] || continue
    actual=$(tool awk '/^Uid:/ {print $2; exit}' "$status" 2>/dev/null) || continue
    state=$(tool awk '/^State:/ {print $2; exit}' "$status" 2>/dev/null) || continue
    if [ "$actual" = "$uid" ] && [ "$state" != Z ]; then return 0; fi
  done
  return 1
}
stop_user() {
  stop_container || return 1
  for turn in 1 2 3 4 5 6 7 8 9 10; do
    terminate_user
    if ! user_alive; then return 0; fi
    tool sleep 0.1
  done
  return 1
}
current_complete() {
  [ -f "$control/current" ] || return 0
  current=$(cat "$control/current")
  case "$current" in *[!0-9a-f]*|'') return 1;; esac
  [ "${#current}" = 32 ] || return 1
  [ -f "$control/commands/$current/finished" ]
}
drop_user() {
  "$drop_program" --reuid "$(id -u "$user")" --regid "$(id -g "$user")" --clear-groups --no-new-privs env -i HOME="$work" PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin /bin/sh "$1"
}
emit_output() {
  if [ -f "$cmd/output-bytes" ]; then output_limit=$(cat "$cmd/output-bytes"); fi
  case "$output_limit" in *[!0-9]*|'') exit 85;; esac
  [ "$output_limit" -ge 1 ] && [ "$output_limit" -le 65536 ] || exit 85
  if [ -f "$cmd/stdout" ]; then tool head -c "$output_limit" "$cmd/stdout"; fi
  if [ -f "$cmd/output-limit" ]; then
    if [ -f "$cmd/stderr" ]; then tool head -c "$output_limit" "$cmd/stderr" >&2; fi
  elif [ -f "$cmd/stderr" ]; then tool head -c "$output_limit" "$cmd/stderr" >&2; fi
}
case "$action" in
stage)
  current_complete && ! user_alive || exit 76
  if [ -f "$control/boot" ]; then [ "$(cat "$control/boot")" = "$boot" ] || exit 77; fi
  if ! id -u "$user" >/dev/null 2>&1; then
    if command -v useradd >/dev/null 2>&1; then useradd -M -U -d "$work" -s /bin/sh "$user"
    elif command -v adduser >/dev/null 2>&1; then adduser -D -H -h "$work" -s /bin/sh "$user"
    else tool adduser -D -H -h "$work" -s /bin/sh "$user"; fi
  fi
  [ "$(id -gn "$user")" = "$user" ] || exit 72
  id -u "$user" >"$control/uid"
  chgrp "$user" "$control"
  chmod 710 "$control"
  mkdir -p "$work"
  if [ "$persistent" = true ]; then
    mkdir -p "$root/.workspaces"
    chmod 711 "$root/.workspaces"
    if [ -f "$root/.workspaces/$workspace_id.initialized" ]; then
      chown -hR "$user:$user" "$work"
      chmod 700 "$work"
      printf '%s' "$boot" >"$control/boot"
      touch "$control/staged"
      exit 0
    fi
  fi
  tool base64 -d >"$control/project.tar" <<'FIRECRAB_PAYLOAD'
__PAYLOAD__
FIRECRAB_PAYLOAD
  tool tar -tf "$control/project.tar" >"$control/next-files"
  [ -f "$control/previous-files" ] || touch "$control/previous-files"
  cat >"$control/refresh" <<FIRECRAB_REFRESH
set -eu
work='$work'
control='$control'
tool() { if [ -x /etc/firecrab/busybox ]; then /etc/firecrab/busybox "\$@"; else "\$@"; fi; }
while IFS= read -r file; do
  if ! tool grep -Fqx -- "\$file" "\$control/next-files"; then rm -f -- "\$work/\$file"; fi
done <"\$control/previous-files"
tool tar -xf "\$control/project.tar" -C "\$work"
FIRECRAB_REFRESH
  chgrp "$user" "$control/project.tar" "$control/previous-files" "$control/next-files" "$control/refresh"
  chmod 640 "$control/project.tar" "$control/previous-files" "$control/next-files" "$control/refresh"
  chown "$user:$user" "$work"
  chmod 700 "$work"
  drop_user "$control/refresh" 9>&-
  if [ "$persistent" = true ]; then touch "$root/.workspaces/$workspace_id.initialized"; fi
  cp "$control/next-files" "$control/previous-files"
  rm -f "$control/project.tar"
  printf '%s' "$boot" >"$control/boot"
  touch "$control/staged"
  ;;
run)
  [ -f "$control/staged" ] || exit 79
  [ "$(cat "$control/boot")" = "$boot" ] || exit 77
  current_complete && ! user_alive || exit 76
  [ ! -e "$cmd" ] || exit 82
  mkdir -p "$control/commands"
  mkdir "$cmd"
  chgrp "$user" "$control/commands" "$cmd"
  chmod 710 "$control/commands" "$cmd"
  tool base64 -d >"$cmd/command" <<'FIRECRAB_PAYLOAD'
__PAYLOAD__
FIRECRAB_PAYLOAD
  printf '%s' "$output_limit" >"$cmd/output-bytes"
  printf '#!/bin/sh\nset -eu\n[ ! -e %s ] || exit 71\ncd %s\nexec /bin/sh %s/command\n' "$fence" "$work" "$cmd" >"$cmd/start"
  chgrp "$user" "$cmd/command" "$cmd/start"
  chmod 640 "$cmd/command" "$cmd/start"
  printf '%s %s\n' "$$" "$(tool awk '{print $22}' /proc/$$/stat)" >"$control/supervisor"
  printf '%s' "$invocation" >"$control/current"
  fail_run() {
    printf '%s\n' "$1" >"$cmd/stderr"
    printf '86' >"$cmd/exit"
    touch "$cmd/finished"
    emit_output
    exit 86
  }
  trap 'stop_container; terminate_user' HUP INT TERM
  trap 'stop_container; terminate_user' EXIT
  printf '#!/bin/sh\nset -eu\nulimit -n 128\nulimit -f 65536\nulimit -t __CPU_SECONDS__\nulimit -v __MEMORY_KIB__\n' >"$cmd/launch"
  case "$mode" in
  direct)
    printf 'exec %s --reuid %s --regid %s --clear-groups --no-new-privs env -i HOME=%s PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin /bin/sh %s/start\n' "$drop_program" "$(id -u "$user")" "$(id -g "$user")" "$work" "$cmd" >>"$cmd/launch"
    ;;
  docker)
    command -v docker >/dev/null 2>&1 || fail_run 'unsupported_docker: install Docker inside the managed guest'
    docker image inspect "$image" >/dev/null 2>&1 || fail_run 'unsupported_docker_image: pre-install the exact digest-pinned image'
    printf '#!/bin/sh\nset -eu\ncd /workspace\nexec /bin/sh /firecrab/command\n' >"$cmd/container-start"
    chgrp "$user" "$cmd/container-start"; chmod 640 "$cmd/container-start"
    printf 'exec docker run --rm --name firecrab-ai-%s-%s --label io.firecrab.attempt=%s --network host --user %s:%s --cap-drop ALL --security-opt no-new-privileges --pids-limit 128 --memory __MEMORY_KIB__k --cpus 1 --ulimit cpu=__CPU_SECONDS__:__CPU_SECONDS__ --ulimit nofile=128:128 --mount type=bind,src=%s,dst=/workspace --mount type=bind,src=%s,dst=/firecrab,readonly --workdir /workspace --env HOME=/workspace --env PATH=/usr/local/bin:/usr/bin:/bin %s /bin/sh /firecrab/container-start\n' "$id" "$invocation" "$id" "$(id -u "$user")" "$(id -g "$user")" "$work" "$cmd" "$image" >>"$cmd/launch"
    ;;
  nspawn)
    command -v systemd-nspawn >/dev/null 2>&1 && [ -d /run/systemd/system ] && [ -f /sys/fs/cgroup/cgroup.controllers ] || fail_run 'unsupported_nspawn: systemd and cgroup v2 are required inside the managed guest'
    digest=${image#sha256:}
    source="/var/lib/firecrab-sandbox-images/$digest.tar"
    [ -f "$source" ] && [ ! -L "$source" ] || fail_run 'unsupported_nspawn_image: pre-install the exact content-addressed root archive'
    actual=$(tool sha256sum "$source"); actual=${actual%% *}
    [ "$actual" = "$digest" ] || fail_run 'nspawn_image_digest_mismatch'
    mkdir "$cmd/rootfs"
    archive_tar -xf "$source" -C "$cmd/rootfs" || fail_run 'unsupported_nspawn_image: root archive extraction failed'
    [ -x "$cmd/rootfs/bin/sh" ] || fail_run 'unsupported_nspawn_image: V1 requires /bin/sh in the container image'
    mkdir -p "$cmd/rootfs/firecrab"
    [ -d "$cmd/rootfs/etc" ] && [ ! -L "$cmd/rootfs/etc" ] && [ ! -L "$cmd/rootfs/etc/passwd" ] && [ ! -L "$cmd/rootfs/etc/group" ] || fail_run 'unsupported_nspawn_image: regular account files are required'
    printf '%s:x:%s:%s:Firecrab Sandbox:/workspace:/bin/sh\n' "$user" "$(id -u "$user")" "$(id -g "$user")" >>"$cmd/rootfs/etc/passwd"
    printf '%s:x:%s:\n' "$user" "$(id -g "$user")" >>"$cmd/rootfs/etc/group"
    printf '#!/bin/sh\nset -eu\ncd /workspace\nexec /bin/sh /firecrab/command\n' >"$cmd/container-start"
    chgrp "$user" "$cmd/container-start"; chmod 640 "$cmd/container-start"
    printf 'exec systemd-nspawn --quiet --register=no --directory=%s/rootfs --machine=fcai-%s --user=%s --private-users=no --drop-capability=all --no-new-privileges=yes --property=MemoryMax=__MEMORY_KIB__K --property=TasksMax=128 --property=CPUQuota=100%% --bind=%s:/workspace --bind-ro=%s/command:/firecrab/command --bind-ro=%s/container-start:/firecrab/container-start --chdir=/workspace --setenv=HOME=/workspace --setenv=PATH=/usr/local/bin:/usr/bin:/bin /bin/sh /firecrab/container-start\n' "$cmd" "$invocation" "$user" "$work" "$cmd" "$cmd" >>"$cmd/launch"
    ;;
  *) fail_run 'unsupported_execution_mode';;
  esac
  tool mkfifo "$cmd/outpipe" "$cmd/errpipe"
  exec 9>&-
  ( tool head -c "$((output_limit + 1))" <"$cmd/outpipe" >"$cmd/stdout"; count=$(tool wc -c <"$cmd/stdout"); if [ "$count" -gt "$output_limit" ]; then touch "$cmd/output-limit"; terminate_user; fi ) & out_reader=$!
  ( tool head -c "$((output_limit + 1))" <"$cmd/errpipe" >"$cmd/stderr"; count=$(tool wc -c <"$cmd/stderr"); if [ "$count" -gt "$output_limit" ]; then touch "$cmd/output-limit"; terminate_user; fi ) & err_reader=$!
  set +e
  tool timeout -s KILL '__SECONDS__' /bin/sh "$cmd/launch" >"$cmd/outpipe" 2>"$cmd/errpipe" & launcher=$!
  while :; do
    wait "$launcher"; code=$?
    if ! kill -0 "$launcher" 2>/dev/null; then break; fi
  done
  stop_user || exit 76
  wait "$out_reader" "$err_reader" 2>/dev/null || true
  if [ -f "$cmd/output-limit" ]; then code=125; fi
  printf '%s' "$code" >"$cmd/exit"
  touch "$cmd/finished"
  emit_output
  exit "$code"
  ;;
cancel|cleanup)
  touch "$fence"
  same_boot=false
  if [ "$(cat "$control/boot" 2>/dev/null || true)" = "$boot" ]; then same_boot=true; fi
  if [ "$same_boot" = true ] && [ -f "$control/supervisor" ]; then
    read -r pid start <"$control/supervisor"
    case "$pid:$start" in *[!0-9:]*|'') exit 73;; esac
    if [ -r "/proc/$pid/stat" ] && [ "$(tool awk '{print $22}' "/proc/$pid/stat")" = "$start" ]; then kill -TERM "$pid" 2>/dev/null || true; fi
  fi
  confirmed=false
  stop_container || exit 76
  for turn in 1 2 3 4 5 6 7 8 9 10; do
    terminate_user
    if ! user_alive && { [ "$same_boot" = false ] || current_complete; }; then confirmed=true; break; fi
    tool sleep 0.2
  done
  [ "$confirmed" = true ] || exit 76
  if [ "$action" = cleanup ]; then
    if [ "$persistent" = true ] && [ -d "$work" ]; then chown -hR 0:0 "$work"; chmod 700 "$work"; fi
    if id -u "$user" >/dev/null 2>&1; then
      if command -v userdel >/dev/null 2>&1; then userdel "$user"; else tool deluser "$user"; fi
    fi
    rm -rf "$base"
  fi
  ;;
recover)
  [ -d "$cmd" ] || exit 79
  [ "$(cat "$control/current" 2>/dev/null || true)" = "$invocation" ] || exit 78
  ! user_alive || exit 76
  emit_output
  ;;
status)
  [ -f "$cmd/finished" ] && ! user_alive || exit 78
  cat "$cmd/exit"
  ;;
export)
  [ -f "$control/staged" ] || exit 79
  { [ "$(cat "$control/boot")" != "$boot" ] || current_complete; } && ! user_alive || exit 76
  [ -d "$work" ] || exit 74
  archive_tar -cf - --exclude=.firecrab-workspace-initialized --exclude=node_modules --exclude=target --exclude=.git --exclude=.venv --exclude=venv --exclude=.next --exclude=dist --exclude=coverage --exclude=.coverage --exclude=.cache --exclude=.npm --exclude=.pnpm-store --exclude=.rustup --exclude=__pycache__ --exclude=.pytest_cache --exclude=.mypy_cache --exclude=.ruff_cache --exclude='.cargo/registry' --exclude='.cargo/git' --exclude='.yarn/cache' --exclude=.ssh --exclude=.aws --exclude=.codex --exclude=.claude --exclude='.env*' --exclude='*.key' --exclude='*.pem' -C "$work" .
  ;;
processes)
  [ -f "$control/boot" ] && [ "$(cat "$control/boot")" = "$boot" ] || exit 77
  uid=$(id -u "$user" 2>/dev/null) || exit 0
  count=0
  for status in /proc/[0-9]*/status; do
    [ -r "$status" ] || continue
    actual=$(tool awk '/^Uid:/ {print $2; exit}' "$status" 2>/dev/null) || continue
    [ "$actual" = "$uid" ] || continue
    pid=${status#/proc/}; pid=${pid%/status}
    start=$(tool awk '{sub(/^.*\) /, ""); print $20}' "/proc/$pid/stat" 2>/dev/null) || continue
    state=$(tool awk '/^State:/ {print $2; exit}' "$status" 2>/dev/null) || continue
    printf '%s %s %s %s\n' "$pid" "$start" "$boot" "$state"
    count=$((count+1)); [ "$count" -lt 128 ] || break
  done
  ;;
signal)
  IFS=: read -r pid start expected signal <<'FIRECRAB_SIGNAL'
__PAYLOAD__
FIRECRAB_SIGNAL
  [ "$expected" = "$boot" ] && [ "$(cat "$control/boot" 2>/dev/null)" = "$boot" ] || exit 77
  case "$pid:$start" in *[!0-9:]*|'') exit 73;; esac
  [ "$pid" -gt 1 ] || exit 73
  case "$signal" in TERM|KILL) ;; *) exit 73;; esac
  uid=$(id -u "$user" 2>/dev/null) || exit 72
  actual=$(tool awk '/^Uid:/ {print $2; exit}' "/proc/$pid/status" 2>/dev/null) || exit 73
  [ "$actual" = "$uid" ] || exit 72
  [ "$(tool awk '{sub(/^.*\) /, ""); print $20}' "/proc/$pid/stat" 2>/dev/null)" = "$start" ] || exit 73
  kill -"$signal" "$pid"
  ;;
*) exit 75;;
esac
"#;

#[cfg(test)]
#[path = "guest_tests.rs"]
mod tests;
