//! install.sh `record_host_baseline` / `restore_host_baseline`: restore only
//! the host state changed by the installer and helper.
//!
//! On the first installation, install.sh records the previous values in
//! `$CONFDIR/host-baseline.env`. Leave installations without a record (from before
//! this feature) untouched because their changes cannot be identified. The Rust installer must use the same file.

use super::env::ServiceEnv;
use super::output;
use super::privileged::Privileged;
use crate::shell::CommandRunner;

/// Baseline file name under `$CONFDIR`. Must match install.sh.
pub const BASELINE_FILE: &str = "host-baseline.env";

const KVM_DEVICE: &str = "/dev/kvm";
const IPV4_FORWARDING: &str = "net.ipv4.ip_forward";
const IPV6_FORWARDING: &str = "net.ipv6.conf.all.forwarding";

/// Host state before installation. An absent value means the setting was not recorded.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Baseline {
    /// The installer added the `g:kvm:rw` ACL to `/dev/kvm`.
    pub kvm_acl_added: bool,
    /// `net.ipv4.ip_forward` before installation.
    pub ip_forward: Option<bool>,
    /// `net.ipv6.conf.all.forwarding` before installation.
    pub ipv6_forwarding: Option<bool>,
}

impl Baseline {
    /// Read only `key=value` lines. Ignore comments and unknown keys.
    pub fn parse(text: &str) -> Self {
        let mut baseline = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.trim().split_once('=') else {
                continue;
            };
            let flag = match value.trim() {
                "1" => Some(true),
                "0" => Some(false),
                _ => None,
            };
            match key.trim() {
                "kvm_acl" => baseline.kvm_acl_added = value.trim() == "added",
                "ip_forward" => baseline.ip_forward = flag,
                "ipv6_forwarding" => baseline.ipv6_forwarding = flag,
                _ => {}
            }
        }
        baseline
    }
}

/// If a baseline exists, restore only its recorded settings and remove the file.
pub fn restore(privileged: &Privileged<'_>, env: &ServiceEnv) {
    let path = env
        .confdir
        .join(BASELINE_FILE)
        .to_string_lossy()
        .into_owned();
    // Read with sudo because the file is owned by root with mode 0640.
    let Some(text) = privileged
        .run("cat", &[&path])
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
    else {
        return;
    };
    let baseline = Baseline::parse(&text);
    if baseline.kvm_acl_added {
        remove_kvm_acl(privileged);
    }
    restore_forwarding(privileged, &baseline);
    // Let the next installation record the host state as it exists then.
    let _ = privileged.run("rm", &["-f", &path]);
}

/// Remove only the ACL added by the installer. Do nothing if it is already absent.
fn remove_kvm_acl(privileged: &Privileged<'_>) {
    let present = privileged
        .runner()
        .run("getfacl", &["-p", KVM_DEVICE])
        .ok()
        .filter(|out| out.status.success())
        .is_some_and(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|line| line.trim() == "group:kvm:rw-")
        });
    if !present {
        return;
    }
    let removed = privileged
        .run("setfacl", &["-x", "g:kvm", KVM_DEVICE])
        .map(|out| out.status.success())
        .unwrap_or(false);
    if removed {
        // `setfacl -x` leaves a bare mask behind. With nothing else named in the
        // ACL that is just noise (`ls -l` shows a `+`), so take it all off;
        // entries another tool added, logind's uaccess say, are kept.
        let left = privileged
            .runner()
            .run("getfacl", &["-p", KVM_DEVICE])
            .ok()
            .filter(|out| out.status.success())
            .is_some_and(|out| has_named_entry(&String::from_utf8_lossy(&out.stdout)));
        if !left {
            let _ = privileged.run("setfacl", &["-b", KVM_DEVICE]);
        }
        output::log("removed the kvm group ACL the installer added to /dev/kvm");
    } else {
        output::warn("could not remove the kvm group ACL from /dev/kvm — it resets at reboot");
    }
}

/// Check `getfacl` output for `user:<name>:` or `group:<name>:` entries.
/// Exclude `user::`/`group::`, which refer to the owner and owning group.
fn has_named_entry(listing: &str) -> bool {
    listing.lines().any(|line| {
        let line = line.trim();
        line.strip_prefix("user:")
            .or_else(|| line.strip_prefix("group:"))
            .is_some_and(|rest| !rest.starts_with(':'))
    })
}

/// Disable forwarding that was off before installation only if it is still on and no one else uses it.
fn restore_forwarding(privileged: &Privileged<'_>, baseline: &Baseline) {
    let runner = privileged.runner();
    let to_disable: Vec<&str> = [
        (IPV4_FORWARDING, baseline.ip_forward),
        (IPV6_FORWARDING, baseline.ipv6_forwarding),
    ]
    .into_iter()
    .filter(|(key, before)| *before == Some(false) && is_on(runner, key))
    .map(|(key, _)| key)
    .collect();
    if to_disable.is_empty() {
        return;
    }
    // Docker, libvirt, or CNI may depend on the same setting. Our bridges are
    // already removed, so keep forwarding on if any bridge or veth remains.
    if others_may_route(runner) {
        output::warn(&format!(
            "left {} on: other bridges or containers are present (resets at reboot)",
            to_disable.join(", ")
        ));
        return;
    }
    for key in to_disable {
        let assignment = format!("{key}=0");
        let ok = privileged
            .run("sysctl", &["-w", &assignment])
            .map(|out| out.status.success())
            .unwrap_or(false);
        if ok {
            output::log(&format!("restored {assignment}"));
        } else {
            output::warn(&format!("could not restore {assignment}"));
        }
    }
}

