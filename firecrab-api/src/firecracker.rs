use std::env;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::watch;
use uuid::Uuid;

use crate::model::{VmRecord, VmState};
use crate::rootfs;
use crate::state::AppState;

const CONFIG_FILE_NAME: &str = "firecracker.json";
const API_SOCK_FILE_NAME: &str = "firecracker.sock";
const CONSOLE_LOG_FILE_NAME: &str = "console.log";
const READY_POLL_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Debug, Error)]
pub enum FirecrackerError {
    #[error("failed to create VM directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to serialize Firecracker config for {path}: {source}")]
    Serialize {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to write Firecracker config {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to remove stale API socket {path}: {source}")]
    StaleSocket {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to open console log {path}: {source}")]
    ConsoleLog {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to spawn Firecracker binary {binary}: {source}")]
    Spawn {
        binary: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Firecracker API socket {path} did not become ready within {timeout:?}")]
    NotReady { path: PathBuf, timeout: Duration },
    #[error("failed to terminate Firecracker process: {source}")]
    // Only the owned-handle stop path builds this; handlers stop via pid
    // signals because the exit monitor owns the child.
    #[allow(dead_code)]
    Terminate {
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Serialize)]
pub struct FirecrackerConfig {
    #[serde(rename = "boot-source")]
    boot_source: BootSource,
    drives: Vec<Drive>,
    #[serde(rename = "machine-config")]
    machine_config: MachineConfig,
}

#[derive(Debug, Serialize)]
struct BootSource {
    kernel_image_path: PathBuf,
    boot_args: String,
}

#[derive(Debug, Serialize)]
struct Drive {
    drive_id: String,
    path_on_host: PathBuf,
    is_root_device: bool,
    is_read_only: bool,
}

#[derive(Debug, Serialize)]
struct MachineConfig {
    vcpu_count: u8,
    mem_size_mib: u32,
}

impl FirecrackerConfig {
    pub fn for_vm(
        vm: &VmRecord,
        kernel_image_path: &Path,
        boot_args: &str,
        rootfs_path: &Path,
    ) -> Self {
        Self {
            boot_source: BootSource {
                kernel_image_path: kernel_image_path.to_owned(),
                boot_args: boot_args.to_owned(),
            },
            drives: vec![Drive {
                drive_id: "rootfs".to_owned(),
                path_on_host: rootfs_path.to_owned(),
                is_root_device: true,
                is_read_only: false,
            }],
            machine_config: MachineConfig {
                vcpu_count: vm.cpu,
                mem_size_mib: vm.ram,
            },
        }
    }
}

/// Renders the VM's Firecracker config to `{vms_dir}/{id}/firecracker.json`,
/// overwriting any previous config. The root drive points at the disk
/// `prepare_rootfs` publishes for the same VM.
pub fn write_config(
    vms_dir: &Path,
    vm: &VmRecord,
    kernel_image_path: &Path,
    boot_args: &str,
) -> Result<PathBuf, FirecrackerError> {
    let vm_dir = vms_dir.join(vm.id.to_string());
    fs::create_dir_all(&vm_dir).map_err(|source| FirecrackerError::CreateDirectory {
        path: vm_dir.clone(),
        source,
    })?;

    let path = vm_dir.join(CONFIG_FILE_NAME);
    let config = FirecrackerConfig::for_vm(
        vm,
        kernel_image_path,
        boot_args,
        &rootfs::rootfs_path(vms_dir, vm.id),
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

pub fn default_firecracker_binary() -> PathBuf {
    env::var_os("FIRECRAB_FIRECRACKER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("firecracker"))
}

pub fn api_sock_path(vms_dir: &Path, id: Uuid) -> PathBuf {
    vms_dir.join(id.to_string()).join(API_SOCK_FILE_NAME)
}

pub fn console_log_path(vms_dir: &Path, id: Uuid) -> PathBuf {
    vms_dir.join(id.to_string()).join(CONSOLE_LOG_FILE_NAME)
}

#[derive(Debug)]
pub struct FirecrackerProcess {
    child: Child,
    api_sock: PathBuf,
}

impl FirecrackerProcess {
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }
}

/// Map entry for a live VM: the process id plus a channel that resolves once
/// the exit monitor has finished recording the terminal state.
#[derive(Debug, Clone)]
pub struct VmProcess {
    pub pid: u32,
    pub exited: watch::Receiver<bool>,
}

pub fn sigterm(pid: u32) {
    // SAFETY: sending a signal is memory-safe; pid races only misdeliver signals.
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
}

pub fn sigkill(pid: u32) {
    // SAFETY: as above.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
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
    state
        .processes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
            id,
            VmProcess {
                pid,
                exited: exited_rx,
            },
        );

    let FirecrackerProcess { mut child, api_sock } = process;
    let state = state.clone();
    tokio::spawn(async move {
        let status = child.wait().await;
        let _ = fs::remove_file(&api_sock);
        state
            .processes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&id);

        let clean_exit = status.as_ref().is_ok_and(|status| status.success());
        let updated = {
            let mut vms = state
                .vms
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match vms.get_mut(&id) {
                Some(vm)
                    if matches!(
                        vm.state,
                        VmState::Starting | VmState::Running | VmState::Stopping
                    ) =>
                {
                    vm.state = if vm.state == VmState::Stopping || clean_exit {
                        VmState::Stopped
                    } else {
                        VmState::Error
                    };
                    Some(vm.clone())
                }
                _ => None,
            }
        };

        if let Some(record) = updated {
            let store = state.store.clone();
            match tokio::task::spawn_blocking(move || store.update(&record)).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    eprintln!("[ERROR] vm {id}: failed to persist exit state: {error}");
                }
                Err(error) => {
                    eprintln!("[ERROR] vm {id}: exit state persistence task failed: {error}");
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
    vms_dir: &Path,
    id: Uuid,
    config_path: &Path,
    ready_timeout: Duration,
) -> Result<FirecrackerProcess, FirecrackerError> {
    let vm_dir = vms_dir.join(id.to_string());
    fs::create_dir_all(&vm_dir).map_err(|source| FirecrackerError::CreateDirectory {
        path: vm_dir.clone(),
        source,
    })?;

    // Firecracker refuses to start if the socket path already exists.
    let api_sock = api_sock_path(vms_dir, id);
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

    let console_log = console_log_path(vms_dir, id);
    let console_error = |source| FirecrackerError::ConsoleLog {
        path: console_log.clone(),
        source,
    };
    let stdout = File::create(&console_log).map_err(console_error)?;
    let stderr = stdout.try_clone().map_err(console_error)?;

    let mut child = Command::new(binary)
        .arg("--api-sock")
        .arg(&api_sock)
        .arg("--config-file")
        .arg(config_path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| FirecrackerError::Spawn {
            binary: binary.to_owned(),
            source,
        })?;

    if let Err(error) = wait_ready(&api_sock, ready_timeout).await {
        let _ = child.kill().await;
        let _ = fs::remove_file(&api_sock);
        return Err(error);
    }

    Ok(FirecrackerProcess { child, api_sock })
}

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

#[cfg(test)]
pub(crate) mod test_support {
    use std::fs;
    use std::path::{Path, PathBuf};

    // Every fake writes "{api_sock}.pid" so tests can probe the process after
    // the FirecrackerProcess handle is consumed.
    pub const FAKE_PRELUDE: &str = r#"#!/usr/bin/env python3
import os, signal, socket, sys, time
sock_path = sys.argv[sys.argv.index("--api-sock") + 1]
open(sock_path + ".pid", "w").write(str(os.getpid()))
"#;

    pub const SERVE_LOOP: &str = r#"
print("booted", flush=True)
srv = socket.socket(socket.AF_UNIX)
srv.bind(sock_path)
srv.listen(1)
while True:
    conn, _ = srv.accept()
    conn.recv(1024)
    conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
    conn.close()
"#;

    pub fn fake_firecracker(directory: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = directory.join("fake-firecracker");
        fs::write(&path, format!("{FAKE_PRELUDE}{body}")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    // Unix socket paths are capped near 108 bytes, so keep test dirs in /tmp
    // instead of a deeply nested TMPDIR.
    pub fn short_tempdir() -> tempfile::TempDir {
        tempfile::tempdir_in("/tmp").unwrap()
    }

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

    fn record(cpu: u8, ram: u32) -> VmRecord {
        VmRecord {
            id: Uuid::new_v4(),
            name: "test-vm".to_owned(),
            state: VmState::Created,
            template: "ubuntu-26.04".to_owned(),
            template_version: "ubuntu-26.04-v1".to_owned(),
            template_kernel_sha256: "kernel".to_owned(),
            template_rootfs_sha256: "rootfs".to_owned(),
            template_boot_args_sha256: "args".to_owned(),
            cpu,
            ram,
        }
    }

    #[test]
    fn config_reflects_requested_resources() {
        let directory = tempdir().unwrap();
        let vms_dir = directory.path().join("vms");
        let vm = record(3, 768);

        let path = write_config(&vms_dir, &vm, Path::new("/images/vmlinux"), "console=ttyS0")
            .unwrap();

        assert_eq!(path, vms_dir.join(vm.id.to_string()).join("firecracker.json"));
        let config: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(config["machine-config"]["vcpu_count"], 3);
        assert_eq!(config["machine-config"]["mem_size_mib"], 768);
    }

    #[test]
    fn config_wires_boot_source_and_root_drive() {
        let directory = tempdir().unwrap();
        let vms_dir = directory.path().join("vms");
        let vm = record(1, 512);

        let path = write_config(&vms_dir, &vm, Path::new("/images/vmlinux"), "console=ttyS0")
            .unwrap();

        let config: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(config["boot-source"]["kernel_image_path"], "/images/vmlinux");
        assert_eq!(config["boot-source"]["boot_args"], "console=ttyS0");

        let drive = &config["drives"][0];
        assert_eq!(drive["drive_id"], "rootfs");
        assert_eq!(drive["is_root_device"], true);
        assert_eq!(drive["is_read_only"], false);
        assert_eq!(
            drive["path_on_host"],
            rootfs::rootfs_path(&vms_dir, vm.id).to_str().unwrap()
        );
    }

    #[test]
    fn rewriting_config_overwrites_previous_content() {
        let directory = tempdir().unwrap();
        let vms_dir = directory.path().join("vms");
        let mut vm = record(1, 512);

        write_config(&vms_dir, &vm, Path::new("/images/vmlinux"), "console=ttyS0").unwrap();
        vm.cpu = 2;
        vm.ram = 1024;
        let path = write_config(&vms_dir, &vm, Path::new("/images/vmlinux"), "console=ttyS0")
            .unwrap();

        let config: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(config["machine-config"]["vcpu_count"], 2);
        assert_eq!(config["machine-config"]["mem_size_mib"], 1024);
    }

    use super::test_support::{fake_firecracker, process_alive, short_tempdir, SERVE_LOOP};

    fn fake_pid(vms_dir: &Path, id: Uuid) -> i32 {
        let pid_file = format!("{}.pid", api_sock_path(vms_dir, id).display());
        fs::read_to_string(pid_file).unwrap().trim().parse().unwrap()
    }

    #[tokio::test]
    async fn spawn_reaches_readiness_and_stop_terminates_the_process() {
        let directory = short_tempdir();
        let vms_dir = directory.path().join("vms");
        let binary = fake_firecracker(
            directory.path(),
            &format!("signal.signal(signal.SIGTERM, lambda *_: sys.exit(0)){SERVE_LOOP}"),
        );
        let config = directory.path().join("firecracker.json");
        fs::write(&config, "{}").unwrap();
        let id = Uuid::new_v4();

        let process = spawn_vm(&binary, &vms_dir, id, &config, Duration::from_secs(5))
            .await
            .unwrap();
        let pid = process.pid().unwrap() as i32;
        assert!(process_alive(pid));

        stop_vm(process, Duration::from_secs(5)).await.unwrap();

        assert!(!process_alive(pid));
        let console = fs::read_to_string(console_log_path(&vms_dir, id)).unwrap();
        assert!(console.contains("booted"));
    }

    #[tokio::test]
    async fn readiness_timeout_cleans_up_the_process() {
        let directory = short_tempdir();
        let vms_dir = directory.path().join("vms");
        let binary = fake_firecracker(
            directory.path(),
            "while True:\n    time.sleep(60)\n",
        );
        let config = directory.path().join("firecracker.json");
        fs::write(&config, "{}").unwrap();
        let id = Uuid::new_v4();

        let error = spawn_vm(&binary, &vms_dir, id, &config, Duration::from_millis(500))
            .await
            .unwrap_err();

        assert!(matches!(error, FirecrackerError::NotReady { .. }));
        assert!(!process_alive(fake_pid(&vms_dir, id)));
    }

    #[tokio::test]
    async fn stop_escalates_to_sigkill_when_sigterm_is_ignored() {
        let directory = short_tempdir();
        let vms_dir = directory.path().join("vms");
        let binary = fake_firecracker(
            directory.path(),
            &format!("signal.signal(signal.SIGTERM, signal.SIG_IGN){SERVE_LOOP}"),
        );
        let config = directory.path().join("firecracker.json");
        fs::write(&config, "{}").unwrap();
        let id = Uuid::new_v4();

        let process = spawn_vm(&binary, &vms_dir, id, &config, Duration::from_secs(5))
            .await
            .unwrap();
        let pid = process.pid().unwrap() as i32;
        let started = std::time::Instant::now();

        stop_vm(process, Duration::from_millis(200)).await.unwrap();

        assert!(started.elapsed() >= Duration::from_millis(200));
        assert!(!process_alive(pid));
    }
}
