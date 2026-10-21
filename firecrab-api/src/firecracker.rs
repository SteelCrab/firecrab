//! Renders and runs the Firecracker microVM: `firecracker.json` generation,
//! launching the VM's shim (`crate::vm_shim`), readiness polling, and the
//! exit monitor that records guest-initiated state transitions.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use firecrab_helper_protocol::network::HelperFailure;
use serde::Serialize;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use crate::console::ConsoleBroker;
use crate::model::{MacAddr, VmRecord, VmState};
use crate::resource_limits::ResourceLimits;
use crate::state::{AppState, RuntimeConfig};
use crate::vm_shim::client::{SessionEvent, ShimConnectError, ShimControl, ShimSession};
use crate::vm_shim::protocol::ExitReport;
use crate::vm_shim::server::ShimConfig;

/// Delay between readiness probe attempts while waiting for the API socket.
const READY_POLL_INTERVAL: Duration = Duration::from_millis(20);
/// How often a unit's runtime directory is checked for the shim's exit.
const UNIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Failure modes for rendering config, launching, or stopping a VM.
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
    /// The helper could not start the VM's systemd unit.
    #[error("the network helper could not start the VM's unit: {0}")]
    Unit(#[source] crate::network::NetworkError),
    /// Couldn't start the VM's shim process.
    #[error("failed to start the VM shim {program}: {source}")]
    Spawn {
        /// The program that failed to spawn.
        program: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The shim exited before accepting a connection — typically because it
    /// could not start Firecracker at all.
    #[error(
        "the VM shim exited before it accepted a connection{}",
        reason.as_deref().map(|reason| format!(": {reason}")).unwrap_or_default()
    )]
    ShimExited {
        /// What the shim recorded in `shim.err`, if anything.
        reason: Option<String>,
    },
    /// The shim never became reachable, or refused the handshake.
    #[error(transparent)]
    Shim(#[from] ShimConnectError),
    /// Firecracker exited before its API socket answered.
    #[error("Firecracker exited before its API socket became ready")]
    ExitedBeforeReady,
    /// The API socket never answered within the readiness timeout.
    #[error("Firecracker API socket {path} did not become ready within {timeout:?}")]
    NotReady {
        /// The API socket path that never became ready.
        path: PathBuf,
        /// The timeout that was exceeded.
        timeout: Duration,
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
                kernel_image_path: absolute(kernel_image_path),
                initrd_path: initrd_path.map(absolute),
                boot_args: boot_args.to_owned(),
            },
            drives: vec![Drive {
                drive_id: "rootfs".to_owned(),
                path_on_host: absolute(rootfs_path),
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

/// `path` against this process's working directory. Storage roots and the
/// image root may be relative to it, and Firecracker may not share it: a
/// shim in a systemd unit runs in its runtime directory.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_owned())
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
#[cfg(test)]
pub fn api_sock_path(runtime: &crate::artifacts::HostRuntimePaths) -> PathBuf {
    runtime.api_socket.clone()
}

/// Path to this start's tee'd guest console log file.
pub fn console_log_path(runtime: &crate::artifacts::HostRuntimePaths) -> PathBuf {
    runtime.console_log.clone()
}

/// How the API starts a VM's shim (`crate::vm_shim`).
#[derive(Debug, Clone)]
pub(crate) enum ShimLauncher {
    /// Asks the privileged helper to run the shim in its own systemd unit
    /// (`firecrab-vm-<uuid>.service`), owned by PID 1: the VM outlives this
    /// process, and startup reconciliation reattaches to it.
    SystemdUnit(crate::network::NetworkClient),
    /// Runs the shim as a task inside this process. Tests use it because a
    /// test binary cannot exec itself as `vm-shim`.
    #[cfg(test)]
    InProcess,
}

impl ShimLauncher {
    /// Every VM runs in a systemd unit. `FIRECRAB_VM_LAUNCHER` used to choose
    /// between that and child processes; a leftover value is reported and
    /// otherwise ignored, so no old setting can tie VMs to the API again.
    pub(crate) fn from_setting(
        setting: Option<&str>,
        network: &crate::network::NetworkClient,
    ) -> Self {
        if let Some(value) = setting
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != "systemd")
        {
            tracing::warn!(
                value,
                "FIRECRAB_VM_LAUNCHER is no longer used; VMs always run in systemd units"
            );
        }
        Self::SystemdUnit(network.clone())
    }
}

/// Resolves `binary` the way `execvp` would, so it can be handed to a
/// process that does not share this one's `PATH` (a systemd unit).
pub(crate) fn resolve_on_path(
    binary: &Path,
    search: Option<&std::ffi::OsStr>,
) -> Result<PathBuf, FirecrackerError> {
    if binary.is_absolute() {
        return Ok(binary.to_owned());
    }
    let not_found = || FirecrackerError::Spawn {
        program: binary.to_owned(),
        source: io::Error::new(io::ErrorKind::NotFound, "not found on PATH"),
    };
    if binary.components().count() > 1 {
        return std::path::absolute(binary).map_err(|_| not_found());
    }
    let search = search.ok_or_else(not_found)?;
    env::split_paths(search)
        .map(|directory| directory.join(binary))
        .find(|candidate| is_executable(candidate))
        .ok_or_else(not_found)
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// How a VM's session with its shim ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VmExit {
    /// Firecracker exited and the shim reported how.
    Exited(ExitReport),
    /// The shim went away without reporting an exit.
    Lost,
}

impl VmExit {
    fn clean(self) -> bool {
        matches!(self, VmExit::Exited(exit) if exit.status.clean())
    }

    /// Whether the end was asked for, through the API or not (`systemctl
    /// stop` of the unit, a host shutdown).
    fn requested(self) -> bool {
        matches!(self, VmExit::Exited(exit) if exit.stop_requested)
    }
}

/// A running shim this API launched.
#[derive(Debug)]
// Only the test-only `Task` variant is small.
#[cfg_attr(test, allow(clippy::large_enum_variant))]
enum ShimHandle {
    /// A shim in a systemd unit. It is not this process's child, so its end is
    /// observed through the files it leaves behind.
    Unit {
        vm_id: Uuid,
        runtime: crate::artifacts::HostRuntimePaths,
        network: crate::network::NetworkClient,
    },
    #[cfg(test)]
    Task(tokio::task::JoinHandle<io::Result<crate::vm_shim::server::ShimExit>>),
}

impl ShimHandle {
    /// Resolves once the shim itself has exited. Awaited at most once.
    async fn wait(&mut self) {
        match self {
            ShimHandle::Unit { runtime, .. } => {
                // The shim writes `exit.json` when Firecracker exits and
                // `shim.err` when it cannot start one; either means it is done.
                while !runtime.exit_status.exists() && !runtime.shim_error.exists() {
                    tokio::time::sleep(UNIT_POLL_INTERVAL).await;
                }
            }
            #[cfg(test)]
            ShimHandle::Task(handle) => {
                let _ = handle.await;
            }
        }
    }
}

impl Drop for ShimHandle {
    fn drop(&mut self) {
        // An in-process shim has to be cancelled, which drops (and so kills)
        // its Firecracker; a unit is stopped through the helper instead.
        #[cfg(test)]
        if let ShimHandle::Task(handle) = self {
            handle.abort();
        }
    }
}

/// A started VM that has not been handed to [`register_and_watch`] yet.
/// Dropping it kills the VM, so an early return during startup never leaks
/// a running guest.
#[derive(Debug)]
pub struct FirecrackerProcess {
    /// `None` once registered: the exit monitor owns the shim from then on.
    shim: Option<ShimHandle>,
    vmm_pid: u32,
    control: ShimControl,
    console: Arc<ConsoleBroker>,
    exit: watch::Receiver<Option<VmExit>>,
    api_sock: PathBuf,
}

impl FirecrackerProcess {
    /// Firecracker's OS process id.
    pub fn pid(&self) -> Option<u32> {
        Some(self.vmm_pid)
    }

    /// This VM's serial console broker, for watching its boot output (e.g.
    /// the network-readiness sentinel line) before it's registered with
    /// [`register_and_watch`].
    pub fn console(&self) -> &ConsoleBroker {
        self.console.as_ref()
    }

    /// Kills the VM and waits (up to `grace` per step) for Firecracker and
    /// its shim to be gone, for start paths that fail after launch.
    pub async fn abort(self, grace: Duration) {
        self.control.kill();
        self.shut_down(grace).await;
    }

    async fn shut_down(mut self, grace: Duration) {
        let mut exit = self.exit.clone();
        let _ = tokio::time::timeout(grace, exit.wait_for(Option::is_some)).await;
        if let Some(mut shim) = self.shim.take() {
            let _ = tokio::time::timeout(grace, shim.wait()).await;
            // Dropping a child or task kills it; nothing kills a unit whose
            // shim is wedged, so it is stopped (a no-op once it has exited).
            abandon(shim, grace).await;
        }
    }
}

impl Drop for FirecrackerProcess {
    fn drop(&mut self) {
        if self.shim.is_some() {
            self.control.kill();
        }
    }
}

/// Map entry for a live VM: Firecracker's process id, a channel that
/// resolves once the exit monitor has recorded the terminal state, the
/// console broker for anyone attaching a terminal to this VM's ttyS0, and
/// the shim control used to stop it.
#[derive(Debug, Clone)]
pub struct VmProcess {
    /// Firecracker's OS process id.
    pub pid: u32,
    /// Resolves to `true` once the exit monitor has recorded the final state.
    pub exited: watch::Receiver<bool>,
    /// Broker for anyone attaching a terminal to this VM's ttyS0.
    pub console: Arc<ConsoleBroker>,
    /// Stop/kill requests for this VM's shim.
    pub control: ShimControl,
}

/// Sends `SIGKILL` to `pid`. The last resort when a VM's shim does not
/// confirm that Firecracker exited; tests also use it to kill a guest behind
/// the API's back.
pub fn sigkill(pid: u32) {
    // SAFETY: sending a signal is memory-safe; pid races only misdeliver signals.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

/// Stops a registered VM: SIGTERM through its shim, then SIGKILL through it,
/// then SIGKILL to Firecracker directly when the shim does not confirm the
/// exit, waiting up to `grace` after each step for the exit monitor.
pub(crate) async fn stop_registered(id: Uuid, process: VmProcess, grace: Duration) {
    let VmProcess {
        pid,
        control,
        mut exited,
        ..
    } = process;
    let mut wait = async || {
        tokio::time::timeout(grace, exited.wait_for(|done| *done))
            .await
            .is_ok_and(|done| done.is_ok())
    };
    control.terminate();
    if wait().await {
        return;
    }
    control.kill();
    if wait().await {
        return;
    }
    // The shim did not confirm the exit — it may be gone or wedged. Kill
    // Firecracker directly so a VM recorded as stopped is never still running.
    tracing::error!(vm_id = %id, pid, "vm shim did not confirm the stop; killing Firecracker directly");
    sigkill(pid);
    wait().await;
}

/// Registers the process in the state map and spawns the exit monitor.
///
/// The monitor is the only writer of guest-initiated terminal states: a
/// clean exit lands on `stopped`, a crash or a lost shim on `error`, and an
/// exit while the record is `stopping` always lands on `stopped` so the stop
/// API and the monitor never fight over the result. A stop the shim was asked
/// for from outside the API (`systemctl stop` of the VM's unit, a host
/// shutdown) also lands on `stopped`, whatever signal ended Firecracker.
pub fn register_and_watch(state: &AppState, id: Uuid, mut process: FirecrackerProcess) {
    let (exited_tx, exited_rx) = watch::channel(false);
    let pid = process.vmm_pid;
    let shim = process.shim.take();
    let mut exit = process.exit.clone();
    let api_sock = process.api_sock.clone();
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
                console: Arc::clone(&process.console),
                control: process.control.clone(),
            },
        );

    let state = state.clone();
    let watched_pid = pid;
    tokio::spawn(async move {
        let outcome = match exit.wait_for(Option::is_some).await {
            Ok(outcome) => outcome.unwrap_or(VmExit::Lost),
            Err(_) => VmExit::Lost,
        };
        if let Some(mut shim) = shim {
            // The shim exits right after reporting; reap it so a `Process`
            // shim never lingers as a zombie.
            let _ = tokio::time::timeout(state.runtime.stop_grace, shim.wait()).await;
        }
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

        let clean_exit = outcome.clean();
        let requested = outcome.requested();
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
                    vm.state = if vm.state == VmState::Stopping || clean_exit || requested {
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
                requested,
                ?outcome,
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

/// Launches the VM's shim, attaches to it, and waits until Firecracker's API
/// socket answers. Any failure kills the VM before the error returns, so a
/// failed start never leaks a running guest.
pub(crate) async fn spawn_vm(
    config: &RuntimeConfig,
    runtime: &crate::artifacts::HostRuntimePaths,
    id: Uuid,
    enable_pci: bool,
    limits: ResourceLimits,
    process_metrics: Arc<Mutex<crate::process_metrics::ProcessMetricsTracker>>,
) -> Result<FirecrackerProcess, FirecrackerError> {
    fs::create_dir_all(&runtime.dir).map_err(|source| FirecrackerError::CreateDirectory {
        path: runtime.dir.clone(),
        source,
    })?;

    let mut shim = launch_shim(
        &config.shim,
        ShimConfig {
            vm_id: id,
            runtime: runtime.clone(),
            firecracker: config.firecracker_binary.clone(),
            enable_pci,
            stop_grace: config.stop_grace,
        },
        limits,
    )
    .await?;
    // A shim that cannot start Firecracker exits without ever accepting;
    // racing its exit keeps that from costing the whole ready timeout.
    let session = tokio::select! {
        session = crate::vm_shim::client::connect(&runtime.shim_socket, id, config.ready_timeout) => match session {
            Ok(session) => session,
            Err(error) => {
                abandon(shim, config.stop_grace).await;
                return Err(error.into());
            }
        },
        () = shim.wait() => {
            let reason = fs::read_to_string(&runtime.shim_error)
                .ok()
                .map(|reason| reason.trim().to_owned())
                .filter(|reason| !reason.is_empty());
            return Err(FirecrackerError::ShimExited { reason });
        }
    };
    let process = attach(
        id,
        session,
        Some(shim),
        runtime.api_socket.clone(),
        process_metrics,
    );

    let mut exited = process.exit.clone();
    let ready = tokio::select! {
        ready = wait_ready(&runtime.api_socket, config.ready_timeout) => ready,
        _ = exited.wait_for(Option::is_some) => Err(FirecrackerError::ExitedBeforeReady),
    };
    if let Err(error) = ready {
        process.abort(config.stop_grace).await;
        return Err(error);
    }
    Ok(process)
}

/// Takes over a VM whose shim outlived the API that launched it (startup
/// reconciliation, #123). This API never launched that shim, so it holds no
/// handle to it: dropping the result leaves the VM running, and the exit
/// monitor learns of its end from the session alone.
pub(crate) fn adopt(
    id: Uuid,
    runtime: &crate::artifacts::HostRuntimePaths,
    session: ShimSession,
    process_metrics: Arc<Mutex<crate::process_metrics::ProcessMetricsTracker>>,
) -> FirecrackerProcess {
    attach(
        id,
        session,
        None,
        runtime.api_socket.clone(),
        process_metrics,
    )
}

/// Wires a shim session to a console broker and an exit channel.
fn attach(
    id: Uuid,
    session: ShimSession,
    shim: Option<ShimHandle>,
    api_sock: PathBuf,
    process_metrics: Arc<Mutex<crate::process_metrics::ProcessMetricsTracker>>,
) -> FirecrackerProcess {
    let ShimSession {
        vmm_pid,
        control,
        events,
    } = session;
    let console = Arc::new(ConsoleBroker::new());
    console.attach_control(control.clone());
    let (exit_tx, exit) = watch::channel(None);
    spawn_event_pump(id, events, Arc::clone(&console), process_metrics, exit_tx);
    FirecrackerProcess {
        shim,
        vmm_pid,
        control,
        console,
        exit,
        api_sock,
    }
}

/// Gives up on a shim that never accepted a connection. A child or task is
/// killed by dropping it; a unit belongs to systemd, so the helper stops it.
async fn abandon(shim: ShimHandle, stop_grace: Duration) {
    if let ShimHandle::Unit { vm_id, network, .. } = &shim
        && let Err(error) = network.stop_vm_unit(*vm_id, stop_grace).await
    {
        tracing::warn!(%vm_id, %error, "failed to stop a VM unit that never answered");
    }
}

async fn launch_shim(
    launcher: &ShimLauncher,
    config: ShimConfig,
    limits: ResourceLimits,
) -> Result<ShimHandle, FirecrackerError> {
    match launcher {
        ShimLauncher::SystemdUnit(network) => {
            // A unit shares neither this process's working directory nor its
            // PATH, and the helper only accepts absolute paths.
            let firecracker = resolve_on_path(&config.firecracker, env::var_os("PATH").as_deref())?;
            let runtime_dir = std::path::absolute(&config.runtime.dir).map_err(|source| {
                FirecrackerError::CreateDirectory {
                    path: config.runtime.dir.clone(),
                    source,
                }
            })?;
            if let Err(error) = network
                .start_vm_unit(
                    config.vm_id,
                    runtime_dir.clone(),
                    firecracker,
                    config.enable_pci,
                    config.stop_grace,
                    limits,
                )
                .await
            {
                // Unless the request never left or the helper refused it,
                // systemd may have started the unit before the answer was
                // lost; nothing else would ever stop it.
                if !matches!(
                    error,
                    crate::network::NetworkError::Unavailable { .. }
                        | crate::network::NetworkError::Helper(
                            HelperFailure::InvalidRequest { .. }
                        )
                ) && let Err(stop_error) =
                    network.stop_vm_unit(config.vm_id, config.stop_grace).await
                {
                    tracing::warn!(vm_id = %config.vm_id, error = %stop_error, "failed to stop a VM unit whose start failed");
                }
                return Err(FirecrackerError::Unit(error));
            }
            Ok(ShimHandle::Unit {
                vm_id: config.vm_id,
                runtime: crate::artifacts::HostRuntimePaths::in_dir(runtime_dir),
                network: network.clone(),
            })
        }
        #[cfg(test)]
        ShimLauncher::InProcess => Ok(ShimHandle::Task(tokio::spawn(
            crate::vm_shim::server::serve(config, std::future::pending()),
        ))),
    }
}

/// Feeds the shim's console stream to the metrics tracker and the console
/// broker, and publishes how the session ended.
fn spawn_event_pump(
    id: Uuid,
    mut events: mpsc::UnboundedReceiver<SessionEvent>,
    console: Arc<ConsoleBroker>,
    process_metrics: Arc<Mutex<crate::process_metrics::ProcessMetricsTracker>>,
    exit: watch::Sender<Option<VmExit>>,
) {
    tokio::spawn(async move {
        let outcome = loop {
            match events.recv().await {
                Some(SessionEvent::Output(chunk)) => {
                    // Metrics read the raw stream (FIRECRAB_USAGE lines);
                    // the broker filters those out for viewers.
                    process_metrics
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .ingest_console(id, &chunk);
                    console.push_output(&chunk);
                }
                Some(SessionEvent::Exited(exit)) => break VmExit::Exited(exit),
                Some(SessionEvent::Lost) | None => break VmExit::Lost,
            }
        };
        let _ = exit.send(Some(outcome));
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

/// Stops an unregistered VM: SIGTERM through the shim, SIGKILL after
/// `grace`, then waits for the shim to exit. Registered VMs are stopped
/// through their [`VmProcess::control`] instead, since the exit monitor owns
/// them.
#[cfg(test)]
pub async fn stop_vm(process: FirecrackerProcess, grace: Duration) -> Result<(), FirecrackerError> {
    process.control.terminate();
    let mut exit = process.exit.clone();
    if tokio::time::timeout(grace, exit.wait_for(Option::is_some))
        .await
        .is_err()
    {
        process.control.kill();
    }
    process.shut_down(grace).await;
    Ok(())
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

    /// Runs a fake Firecracker, tolerating `ETXTBSY` the way
    /// [`super::spawn_firecracker`] does.
    ///
    /// `execve` refuses a file that any process still holds open for writing,
    /// and a concurrent test's forked child inherits exactly such a descriptor
    /// for the moment between its `fork` and its `exec`. `rename` cannot help,
    /// because the descriptor follows the inode.
    pub fn exec_tolerating_busy(path: &Path, api_sock: &Path) -> std::process::ExitStatus {
        const BUSY_ATTEMPTS: u32 = 8;
        let mut attempt = 0;
        loop {
            match std::process::Command::new(path)
                .arg("--api-sock")
                .arg(api_sock)
                .status()
            {
                Ok(status) => return status,
                Err(source)
                    if source.kind() == std::io::ErrorKind::ExecutableFileBusy
                        && attempt < BUSY_ATTEMPTS =>
                {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(10 * u64::from(attempt)));
                }
                Err(source) => panic!("fake-firecracker must be executable: {source}"),
            }
        }
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

    fn test_config(binary: &Path, ready_timeout: Duration) -> RuntimeConfig {
        RuntimeConfig {
            vms_dir: PathBuf::from("/nonexistent"),
            firecracker_binary: binary.to_owned(),
            ready_timeout,
            stop_grace: Duration::from_secs(5),
            network_ready_timeout: Duration::from_secs(5),
            shim: ShimLauncher::InProcess,
        }
    }

    fn typical_limits() -> ResourceLimits {
        ResourceLimits::for_vm(512, 1)
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
    fn config_paths_do_not_depend_on_the_working_directory() {
        // The default storage root is `data`, relative to the API's working
        // directory; a shim in a systemd unit runs in its runtime directory.
        let directory = tempdir().unwrap();
        let vm = record(1, 512);
        let runtime = runtime_for(&directory.path().join("vms"), vm.id);
        let cwd = env::current_dir().unwrap();

        let path = write_config(
            &runtime,
            Path::new("data/vms/x/d/disk.ext4"),
            &vm,
            Path::new("images/vmlinux"),
            Some(Path::new("images/initrd")),
            "console=ttyS0",
            None,
        )
        .unwrap();

        let config: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let absolute = |relative: &str| cwd.join(relative).to_str().unwrap().to_owned();
        assert_eq!(
            config["boot-source"]["kernel_image_path"],
            absolute("images/vmlinux")
        );
        assert_eq!(
            config["boot-source"]["initrd_path"],
            absolute("images/initrd")
        );
        assert_eq!(
            config["drives"][0]["path_on_host"],
            absolute("data/vms/x/d/disk.ext4")
        );
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

    use super::test_support::{
        SERVE_LOOP, exec_tolerating_busy, fake_firecracker, process_alive, short_tempdir,
    };

    /// Guards the `fake_firecracker` write: the +x bit is set and the body is
    /// complete, so a fresh fake runs.
    ///
    /// `ETXTBSY` is tolerated the same way [`spawn_firecracker`] tolerates it,
    /// because it is not something the helper can prevent: another test's
    /// forked child can still hold a write descriptor on the file we just
    /// wrote, and `rename` leaves the inode — and therefore that descriptor —
    /// untouched.
    #[test]
    fn fake_firecracker_script_can_be_execd_immediately() {
        let directory = short_tempdir();
        let sock = directory.path().join("sock");
        let path = fake_firecracker(directory.path(), "sys.exit(0)\n");
        let status = exec_tolerating_busy(&path, &sock);
        assert!(
            status.success(),
            "fresh fake-firecracker must be executable"
        );
    }

    /// Exercises the `ETXTBSY` retry instead of assuming it: an open write
    /// descriptor makes `execve` fail for exactly as long as it lives, which
    /// is the race a parallel run hits by accident.
    #[test]
    fn a_busy_fake_firecracker_is_retried_until_the_writer_closes_it() {
        let directory = short_tempdir();
        let sock = directory.path().join("sock");
        let path = fake_firecracker(directory.path(), "sys.exit(0)\n");

        let writer = fs::OpenOptions::new().write(true).open(&path).unwrap();
        match std::process::Command::new(&path).status() {
            Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy => {}
            other => panic!(
                "an open write descriptor must make execve fail with ETXTBSY, \
                 otherwise this test proves nothing: {other:?}"
            ),
        }
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            drop(writer);
        });

        let status = exec_tolerating_busy(&path, &sock);
        releaser.join().unwrap();
        assert!(
            status.success(),
            "the retry must outlast a transient ETXTBSY"
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
            &test_config(&binary, Duration::from_secs(5)),
            &runtime,
            id,
            false,
            typical_limits(),
            test_metrics(),
        )
        .await
        .unwrap();
        let pid = process.pid().unwrap() as i32;
        assert!(process_alive(pid));

        // The broker must see the same bytes the shim tees to the log file.
        // They travel shim → socket → broker, so give them a moment.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let (backlog, _receiver) = process.console.subscribe();
            if String::from_utf8_lossy(&backlog).contains("booted") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the console broker never saw the guest's boot output"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

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
            &test_config(&binary, Duration::from_millis(500)),
            &runtime,
            id,
            false,
            typical_limits(),
            test_metrics(),
        )
        .await
        .unwrap_err();

        assert_matches!(error, FirecrackerError::NotReady { .. });
        assert!(!process_alive(fake_pid(&runtime)));
    }

    #[tokio::test]
    async fn a_vmm_that_dies_before_readiness_fails_the_start_fast() {
        let directory = short_tempdir();
        let vms_dir = directory.path().join("vms");
        let binary = fake_firecracker(directory.path(), "sys.exit(7)\n");
        let id = Uuid::new_v4();
        let runtime = runtime_for(&vms_dir, id);
        fs::write(&runtime.config, "{}").unwrap();

        let started = std::time::Instant::now();
        let error = spawn_vm(
            &test_config(&binary, Duration::from_secs(5)),
            &runtime,
            id,
            false,
            typical_limits(),
            test_metrics(),
        )
        .await
        .unwrap_err();

        // The shim can exit before attachment, during the handshake (resetting
        // the socket), or after its greeting. Check the actual VMM exit rather
        // than which branch wins that race, so unrelated startup errors cannot
        // satisfy this test.
        let exit: ExitReport =
            serde_json::from_slice(&fs::read(&runtime.exit_status).unwrap()).unwrap();
        assert_eq!(exit.status.code, Some(7));
        assert_eq!(exit.status.signal, None);
        assert!(!exit.stop_requested);
        assert!(!process_alive(fake_pid(&runtime)));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "an exited VMM must fail the start without waiting out the ready timeout, got {error:?} after {:?}",
            started.elapsed()
        );
    }

    /// The most common install error must stay visible in the startup
    /// timeline, not only in the API's journal.
    #[tokio::test]
    async fn a_missing_firecracker_binary_is_named_in_the_start_error() {
        let directory = short_tempdir();
        let id = Uuid::new_v4();
        let runtime = runtime_for(&directory.path().join("vms"), id);
        fs::write(&runtime.config, "{}").unwrap();
        let missing = directory.path().join("no-such-firecracker");

        let error = spawn_vm(
            &test_config(&missing, Duration::from_secs(5)),
            &runtime,
            id,
            false,
            typical_limits(),
            test_metrics(),
        )
        .await
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("no-such-firecracker"), "{message}");
        assert!(message.contains("No such file"), "{message}");
    }

    /// How the fake unit helper answers `StartVmUnit`.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum UnitHelper {
        /// Runs the real shim in-process, as systemd would, and answers Ok.
        StartShims,
        /// Answers Ok without starting anything: a unit whose shim hangs.
        Silent,
        /// Closes the connection without answering, as a helper that dies
        /// (or a call that times out) after systemd already started the unit.
        DropAnswer,
    }

    /// A fake helper for the systemd launcher that records every request.
    fn spawn_unit_helper(
        socket: &Path,
        mode: UnitHelper,
    ) -> Arc<Mutex<Vec<firecrab_helper_protocol::network::NetworkRequest>>> {
        use firecrab_helper_protocol::PROTOCOL_VERSION;
        use firecrab_helper_protocol::framing::{read_frame, write_frame};
        use firecrab_helper_protocol::network::{
            NetworkRequest, NetworkRequestEnvelope, NetworkResponseEnvelope,
        };

        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let listener = tokio::net::UnixListener::bind(socket).unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let recorded = Arc::clone(&recorded);
                tokio::spawn(async move {
                    while let Ok(envelope) =
                        read_frame::<_, NetworkRequestEnvelope>(&mut stream).await
                    {
                        if let NetworkRequest::StartVmUnit {
                            vm_id,
                            runtime_dir,
                            firecracker,
                            enable_pci,
                            stop_grace_ms,
                            ..
                        } = &envelope.request
                            && mode == UnitHelper::StartShims
                        {
                            tokio::spawn(crate::vm_shim::server::serve(
                                ShimConfig {
                                    vm_id: *vm_id,
                                    runtime: crate::artifacts::HostRuntimePaths::in_dir(
                                        runtime_dir.clone(),
                                    ),
                                    firecracker: firecracker.clone(),
                                    enable_pci: *enable_pci,
                                    stop_grace: Duration::from_millis(*stop_grace_ms),
                                },
                                std::future::pending(),
                            ));
                        }
                        recorded.lock().unwrap().push(envelope.request.clone());
                        if mode == UnitHelper::DropAnswer
                            && matches!(envelope.request, NetworkRequest::StartVmUnit { .. })
                        {
                            return;
                        }
                        let response = NetworkResponseEnvelope {
                            pipe: None,
                            version: PROTOCOL_VERSION,
                            request_id: envelope.request_id,
                            result: Ok(()),
                        };
                        if write_frame(&mut stream, &response).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        requests
    }

    fn unit_config(directory: &Path, binary: &Path, ready_timeout: Duration) -> RuntimeConfig {
        let socket = directory.join("helper.sock");
        let mut config = test_config(binary, ready_timeout);
        config.shim =
            ShimLauncher::SystemdUnit(crate::network::NetworkClient::with_socket_path(socket));
        config
    }

    #[test]
    fn every_launcher_setting_runs_vms_in_systemd_units() {
        let network = crate::network::NetworkClient::with_socket_path(PathBuf::from("/x"));
        // `process` was removed; an old setting must not keep VMs tied to the API.
        for setting in [
            None,
            Some(""),
            Some("systemd"),
            Some("process"),
            Some("bogus"),
        ] {
            assert_matches!(
                ShimLauncher::from_setting(setting, &network),
                ShimLauncher::SystemdUnit(_),
                "{setting:?}"
            );
        }
    }

    #[test]
    fn a_bare_firecracker_name_is_resolved_on_the_path() {
        use std::os::unix::fs::PermissionsExt;

        let directory = short_tempdir();
        let bin = directory.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let firecracker = bin.join("firecracker");
        fs::write(&firecracker, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&firecracker, fs::Permissions::from_mode(0o755)).unwrap();
        let search = std::env::join_paths([Path::new("/nonexistent"), bin.as_path()]).unwrap();

        assert_eq!(
            resolve_on_path(Path::new("firecracker"), Some(&search)).unwrap(),
            firecracker
        );
        assert_eq!(
            resolve_on_path(Path::new("/opt/firecracker"), Some(&search)).unwrap(),
            Path::new("/opt/firecracker")
        );
        assert!(resolve_on_path(Path::new("no-such-binary"), Some(&search)).is_err());
    }

    #[tokio::test]
    async fn the_systemd_launcher_starts_the_vm_through_the_helper() {
        let directory = short_tempdir();
        let binary = fake_firecracker(
            directory.path(),
            &format!("signal.signal(signal.SIGTERM, lambda *_: sys.exit(0)){SERVE_LOOP}"),
        );
        let requests = spawn_unit_helper(
            &directory.path().join("helper.sock"),
            UnitHelper::StartShims,
        );
        let id = Uuid::new_v4();
        let runtime = runtime_for(&directory.path().join("vms"), id);
        fs::write(&runtime.config, "{}").unwrap();

        let config = unit_config(directory.path(), &binary, Duration::from_secs(5));
        let process = spawn_vm(
            &config,
            &runtime,
            id,
            false,
            typical_limits(),
            test_metrics(),
        )
        .await
        .unwrap();

        let started = requests.lock().unwrap().clone();
        assert_matches!(
            started.as_slice(),
            [firecrab_helper_protocol::network::NetworkRequest::StartVmUnit {
                vm_id,
                runtime_dir,
                firecracker,
                ..
            }] if *vm_id == id && runtime_dir.is_absolute() && firecracker == &binary
        );
        stop_vm(process, Duration::from_secs(5)).await.unwrap();
    }

    #[tokio::test]
    async fn the_unit_request_carries_the_vms_resource_limits() {
        let directory = short_tempdir();
        let binary = fake_firecracker(
            directory.path(),
            &format!("signal.signal(signal.SIGTERM, lambda *_: sys.exit(0)){SERVE_LOOP}"),
        );
        let requests = spawn_unit_helper(
            &directory.path().join("helper.sock"),
            UnitHelper::StartShims,
        );
        let id = Uuid::new_v4();
        let runtime = runtime_for(&directory.path().join("vms"), id);
        fs::write(&runtime.config, "{}").unwrap();

        // Not the limits `typical_limits` gives the other tests, so a default
        // slipping through cannot pass.
        let config = unit_config(directory.path(), &binary, Duration::from_secs(5));
        let limits = ResourceLimits::for_vm(2048, 4);
        let process = spawn_vm(&config, &runtime, id, false, limits, test_metrics())
            .await
            .unwrap();

        let started = requests.lock().unwrap().clone();
        assert_matches!(
            started.as_slice(),
            [
                firecrab_helper_protocol::network::NetworkRequest::StartVmUnit {
                    memory_max_mib: Some(2304),
                    cpu_quota_percent: Some(500),
                    ..
                }
            ]
        );
        stop_vm(process, Duration::from_secs(5)).await.unwrap();
    }

    #[tokio::test]
    async fn a_unit_whose_shim_cannot_start_firecracker_fails_fast_with_the_reason() {
        let directory = short_tempdir();
        spawn_unit_helper(
            &directory.path().join("helper.sock"),
            UnitHelper::StartShims,
        );
        let id = Uuid::new_v4();
        let runtime = runtime_for(&directory.path().join("vms"), id);
        fs::write(&runtime.config, "{}").unwrap();
        let missing = directory.path().join("no-such-firecracker");

        let started = std::time::Instant::now();
        let error = spawn_vm(
            &unit_config(directory.path(), &missing, Duration::from_secs(5)),
            &runtime,
            id,
            false,
            typical_limits(),
            test_metrics(),
        )
        .await
        .unwrap_err();

        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(error.to_string().contains("no-such-firecracker"), "{error}");
    }

    #[tokio::test]
    async fn a_unit_that_never_answers_is_stopped_through_the_helper() {
        let directory = short_tempdir();
        let binary = fake_firecracker(directory.path(), SERVE_LOOP);
        let requests = spawn_unit_helper(&directory.path().join("helper.sock"), UnitHelper::Silent);
        let id = Uuid::new_v4();
        let runtime = runtime_for(&directory.path().join("vms"), id);
        fs::write(&runtime.config, "{}").unwrap();

        let result = spawn_vm(
            &unit_config(directory.path(), &binary, Duration::from_millis(300)),
            &runtime,
            id,
            false,
            typical_limits(),
            test_metrics(),
        )
        .await;

        assert!(result.is_err());
        let seen = requests.lock().unwrap().clone();
        assert!(
            seen.iter().any(|request| matches!(
                request,
                firecrab_helper_protocol::network::NetworkRequest::StopVmUnit { vm_id } if *vm_id == id
            )),
            "a unit that never accepted must be stopped, got {seen:?}"
        );
    }

    fn stopped_unit(
        requests: &Mutex<Vec<firecrab_helper_protocol::network::NetworkRequest>>,
        id: Uuid,
    ) -> bool {
        requests.lock().unwrap().iter().any(|request| {
            matches!(
                request,
                firecrab_helper_protocol::network::NetworkRequest::StopVmUnit { vm_id } if *vm_id == id
            )
        })
    }

    #[tokio::test]
    async fn a_unit_start_whose_answer_is_lost_is_stopped_through_the_helper() {
        let directory = short_tempdir();
        let binary = fake_firecracker(directory.path(), SERVE_LOOP);
        let requests = spawn_unit_helper(
            &directory.path().join("helper.sock"),
            UnitHelper::DropAnswer,
        );
        let id = Uuid::new_v4();
        let runtime = runtime_for(&directory.path().join("vms"), id);
        fs::write(&runtime.config, "{}").unwrap();

        let result = spawn_vm(
            &unit_config(directory.path(), &binary, Duration::from_secs(5)),
            &runtime,
            id,
            false,
            typical_limits(),
            test_metrics(),
        )
        .await;

        assert!(result.is_err());
        assert!(
            stopped_unit(&requests, id),
            "systemd may have started the unit; it must be stopped"
        );
    }

    #[tokio::test]
    async fn a_unit_that_never_gets_ready_is_stopped_through_the_helper() {
        let directory = short_tempdir();
        let binary = fake_firecracker(directory.path(), "time.sleep(60)\n");
        let requests = spawn_unit_helper(
            &directory.path().join("helper.sock"),
            UnitHelper::StartShims,
        );
        let id = Uuid::new_v4();
        let runtime = runtime_for(&directory.path().join("vms"), id);
        fs::write(&runtime.config, "{}").unwrap();

        let result = spawn_vm(
            &unit_config(directory.path(), &binary, Duration::from_millis(300)),
            &runtime,
            id,
            false,
            typical_limits(),
            test_metrics(),
        )
        .await;

        assert_matches!(result, Err(FirecrackerError::NotReady { .. }));
        assert!(
            stopped_unit(&requests, id),
            "an aborted start must not leave its unit behind"
        );
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
            &test_config(&binary, Duration::from_secs(5)),
            &runtime,
            id,
            false,
            typical_limits(),
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
            &test_config(&binary, Duration::from_secs(5)),
            &runtime,
            id,
            true,
            typical_limits(),
            test_metrics(),
        )
        .await
        .unwrap();
        stop_vm(process, Duration::from_secs(5)).await.unwrap();
    }
}