fn is_on(runner: &dyn CommandRunner, key: &str) -> bool {
    runner
        .run("sysctl", &["-n", key])
        .ok()
        .filter(|out| out.status.success())
        .is_some_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "1")
}

/// If `ip` cannot determine usage, assume another service uses it to avoid disrupting that service.
fn others_may_route(runner: &dyn CommandRunner) -> bool {
    ["bridge", "veth"].into_iter().any(|kind| {
        match runner.run("ip", &["-o", "link", "show", "type", kind]) {
            Ok(out) if out.status.success() => !out.stdout.iter().all(u8::is_ascii_whitespace),
            _ => true,
        }
    })
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

    fn baseline_path(env: &ServiceEnv) -> String {
        env.confdir.join(BASELINE_FILE).display().to_string()
    }

    /// A runner whose baseline file holds `contents`, on a host with the given probes.
    fn host(env: &ServiceEnv, contents: &str) -> FakeCommandRunner {
        let mut fake = FakeCommandRunner::permissive();
        fake.set("sudo", &["cat", &baseline_path(env)], 0, contents, "");
        fake
    }

    fn run_restore(fake: &FakeCommandRunner, env: &ServiceEnv) -> Vec<String> {
        restore(&Privileged::with_sudo(fake, true), env);
        fake.calls()
    }

    fn mutating(calls: &[String]) -> Vec<&String> {
        calls
            .iter()
            .filter(|c| {
                c.starts_with("sudo setfacl")
                    || c.starts_with("sudo sysctl")
                    || c.starts_with("sudo rm")
            })
            .collect()
    }

    const ACL_PRESENT: &str = "# file: dev/kvm\nuser::rw-\nuser:root:rw-\ngroup::rw-\ngroup:kvm:rw-\nmask::rw-\nother::---\n";

    #[test]
    fn the_baseline_file_is_parsed_key_by_key() {
        let parsed = Baseline::parse(
            "# note\nkvm_acl=added\nip_forward=0\nipv6_forwarding=1\nbogus=1\nnoequals\n",
        );
        assert_eq!(
            parsed,
            Baseline {
                kvm_acl_added: true,
                ip_forward: Some(false),
                ipv6_forwarding: Some(true),
            }
        );
        assert_eq!(Baseline::parse(""), Baseline::default());
        assert_eq!(Baseline::parse("ip_forward=maybe").ip_forward, None);
    }

    #[test]
    fn a_host_installed_before_baselines_existed_is_left_exactly_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        let mut fake = FakeCommandRunner::permissive();
        fake.set(
            "sudo",
            &["cat", &baseline_path(&env)],
            1,
            "",
            "No such file",
        );
        let calls = run_restore(&fake, &env);
        assert!(mutating(&calls).is_empty(), "{calls:?}");
    }

    #[test]
    fn the_acl_the_installer_added_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        let mut fake = host(&env, "kvm_acl=added\n");
        fake.set("getfacl", &["-p", "/dev/kvm"], 0, ACL_PRESENT, "");
        let calls = run_restore(&fake, &env);
        assert!(
            calls.contains(&"sudo setfacl -x g:kvm /dev/kvm".to_owned()),
            "{calls:?}"
        );
    }

    const ACL_MASK_ONLY: &str = "# file: dev/kvm\nuser::rw-\ngroup::rw-\nmask::rw-\nother::---\n";
    const ACL_OTHER_USER: &str =
        "# file: dev/kvm\nuser::rw-\nuser:alice:rw-\ngroup::rw-\nmask::rw-\nother::---\n";

    #[test]
    fn the_empty_mask_left_behind_goes_too_so_the_device_has_no_acl_again() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        let mut fake = host(&env, "kvm_acl=added\n");
        // Before the removal the entry is there; after it only a bare mask is.
        fake.set_seq(
            "getfacl",
            &["-p", "/dev/kvm"],
            &[(0, ACL_PRESENT, ""), (0, ACL_MASK_ONLY, "")],
        );
        let calls = run_restore(&fake, &env);
        assert!(
            calls.contains(&"sudo setfacl -x g:kvm /dev/kvm".to_owned()),
            "{calls:?}"
        );
        assert!(
            calls.contains(&"sudo setfacl -b /dev/kvm".to_owned()),
            "{calls:?}"
        );
    }

    #[test]
    fn an_entry_another_tool_added_to_the_device_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        let mut fake = host(&env, "kvm_acl=added\n");
        fake.set_seq(
            "getfacl",
            &["-p", "/dev/kvm"],
            &[(0, ACL_PRESENT, ""), (0, ACL_OTHER_USER, "")],
        );
        let calls = run_restore(&fake, &env);
        assert!(
            calls.contains(&"sudo setfacl -x g:kvm /dev/kvm".to_owned()),
            "{calls:?}"
        );
        assert!(!calls.iter().any(|c| c.contains("setfacl -b")), "{calls:?}");
    }

    #[test]
    fn named_acl_entries_are_told_from_the_base_ones() {
        assert!(has_named_entry(ACL_PRESENT));
        assert!(has_named_entry(ACL_OTHER_USER));
        assert!(!has_named_entry(ACL_MASK_ONLY));
        assert!(!has_named_entry("user::rw-\ngroup::rw-\nother::---\n"));
        assert!(!has_named_entry(""));
    }

    #[test]
    fn an_acl_the_installer_did_not_add_is_not_ours_to_remove() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        let mut fake = host(&env, "ip_forward=1\n");
        fake.set("getfacl", &["-p", "/dev/kvm"], 0, ACL_PRESENT, "");
        let calls = run_restore(&fake, &env);
        assert!(!calls.iter().any(|c| c.contains("setfacl")), "{calls:?}");
    }

    #[test]
    fn an_acl_that_is_already_gone_is_not_removed_again() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        let mut fake = host(&env, "kvm_acl=added\n");
        fake.set(
            "getfacl",
            &["-p", "/dev/kvm"],
            0,
            "user::rw-\ngroup::rw-\nother::---\n",
            "",
        );
        let calls = run_restore(&fake, &env);
        assert!(!calls.iter().any(|c| c.contains("setfacl")), "{calls:?}");
    }

    #[test]
    fn forwarding_the_host_had_off_is_switched_off_again_when_nothing_else_routes() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        let mut fake = host(&env, "ip_forward=0\nipv6_forwarding=0\n");
        fake.set("sysctl", &["-n", "net.ipv4.ip_forward"], 0, "1\n", "");
        fake.set(
            "sysctl",
            &["-n", "net.ipv6.conf.all.forwarding"],
            0,
            "1\n",
            "",
        );
        let calls = run_restore(&fake, &env);
        assert!(
            calls.contains(&"sudo sysctl -w net.ipv4.ip_forward=0".to_owned()),
            "{calls:?}"
        );
        assert!(
            calls.contains(&"sudo sysctl -w net.ipv6.conf.all.forwarding=0".to_owned()),
            "{calls:?}"
        );
    }

    #[test]
    fn forwarding_stays_on_while_another_bridge_or_container_network_exists() {
        for kind in ["bridge", "veth"] {
            let dir = tempfile::tempdir().unwrap();
            let env = env_for(dir.path());
            let mut fake = host(&env, "ip_forward=0\n");
            fake.set("sysctl", &["-n", "net.ipv4.ip_forward"], 0, "1\n", "");
            fake.set(
                "ip",
                &["-o", "link", "show", "type", kind],
                0,
                "7: docker0: <BROADCAST,MULTICAST,UP> mtu 1500\n",
                "",
            );
            let calls = run_restore(&fake, &env);
            assert!(
                !calls.iter().any(|c| c.starts_with("sudo sysctl")),
                "{kind}: {calls:?}"
            );
        }
    }

    #[test]
    fn forwarding_stays_on_when_the_probe_cannot_run() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        let mut fake = host(&env, "ip_forward=0\n");
        fake.set("sysctl", &["-n", "net.ipv4.ip_forward"], 0, "1\n", "");
        fake.set(
            "ip",
            &["-o", "link", "show", "type", "bridge"],
            1,
            "",
            "ip: command failed",
        );
        let calls = run_restore(&fake, &env);
        assert!(
            !calls.iter().any(|c| c.starts_with("sudo sysctl")),
            "{calls:?}"
        );
    }

    #[test]
    fn forwarding_the_host_already_had_on_is_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        let mut fake = host(&env, "ip_forward=1\nipv6_forwarding=1\n");
        fake.set("sysctl", &["-n", "net.ipv4.ip_forward"], 0, "1\n", "");
        let calls = run_restore(&fake, &env);
        assert!(
            !calls.iter().any(|c| c.starts_with("sudo sysctl")),
            "{calls:?}"
        );
    }

    #[test]
    fn forwarding_that_someone_else_already_turned_off_is_not_written_either() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        let mut fake = host(&env, "ip_forward=0\n");
        fake.set("sysctl", &["-n", "net.ipv4.ip_forward"], 0, "0\n", "");
        let calls = run_restore(&fake, &env);
        assert!(
            !calls.iter().any(|c| c.starts_with("sudo sysctl")),
            "{calls:?}"
        );
    }

    #[test]
    fn the_record_is_deleted_so_the_next_install_records_the_host_afresh() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        let fake = host(&env, "ip_forward=1\n");
        let calls = run_restore(&fake, &env);
        assert!(
            calls.contains(&format!("sudo rm -f {}", baseline_path(&env))),
            "{calls:?}"
        );
    }
}
