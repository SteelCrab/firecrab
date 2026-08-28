//! Renders and runs the Firecracker microVM process: `firecracker.json`
//! generation, spawning + readiness polling of the child, and the exit
//! monitor that records guest-initiated state transitions.

use std::env;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::watch;
use uuid::Uuid;

use crate::console::ConsoleBroker;
use crate::model::{MacAddr, VmRecord, VmState};

use crate::state::AppState;

/// Bytes read off the guest console per `read()` call before it is teed to
/// disk and broadcast to viewers.
const CONSOLE_READ_CHUNK: usize = 4096;

/// Delay between readiness probe attempts while waiting for the API socket.
const READY_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Failure modes for rendering config, spawning, or stopping Firecracker.
#[derive(Debug, Error)]
pub enum FirecrackerError {
    /// Couldn't create the VM's own directory.
    #[error("failed to create VM directory {path}: {source}")]
    CreateDirectory {
        /// The directory that couldn't be created.
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Couldn't serialize the config to JSON.
    #[error("failed to serialize Firecracker config for {path}: {source}")]
    Serialize {
        /// The config file path this was destined for.
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// Couldn't write the rendered config to disk.
    #[error("failed to write Firecracker config {path}: {source}")]
    Write {
        /// The config file path that failed to write.
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A pre-existing API socket file couldn't be removed before binding.
    #[error("failed to remove stale API socket {path}: {source}")]
    StaleSocket {
        /// The stale socket path.
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Couldn't open the console log file for writing.
    #[error("failed to open console log {path}: {source}")]
    ConsoleLog {
        /// The console log path that couldn't be opened.
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Couldn't spawn the Firecracker binary.
    #[error("failed to spawn Firecracker binary {binary}: {source}")]
    Spawn {
        /// The binary path that failed to spawn.
        binary: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The API socket never answered within the readiness timeout.
    #[error("Firecracker API socket {path} did not become ready within {timeout:?}")]
    NotReady {
        /// The API socket path that never became ready.
        path: PathBuf,
        /// The timeout that was exceeded.
        timeout: Duration,
    },
    /// Waiting for or killing the process failed.
    #[error("failed to terminate Firecracker process: {source}")]
    // Only the owned-handle stop path builds this; handlers stop via pid
    // signals because the exit monitor owns the child.
    #[allow(dead_code)]
    Terminate {
        #[source]
        source: io::Error,
    },
}

/// The JSON body Firecracker's `--config-file` expects.
#[derive(Debug, Serialize)]
pub struct FirecrackerConfig {
    /// Kernel image path and boot args.
    #[serde(rename = "boot-source")]
    boot_source: BootSource,
    /// Block devices, always exactly the one root drive today.
    drives: Vec<Drive>,
    /// The VM's TAP interface, absent until TAP automation assigns one.
    #[serde(rename = "network-interfaces", skip_serializing_if = "Vec::is_empty")]
    network_interfaces: Vec<NetworkInterface>,
    /// vCPU/memory sizing.
    #[serde(rename = "machine-config")]
    machine_config: MachineConfig,
}

/// `boot-source` section of [`FirecrackerConfig`].
#[derive(Debug, Serialize)]
struct BootSource {
    /// Absolute path to the kernel image.
    kernel_image_path: PathBuf,
    /// Absolute path to the initrd image, if this template's kernel needs
    /// one (e.g. a distro kernel whose virtio_blk/ext4 are modules rather
    /// than builtin).
    #[serde(skip_serializing_if = "Option::is_none")]
    initrd_path: Option<PathBuf>,
    /// Kernel command line.
    boot_args: String,
}

/// One entry in [`FirecrackerConfig`]'s `drives` list.
#[derive(Debug, Serialize)]
struct Drive {
    /// Firecracker-side drive identifier.
    drive_id: String,
    /// Absolute host path to the backing file.
    path_on_host: PathBuf,
    /// Whether this drive is mounted as the VM's root filesystem.
    is_root_device: bool,
    /// Whether the drive is exposed read-only to the guest.
    is_read_only: bool,
}

/// `machine-config` section of [`FirecrackerConfig`].
#[derive(Debug, Serialize)]
struct MachineConfig {
    /// vCPU count.
    vcpu_count: u8,
    /// RAM in MiB.
    mem_size_mib: u32,
}

/// One entry in [`FirecrackerConfig`]'s `network-interfaces` list.
#[derive(Debug, Serialize)]
struct NetworkInterface {
    /// Firecracker-side interface identifier.
    iface_id: String,
    /// MAC address presented to the guest.
    guest_mac: String,
    /// Host-side TAP device name this interface is backed by.
    host_dev_name: String,
}

/// The VM's TAP attachment: its host TAP device name (from
/// [`crate::network::tap_name`]) and the guest MAC its IPAM lease pins.
#[derive(Debug, Clone)]
pub struct VmNetwork {
    /// Host-side TAP device name.
    pub tap_name: String,
    /// MAC address the guest's `eth0` presents.
    pub guest_mac: MacAddr,
}

impl FirecrackerConfig {
    /// Builds the config for `vm`'s single root drive at `rootfs_path`, and
    /// its network interface if TAP automation has attached one.
    pub fn for_vm(
        vm: &VmRecord,
        kernel_image_path: &Path,
        initrd_path: Option<&Path>,
        boot_args: &str,
        rootfs_path: &Path,
        network: Option<&VmNetwork>,
    ) -> Self {
        Self {
            boot_source: BootSource {
                kernel_image_path: kernel_image_path.to_owned(),
                initrd_path: initrd_path.map(Path::to_owned),
                boot_args: boot_args.to_owned(),
            },
            drives: vec![Drive {
                drive_id: "rootfs".to_owned(),
                path_on_host: rootfs_path.to_owned(),
                is_root_device: true,
                is_read_only: false,
            }],
            network_interfaces: network
                .map(|network| {
                    vec![NetworkInterface {
                        iface_id: "eth0".to_owned(),
                        guest_mac: network.guest_mac.to_string(),
                        host_dev_name: network.tap_name.clone(),
                    }]
                })
                .unwrap_or_default(),
            machine_config: MachineConfig {
                vcpu_count: vm.cpu,
                mem_size_mib: vm.ram,
            },
        }
    }
}

/// Renders the VM's Firecracker config into this start's runtime directory.
/// The root drive points at the active generation file from `prepare_rootfs`.
pub fn write_config(
    runtime: &crate::artifacts::HostRuntimePaths,
    rootfs_path: &Path,
    vm: &VmRecord,
    kernel_image_path: &Path,
    initrd_path: Option<&Path>,
    boot_args: &str,
    network: Option<&VmNetwork>,
) -> Result<PathBuf, FirecrackerError> {
    fs::create_dir_all(&runtime.dir).map_err(|source| FirecrackerError::CreateDirectory {
        path: runtime.dir.clone(),
        source,
    })?;

    let path = runtime.config.clone();
    let config = FirecrackerConfig::for_vm(
        vm,
        kernel_image_path,
        initrd_path,
        boot_args,
        rootfs_path,
        network,
    );
    let json =
        serde_json::to_vec_pretty(&config).map_err(|source| FirecrackerError::Serialize {
            path: path.clone(),
            source,
        })?;
    fs::write(&path, json).map_err(|source| FirecrackerError::Write {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

/// Resolves the Firecracker binary path: `FIRECRAB_FIRECRACKER_BIN` if set,
/// otherwise `firecracker` looked up on `PATH`.
pub fn default_firecracker_binary() -> PathBuf {
    env::var_os("FIRECRAB_FIRECRACKER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("firecracker"))
}

/// Path to this start's Firecracker API Unix socket.
pub fn api_sock_path(runtime: &crate::artifacts::HostRuntimePaths) -> PathBuf {
    runtime.api_socket.clone()
}

/// Path to this start's tee'd guest console log file.
pub fn console_log_path(runtime: &crate::artifacts::HostRuntimePaths) -> PathBuf {
    runtime.console_log.clone()
}

/// A live, owned Firecracker child process and its associated resources.
#[derive(Debug)]
pub struct FirecrackerProcess {
    /// The spawned child process.
    child: Child,
    /// Path to its API socket, removed on stop/exit.
    api_sock: PathBuf,
    /// Broker fanning out this VM's serial console to attached viewers.
    console: Arc<ConsoleBroker>,
}

impl FirecrackerProcess {
    /// The child's OS process id, if it hasn't already been reaped.
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// This VM's serial console broker, for watching its boot output (e.g.
    /// the network-readiness sentinel line) before it's registered with
    /// [`register_and_watch`].
    pub fn console(&self) -> &ConsoleBroker {
        self.console.as_ref()
    }
}

/// Map entry for a live VM: the process id plus a channel that resolves once
/// the exit monitor has finished recording the terminal state, plus the
/// console broker for anyone attaching a terminal to this VM's ttyS0.
#[derive(Debug, Clone)]
pub struct VmProcess {
    /// The Firecracker process's OS process id.
    pub pid: u32,
    /// Resolves to `true` once the exit monitor has recorded the final state.
    pub exited: watch::Receiver<bool>,
    /// Broker for anyone attaching a terminal to this VM's ttyS0.
    pub console: Arc<ConsoleBroker>,
}

/// Sends `SIGTERM` to `pid`, requesting graceful shutdown.
pub fn sigterm(pid: u32) {
    // SAFETY: sending a signal is memory-safe; pid races only misdeliver signals.
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
}

/// Sends `SIGKILL` to `pid`, forcing immediate termination.
pub fn sigkill(pid: u32) {
    // SAFETY: as above.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

/// Makes Firecracker terminate if the API that owns it disappears. Tokio's
/// child handle normally has `kill_on_drop`, but a service manager can tear
/// down the API runtime before its asynchronous child watcher is polled. In
/// that window Firecracker would otherwise be re-parented to PID 1 while its
/// TAP, lease, and nft policy still look owned by the old API.
///
/// `PR_SET_PDEATHSIG` closes that gap in the child immediately before exec.
/// The parent-PID check handles the tiny race where the API exits between the
/// fork and the prctl call: a re-parented child then refuses to start rather
/// than becoming an untracked MicroVM.
fn terminate_with_parent(command: &mut Command) {
    // SAFETY: reads this process's PID before the child is forked.
    let parent_pid = unsafe { libc::getpid() };
    // SAFETY: `pre_exec` runs the closure only in the child between fork and
    // exec. The closure uses only Linux process-control syscalls and reports
    // failure back through `spawn`, before Firecracker receives user input.
    unsafe {
        command.pre_exec(move || {
            // SAFETY: both libc calls are async-signal-safe process-control
            // syscalls and this is the only code run in the post-fork child.
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != parent_pid {
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
}

/// Registers the process in the state map and spawns the exit monitor.
///
/// The monitor owns the child and is the only writer of guest-initiated
/// terminal states: a clean exit lands on `stopped`, a crash on `error`, and
/// an exit while the record is `stopping` always lands on `stopped` so the
/// stop API and the monitor never fight over the result.
pub fn register_and_watch(state: &AppState, id: Uuid, process: FirecrackerProcess) {
    let (exited_tx, exited_rx) = watch::channel(false);
    let pid = process.pid().unwrap_or_default();
    let FirecrackerProcess {
        mut child,
        api_sock,
        console,
    } = process;
    // Drop any leftover samples from a previous generation before this PID
    // starts reporting (exit monitors only clear when they still own the map).
    state
        .process_metrics
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear(id);
    state
        .processes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
            id,
            VmProcess {
                pid,
                exited: exited_rx,
                console,
            },
        );

    let state = state.clone();
    let watched_pid = pid;
    tokio::spawn(async move {
        let status = child.wait().await;
        let _ = fs::remove_file(&api_sock);

        // Only tear down map/metrics entries for *this* process. After a
        // quick stop→start the new generation may already be registered under
        // the same VM id; a late exit monitor must not remove it or wipe the
        // new guest-agent samples (the root cause of empty Resource usage
        // after restart).
        let still_ours = {
            let mut processes = state
                .processes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match processes.get(&id) {
                Some(current) if current.pid == watched_pid => {
                    processes.remove(&id);
                    true
                }
                // Newer generation already registered, or this id is no longer
                // in the map (stop/start race). Do not claim ownership — a late
                // monitor must not wipe the new process's metrics or flip state.
                Some(_) | None => false,
            }
        };
        if still_ours {
            state
                .process_metrics
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clear(id);
        }

        let clean_exit = status.as_ref().is_ok_and(|status| status.success());
        let updated = {
            let mut vms = state
                .vms
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match vms.get_mut(&id) {
                Some(vm)
                    if still_ours
                        && matches!(
                            vm.state,
                            VmState::Starting | VmState::Running | VmState::Stopping
                        ) =>
                {
                    vm.state = if vm.state == VmState::Stopping || clean_exit {
                        VmState::Stopped
                    } else {
                        VmState::Error
                    };
                    vm.startup_step = None;
                    Some(vm.clone())
                }
                _ => None,
            }
        };

        if let Some(record) = updated {
            tracing::info!(
                vm_id = %id,
                clean_exit,
                state = ?record.state,
                "vm process exited"
            );
            // Only stop_vm's own SIGTERM path tears the network down today;
            // a guest-initiated poweroff or an external kill lands here
            // instead and would otherwise leave the TAP/policy orphaned.
            crate::handlers::vms::teardown_vm_network(&state, id).await;
            let store = state.store.clone();
            match tokio::task::spawn_blocking(move || store.update(&record)).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::error!(vm_id = %id, %error, "failed to persist exit state");
                }
                Err(error) => {
                    tracing::error!(vm_id = %id, %error, "exit state persistence task failed");
                }
            }
        }

        let _ = exited_tx.send(true);
    });
}

/// Spawns Firecracker for the VM and waits until its API socket answers.
/// On readiness timeout the process is killed before the error returns, so a
/// failed start never leaks a stray Firecracker.
pub async fn spawn_vm(
    binary: &Path,
    runtime: &crate::artifacts::HostRuntimePaths,
    id: Uuid,
    enable_pci: bool,
    ready_timeout: Duration,
    process_metrics: Arc<Mutex<crate::process_metrics::ProcessMetricsTracker>>,
) -> Result<FirecrackerProcess, FirecrackerError> {
    fs::create_dir_all(&runtime.dir).map_err(|source| FirecrackerError::CreateDirectory {
        path: runtime.dir.clone(),
        source,
    })?;

    // Firecracker refuses to start if the socket path already exists.
    let api_sock = api_sock_path(runtime);
    match fs::remove_file(&api_sock) {
        Ok(()) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(FirecrackerError::StaleSocket {
                path: api_sock,
                source,
            });
        }
    }

    let console_log = console_log_path(runtime);
    let console_error = |source| FirecrackerError::ConsoleLog {
        path: console_log.clone(),
        source,
    };
    // The guest's ttyS0 is Firecracker's own stdin/stdout (no PTY needed —
    // the guest kernel already owns a real tty on its end, so raw bytes both
    // ways are enough). stderr is Firecracker's own diagnostics, unrelated
    // to the guest console, so it keeps going straight to the log file.
    let log_file = File::create(&console_log).map_err(console_error)?;
    let stderr = log_file.try_clone().map_err(console_error)?;

    let mut command = Command::new(binary);
    command
        .arg("--api-sock")
        .arg(&api_sock)
        .arg("--config-file")
        .arg(&runtime.config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true);
    // Firecracker defaults to virtio-mmio. Templates whose distro kernels
    // need the PCI device model opt in through `requires_pci_transport`.
    if enable_pci {
        command.arg("--enable-pci");
    }
    terminate_with_parent(&mut command);

    let mut child = spawn_firecracker(binary, &mut command).await?;

    let stdin = child.stdin.take().expect("stdin was piped");
    let stdout = child.stdout.take().expect("stdout was piped");
    let console = Arc::new(ConsoleBroker::new());
    console.attach_stdin(stdin).await;
    spawn_console_reader(id, stdout, log_file, Arc::clone(&console), process_metrics);

    if let Err(error) = wait_ready(&api_sock, ready_timeout).await {
        let _ = child.kill().await;
        let _ = fs::remove_file(&api_sock);
        return Err(error);
    }

    Ok(FirecrackerProcess {
        child,
        api_sock,
        console,
    })
}

/// Reads the guest's console once and fans it out: every chunk is teed to
/// `console.log` on disk and pushed to the broker for any attached viewers.
/// Runs until the pipe closes (the Firecracker process exiting closes its
/// stdout, which is this loop's only way to know to stop).
fn spawn_console_reader(
    id: Uuid,
    mut stdout: tokio::process::ChildStdout,
    log_file: File,
    console: Arc<ConsoleBroker>,
    process_metrics: Arc<Mutex<crate::process_metrics::ProcessMetricsTracker>>,
) {
    let mut log_file = tokio::fs::File::from_std(log_file);
    tokio::spawn(async move {
        let mut buffer = [0_u8; CONSOLE_READ_CHUNK];
        // Shared filter for the log file so FIRECRAB_USAGE never lands on disk
        // (dashboard log view reads this file). Terminal filtering is owned by
        // ConsoleBroker; metrics always see the raw chunk first.
        let mut log_usage_filter = Vec::new();
        loop {
            let read = match stdout.read(&mut buffer).await {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) => {
                    tracing::warn!(vm_id = %id, %error, "console read failed");
                    break;
                }
            };
            let chunk = &buffer[..read];
            // Ingest metrics from the raw stream first.
            process_metrics
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .ingest_console(id, chunk);
            let for_log =
                crate::console::filter_guest_usage_for_terminal(&mut log_usage_filter, chunk);
            if !for_log.is_empty()
                && let Err(error) = log_file.write_all(&for_log).await
            {
                tracing::warn!(vm_id = %id, %error, "failed to tee console output to log file");
            }
            console.push_output(chunk);
        }
    });
}

/// Polls the API socket with a minimal HTTP request until it answers or
/// `timeout` elapses.
async fn wait_ready(api_sock: &Path, timeout: Duration) -> Result<(), FirecrackerError> {
    let probe = async {
        loop {
            if let Ok(mut stream) = UnixStream::connect(api_sock).await
                && stream
                    .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
                    .await
                    .is_ok()
            {
                let mut buffer = [0_u8; 32];
                if let Ok(read) = stream.read(&mut buffer).await
                    && read > 0
                {
                    return;
                }
            }
            tokio::time::sleep(READY_POLL_INTERVAL).await;
        }
    };

    tokio::time::timeout(timeout, probe)
        .await
        .map_err(|_| FirecrackerError::NotReady {
            path: api_sock.to_owned(),
            timeout,
        })
}

/// Stops the process with SIGTERM, escalating to SIGKILL after `grace`.
/// Always reaps the child before returning.
///
/// Handlers stop VMs via pid signals since the exit monitor owns the child;
/// this owned-handle primitive remains for shutdown paths that hold the
/// process directly (e.g. draining on graceful shutdown).
#[allow(dead_code)]
pub async fn stop_vm(
    mut process: FirecrackerProcess,
    grace: Duration,
) -> Result<(), FirecrackerError> {
    let terminate = |source| FirecrackerError::Terminate { source };

    match process.child.id() {
        Some(pid) => {
            // SAFETY: pid belongs to the still-unreaped child we own.
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
            match tokio::time::timeout(grace, process.child.wait()).await {
                Ok(status) => {
                    status.map_err(terminate)?;
                }
                Err(_) => {
                    process.child.kill().await.map_err(terminate)?;
                }
            }
        }
        None => {
            process.child.wait().await.map_err(terminate)?;
        }
    }

    let _ = fs::remove_file(&process.api_sock);
    Ok(())
}

/// `execve` of a script that was just written can fail with `ETXTBSY` on
/// this CI sandbox when many tests spawn at once. Production Firecracker
/// is a stable binary, so the retry is test-only.
async fn spawn_firecracker(
    binary: &Path,
    command: &mut Command,
) -> Result<Child, FirecrackerError> {
    const BUSY_ATTEMPTS: u32 = 8;
    let mut attempt = 0;
    loop {
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(source)
                if cfg!(test)
                    && source.kind() == io::ErrorKind::ExecutableFileBusy
                    && attempt < BUSY_ATTEMPTS =>
            {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(10 * u64::from(attempt))).await;
            }
            Err(source) => {
                return Err(FirecrackerError::Spawn {
                    binary: binary.to_owned(),
                    source,
                });
            }
        }
    }
}

/// Test-only helpers for standing in a fake Firecracker binary (a small
/// Python script) so process-spawning tests don't need the real one.
#[cfg(test)]
pub(crate) mod test_support {
    use std::fs;
    use std::path::{Path, PathBuf};

    /// Every fake writes "{api_sock}.pid" so tests can probe the process after
    /// the FirecrackerProcess handle is consumed.
    pub const FAKE_PRELUDE: &str = r#"#!/usr/bin/env python3
import os, signal, socket, sys, time
sock_path = sys.argv[sys.argv.index("--api-sock") + 1]
open(sock_path + ".pid", "w").write(str(os.getpid()))
"#;

    /// Answers the readiness probe forever, like a running guest.
    pub const SERVE_LOOP: &str = r#"
print("booted", flush=True)
print("FIRECRAB_NETWORK_READY 172.30.0.5", flush=True)
srv = socket.socket(socket.AF_UNIX)
srv.bind(sock_path)
srv.listen(1)
while True:
    conn, _ = srv.accept()
    conn.recv(1024)
    conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
    conn.close()
"#;

    /// Serves the readiness probe once, then exits like a guest poweroff.
    pub const SERVE_ONCE_THEN_EXIT: &str = r#"
print("FIRECRAB_NETWORK_READY 172.30.0.5", flush=True)
srv = socket.socket(socket.AF_UNIX)
srv.bind(sock_path)
srv.listen(1)
conn, _ = srv.accept()
conn.recv(1024)
conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
conn.close()
sys.exit(0)
"#;

    /// Writes an executable fake Firecracker script combining the prelude
    /// with `body`.
    ///
    /// The bytes are fsync'd and renamed into place so `execve` does not see
    /// a writer still attached. Without that, concurrent `cargo test --release`
    /// runs hit `ETXTBSY` (`ExecutableFileBusy`) on this sandbox.
    pub fn fake_firecracker(directory: &Path, body: &str) -> PathBuf {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        fs::create_dir_all(directory).unwrap();
        let path = directory.join("fake-firecracker");
        let tmp = directory.join("fake-firecracker.tmp");
        {
            let mut file = fs::File::create(&tmp).unwrap();
            file.write_all(format!("{FAKE_PRELUDE}{body}").as_bytes())
                .unwrap();
            file.sync_all().unwrap();
        }
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).unwrap();
        fs::rename(&tmp, &path).unwrap();
        path
    }

    /// A tempdir under `/tmp`: Unix socket paths are capped near 108 bytes,
    /// so this avoids a deeply nested `TMPDIR`.
    pub fn short_tempdir() -> tempfile::TempDir {
        tempfile::tempdir_in("/tmp").unwrap()
    }

    /// Whether a process with `pid` still exists.
    pub fn process_alive(pid: i32) -> bool {
        // SAFETY: signal 0 only probes for existence.
        unsafe { libc::kill(pid, 0) == 0 }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;
    use uuid::Uuid;

    use super::*;
    use crate::model::VmState;
    use core::assert_matches;

    fn record(cpu: u8, ram: u32) -> VmRecord {
        VmRecord {
            id: Uuid::new_v4(),
            name: "test-vm".to_owned(),
            purpose: crate::model::VmPurpose::Instance,
            state: VmState::Created,
            template: "ubuntu-26.04".to_owned(),
            template_version: "ubuntu-26.04-v1".to_owned(),
            template_kernel_sha256: "kernel".to_owned(),
            template_rootfs_sha256: "rootfs".to_owned(),
            template_boot_args_sha256: "args".to_owned(),
            cpu,
            ram,
            disk_gb: 2,
            egress_policy: Default::default(),
            micro_network_id: Uuid::from_u128(1),
            storage_root: "default".to_owned(),
            disk_generation: None,
            last_runtime_id: None,
            startup_step: None,
            startup_timeline: Vec::new(),
            env: Default::default(),
        }
    }

    fn runtime_for(vms_root: &Path, vm_id: Uuid) -> crate::artifacts::HostRuntimePaths {
        let paths = crate::artifacts::VmArtifactPaths::for_vm(vms_root, vm_id);
        paths.create_runtime(Uuid::new_v4()).unwrap()
    }

    fn test_metrics() -> Arc<Mutex<crate::process_metrics::ProcessMetricsTracker>> {
        Arc::new(Mutex::new(
            crate::process_metrics::ProcessMetricsTracker::default(),
        ))
    }

    #[test]
    fn config_reflects_requested_resources() {
        let directory = tempdir().unwrap();
        let vms_dir = directory.path().join("vms");
        let vm = record(3, 768);
        let runtime = runtime_for(&vms_dir, vm.id);
        let rootfs = Path::new("/tmp/rootfs.ext4");

        let path = write_config(
            &runtime,
            rootfs,
            &vm,
            Path::new("/images/vmlinux"),
            None,
            "console=ttyS0",
            None,
        )
        .unwrap();

        assert_eq!(path, runtime.config);
        let config: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(config["machine-config"]["vcpu_count"], 3);
        assert_eq!(config["machine-config"]["mem_size_mib"], 768);
    }

    #[test]
    fn config_wires_boot_source_and_root_drive() {
        let directory = tempdir().unwrap();
        let vms_dir = directory.path().join("vms");
        let vm = record(1, 512);
        let runtime = runtime_for(&vms_dir, vm.id);
        let rootfs = Path::new("/tmp/guest-rootfs.ext4");

        let path = write_config(
            &runtime,
            rootfs,
            &vm,
            Path::new("/images/vmlinux"),
            None,
            "console=ttyS0",
            None,
        )
        .unwrap();

        let config: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            config["boot-source"]["kernel_image_path"],
            "/images/vmlinux"
        );
        assert_eq!(config["boot-source"]["boot_args"], "console=ttyS0");

        let drive = &config["drives"][0];
        assert_eq!(drive["drive_id"], "rootfs");
        assert_eq!(drive["is_root_device"], true);
        assert_eq!(drive["is_read_only"], false);
        assert_eq!(drive["path_on_host"], rootfs.to_str().unwrap());
    }

    #[test]
    fn initrd_path_is_omitted_when_the_template_has_none() {
        let directory = tempdir().unwrap();
        let vms_dir = directory.path().join("vms");
        let vm = record(1, 512);
        let runtime = runtime_for(&vms_dir, vm.id);

        let path = write_config(
            &runtime,
            Path::new("/tmp/rootfs.ext4"),
            &vm,
            Path::new("/images/vmlinux"),
            None,
            "console=ttyS0",
            None,
        )
        .unwrap();

        let config: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(config["boot-source"].get("initrd_path").is_none());
    }

    #[test]
    fn initrd_path_is_wired_through_when_the_template_has_one() {
        let directory = tempdir().unwrap();
        let vms_dir = directory.path().join("vms");
        let vm = record(1, 512);
        let runtime = runtime_for(&vms_dir, vm.id);

        let path = write_config(
            &runtime,
            Path::new("/tmp/rootfs.ext4"),
            &vm,
            Path::new("/images/vmlinux"),
            Some(Path::new("/images/initrd")),
            "console=ttyS0",
            None,
        )
        .unwrap();

        let config: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(config["boot-source"]["initrd_path"], "/images/initrd");
    }

    #[test]
    fn rewriting_config_overwrites_previous_content() {
        let directory = tempdir().unwrap();
        let vms_dir = directory.path().join("vms");
        let mut vm = record(1, 512);
        let runtime = runtime_for(&vms_dir, vm.id);

        write_config(
            &runtime,
            Path::new("/tmp/rootfs.ext4"),
            &vm,
            Path::new("/images/vmlinux"),
            None,
            "console=ttyS0",
            None,
        )
        .unwrap();
        vm.cpu = 2;
        vm.ram = 1024;
        let path = write_config(
            &runtime,
            Path::new("/tmp/rootfs.ext4"),
            &vm,
            Path::new("/images/vmlinux"),
            None,
            "console=ttyS0",
            None,
        )
        .unwrap();

        let config: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(config["machine-config"]["vcpu_count"], 2);
        assert_eq!(config["machine-config"]["mem_size_mib"], 1024);
    }

    use super::test_support::{SERVE_LOOP, fake_firecracker, process_alive, short_tempdir};

    #[test]
    fn fake_firecracker_script_can_be_execd_immediately() {
        let directory = short_tempdir();
        let sock = directory.path().join("sock");
        let path = fake_firecracker(directory.path(), "sys.exit(0)\n");
        let status = std::process::Command::new(&path)
            .arg("--api-sock")
            .arg(&sock)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "fresh fake-firecracker must be executable"
        );
    }

    fn fake_pid(runtime: &crate::artifacts::HostRuntimePaths) -> i32 {
        let pid_file = format!("{}.pid", api_sock_path(runtime).display());
        fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    #[tokio::test]
    async fn spawn_reaches_readiness_and_stop_terminates_the_process() {
        let directory = short_tempdir();
        let vms_dir = directory.path().join("vms");
        let binary = fake_firecracker(
            directory.path(),
            &format!("signal.signal(signal.SIGTERM, lambda *_: sys.exit(0)){SERVE_LOOP}"),
        );
        let id = Uuid::new_v4();
        let mut runtime = runtime_for(&vms_dir, id);
        fs::write(&runtime.config, "{}").unwrap();

        let process = spawn_vm(
            &binary,
            &runtime,
            id,
            false,
            Duration::from_secs(5),
            test_metrics(),
        )
        .await
        .unwrap();
        let pid = process.pid().unwrap() as i32;
        assert!(process_alive(pid));

        // The broker must see the same bytes that get tee'd to the log file.
        let (backlog, _receiver) = process.console.subscribe();
        assert!(
            String::from_utf8_lossy(&backlog).contains("booted"),
            "the console broker must have already seen the guest's boot output"
        );

        stop_vm(process, Duration::from_secs(5)).await.unwrap();

        assert!(!process_alive(pid));
        let console = fs::read_to_string(console_log_path(&runtime)).unwrap();
        assert!(console.contains("booted"));
        let _ = &mut runtime;
    }

    #[tokio::test]
    async fn readiness_timeout_cleans_up_the_process() {
        let directory = short_tempdir();
        let vms_dir = directory.path().join("vms");
        let binary = fake_firecracker(directory.path(), "while True:\n    time.sleep(60)\n");
        let id = Uuid::new_v4();
        let runtime = runtime_for(&vms_dir, id);
        fs::write(&runtime.config, "{}").unwrap();

        let error = spawn_vm(
            &binary,
            &runtime,
            id,
            false,
            Duration::from_millis(500),
            test_metrics(),
        )
        .await
        .unwrap_err();

        assert_matches!(error, FirecrackerError::NotReady { .. });
        assert!(!process_alive(fake_pid(&runtime)));
    }

    #[tokio::test]
    async fn stop_escalates_to_sigkill_when_sigterm_is_ignored() {
        let directory = short_tempdir();
        let vms_dir = directory.path().join("vms");
        let binary = fake_firecracker(
            directory.path(),
            &format!("signal.signal(signal.SIGTERM, signal.SIG_IGN){SERVE_LOOP}"),
        );
        let id = Uuid::new_v4();
        let runtime = runtime_for(&vms_dir, id);
        fs::write(&runtime.config, "{}").unwrap();

        let process = spawn_vm(
            &binary,
            &runtime,
            id,
            false,
            Duration::from_secs(5),
            test_metrics(),
        )
        .await
        .unwrap();
        let pid = process.pid().unwrap() as i32;
        let started = std::time::Instant::now();

        stop_vm(process, Duration::from_millis(200)).await.unwrap();

        assert!(started.elapsed() >= Duration::from_millis(200));
        assert!(!process_alive(pid));
    }

    #[tokio::test]
    async fn spawn_enables_pci_only_when_the_template_requests_it() {
        let directory = short_tempdir();
        let vms_dir = directory.path().join("vms");
        let binary = fake_firecracker(
            directory.path(),
            &format!(
                "if '--enable-pci' not in sys.argv:\n    raise SystemExit('missing --enable-pci')\n{SERVE_LOOP}"
            ),
        );
        let id = Uuid::new_v4();
        let runtime = runtime_for(&vms_dir, id);
        fs::write(&runtime.config, "{}").unwrap();

        let process = spawn_vm(
            &binary,
            &runtime,
            id,
            true,
            Duration::from_secs(5),
            test_metrics(),
        )
        .await
        .unwrap();
        stop_vm(process, Duration::from_secs(5)).await.unwrap();
    }
}
