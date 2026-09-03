//! install.sh `ensure` / `ensure_runtime_deps`: 두 데몬이 런타임에 shell out 하는
//! 명령들과 설치 스크립트 자체가 필요로 하는 몇 가지를 채운다.

use super::output;
use super::pkg::{Packages, PkgManager, have};

/// 무엇이 빠졌고 무엇을 경고했는지. `missing`이 비면 성공이다.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DepsOutcome {
    /// 설치하지 못한, 반드시 필요한 명령들.
    pub missing: Vec<String>,
    /// 없어도 진행 가능한 항목에 대한 경고.
    pub warnings: Vec<String>,
}

/// install.sh `ensure`: 실제로 없는 것만 설치한다.
fn ensure(
    packages: &mut Packages<'_>,
    outcome: &mut DepsOutcome,
    command_name: &str,
    generic: &str,
    install_deps: bool,
    check_only: bool,
) -> bool {
    if have(packages.runner(), command_name) {
        return true;
    }
    let manager = packages.manager();
    let names: Vec<&str> = manager
        .map(|m| m.packages_for(generic))
        .unwrap_or_else(|| vec![generic]);
    if check_only {
        output::warn(&format!(
            "missing: {command_name} (would install: {})",
            names.join(" ")
        ));
        outcome.missing.push(command_name.to_owned());
        return false;
    }
    if !install_deps {
        output::warn(&format!(
            "missing: {command_name} (--no-deps, install '{}' yourself)",
            names.join(" ")
        ));
        outcome.missing.push(command_name.to_owned());
        return false;
    }
    if manager.is_none() {
        output::warn(&format!(
            "missing: {command_name} and no known package manager — install '{}' yourself",
            names.join(" ")
        ));
        outcome.missing.push(command_name.to_owned());
        return false;
    }
    output::step(&format!(
        "installing {} (for {command_name})",
        names.join(" ")
    ));
    if let Err(err) = packages.install("deps", &names) {
        output::warn(&format!("failed to install {}: {err}", names.join(" ")));
        outcome.missing.push(command_name.to_owned());
        return false;
    }
    // install.sh re-checks `have` even after a successful `pkg_install`: a
    // package can install with exit 0 and still not provide the binary
    // (wrong or incomplete generic-name mapping), and that must count as
    // missing, not satisfied.
    if have(packages.runner(), command_name) {
        true
    } else {
        outcome.missing.push(command_name.to_owned());
        false
    }
}

