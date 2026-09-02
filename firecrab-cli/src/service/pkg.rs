//! install.sh의 `detect_pkg` / `pkg_name` / `pkg_refresh` / `pkg_install`.

use crate::shell::CommandRunner;

use super::Error;
use super::privileged::Privileged;

/// install.sh `have`: `command -v <cmd>`.
///
/// `sh -c` 경유인 이유는 `command`가 셸 빌트인이라 `std::process::Command`로
/// 직접 부를 수 없기 때문이다.
pub fn have(runner: &dyn CommandRunner, cmd: &str) -> bool {
    let probe = format!("command -v {cmd}");
    runner
        .run("sh", &["-c", &probe])
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// The package managers install.sh knows, in its probe order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkgManager {
    /// Debian, Ubuntu.
    AptGet,
    /// Fedora, RHEL.
    Dnf,
    /// openSUSE.
    Zypper,
    /// Arch.
    Pacman,
    /// Alpine.
    Apk,
}

impl PkgManager {
    /// The first manager on `$PATH`, probed in install.sh's order.
    pub fn detect(runner: &dyn CommandRunner) -> Option<Self> {
        [
            Self::AptGet,
            Self::Dnf,
            Self::Zypper,
            Self::Pacman,
            Self::Apk,
        ]
        .into_iter()
        .find(|candidate| have(runner, candidate.as_str()))
    }

    /// The binary name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AptGet => "apt-get",
            Self::Dnf => "dnf",
            Self::Zypper => "zypper",
            Self::Pacman => "pacman",
            Self::Apk => "apk",
        }
    }

    /// install.sh `pkg_name`: the distro package(s) providing a generic name.
    /// One generic name can map to two packages, so this returns a list.
    pub fn packages_for<'a>(&self, generic: &'a str) -> Vec<&'a str> {
        match (self, generic) {
            (Self::Dnf, "iproute2") => vec!["iproute"],
            // dhcp_release ships separately on Debian/Alpine; RHEL has no such package.
            (Self::Dnf, "dnsmasq-utils") => vec!["dnsmasq"],
            (Self::Apk, "e2fsprogs") => vec!["e2fsprogs-extra"],
            (Self::AptGet, "xz") => vec!["xz-utils"],
            // semanage lives in a python tooling package on the SELinux distros.
            (Self::Dnf | Self::AptGet | Self::Zypper, "selinux-tools") => {
                vec!["policycoreutils-python-utils"]
            }
            (_, "selinux-tools") => vec!["policycoreutils"],
            (_, "iproute2") => vec!["iproute2"],
            (_, "dnsmasq-utils") => vec!["dnsmasq-utils"],
            (_, "e2fsprogs") => vec!["e2fsprogs"],
            (_, "xz") => vec!["xz"],
            (_, "nftables") => vec!["nftables"],
            (_, "dnsmasq") => vec!["dnsmasq"],
            (_, "curl") => vec!["curl"],
            (_, "findutils") => vec!["findutils"],
            (_, "tar") => vec!["tar"],
            (_, "zstd") => vec!["zstd"],
            (_, "coreutils") => vec!["coreutils"],
            // Unknown generics pass through as-is.
            (_, other) => vec![other],
        }
    }
}

/// Installs packages, refreshing the index at most once per run.
pub struct Packages<'a> {
    manager: Option<PkgManager>,
    privileged: &'a Privileged<'a>,
    refreshed: bool,
}

impl<'a> Packages<'a> {
    /// Binds a detected manager (or none) to a privileged runner.
    pub fn new(manager: Option<PkgManager>, privileged: &'a Privileged<'a>) -> Self {
        Self {
            manager,
            privileged,
            refreshed: false,
        }
    }

    /// The detected manager, if any.
    pub fn manager(&self) -> Option<PkgManager> {
        self.manager
    }

