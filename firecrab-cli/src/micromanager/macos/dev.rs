use std::fs::File;
use std::net::IpAddr;
use std::process::{Command, Stdio};

use super::lifecycle::Layout;
pub use crate::micromanager::dev::{Checkout, Error};
use crate::micromanager::{
    dev::{guest_script, io_error},
    report,
};

pub fn deploy(
    layout: &Layout,
    ip: IpAddr,
    checkout: Option<&Checkout>,
    release: bool,
) -> Result<(), Error> {
    // A guest marker is written before the host tunnel succeeds and can survive
    // a failed boot. Verify the live SSH connection before uploading the snapshot.
    let output = ssh_command(layout, ip, "true")
        .stdin(Stdio::null())
        .output()
        .map_err(|source| io_error("probe management SSH", source))?;
    if !output.status.success() {
        return Err(Error::GuestSshUnavailable {
            ip,
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    let archive_name = format!("incoming-{}.tar", uuid::Uuid::new_v4());
    let (profile, channel) = match checkout {
        Some(checkout) => {
            report!("[SOURCE] {}", checkout.root.display());
            let archive = checkout
                .archive
                .reopen()
                .map_err(|source| io_error("read source archive", source))?;
            run_ssh(
                layout,
                ip,
                &format!(
                    "install -d -m 0700 /var/lib/firecrab/dev && umask 077 && cat > /var/lib/firecrab/dev/{archive_name}"
                ),
                archive,
                "source upload",
            )?;
            (
                if release { "release" } else { "debug" },
                checkout.channel.as_str(),
            )
        }
        None => ("restore", "unused"),
    };
    // Send the embedded script independently of the snapshot so an installed CLI
    // can run from any checkout directory and restore without source files.
    let script = guest_script()?;
    report!("[GUEST] {profile}: API + helper");
    run_ssh(
        layout,
        ip,
        &format!("bash -s -- {profile} {channel} {archive_name}"),
        script,
        "guest development build/deployment",
    )
}

fn run_ssh(
    layout: &Layout,
    ip: IpAddr,
    command: &str,
    stdin: File,
    action: &'static str,
) -> Result<(), Error> {
    let status = ssh_command(layout, ip, command)
        .stdin(Stdio::from(stdin))
        .status()
        .map_err(|source| io_error(action, source))?;
    if !status.success() {
        return Err(if status.code() == Some(255) {
            Error::GuestSshInterrupted(action)
        } else {
            Error::Command(action)
        });
    }
    Ok(())
}

fn ssh_command(layout: &Layout, ip: IpAddr, command: &str) -> Command {
    let runtime = layout.managed_home.join("runtime");
    let mut ssh = Command::new("/usr/bin/ssh");
    ssh.arg("-i")
        .arg(runtime.join("manager_ed25519"))
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=5"])
        .args(["-o", "StrictHostKeyChecking=yes", "-o", "UpdateHostKeys=no"])
        .args([
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
        ])
        .arg("-o")
        .arg(format!(
            "UserKnownHostsFile={}",
            runtime.join("known_hosts").display()
        ))
        .arg(format!("root@{ip}"))
        .arg(command);
    ssh
}
