//! install.sh `ensure_account` … `install_config`: 계정, 디렉터리, 컴플라이언스
//! 자료, 바이너리·대시보드, 그리고 api.env.

use std::path::Path;

use super::Error;
use super::env::ServiceEnv;
use super::output;
use super::payload::{BUNDLE_BINARIES, EXTRACT_HELPERS, Payload, resolve_binary};
use super::pkg::have;
use super::privileged::Privileged;

/// install.sh가 처음 한 번 심는 `$CONFDIR/api.env`.
pub const API_ENV_TEMPLATE: &str = "\
# firecrab API settings. The unit already sets the image root, the dashboard
# assets and the working directory; uncomment only what you want to change.
#
# FIRECRAB_BIND_ADDR=127.0.0.1:5523
# A non-loopback address requires authentication AND TLS to be enabled.
# FIRECRAB_ALLOWED_ORIGINS=
# Empty is correct while the dashboard is served from this same origin.
# FIRECRAB_FIRECRACKER_BIN=firecracker
#
# M2Image install (Images page / POST /api/images/{alias}/install).
# Points at a directory-style base that serves `{alias}.tar.zst` packages
# (see scripts/package-m2images.sh). Unset = use the public MicroRegistry.
# Example local mirror:
# FIRECRAB_IMAGE_BASE_URL=http://127.0.0.1:8765
# Requires host tools: tar, zstd. Restart firecrab-api after changing this.
";

const STEP_ACCOUNT: &str = "account";
const STEP_DIRS: &str = "directories";
const STEP_COMPLIANCE: &str = "compliance";
const STEP_BINARIES: &str = "binaries";
const STEP_CONFIG: &str = "config";

