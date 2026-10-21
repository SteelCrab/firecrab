//! `firecrab service update`: bring the Firecrab inside the managed guest to the
//! latest release, on every host that runs Firecrab in a managed Debian VM.
//!
//! `firecrab update` swaps a Linux host's own executables through its
//! privileged helper, so it exists only there. Here the guest is the host, and
//! the release's installer upgrades it. Unlike `update --apply`, the installer
//! also migrates unit files, which a release such as v0.3.1 changes.

use super::release::{self, Release};
use super::report;

/// Where the guest script is kept in the managed home's `provision` directory.
pub const GUEST_SCRIPT_FILE: &str = "guest-update.sh";

/// Runs in the guest as root. It finds the managed home, which the guest shares
/// with the host, from its own path, so one text serves macOS and Windows.
pub fn guest_script() -> String {
    format!(
        r#"#!/bin/bash
set -Eeuo pipefail
share=$(cd "$(dirname "$0")/.." && pwd)
bash "$share/downloads/{installer}" --no-deps
systemctl is-active --quiet firecrab-api
systemctl is-active --quiet firecrab-helper || systemctl is-active --quiet firecrab-net-helper
firecrab --version
"#,
        installer = release::INSTALLER_FILE,
    )
}

/// What `service update` needs from the host that runs the managed guest.
pub trait Guest {
    type Error;

    /// The Firecrab version installed in the guest, when it can be read.
    fn installed_version(&self) -> Option<String>;

    /// Downloads and verifies what the installer needs from `release`.
    fn download(&self, release: &Release, assume_yes: bool) -> Result<(), Self::Error>;

    /// Runs the downloaded installer in the guest and returns its exit code.
    fn run_installer(&self) -> Result<i32, Self::Error>;
}

#[derive(Debug, PartialEq, Eq)]
enum Standing {
    Current,
    Ahead,
    Behind,
    Unknown,
}

fn standing(installed: Option<&str>, latest_tag: &str) -> Standing {
    let latest = latest_tag
        .strip_prefix('v')
        .and_then(release::parse_version);
    match (installed.and_then(release::parse_version), latest) {
        (Some(installed), Some(latest)) => match installed.cmp(&latest) {
            std::cmp::Ordering::Equal => Standing::Current,
            std::cmp::Ordering::Greater => Standing::Ahead,
            std::cmp::Ordering::Less => Standing::Behind,
        },
        _ => Standing::Unknown,
    }
}

/// The version `firecrab --version` printed in the guest, as `firecrab 0.3.1`.
pub fn version_of(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let version = line.trim().strip_prefix("firecrab ")?;
        release::parse_version(version).map(|_| version.to_owned())
    })
}

