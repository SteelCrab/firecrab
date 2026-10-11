//! Guest shutdown over SSH, with the host-owned serial console as a fallback.

use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use uuid::Uuid;

use crate::artifacts::VmArtifactPaths;
use crate::console::ConsoleBroker;
use crate::handlers::console_sentinel::wait_for_completion_with_sentinel;
use crate::state::AppState;

use super::VmSshPaths;

// Use the selected init, not the mere presence of systemctl or BusyBox:
// native systemd/OpenRC OCI guests also contain our BusyBox toolbox.
// Neither path uses -f, so init can stop services and unmount filesystems.
const POWEROFF: &str = "if [ \"$(cat /etc/firecrab/init-system 2>/dev/null)\" = busybox ]; then /etc/firecrab/busybox sync && exec /etc/firecrab/busybox poweroff; else sync && exec /sbin/poweroff; fi";

/// Flushes guest writes and asks its init to power off. Completion of SSH is
/// not proof of shutdown: the caller must also wait for the VMM's exit.
pub(crate) async fn poweroff(
    state: &AppState,
    id: Uuid,
    console: &ConsoleBroker,
) -> Result<(), String> {
    // Imported guests install SSH after becoming running. Failed packages
    // must not turn an otherwise responsive root console into a forced stop.
    if let Ok(Ok(())) = tokio::time::timeout(Duration::from_secs(2), ssh_poweroff(state, id)).await
    {
        return Ok(());
    }
    tracing::info!(vm_id = %id, "guest SSH unavailable; requesting shutdown through serial console");
    serial_poweroff(console).await
}

async fn serial_poweroff(console: &ConsoleBroker) -> Result<(), String> {
    let (_backlog, mut output) = console.subscribe();
    let probe = format!("FIRECRAB_SHUTDOWN_ROOT_{}", Uuid::new_v4().simple());
    let flushed = format!("FIRECRAB_SHUTDOWN_FLUSHED_{}", Uuid::new_v4().simple());
    loop {
        // Stop was explicitly requested. Interrupt a foreground program and
        // erase an unfinished command before using the autologin root shell.
        // Separate the writes so tty signal handling cannot flush our probe.
        console.write_input(b"\x03\x15").await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        console
            .write_input(format!("\x15test \"$(id -u)\" = 0; echo \"{probe}:$?\"\r").as_bytes())
            .await;
        match wait_for_completion_with_sentinel(&mut output, Duration::from_secs(1), &probe).await {
            Ok((0, _)) => break,
            Ok((1, _)) => return Err("guest serial console is not a root shell".into()),
            // The console may still be starting after network readiness.
            // A terminal query can also consume initial input (exit 127).
            // Retry only within stop_registered's overall shutdown deadline.
            Ok((_, _)) | Err(_) => continue,
        }
    }

    // The marker is emitted only after guest sync succeeds. Command echo
    // contains an unexpanded $? and cannot be mistaken for confirmation.
    let command = format!(
        "\x15if [ \"$(cat /etc/firecrab/init-system 2>/dev/null)\" = busybox ]; then /etc/firecrab/busybox sync && echo \"{flushed}:$?\" && /etc/firecrab/busybox poweroff; else sync && echo \"{flushed}:$?\" && /sbin/poweroff; fi\r"
    );
    console.write_input(command.as_bytes()).await;
    let (code, _) =
        wait_for_completion_with_sentinel(&mut output, Duration::from_secs(25), &flushed).await?;
    if code != 0 {
        return Err(format!("guest serial sync failed with exit code {code}"));
    }
    // Flushing is not VM exit. stop_registered still waits for the monitor.
    Ok(())
}