fn succeeded(privileged: &Privileged<'_>, cmd: &str, args: &[&str]) -> bool {
    privileged
        .runner()
        .run(cmd, args)
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// install.sh `ensure_account`: 시스템 계정과 kvm 그룹 소속, /dev/kvm ACL.
pub fn ensure_account(privileged: &Privileged<'_>, env: &ServiceEnv) -> Result<(), Error> {
    if succeeded(privileged, "getent", &["group", &env.group]) {
        output::log(&format!("group {} exists", env.group));
    } else {
        output::step(&format!("creating group {}", env.group));
        privileged.run_ok(STEP_ACCOUNT, "groupadd", &["--system", &env.group])?;
    }

    if succeeded(privileged, "id", &[&env.user]) {
        output::log(&format!("user {} exists", env.user));
    } else {
        output::step(&format!("creating user {}", env.user));
        let home = env.datadir.to_string_lossy().into_owned();
        privileged.run_ok(
            STEP_ACCOUNT,
            "useradd",
            &[
                "--system",
                "--gid",
                &env.group,
                "--home-dir",
                &home,
                "--shell",
                "/usr/sbin/nologin",
                &env.user,
            ],
        )?;
    }

    // The API spawns firecracker, which opens /dev/kvm.
    if succeeded(privileged, "getent", &["group", "kvm"]) {
        let in_kvm = privileged
            .runner()
            .run("id", &["-nG", &env.user])
            .map(|out| {
                String::from_utf8_lossy(&out.stdout)
                    .split_whitespace()
                    .any(|g| g == "kvm")
            })
            .unwrap_or(false);
        if !in_kvm {
            output::step(&format!("adding {} to the kvm group", env.user));
            privileged.run_ok(
                STEP_ACCOUNT,
                "usermod",
                &["--append", "--groups", "kvm", &env.user],
            )?;
        }
    }

    // Group membership is necessary but not always sufficient: some hosts'
    // /dev/kvm does not land in the kvm group the standard udev rule asks for.
    // The ACL entry is narrow and idempotent.
    if Path::new("/dev/kvm").exists() {
        if have(privileged.runner(), "setfacl") {
            let has_entry = privileged
                .runner()
                .run("getfacl", &["-p", "/dev/kvm"])
                .map(|out| {
                    String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .any(|l| l.trim() == "group:kvm:rw-")
                })
                .unwrap_or(false);
            if !has_entry {
                output::step("granting the kvm group ACL access to /dev/kvm");
                privileged.run_ok(STEP_ACCOUNT, "setfacl", &["-m", "g:kvm:rw", "/dev/kvm"])?;
            }
        } else {
            output::warn(
                "setfacl not found — skipping the /dev/kvm ACL fixup (install the 'acl' package if VMs fail with a KVM permission error)",
            );
        }
    }
    Ok(())
}

/// install.sh `ensure_directories`.
pub fn ensure_directories(privileged: &Privileged<'_>, env: &ServiceEnv) -> Result<(), Error> {
    privileged.install_dirs(
        STEP_DIRS,
        &[&env.libdir, &env.sharedir],
        "root",
        "root",
        "0755",
    )?;
    let data = env.datadir.join("data");
    let images = env.datadir.join("images");
    let updates = env.datadir.join("updates");
    privileged.install_dirs(
        STEP_DIRS,
        &[&env.datadir, &data, &images, &updates],
        &env.user,
        &env.group,
        "0750",
    )?;
    privileged.install_dirs(STEP_DIRS, &[&env.confdir], "root", &env.group, "0750")?;
    output::log(&format!(
        "directories ready under {}, {}",
        env.datadir.display(),
        env.confdir.display()
    ));
    Ok(())
}

/// install.sh `install_compliance`.
pub fn install_compliance(
    privileged: &Privileged<'_>,
    env: &ServiceEnv,
    payload: &Payload,
) -> Result<(), Error> {
    let licenses = env.sharedir.join("licenses");
    privileged.install_dirs(STEP_COMPLIANCE, &[&licenses], "root", "root", "0755")?;
    privileged.install_file(
        STEP_COMPLIANCE,
        &payload.license,
        &env.sharedir.join("LICENSE"),
        "root",
        "root",
        "0644",
    )?;
    privileged.install_file(
        STEP_COMPLIANCE,
        &payload.gpl,
        &licenses.join("GPL-2.0-only.txt"),
        "root",
        "root",
        "0644",
    )?;
    match (&payload.third_party, &payload.inventory) {
        (Some(notices), Some(inventory)) => {
            privileged.install_file(
                STEP_COMPLIANCE,
                notices,
                &env.sharedir.join("THIRD_PARTY_NOTICES.txt"),
                "root",
                "root",
                "0644",
            )?;
            privileged.install_file(
                STEP_COMPLIANCE,
                inventory,
                &env.sharedir.join("release-license-inventory.json"),
                "root",
                "root",
                "0644",
            )?;
        }
        _ => {
            // A checkout may not have run the compliance-generation step;
            // stale copies from a previous release install must not linger.
            let notices = env.sharedir.join("THIRD_PARTY_NOTICES.txt");
            let inventory = env.sharedir.join("release-license-inventory.json");
            let (a, b) = (
                notices.to_string_lossy().into_owned(),
                inventory.to_string_lossy().into_owned(),
            );
            privileged.run_ok(STEP_COMPLIANCE, "rm", &["-f", &a, &b])?;
        }
    }
    output::log(&format!(
        "license and attribution material installed to {}",
        env.sharedir.display()
    ));
    Ok(())
}

/// install.sh `install_binaries`.
pub fn install_binaries(
    privileged: &Privileged<'_>,
    env: &ServiceEnv,
    payload: &Payload,
    with_frontend: bool,
) -> Result<(), Error> {
    for name in ["firecrab-api", "firecrab-net-helper"] {
        let src = resolve_binary(name, Some(&payload.bin), &env.libdir).ok_or_else(|| {
            Error::step_fix(
                STEP_BINARIES,
                format!(
                    "no {name} in {} or {}",
                    payload.bin.display(),
                    env.libdir.display()
                ),
                "pass it via --bin-dir, or install a release",
            )
        })?;
        let dest = env.libdir.join(name);
        if src == dest {
            output::log(&format!("keeping existing {}", dest.display()));
        } else {
            privileged.install_file(STEP_BINARIES, &src, &dest, "root", "root", "0755")?;
        }
    }

    // The API turns a distro vmlinuz into the format Firecracker boots with
    // these; installing them next to the binary means it never needs the checkout.
    for helper in EXTRACT_HELPERS {
        let src = payload.extract.join(helper);
        if !src.is_file() {
            return Err(Error::step(
                STEP_BINARIES,
                format!("missing {}", src.display()),
            ));
        }
        privileged.install_file(
            STEP_BINARIES,
            &src,
            &env.libdir.join(helper),
            "root",
            "root",
            "0755",
        )?;
    }

    let cli = BUNDLE_BINARIES[2];
    let src = resolve_binary(cli, Some(&payload.bin), &env.bindir).ok_or_else(|| {
        Error::step_fix(
            STEP_BINARIES,
            format!(
                "no {cli} (CLI) in {} or {}",
                payload.bin.display(),
                env.bindir.display()
            ),
            "pass it via --bin-dir, or install a release",
        )
    })?;
    let dest = env.bindir.join(cli);
    if src == dest {
        output::log(&format!("keeping existing {}", dest.display()));
    } else {
        privileged.install_dirs(STEP_BINARIES, &[&env.bindir], "root", "root", "0755")?;
        privileged.install_file(STEP_BINARIES, &src, &dest, "root", "root", "0755")?;
    }
    output::log(&format!(
        "binaries installed to {} (cli → {})",
        env.libdir.display(),
        dest.display()
    ));

    if !with_frontend {
        return Ok(());
    }
    let target = env.sharedir.join("dashboard");
    match &payload.dashboard {
        Some(dir) if dir.join("index.html").is_file() => {
            let (target_s, source_s) = (
                target.to_string_lossy().into_owned(),
                format!("{}/.", dir.display()),
            );
            privileged.run_ok(STEP_BINARIES, "rm", &["-rf", &target_s])?;
            privileged.install_dirs(STEP_BINARIES, &[&target], "root", "root", "0755")?;
            privileged.run_ok(
                STEP_BINARIES,
                "cp",
                &["-r", &source_s, &format!("{target_s}/")],
            )?;
            privileged.run_ok(STEP_BINARIES, "chown", &["-R", "root:root", &target_s])?;
            output::log(&format!("dashboard installed to {target_s}"));
            Ok(())
        }
        _ if target.join("index.html").is_file() => {
            output::log(&format!("keeping existing {}", target.display()));
            Ok(())
        }
        _ => Err(Error::step_fix(
            STEP_BINARIES,
            "dashboard not found",
            "pass --dashboard-dir, or use a release bundle",
        )),
    }
}

/// install.sh `install_config`: 한 번만 심어 운영자의 수정이 재실행에서 살아남는다.
pub fn install_config(privileged: &Privileged<'_>, env: &ServiceEnv) -> Result<(), Error> {
    let api_env = env.confdir.join("api.env");
    if api_env.is_file() {
        output::log(&format!("keeping existing {}", api_env.display()));
        return Ok(());
    }
    privileged.write_file(STEP_CONFIG, &api_env, API_ENV_TEMPLATE.as_bytes(), "0640")?;
    let path = api_env.to_string_lossy().into_owned();
    privileged.run_ok(
        STEP_CONFIG,
        "chown",
        &[&format!("root:{}", env.group), &path],
    )?;
    output::log(&format!("wrote {path}"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::FakeCommandRunner;
    use std::path::Path;

    fn env_for(root: &Path) -> ServiceEnv {
        ServiceEnv::from_values(
            "firecrab",
            "firecrab",
            &root.join("usr/local"),
            &root.join("var/lib/firecrab"),
            &root.join("etc/firecrab"),
            &root.join("etc/systemd/system"),
        )
    }

    #[test]
    fn account_creation_is_skipped_when_the_user_and_group_exist() {
        let mut fake = FakeCommandRunner::permissive();
        fake.set("getent", &["group", "firecrab"], 0, "firecrab:x:900:\n", "");
        fake.set("id", &["firecrab"], 0, "uid=900\n", "");
        fake.set("getent", &["group", "kvm"], 0, "kvm:x:104:firecrab\n", "");
        fake.set("id", &["-nG", "firecrab"], 0, "firecrab kvm\n", "");
        let dir = tempfile::tempdir().unwrap();
        ensure_account(&Privileged::with_sudo(&fake, true), &env_for(dir.path())).unwrap();
        let calls = fake.calls();
        assert!(!calls.iter().any(|c| c.contains("groupadd")), "{calls:?}");
        assert!(!calls.iter().any(|c| c.contains("useradd")), "{calls:?}");
        assert!(!calls.iter().any(|c| c.contains("usermod")), "{calls:?}");
    }

    #[test]
    fn account_creation_adds_group_user_and_kvm_membership() {
        let mut fake = FakeCommandRunner::permissive();
        fake.set("getent", &["group", "firecrab"], 1, "", "");
        fake.set("id", &["firecrab"], 1, "", "");
        fake.set("getent", &["group", "kvm"], 0, "kvm:x:104:\n", "");
        fake.set("id", &["-nG", "firecrab"], 0, "firecrab\n", "");
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        ensure_account(&Privileged::with_sudo(&fake, true), &env).unwrap();
        let calls = fake.calls();
        assert!(
            calls.iter().any(|c| c == "sudo groupadd --system firecrab"),
            "{calls:?}"
        );
        assert!(
            calls.iter().any(|c| c
                == &format!(
                    "sudo useradd --system --gid firecrab --home-dir {} --shell /usr/sbin/nologin firecrab",
                    env.datadir.display()
                )),
            "{calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|c| c == "sudo usermod --append --groups kvm firecrab"),
            "{calls:?}"
        );
    }

    #[test]
    fn directories_use_install_sh_owners_and_modes() {
        let fake = FakeCommandRunner::permissive();
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        ensure_directories(&Privileged::with_sudo(&fake, true), &env).unwrap();
        let calls = fake.calls();
        assert!(
            calls.iter().any(|c| c
                == &format!(
                    "sudo install -d -o root -g root -m 0755 {} {}",
                    env.libdir.display(),
                    env.sharedir.display()
                )),
            "{calls:?}"
        );
        assert!(
            calls.iter().any(
                |c| c.starts_with("sudo install -d -o firecrab -g firecrab -m 0750")
                    && c.contains(&env.datadir.join("images").display().to_string())
                    && c.contains(&env.datadir.join("updates").display().to_string())
            ),
            "{calls:?}"
        );
        assert!(
            calls.iter().any(|c| c
                == &format!(
                    "sudo install -d -o root -g firecrab -m 0750 {}",
                    env.confdir.display()
                )),
            "{calls:?}"
        );
    }

    #[test]
    fn binaries_land_in_libdir_and_the_cli_in_bindir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let env = env_for(root);
        let bin = root.join("payload");
        std::fs::create_dir_all(&bin).unwrap();
        for name in [
            "firecrab-api",
            "firecrab-net-helper",
            "firecrab",
            "extract-vmlinux",
            "extract-arm64-image",
        ] {
            std::fs::write(bin.join(name), b"x").unwrap();
        }
        let dash = root.join("dash");
        std::fs::create_dir_all(&dash).unwrap();
        std::fs::write(dash.join("index.html"), b"<html>").unwrap();
        let payload = Payload {
            bin: bin.clone(),
            units: root.join("units"),
            extract: bin.clone(),
            dashboard: Some(dash.clone()),
            license: root.join("LICENSE"),
            gpl: root.join("GPL"),
            third_party: None,
            inventory: None,
            checkout: None,
            _temp: None,
        };
        let fake = FakeCommandRunner::permissive();
        install_binaries(&Privileged::with_sudo(&fake, true), &env, &payload, true).unwrap();
        let calls = fake.calls();
        assert!(
            calls.iter().any(|c| c
                == &format!(
                    "sudo install -o root -g root -m 0755 {} {}",
                    bin.join("firecrab-api").display(),
                    env.libdir.join("firecrab-api").display()
                )),
            "{calls:?}"
        );
        assert!(
            calls.iter().any(|c| c
                == &format!(
                    "sudo install -o root -g root -m 0755 {} {}",
                    bin.join("firecrab").display(),
                    env.bindir.join("firecrab").display()
                )),
            "{calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.contains("extract-vmlinux")),
            "{calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("sudo cp -r ") && c.contains("dashboard")),
            "{calls:?}"
        );
    }

    #[test]
    fn no_frontend_skips_the_dashboard() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let env = env_for(root);
        let bin = root.join("payload");
        std::fs::create_dir_all(&bin).unwrap();
        for name in [
            "firecrab-api",
            "firecrab-net-helper",
            "firecrab",
            "extract-vmlinux",
            "extract-arm64-image",
        ] {
            std::fs::write(bin.join(name), b"x").unwrap();
        }
        let payload = Payload {
            bin: bin.clone(),
            units: root.join("units"),
            extract: bin.clone(),
            dashboard: None,
            license: root.join("LICENSE"),
            gpl: root.join("GPL"),
            third_party: None,
            inventory: None,
            checkout: None,
            _temp: None,
        };
        let fake = FakeCommandRunner::permissive();
        install_binaries(&Privileged::with_sudo(&fake, true), &env, &payload, false).unwrap();
        assert!(
            !fake.calls().iter().any(|c| c.contains("dashboard")),
            "{:?}",
            fake.calls()
        );
    }

    #[test]
    fn config_is_seeded_once_and_then_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        std::fs::create_dir_all(&env.confdir).unwrap();
        let fake = FakeCommandRunner::permissive();
        let privileged = Privileged::with_sudo(&fake, true);
        install_config(&privileged, &env).unwrap();
        let api_env = env.confdir.join("api.env");
        assert_eq!(
            fake.stdin_of(&format!("sudo tee {}", api_env.display())),
            Some(API_ENV_TEMPLATE.as_bytes().to_vec())
        );

        // The fake never actually writes, so simulate the file the tee created.
        std::fs::write(&api_env, API_ENV_TEMPLATE).unwrap();
        let fake2 = FakeCommandRunner::permissive();
        install_config(&Privileged::with_sudo(&fake2, true), &env).unwrap();
        assert!(
            !fake2.calls().iter().any(|c| c.contains("tee")),
            "{:?}",
            fake2.calls()
        );
    }
}