/// Reports where the guest stands against `release` and, with `apply`, brings
/// it up to date. Returns the exit code: 0 unless the installer failed.
pub fn run<G: Guest>(
    guest: &G,
    release: &Release,
    apply: bool,
    assume_yes: bool,
) -> Result<i32, G::Error> {
    let installed = guest.installed_version();
    let tag = release.tag();
    let standing = standing(installed.as_deref(), tag);
    let version = installed.as_deref().unwrap_or("unknown");
    match standing {
        Standing::Current => report!("[PASS] guest: Firecrab {version} is the latest release"),
        Standing::Ahead => {
            report!("[PASS] guest: Firecrab {version} is newer than the latest release {tag}");
        }
        Standing::Behind => report!("[WARNING] guest: Firecrab {version} can be updated to {tag}"),
        Standing::Unknown => report!(
            "[WARNING] guest: the installed Firecrab version is unknown; the latest release is {tag}"
        ),
    }
    if matches!(standing, Standing::Current | Standing::Ahead) {
        return Ok(0);
    }
    if !apply {
        report!("  run `firecrab service update --apply` to update");
        return Ok(0);
    }

    guest.download(release, assume_yes)?;
    report!("[GUEST] running the {tag} installer; firecrab-api and firecrab-helper restart");
    let code = guest.run_installer()?;
    if code != 0 {
        report!(
            "[FAILED] guest: the installer exited with {code}; inspect `firecrab service debug --logs`"
        );
        return Ok(code);
    }
    report!("[PASS] guest: Firecrab {tag} installed");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    struct Fake {
        version: Option<&'static str>,
        installer_code: i32,
        fail_download: bool,
        calls: RefCell<Vec<&'static str>>,
    }

    impl Fake {
        fn at(version: Option<&'static str>) -> Self {
            Self {
                version,
                installer_code: 0,
                fail_download: false,
                calls: RefCell::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.borrow().clone()
        }
    }

    impl Guest for Fake {
        type Error = &'static str;

        fn installed_version(&self) -> Option<String> {
            self.calls.borrow_mut().push("version");
            self.version.map(str::to_owned)
        }

        fn download(&self, _: &Release, _: bool) -> Result<(), Self::Error> {
            self.calls.borrow_mut().push("download");
            if self.fail_download {
                return Err("download failed");
            }
            Ok(())
        }

        fn run_installer(&self) -> Result<i32, Self::Error> {
            self.calls.borrow_mut().push("installer");
            Ok(self.installer_code)
        }
    }

    fn latest() -> Release {
        Release::fixture("v0.3.1", "https://example.invalid/releases", "")
    }

    #[test]
    fn a_check_reports_without_changing_anything() {
        let guest = Fake::at(Some("0.2.2"));
        assert_eq!(run(&guest, &latest(), false, false), Ok(0));
        assert_eq!(guest.calls(), ["version"]);
    }

    #[test]
    fn applying_downloads_then_runs_the_installer() {
        let guest = Fake::at(Some("0.2.2"));
        assert_eq!(run(&guest, &latest(), true, true), Ok(0));
        assert_eq!(guest.calls(), ["version", "download", "installer"]);
    }

    #[test]
    fn a_guest_that_is_current_or_newer_is_left_alone() {
        for version in ["0.3.1", "0.3.2", "0.4.0", "1.0.0"] {
            let guest = Fake::at(Some(version));
            assert_eq!(run(&guest, &latest(), true, true), Ok(0), "{version}");
            assert_eq!(guest.calls(), ["version"], "{version}");
        }
    }

    #[test]
    fn an_unreadable_version_is_updated_rather_than_trusted() {
        let guest = Fake::at(None);
        assert_eq!(run(&guest, &latest(), true, true), Ok(0));
        assert_eq!(guest.calls(), ["version", "download", "installer"]);
    }

    #[test]
    fn a_failed_installer_is_the_exit_code() {
        let guest = Fake {
            installer_code: 7,
            ..Fake::at(Some("0.3.0"))
        };
        assert_eq!(run(&guest, &latest(), true, true), Ok(7));
    }

    #[test]
    fn a_failed_download_stops_before_the_installer() {
        let guest = Fake {
            fail_download: true,
            ..Fake::at(Some("0.3.0"))
        };
        assert_eq!(run(&guest, &latest(), true, true), Err("download failed"));
        assert_eq!(guest.calls(), ["version", "download"]);
    }

    #[test]
    fn standing_compares_numbers_not_text() {
        assert_eq!(standing(Some("0.3.9"), "v0.3.10"), Standing::Behind);
        assert_eq!(standing(Some("0.3.10"), "v0.3.9"), Standing::Ahead);
        assert_eq!(standing(Some("0.3.1"), "v0.3.1"), Standing::Current);
        assert_eq!(standing(None, "v0.3.1"), Standing::Unknown);
        assert_eq!(standing(Some("0.3.1-dev"), "v0.3.1"), Standing::Unknown);
    }

    #[test]
    fn the_version_is_read_from_the_cli_banner() {
        assert_eq!(version_of("firecrab 0.3.1\n").as_deref(), Some("0.3.1"));
        // A login banner or other noise around it does not matter.
        assert_eq!(
            version_of("Warning: noise\r\nfirecrab 0.3.1\r\n").as_deref(),
            Some("0.3.1")
        );
        for output in [
            "",
            "firecrab",
            "firecrab dev",
            "sh: firecrab: not found",
            "0.3.1",
        ] {
            assert_eq!(version_of(output), None, "{output:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_guest_script_parses_as_bash() {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let mut bash = Command::new("bash")
            .args(["-n", "/dev/stdin"])
            .stdin(Stdio::piped())
            .spawn()
            .expect("bash is available on Unix hosts");
        let mut stdin = bash.stdin.take().expect("piped stdin");
        stdin.write_all(guest_script().as_bytes()).unwrap();
        drop(stdin);
        assert!(bash.wait().unwrap().success());
    }

    #[test]
    fn the_guest_script_runs_the_verified_installer_and_checks_the_services() {
        let script = guest_script();
        assert!(script.starts_with("#!/bin/bash\nset -Eeuo pipefail\n"));
        assert!(script.contains("bash \"$share/downloads/install-firecrab.sh\" --no-deps"));
        // The home is found from the script's own path, not a host-specific one.
        assert!(script.contains("share=$(cd \"$(dirname \"$0\")/..\" && pwd)"));
        assert!(script.contains("systemctl is-active --quiet firecrab-api"));
        assert!(
            script.contains("firecrab-helper || systemctl is-active --quiet firecrab-net-helper")
        );
        assert!(script.trim_end().ends_with("firecrab --version"));
    }
}