    /// The underlying command runner, for read-only probes.
    pub fn runner(&self) -> &'a dyn CommandRunner {
        self.privileged.runner()
    }

    /// install.sh `pkg_refresh`: once per process, and only where it matters.
    fn refresh(&mut self, step: &'static str) -> Result<(), Error> {
        if self.refreshed {
            return Ok(());
        }
        self.refreshed = true;
        match self.manager {
            Some(PkgManager::AptGet) => {
                self.privileged.run_env(
                    step,
                    &[("DEBIAN_FRONTEND", "noninteractive")],
                    "apt-get",
                    &["update", "-qq"],
                )?;
            }
            Some(PkgManager::Apk) => {
                self.privileged.run_ok(step, "apk", &["update", "-q"])?;
            }
            // dnf/zypper/pacman refresh as part of install.
            _ => {}
        }
        Ok(())
    }

    fn install_once(&self, step: &'static str, packages: &[&str]) -> Result<(), Error> {
        let manager = self.manager.ok_or_else(|| {
            Error::step(
                step,
                format!("no known package manager for: {}", packages.join(" ")),
            )
        })?;
        match manager {
            PkgManager::AptGet => {
                let mut args = vec!["install", "-y", "-qq"];
                args.extend_from_slice(packages);
                self.privileged.run_env(
                    step,
                    &[("DEBIAN_FRONTEND", "noninteractive")],
                    "apt-get",
                    &args,
                )?;
            }
            PkgManager::Dnf => {
                let mut args = vec!["install", "-y", "-q"];
                args.extend_from_slice(packages);
                self.privileged.run_ok(step, "dnf", &args)?;
            }
            PkgManager::Zypper => {
                let mut args = vec!["--non-interactive", "install", "-y"];
                args.extend_from_slice(packages);
                self.privileged.run_ok(step, "zypper", &args)?;
            }
            PkgManager::Pacman => {
                let mut args = vec!["-Sy", "--noconfirm", "--needed"];
                args.extend_from_slice(packages);
                self.privileged.run_ok(step, "pacman", &args)?;
            }
            PkgManager::Apk => {
                let mut args = vec!["add", "--no-cache"];
                args.extend_from_slice(packages);
                self.privileged.run_ok(step, "apk", &args)?;
            }
        }
        Ok(())
    }

    /// install.sh `pkg_install`: refresh once, then one retry — Fedora's
    /// metalink/mirror writes fail transiently.
    ///
    /// install.sh sleeps 2s between the failed attempt and the retry
    /// (install.sh:280); deliberately omitted here so tests stay fast.
    pub fn install(&mut self, step: &'static str, packages: &[&str]) -> Result<(), Error> {
        if self.manager.is_none() {
            return Err(Error::step(
                step,
                format!("no known package manager for: {}", packages.join(" ")),
            ));
        }
        self.refresh(step)?;
        match self.install_once(step, packages) {
            Ok(()) => Ok(()),
            Err(_) => {
                super::output::warn("package install failed; retrying once");
                self.install_once(step, packages)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::privileged::Privileged;
    use crate::shell::FakeCommandRunner;

    #[test]
    fn detect_prefers_the_first_manager_on_path() {
        let mut fake = FakeCommandRunner::new();
        fake.set("sh", &["-c", "command -v dnf"], 0, "/usr/bin/dnf\n", "");
        // apt-get is probed first and is not registered → NotFound → absent.
        assert_eq!(PkgManager::detect(&fake), Some(PkgManager::Dnf));
    }

    #[test]
    fn detect_returns_none_without_any_manager() {
        assert_eq!(PkgManager::detect(&FakeCommandRunner::new()), None);
    }

    #[test]
    fn package_names_follow_install_sh_mapping() {
        assert_eq!(PkgManager::Dnf.packages_for("iproute2"), vec!["iproute"]);
        assert_eq!(
            PkgManager::AptGet.packages_for("iproute2"),
            vec!["iproute2"]
        );
        assert_eq!(
            PkgManager::Dnf.packages_for("dnsmasq-utils"),
            vec!["dnsmasq"]
        );
        assert_eq!(
            PkgManager::Apk.packages_for("dnsmasq-utils"),
            vec!["dnsmasq-utils"]
        );
        assert_eq!(
            PkgManager::Apk.packages_for("e2fsprogs"),
            vec!["e2fsprogs-extra"]
        );
        assert_eq!(
            PkgManager::Pacman.packages_for("e2fsprogs"),
            vec!["e2fsprogs"]
        );
        assert_eq!(PkgManager::AptGet.packages_for("xz"), vec!["xz-utils"]);
        assert_eq!(PkgManager::Zypper.packages_for("xz"), vec!["xz"]);
        assert_eq!(
            PkgManager::Dnf.packages_for("selinux-tools"),
            vec!["policycoreutils-python-utils"]
        );
        assert_eq!(
            PkgManager::Pacman.packages_for("selinux-tools"),
            vec!["policycoreutils"]
        );
        assert_eq!(PkgManager::AptGet.packages_for("curl"), vec!["curl"]);
    }

    #[test]
    fn install_refreshes_the_index_at_most_once() {
        let fake = FakeCommandRunner::permissive();
        let privileged = Privileged::with_sudo(&fake, true);
        let mut packages = Packages::new(Some(PkgManager::AptGet), &privileged);
        packages.install("deps", &["nftables"]).unwrap();
        packages.install("deps", &["dnsmasq"]).unwrap();
        assert_eq!(
            fake.calls(),
            vec![
                "sudo env DEBIAN_FRONTEND=noninteractive apt-get update -qq",
                "sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq nftables",
                "sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq dnsmasq",
            ]
        );
    }

    #[test]
    fn install_retries_once_before_failing() {
        let mut fake = FakeCommandRunner::new();
        fake.set(
            "sudo",
            &["dnf", "install", "-y", "-q", "nftables"],
            1,
            "",
            "mirror error\n",
        );
        let privileged = Privileged::with_sudo(&fake, true);
        let mut packages = Packages::new(Some(PkgManager::Dnf), &privileged);
        let err = packages.install("deps", &["nftables"]).unwrap_err();
        assert!(matches!(err, Error::Step { step: "deps", .. }), "{err:?}");
        assert_eq!(
            fake.calls()
                .iter()
                .filter(|c| c.contains("dnf install"))
                .count(),
            2,
            "one retry, then give up"
        );
    }

    #[test]
    fn install_without_a_manager_is_an_error() {
        let fake = FakeCommandRunner::permissive();
        let privileged = Privileged::with_sudo(&fake, true);
        let mut packages = Packages::new(None, &privileged);
        assert!(packages.install("deps", &["nftables"]).is_err());
    }

    #[test]
    fn have_probes_with_command_v() {
        let mut fake = FakeCommandRunner::new();
        fake.set("sh", &["-c", "command -v nft"], 0, "/usr/sbin/nft\n", "");
        fake.set("sh", &["-c", "command -v missing"], 1, "", "");
        assert!(have(&fake, "nft"));
        assert!(!have(&fake, "missing"));
    }
}