/// install.sh `ensure_runtime_deps` 전체.
pub fn ensure_runtime_deps(
    packages: &mut Packages<'_>,
    install_deps: bool,
    check_only: bool,
    selinux_on: bool,
) -> DepsOutcome {
    let mut outcome = DepsOutcome::default();
    for (command_name, generic) in [
        ("ip", "iproute2"),
        ("nft", "nftables"),
        ("dnsmasq", "dnsmasq"),
    ] {
        ensure(
            packages,
            &mut outcome,
            command_name,
            generic,
            install_deps,
            check_only,
        );
    }

    // dhcp_release releases DHCP leases on VM stop; VMs still run without it,
    // leases just expire on their own. Fedora/RHEL ship dnsmasq without that
    // binary — do not reinstall dnsmasq for it.
    if !have(packages.runner(), "dhcp_release") {
        if packages.manager() == Some(PkgManager::Dnf) {
            outcome.warnings.push(
                "dhcp_release is not available on this distribution; leases expire on their own"
                    .to_owned(),
            );
        } else {
            let mut probe = DepsOutcome::default();
            if !ensure(
                packages,
                &mut probe,
                "dhcp_release",
                "dnsmasq-utils",
                install_deps,
                check_only,
            ) {
                outcome.warnings.push(
                    "dhcp_release not found; install dnsmasq-utils to release leases on VM stop"
                        .to_owned(),
                );
            }
        }
    }

    // Without semanage the services stay in SELinux's init_t domain, where they
    // cannot reach a registry or exec nft — an install that looks successful
    // and works for nothing.
    if selinux_on && !have(packages.runner(), "semanage") {
        let mut probe = DepsOutcome::default();
        if !ensure(
            packages,
            &mut probe,
            "semanage",
            "selinux-tools",
            install_deps,
            check_only,
        ) {
            outcome.warnings.push(
                "SELinux is on but semanage is missing; the services will be confined to init_t"
                    .to_owned(),
            );
        }
    }

    for (command_name, generic) in [
        ("mkfs.ext4", "e2fsprogs"),
        ("curl", "curl"),
        ("find", "findutils"),
        ("tar", "tar"),
        ("zstd", "zstd"),
        ("sha256sum", "coreutils"),
    ] {
        ensure(
            packages,
            &mut outcome,
            command_name,
            generic,
            install_deps,
            check_only,
        );
    }

    for warning in &outcome.warnings {
        output::warn(warning);
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::privileged::Privileged;
    use crate::shell::FakeCommandRunner;

    /// Registers `command -v <cmd>` as present for each listed command;
    /// everything else probes as absent.
    fn fake_with_present(present: &[&str]) -> FakeCommandRunner {
        let mut fake = FakeCommandRunner::permissive();
        for cmd in present {
            fake.set(
                "sh",
                &["-c", &format!("command -v {cmd}")],
                0,
                "/usr/bin/x\n",
                "",
            );
        }
        for cmd in [
            "ip",
            "nft",
            "dnsmasq",
            "dhcp_release",
            "mkfs.ext4",
            "curl",
            "find",
            "tar",
            "zstd",
            "sha256sum",
            "semanage",
        ] {
            if !present.contains(&cmd) {
                fake.set("sh", &["-c", &format!("command -v {cmd}")], 1, "", "");
            }
        }
        fake
    }

    const ALL: &[&str] = &[
        "ip",
        "nft",
        "dnsmasq",
        "dhcp_release",
        "mkfs.ext4",
        "curl",
        "find",
        "tar",
        "zstd",
        "sha256sum",
        "semanage",
    ];

    #[test]
    fn nothing_is_installed_when_everything_is_present() {
        let fake = fake_with_present(ALL);
        let privileged = Privileged::with_sudo(&fake, true);
        let mut packages = Packages::new(Some(PkgManager::AptGet), &privileged);
        let outcome = ensure_runtime_deps(&mut packages, true, false, false);
        assert!(outcome.missing.is_empty());
        assert!(
            !fake.calls().iter().any(|c| c.contains("apt-get install")),
            "{:?}",
            fake.calls()
        );
    }

    #[test]
    fn missing_commands_are_installed_by_generic_name() {
        let mut fake = fake_with_present(&[
            "ip",
            "dnsmasq",
            "dhcp_release",
            "mkfs.ext4",
            "curl",
            "find",
            "tar",
            "zstd",
            "sha256sum",
        ]);
        // `nft` is absent on the first probe (before install), then present
        // on the second (install.sh's post-install `have` re-check).
        fake.set_seq(
            "sh",
            &["-c", "command -v nft"],
            &[(1, "", ""), (0, "/usr/sbin/nft\n", "")],
        );
        let privileged = Privileged::with_sudo(&fake, true);
        let mut packages = Packages::new(Some(PkgManager::AptGet), &privileged);
        let outcome = ensure_runtime_deps(&mut packages, true, false, false);
        assert!(outcome.missing.is_empty(), "{:?}", outcome.missing);
        assert!(
            fake.calls()
                .iter()
                .any(|c| c.ends_with("apt-get install -y -qq nftables")),
            "{:?}",
            fake.calls()
        );
    }

    #[test]
    fn a_package_that_installs_but_leaves_the_command_absent_is_reported_missing() {
        // `nft` is absent on both probes: apt-get reports success but the
        // package-name mapping was wrong (or incomplete), so the binary
        // never actually appears. install.sh's re-check catches this.
        let fake = fake_with_present(&[
            "ip",
            "dnsmasq",
            "dhcp_release",
            "mkfs.ext4",
            "curl",
            "find",
            "tar",
            "zstd",
            "sha256sum",
        ]);
        let privileged = Privileged::with_sudo(&fake, true);
        let mut packages = Packages::new(Some(PkgManager::AptGet), &privileged);
        let outcome = ensure_runtime_deps(&mut packages, true, false, false);
        assert!(
            outcome.missing.contains(&"nft".to_owned()),
            "{:?}",
            outcome.missing
        );
        assert!(
            fake.calls()
                .iter()
                .any(|c| c.ends_with("apt-get install -y -qq nftables")),
            "install was still attempted: {:?}",
            fake.calls()
        );
    }

    #[test]
    fn check_only_reports_without_installing() {
        let fake = fake_with_present(&[]);
        let privileged = Privileged::with_sudo(&fake, true);
        let mut packages = Packages::new(Some(PkgManager::AptGet), &privileged);
        let outcome = ensure_runtime_deps(&mut packages, true, true, false);
        assert!(
            outcome.missing.contains(&"nft".to_owned()),
            "{:?}",
            outcome.missing
        );
        assert!(
            !fake.calls().iter().any(|c| c.contains("install")),
            "{:?}",
            fake.calls()
        );
    }

    #[test]
    fn no_deps_reports_missing_without_installing() {
        let fake = fake_with_present(&[]);
        let privileged = Privileged::with_sudo(&fake, true);
        let mut packages = Packages::new(Some(PkgManager::AptGet), &privileged);
        let outcome = ensure_runtime_deps(&mut packages, false, false, false);
        assert!(outcome.missing.contains(&"ip".to_owned()));
        assert!(!fake.calls().iter().any(|c| c.contains("install")));
    }

    #[test]
    fn dhcp_release_is_a_warning_not_a_failure_and_never_reinstalls_dnsmasq_on_dnf() {
        let fake = fake_with_present(&[
            "ip",
            "nft",
            "dnsmasq",
            "mkfs.ext4",
            "curl",
            "find",
            "tar",
            "zstd",
            "sha256sum",
        ]);
        let privileged = Privileged::with_sudo(&fake, true);
        let mut packages = Packages::new(Some(PkgManager::Dnf), &privileged);
        let outcome = ensure_runtime_deps(&mut packages, true, false, false);
        assert!(outcome.missing.is_empty(), "{:?}", outcome.missing);
        assert!(
            outcome.warnings.iter().any(|w| w.contains("dhcp_release")),
            "{:?}",
            outcome.warnings
        );
        assert!(
            !fake.calls().iter().any(|c| c.contains("dnf install")),
            "{:?}",
            fake.calls()
        );
    }

    #[test]
    fn semanage_is_only_pursued_when_selinux_is_on() {
        let present = &[
            "ip",
            "nft",
            "dnsmasq",
            "dhcp_release",
            "mkfs.ext4",
            "curl",
            "find",
            "tar",
            "zstd",
            "sha256sum",
        ];
        let fake = fake_with_present(present);
        let privileged = Privileged::with_sudo(&fake, true);
        let mut packages = Packages::new(Some(PkgManager::Dnf), &privileged);
        ensure_runtime_deps(&mut packages, true, false, false);
        assert!(
            !fake.calls().iter().any(|c| c.contains("policycoreutils")),
            "{:?}",
            fake.calls()
        );

        let fake = fake_with_present(present);
        let privileged = Privileged::with_sudo(&fake, true);
        let mut packages = Packages::new(Some(PkgManager::Dnf), &privileged);
        ensure_runtime_deps(&mut packages, true, false, true);
        assert!(
            fake.calls()
                .iter()
                .any(|c| c.contains("policycoreutils-python-utils")),
            "{:?}",
            fake.calls()
        );
    }
}
