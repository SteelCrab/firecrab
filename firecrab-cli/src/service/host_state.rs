//! install.sh `record_host_baseline` / `restore_host_baseline`: 설치와 helper가 바꾼
//! 호스트 상태 중 우리가 바꾼 것만 되돌린다.
//!
//! install.sh는 처음 설치할 때 `$CONFDIR/host-baseline.env`에 설치 직전 값을
//! 적는다. 기록이 없는 설치(이 기능 이전)는 무엇이 우리 것인지 알 수 없으므로
//! 건드리지 않는다. Rust 설치기가 구현되면 같은 파일을 써야 한다.

use super::env::ServiceEnv;
use super::output;
use super::privileged::Privileged;
use crate::shell::CommandRunner;

/// `$CONFDIR` 아래 기록 파일 이름. install.sh와 같아야 한다.
pub const BASELINE_FILE: &str = "host-baseline.env";

const KVM_DEVICE: &str = "/dev/kvm";
const IPV4_FORWARDING: &str = "net.ipv4.ip_forward";
const IPV6_FORWARDING: &str = "net.ipv6.conf.all.forwarding";

/// 설치 직전 호스트 상태. 값이 없으면 그 항목은 기록되지 않은 것이다.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Baseline {
    /// 설치가 `/dev/kvm`에 `g:kvm:rw` ACL을 새로 붙였다.
    pub kvm_acl_added: bool,
    /// 설치 직전 `net.ipv4.ip_forward`.
    pub ip_forward: Option<bool>,
    /// 설치 직전 `net.ipv6.conf.all.forwarding`.
    pub ipv6_forwarding: Option<bool>,
}

impl Baseline {
    /// `key=value` 줄만 읽는다. 주석과 모르는 키는 무시한다.
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

/// 기록이 있으면 그 기록에 있는 것만 되돌리고 기록 파일을 지운다.
pub fn restore(privileged: &Privileged<'_>, env: &ServiceEnv) {
    let path = env
        .confdir
        .join(BASELINE_FILE)
        .to_string_lossy()
        .into_owned();
    // root 소유 0640이라 sudo로 읽는다.
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
    // 다음 설치가 그때의 호스트 상태를 새로 기록하도록.
    let _ = privileged.run("rm", &["-f", &path]);
}

/// 설치기가 붙인 ACL만 뗀다. 이미 없으면 할 일이 없다.
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

/// `getfacl` 출력에 `user:<이름>:` 또는 `group:<이름>:` 항목이 있는가. 소유자·
/// 소유 그룹을 뜻하는 `user::`/`group::`은 세지 않는다.
fn has_named_entry(listing: &str) -> bool {
    listing.lines().any(|line| {
        let line = line.trim();
        line.strip_prefix("user:")
            .or_else(|| line.strip_prefix("group:"))
            .is_some_and(|rest| !rest.starts_with(':'))
    })
}

/// 설치 전에 꺼져 있던 포워딩을, 아직 켜져 있고 다른 누가 쓰지 않을 때만 끈다.
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
    // Docker·libvirt·CNI 같은 쪽이 같은 설정에 기대고 있을 수 있다. 우리 브리지는
    // 이미 걷혔으니, 브리지나 veth가 남아 있으면 끄지 않는다.
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

/// `ip`로 확인하지 못하면 안전한 쪽(다른 누가 쓴다)으로 본다.
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
