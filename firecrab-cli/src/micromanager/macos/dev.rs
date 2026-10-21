use std::net::IpAddr;
use std::process::{Command, Stdio};

use super::lifecycle::Layout;
pub use crate::micromanager::dev::{Checkout, Error};
use crate::micromanager::{
    DevCommand,
    dev::{guest_script, io_error, session},
    report,
};

pub fn control(layout: &Layout, ip: IpAddr, command: DevCommand) -> Result<(), Error> {
    run_ssh(
        layout,
        ip,
        &format!("bash -s -- {}", command.as_str()),
        session::Input::File(guest_script()?),
        "guest service control",
    )
}

pub fn deploy(
    layout: &Layout,
    ip: IpAddr,
    checkout: Option<&Checkout>,
    release: bool,
) -> Result<(), Error> {
    // A guest marker is written before the host tunnel succeeds and can survive
    // a failed boot. Verify the live SSH connection before uploading the snapshot.
    let output = ssh_command(layout, ip, "bash -s -- check")?
        .stdin(Stdio::from(guest_script()?))
        .output()
        .map_err(|source| io_error("probe management SSH", source))?;
    if !output.status.success() {
        if output.status.code() == Some(74) {
            return Err(Error::GuestFilesystemUnhealthy(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
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
                session::Input::File(archive),
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
    report!("[GUEST] {profile}: API + helper + CLI");
    run_ssh(
        layout,
        ip,
        &format!(
            "bash -c '{}' -- {profile} {channel} {archive_name}",
            session::runner().replace('\'', "'\\''")
        ),
        session::Input::Session(script),
        "guest development build/deployment",
    )
}

fn run_ssh(
    layout: &Layout,
    ip: IpAddr,
    command: &str,
    stdin: session::Input,
    action: &'static str,
) -> Result<(), Error> {
    let status = stdin
        .status(&mut ssh_command(layout, ip, command)?)
        .map_err(|source| io_error(action, source))?;
    if !status.success() {
        return Err(if status.code() == Some(74) {
            Error::GuestFilesystemUnhealthy(action.to_string())
        } else if status.code() == Some(255) {
            Error::GuestSshInterrupted(action)
        } else {
            Error::Command(action)
        });
    }
    Ok(())
}

fn ssh_command(layout: &Layout, ip: IpAddr, command: &str) -> Result<Command, Error> {
    let runtime = layout.managed_home.join("runtime");
    let settings = super::ssh::load(&layout.managed_home)?;
    let mut ssh = Command::new("/usr/bin/ssh");
    ssh.arg("-i")
        .arg(runtime.join("manager_ed25519"))
        .args(["-o", "BatchMode=yes"])
        .args(super::ssh::arguments(
            &runtime,
            &settings,
            super::ssh::Session::Command,
        ))
        .args(["-o", "StrictHostKeyChecking=yes", "-o", "UpdateHostKeys=no"])
        .arg("-o")
        .arg(super::known_hosts_option(&runtime.join("known_hosts")))
        .arg(format!("root@{ip}"))
        .arg(command);
    Ok(ssh)
}