async fn ssh_poweroff(state: &AppState, id: Uuid) -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::vm_shim::client::ShimControl;
    use crate::vm_shim::protocol::ShimRequest;

    use super::*;

    async fn input(requests: &mut tokio::sync::mpsc::Receiver<ShimRequest>) -> Vec<u8> {
        match requests.recv().await.unwrap() {
            ShimRequest::Input(bytes) => bytes,
            request => panic!("shutdown must use guest input, got {request:?}"),
        }
    }

    fn marker(command: &[u8]) -> String {
        String::from_utf8_lossy(command)
            .split_once("echo \"")
            .unwrap()
            .1
            .split_once(":$?")
            .unwrap()
            .0
            .to_owned()
    }

    #[tokio::test(start_paused = true)]
    async fn serial_shutdown_requires_root_and_real_guest_flush_confirmation() {
        let console = Arc::new(ConsoleBroker::new());
        let (control, mut requests) = ShimControl::for_test();
        console.attach_control(control);
        let task_console = console.clone();
        let task = tokio::spawn(async move { serial_poweroff(&task_console).await });

        assert_eq!(input(&mut requests).await, b"\x03\x15");
        let probe = input(&mut requests).await;
        console.push_output(&probe);
        console.push_output(b"\n"); // tty echoes Enter as CRLF.
        tokio::task::yield_now().await;
        assert!(
            requests.try_recv().is_err(),
            "command echo is not root confirmation"
        );
        console.push_output(format!("{}:0\r\n", marker(&probe)).as_bytes());

        let stop = input(&mut requests).await;
        let command = String::from_utf8_lossy(&stop);
        assert!(command.contains("busybox sync"));
        assert!(command.contains("/sbin/poweroff"));
        assert!(!command.contains("poweroff -f"));
        console.push_output(&stop);
        console.push_output(b"\n");
        tokio::task::yield_now().await;
        assert!(
            !task.is_finished(),
            "command echo is not flush confirmation"
        );
        let confirmation = format!("{}:0\r\n", marker(&stop));
        let half = confirmation.len() / 2;
        console.push_output(&confirmation.as_bytes()[..half]);
        console.push_output(&confirmation.as_bytes()[half..]);
        task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn serial_shutdown_retries_until_the_first_boot_console_is_ready() {
        let console = Arc::new(ConsoleBroker::new());
        let (control, mut requests) = ShimControl::for_test();
        console.attach_control(control);
        let task_console = console.clone();
        let task = tokio::spawn(async move { serial_poweroff(&task_console).await });

        assert_eq!(input(&mut requests).await, b"\x03\x15");
        let _lost_probe = input(&mut requests).await;
        // init had not started the root console, so the first probe is lost.
        assert_eq!(input(&mut requests).await, b"\x03\x15");
        let probe = input(&mut requests).await;
        console.push_output(format!("{}:0\r\n", marker(&probe)).as_bytes());
        let stop = input(&mut requests).await;
        console.push_output(format!("{}:0\r\n", marker(&stop)).as_bytes());
        task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn serial_shutdown_retries_a_probe_partly_consumed_by_terminal_setup() {
        let console = Arc::new(ConsoleBroker::new());
        let (control, mut requests) = ShimControl::for_test();
        console.attach_control(control);
        let task_console = console.clone();
        let task = tokio::spawn(async move { serial_poweroff(&task_console).await });

        let _interrupt = input(&mut requests).await;
        let first = input(&mut requests).await;
        assert_eq!(first[0], 0x15, "clear stale input before every command");
        console.push_output(format!("{}:1", marker(&first)).as_bytes());
        tokio::task::yield_now().await;
        assert!(
            !task.is_finished(),
            "partial 127 must not be read as non-root exit 1"
        );
        console.push_output(b"27\r\n");
        assert_eq!(input(&mut requests).await, b"\x03\x15");
        let probe = input(&mut requests).await;
        console.push_output(format!("{}:0\r\n", marker(&probe)).as_bytes());
        let stop = input(&mut requests).await;
        assert_eq!(stop[0], 0x15);
        console.push_output(format!("{}:0\r\n", marker(&stop)).as_bytes());
        task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn serial_shutdown_does_not_poweroff_from_a_non_root_console() {
        let console = Arc::new(ConsoleBroker::new());
        let (control, mut requests) = ShimControl::for_test();
        console.attach_control(control);
        let task_console = console.clone();
        let task = tokio::spawn(async move { serial_poweroff(&task_console).await });

        let _interrupt = input(&mut requests).await;
        let probe = input(&mut requests).await;
        console.push_output(format!("{}:1\r\n", marker(&probe)).as_bytes());
        assert!(
            task.await
                .unwrap()
                .unwrap_err()
                .contains("not a root shell")
        );
        assert!(requests.try_recv().is_err());
    }
}
