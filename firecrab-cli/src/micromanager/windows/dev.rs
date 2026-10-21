//! The shared source snapshot and deployment transaction over WSL stdin.

use super::{
    Error,
    wsl::{self, DISTRO_NAME},
};
use crate::micromanager::{
    DevCommand,
    dev::{Checkout, guest_script, io_error, session},
    report,
};

pub fn control(command: DevCommand) -> Result<(), Error> {
    wsl::run_with_input(
        &[
            "-d",
            DISTRO_NAME,
            "-u",
            "root",
            "--exec",
            "bash",
            "-s",
            "--",
            command.as_str(),
        ],
        session::Input::File(guest_script()?),
    )?;
    Ok(())
}

pub fn deploy(checkout: Option<&Checkout>, release: bool) -> Result<(), Error> {
    // Probe systemd and the installed units, not whether the previous API works.
    wsl::root_shell(
        "test -d /run/systemd/system && systemctl cat firecrab-api >/dev/null && (systemctl cat firecrab-helper >/dev/null 2>&1 || systemctl cat firecrab-net-helper >/dev/null)",
    )?;
    let archive_name = format!("incoming-{}.tar", uuid::Uuid::new_v4());
    let (profile, channel) = match checkout {
        Some(checkout) => {
            report!("[SOURCE] {}", checkout.root.display());
            let input = checkout
                .archive
                .reopen()
                .map_err(|source| io_error("read source archive", source))?;
            let upload = format!(
                "install -d -m 0700 /var/lib/firecrab/dev && umask 077 && cat > /var/lib/firecrab/dev/{archive_name}"
            );
            wsl::run_with_input(
                &[
                    "-d",
                    DISTRO_NAME,
                    "-u",
                    "root",
                    "--exec",
                    "sh",
                    "-c",
                    &upload,
                ],
                session::Input::File(input),
            )?;
            (
                if release { "release" } else { "debug" },
                checkout.channel.as_str(),
            )
        }
        None => ("restore", "unused"),
    };
    report!("[GUEST] {profile}: API + helper + CLI");
    wsl::run_with_input(
        &[
            "-d",
            DISTRO_NAME,
            "-u",
            "root",
            "--exec",
            "bash",
            "-c",
            &session::runner(),
            "--",
            profile,
            channel,
            &archive_name,
        ],
        session::Input::Session(guest_script()?),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_snapshot_upload_precedes_the_selected_guest_build() {
        let directory = crate::micromanager::dev::tests::checkout_fixture();
        let checkout = Checkout::prepare(directory.path()).unwrap();
        for (release, profile) in [(false, "debug"), (true, "release")] {
            let wsl = wsl::fake::answer(|_| Ok(String::new()));
            deploy(Some(&checkout), release).unwrap();
            let calls = wsl.calls();
            assert_eq!(calls.len(), 3);
            assert!(calls[1].contains("umask 077 && cat > /var/lib/firecrab/dev/incoming-"));
            assert!(calls[2].contains("--exec bash -c"));
            assert!(calls[2].contains(&format!("-- {profile} 1.97.1 incoming-")));
        }
    }

    #[test]
    fn failed_upload_does_not_start_a_guest_build() {
        let directory = crate::micromanager::dev::tests::checkout_fixture();
        let checkout = Checkout::prepare(directory.path()).unwrap();
        let wsl = wsl::fake::answer(|line| {
            if line.contains("cat >") {
                Err("upload failed".to_string())
            } else {
                Ok(String::new())
            }
        });
        assert!(matches!(deploy(Some(&checkout), false), Err(Error::Wsl(_))));
        assert_eq!(wsl.calls().len(), 2);
        assert!(!wsl.calls().iter().any(|line| line.contains("bash -s")));
    }

    #[test]
    fn restore_uses_embedded_script_without_source_or_a_healthy_api() {
        let wsl = wsl::fake::answer(|_| Ok(String::new()));
        deploy(None, false).unwrap();
        let calls = wsl.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].contains("systemctl cat"));
        assert!(calls[1].contains("--exec bash -c"));
        assert!(calls[1].contains("-- restore unused incoming-"));
        assert!(!calls.iter().any(|line| line.contains("cat >")));
    }

    #[test]
    fn unavailable_guest_fails_before_upload_or_deployment() {
        let wsl = wsl::fake::answer(|_| Err("systemd unavailable".to_string()));
        assert!(matches!(deploy(None, false), Err(Error::Wsl(_))));
        assert_eq!(wsl.calls().len(), 1);
    }

    #[test]
    fn guest_deployment_failure_is_reported() {
        let wsl = wsl::fake::answer(|line| {
            if line.contains("bash -c") {
                Err("failed to restore".to_string())
            } else {
                Ok(String::new())
            }
        });
        assert!(matches!(deploy(None, false), Err(Error::Wsl(_))));
        assert_eq!(wsl.calls().len(), 2);
    }
}
