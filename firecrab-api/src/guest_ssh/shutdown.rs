//! Guest shutdown over the VM's authenticated management SSH connection.

use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use uuid::Uuid;

use crate::artifacts::VmArtifactPaths;
use crate::state::AppState;

use super::VmSshPaths;

// Use the selected init, not the mere presence of systemctl or BusyBox:
// native systemd/OpenRC OCI guests also contain our BusyBox toolbox.
// Neither path uses -f, so init can stop services and unmount filesystems.
const POWEROFF: &str = "if [ \"$(cat /etc/firecrab/init-system 2>/dev/null)\" = busybox ]; then /etc/firecrab/busybox sync && exec /etc/firecrab/busybox poweroff; else sync && exec /sbin/poweroff; fi";

/// Flushes guest writes and asks its init to power off. Completion of SSH is
/// not proof of shutdown: the caller must also wait for the VMM's exit.
pub(crate) async fn poweroff(state: &AppState, id: Uuid) -> Result<(), String> {
    let storage_root = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&id)
        .ok_or("VM record is missing")?
        .storage_root
        .clone();
    let paths = VmArtifactPaths::for_vm(&state.vms_dir_for(&storage_root), id);
    let ssh = VmSshPaths::from_artifacts(&paths);
    let public_key = tokio::fs::read_to_string(&ssh.host_public)
        .await
        .map_err(|error| format!("read guest host key: {error}"))?;
    let store = state.store.clone();
    let lease = tokio::task::spawn_blocking(move || store.active_lease(id))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?
        .ok_or("VM has no management address")?;

    // Pin the key injected into this VM, without trusting a live keyscan or
    // modifying the service account's global known_hosts.
    let alias = format!("firecrab-{id}");
    let known_hosts = ssh.dir.join("shutdown_known_hosts");
    tokio::fs::write(&known_hosts, format!("{alias} {public_key}"))
        .await
        .map_err(|error| format!("write guest known_hosts: {error}"))?;
    let known_hosts = known_hosts
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let mut command = Command::new("ssh");
    command
        .args(["-F", "/dev/null", "-T", "-i"])
        .arg(&ssh.operator_private)
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "GlobalKnownHostsFile=/dev/null",
            "-o",
            "ConnectTimeout=5",
            "-o",
            "ConnectionAttempts=1",
            "-o",
            "LogLevel=ERROR",
        ])
        .arg("-o")
        .arg(format!("HostKeyAlias={alias}"))
        .arg("-o")
        .arg(format!("UserKnownHostsFile=\"{known_hosts}\""))
        .arg(format!("root@{}", lease.ipv4))
        .arg(POWEROFF)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    loop {
        let status = command
            .status()
            .await
            .map_err(|error| format!("run guest shutdown SSH: {error}"))?;
        if status.success() {
            return Ok(());
        }
        if status.code() != Some(255) {
            return Err(format!("guest shutdown SSH exited {status}"));
        }
        // sshd can still be starting when DHCP marks a guest ready. Retry
        // transport failures within the caller's shutdown deadline. A real
        // poweroff can also disconnect SSH; the exit monitor cancels us then.
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
