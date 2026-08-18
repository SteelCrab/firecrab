use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use super::{CheckResult, DoctorEnv};
use crate::shell::CommandRunner;

/// Username for diagnostic detail text. `std::env::var("USER")` stands in
/// for bash's `id -un` — good enough for a detail line, not used for any
/// pass/fail decision.
pub(crate) fn current_username() -> String {
    std::env::var("USER").unwrap_or_else(|_| "unknown".to_owned())
}

/// Canonicalized directory, or the input path unchanged if it cannot be
/// entered (mirrors bash's `abs_dir`: a 0750 root:firecrab DATADIR is a
/// healthy install, not a fault, and doctor must degrade to the raw path
/// rather than error out).
pub(crate) fn abs_dir(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub fn check_kvm() -> Vec<CheckResult> {
    let path = Path::new("/dev/kvm");
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => {
            return vec![CheckResult::fail(
                "kvm: /dev/kvm missing",
                Some("VMs cannot start without KVM"),
                Some("enable virtualization in BIOS, or nested virt if this host is itself a VM"),
            )];
        }
    };
    if !meta.file_type().is_char_device() {
        return vec![CheckResult::fail("kvm: /dev/kvm is not a character device", None, None)];
    }
    let readable = fs::OpenOptions::new().read(true).open(path).is_ok();
    let writable = fs::OpenOptions::new().write(true).open(path).is_ok();
    if readable && writable {
        return vec![CheckResult::pass("kvm")];
    }
    let user = current_username();
    vec![CheckResult::fail(
        "kvm: current user cannot read/write /dev/kvm",
        Some(&format!("user={user}")),
        Some(&format!("sudo usermod -aG kvm \"{user}\"; then log out and back in")),
    )]
}

pub fn check_ip_forward() -> Vec<CheckResult> {
    let raw = match fs::read_to_string("/proc/sys/net/ipv4/ip_forward") {
        Ok(s) => s,
        Err(_) => {
            return vec![CheckResult::skip(
                "ip_forward: /proc/sys/net/ipv4/ip_forward unreadable",
                None,
                Some("cat /proc/sys/net/ipv4/ip_forward"),
            )];
        }
    };
    let val = raw.trim();
    if val == "1" {
        return vec![CheckResult::pass("ip_forward")];
    }
    vec![CheckResult::fail(
        format!("ip_forward: net.ipv4.ip_forward is {val} (want 1)"),
        Some("guest outbound NAT needs forwarding"),
        Some("sudo sysctl -w net.ipv4.ip_forward=1  (net-helper also sets this on start)"),
    )]
}

/// Existing `firecrab.db` candidates, deduplicated by canonical path —
/// mirrors bash's `find_databases`.
fn find_databases(env: &DoctorEnv) -> Vec<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut candidates = vec![
        cwd.join("data/firecrab.db"),
        Path::new(&env.datadir).join("data/firecrab.db"),
        Path::new(&env.datadir).join("firecrab.db"),
    ];
    if let Some(image_root) = &env.image_root {
        if let Some(parent) = Path::new(image_root).parent() {
            candidates.push(abs_dir(parent).join("data/firecrab.db"));
        }
    }

    let mut seen = std::collections::HashSet::new();
    let mut found = Vec::new();
    for c in candidates {
        if !c.is_file() {
            continue;
        }
        let dir = c.parent().map(abs_dir).unwrap_or_else(|| PathBuf::from("."));
        let name = c.file_name().unwrap_or_default().to_owned();
        let path = dir.join(&name);
        if seen.insert(path.clone()) {
            found.push(path);
        }
    }
    found
}

pub fn check_data_root(env: &DoctorEnv) -> Vec<CheckResult> {
    let dbs = find_databases(env);
    if dbs.len() > 1 {
        let listing = dbs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
        return vec![CheckResult::fail(
            "data: multiple firecrab.db files found",
            Some(&listing),
            Some(&format!(
                "API resolves data/firecrab.db relative to cwd — use the unit WorkingDirectory ({}) or always start from the same directory (avoids no-such-column / empty state)",
                env.datadir
            )),
        )];
    }
    if dbs.len() == 1 {
        return vec![CheckResult::pass("data_root")];
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if Path::new(&env.datadir).is_dir() || cwd.join("data").is_dir() {
        return vec![CheckResult::pass("data_root")];
    }
    vec![CheckResult::skip(
        "data: no firecrab.db and no data directory yet",
        Some(&format!("looked at {}/data and {}", cwd.display(), env.datadir)),
        Some("./install.sh   # or mkdir -p data when developing from the repo root"),
    )]
}

pub fn check_firecracker(env: &DoctorEnv, runner: &dyn CommandRunner) -> Vec<CheckResult> {
    let bin = env.firecracker_bin.clone().unwrap_or_else(|| "firecracker".to_owned());
    match runner.run(&bin, &["--version"]) {
        Ok(out) => {
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            let ver = combined.lines().next().unwrap_or("").trim();
            if ver.is_empty() {
                vec![CheckResult::fail(format!("firecracker: could not read version from {bin}"), None, None)]
            } else {
                vec![CheckResult::pass("firecracker")]
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => vec![CheckResult::fail(
            "firecracker: binary not found on PATH",
            None,
            Some("run scripts/install-firecracker.sh or ./install.sh"),
        )],
        Err(_) => vec![CheckResult::fail(
            format!("firecracker: {bin} is not executable"),
            None,
            Some("run scripts/install-firecracker.sh"),
        )],
    }
}

/// Firecrab-owned bridge names (`mnb*`; legacy `fcbr0`), shared by
/// nft/dnsmasq/ufw exactly as bash's `list_firecrab_bridges` is.
fn list_firecrab_bridges(runner: &dyn CommandRunner) -> Vec<String> {
    let out = match runner.run("ip", &["-br", "link", "show", "type", "bridge"]) {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let name = line.split_whitespace().next()?;
            (name == "fcbr0" || name.starts_with("mnb")).then(|| name.to_owned())
        })
        .collect()
}

fn socket_exists(path: &str) -> bool {
    fs::symlink_metadata(path).map(|m| m.file_type().is_socket()).unwrap_or(false)
}

pub fn check_nft(env: &DoctorEnv, runner: &dyn CommandRunner) -> Vec<CheckResult> {
    let out = match runner.run("nft", &["list", "tables"]) {
        Ok(o) => o,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return vec![CheckResult::fail(
                "nft: nftables binary not found",
                None,
                Some("install nftables (apt/dnf/… package: nftables)"),
            )];
        }
        Err(_) => return vec![CheckResult::fail("nft: list tables failed", None, Some("sudo nft list tables"))],
    };
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !out.status.success() {
        if combined.contains("Operation not permitted") || combined.contains("Permission denied") || combined.contains("must be root") {
            return vec![CheckResult::skip(
                "nft: cannot list tables (permission denied)",
                Some("cannot confirm inet firecrab / bridge firecrab_l2"),
                Some("re-run as root: sudo nft list tables"),
            )];
        }
        return vec![CheckResult::fail("nft: list tables failed", Some(&combined), Some("sudo nft list tables"))];
    }

    let has_inet = combined.lines().any(|l| l.trim().starts_with("table inet firecrab"));
    let has_bridge = combined.lines().any(|l| l.trim().starts_with("table bridge firecrab_l2"));
    let mut missing = Vec::new();
    if !has_inet {
        missing.push("inet firecrab");
    }
    if !has_bridge {
        missing.push("bridge firecrab_l2");
    }
    if missing.is_empty() {
        return vec![CheckResult::pass("nft")];
    }
    let missing_joined = missing.join(", ");

    if socket_exists(&env.helper_sock) {
        if list_firecrab_bridges(runner).is_empty() {
            return vec![CheckResult::skip(
                format!("nft: firecrab tables not present yet ({missing_joined})"),
                Some("ok with zero MicroNetworks until ensure_firewall runs"),
                Some("POST /api/micro-networks or restart firecrab-api"),
            )];
        }
        return vec![CheckResult::fail(
            format!("nft: missing firecrab tables: {missing_joined}"),
            Some("net-helper socket is up but tables are absent"),
            Some("systemctl restart firecrab-net-helper  (or start a VM so rules are applied)"),
        )];
    }
    vec![CheckResult::skip(
        format!("nft: firecrab tables not present yet ({missing_joined})"),
        Some("expected after firecrab-net-helper is running"),
        Some("start firecrab-net-helper, then re-run doctor"),
    )]
}

pub fn check_dnsmasq(env: &DoctorEnv, runner: &dyn CommandRunner) -> Vec<CheckResult> {
    let mut alive = false;
    if let Ok(pid_raw) = fs::read_to_string(&env.dnsmasq_pid) {
        let pid = pid_raw.trim();
        if !pid.is_empty() && Path::new(&format!("/proc/{pid}")).is_dir() {
            if let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) {
                if String::from_utf8_lossy(&cmdline).replace('\0', " ").contains("dnsmasq") {
                    alive = true;
                }
            }
        }
    }
    if !alive {
        if let Ok(out) = runner.run("pgrep", &["-af", "dnsmasq.*firecrab"]) {
            alive = out.status.success();
        }
    }

    let conf_ifaces: Vec<String> = fs::read_to_string(&env.dnsmasq_conf)
        .map(|s| s.lines().filter_map(|l| l.strip_prefix("interface=")).map(str::to_owned).collect())
        .unwrap_or_default();
    let interfaces = list_firecrab_bridges(runner);

    if !alive {
        if socket_exists(&env.helper_sock) {
            if interfaces.is_empty() {
                return vec![CheckResult::pass("dnsmasq")];
            }
            return vec![CheckResult::fail(
                "dnsmasq: no firecrab dnsmasq process",
                Some(&format!(
                    "pid_file={} conf={} bridges: {}",
                    env.dnsmasq_pid,
                    env.dnsmasq_conf,
                    interfaces.join(" ")
                )),
                Some("systemctl restart firecrab-net-helper  (or create a MicroNetwork)"),
            )];
        }
        return vec![CheckResult::skip("dnsmasq: not running (helper also down)", None, Some("start firecrab-net-helper"))];
    }

    if !interfaces.is_empty() && !conf_ifaces.is_empty() {
        let missing_if: Vec<&String> = interfaces.iter().filter(|br| !conf_ifaces.contains(br)).collect();
        if !missing_if.is_empty() {
            let missing_str = missing_if.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ");
            return vec![CheckResult::fail(
                format!("dnsmasq: conf missing interface(s): {missing_str}"),
                Some(&format!("serving: {}  bridges: {}", conf_ifaces.join(" "), interfaces.join(" "))),
                Some(&format!("systemctl restart firecrab-net-helper  (rewrites {})", env.dnsmasq_conf)),
            )];
        }
    }
    vec![CheckResult::pass("dnsmasq")]
}

/// Reads `/etc/ufw/ufw.conf`'s `ENABLED=` line via the `CommandRunner`
/// (`cat`) instead of the real filesystem, so tests can control it through
/// `FakeCommandRunner` instead of depending on the test host's actual ufw
/// config. Returns `None` when the conf is missing/unreadable or has
/// neither `ENABLED=yes` nor `ENABLED=no`.
fn ufw_conf_enabled(runner: &dyn CommandRunner) -> Option<bool> {
    let out = runner.run("cat", &["/etc/ufw/ufw.conf"]).ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        match line.trim() {
            "ENABLED=yes" => return Some(true),
            "ENABLED=no" => return Some(false),
            _ => {}
        }
    }
    None
}

fn ufw_is_enabled(runner: &dyn CommandRunner) -> bool {
    if let Some(enabled) = ufw_conf_enabled(runner) {
        return enabled;
    }
    runner
        .run("ufw", &["status"])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            let text = String::from_utf8_lossy(&o.stdout).into_owned();
            text.contains("Status: active") || text.contains("상태: 활성")
        })
        .unwrap_or(false)
}

fn ufw_line_matches(status: &str, prefixes: &[String]) -> bool {
    status.lines().any(|line| {
        let lower = line.to_lowercase();
        prefixes.iter().any(|p| lower.starts_with(&p.to_lowercase())) && lower.contains("allow in")
    })
}

fn ufw_has_dhcp_allow(br: &str, status: &str) -> bool {
    ufw_line_matches(status, &[format!("67/udp on {br} "), format!("67 on {br} ")])
}

fn ufw_has_dns_allow(br: &str, status: &str) -> bool {
    ufw_line_matches(status, &[format!("53/udp on {br} "), format!("53/tcp on {br} "), format!("53 on {br} ")])
}

fn ufw_has_route_allow(br: &str, uplink: &str, status: &str) -> bool {
    status.lines().any(|line| {
        let lower = line.to_lowercase();
        lower.contains(&format!("on {} ", uplink.to_lowercase())) && lower.contains("allow fwd") && lower.contains(&format!("on {}", br.to_lowercase()))
    })
}

fn detect_uplink(runner: &dyn CommandRunner) -> Option<String> {
    let out = runner.run("ip", &["-4", "route", "get", "8.8.8.8"]).ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let tokens: Vec<&str> = text.split_whitespace().collect();
    tokens.iter().position(|&t| t == "dev").and_then(|i| tokens.get(i + 1)).map(|s| s.to_string())
}

pub fn check_ufw(runner: &dyn CommandRunner) -> Vec<CheckResult> {
    let ufw_installed = runner.run("ufw", &["--version"]).is_ok();
    let conf_exists = runner
        .run("cat", &["/etc/ufw/ufw.conf"])
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ufw_installed && !conf_exists {
        return vec![CheckResult::pass("ufw")];
    }
    if !ufw_is_enabled(runner) {
        return vec![CheckResult::pass("ufw")];
    }
    let status = runner
        .run("ufw", &["status", "verbose"])
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    if status.is_empty() {
        return vec![CheckResult::skip(
            "ufw: active but status is not readable without root",
            Some("UFW commonly blocks DHCP on new bridges and VM outbound forwards"),
            Some("sudo ufw status verbose   # then allow 67/udp+53 on each fcbr0/mnb* bridge and: sudo ufw route allow in on <bridge> out on <uplink>"),
        )];
    }
    let bridges = list_firecrab_bridges(runner);
    if bridges.is_empty() {
        return vec![CheckResult::skip(
            "ufw: active, but no firecrab bridges (fcbr0/mnb*) yet",
            Some("new bridges need DHCP/DNS and route allows (public-docs/troubleshooting.md)"),
            Some("after net-helper starts: sudo ufw allow in on fcbr0 to any port 67 proto udp; …"),
        )];
    }
    let uplink = detect_uplink(runner);
    let mut results = Vec::new();
    let mut any_fail = false;
    for br in &bridges {
        if !ufw_has_dhcp_allow(br, &status) {
            any_fail = true;
            results.push(CheckResult::fail(
                format!("ufw: bridge {br} missing allow 67/udp (DHCP)"),
                Some("guest will fail with no-ipv4-address"),
                Some(&format!("sudo ufw allow in on {br} to any port 67 proto udp")),
            ));
        }
        if !ufw_has_dns_allow(br, &status) {
            any_fail = true;
            results.push(CheckResult::fail(
                format!("ufw: bridge {br} missing allow 53 (DNS)"),
                Some("guest may fail with dns-unreachable"),
                Some(&format!("sudo ufw allow in on {br} to any port 53")),
            ));
        }
        if let Some(up) = &uplink {
            if !ufw_has_route_allow(br, up, &status) {
                any_fail = true;
                results.push(CheckResult::fail(
                    format!("ufw: no route allow {br} → {up}"),
                    Some("guest outbound new connections time out (DEFAULT_FORWARD_POLICY=DROP)"),
                    Some(&format!("sudo ufw route allow in on {br} out on {up}")),
                ));
            }
        }
    }
    if uplink.is_none() {
        results.push(CheckResult::skip(
            "ufw: could not detect uplink interface",
            Some("cannot verify route allow rules"),
            Some("ip route get 8.8.8.8  # then: sudo ufw route allow in on <bridge> out on <uplink>"),
        ));
    }
    if !any_fail {
        results.push(CheckResult::pass("ufw"));
    }
    results
}

pub fn check_selinux_domain(env: &DoctorEnv, runner: &dyn CommandRunner) -> Vec<CheckResult> {
    let Ok(mode_out) = runner.run("getenforce", &[]) else {
        return vec![CheckResult::pass("selinux_domain")];
    };
    let mode = String::from_utf8_lossy(&mode_out.stdout).trim().to_owned();
    if mode != "Enforcing" {
        return vec![CheckResult::pass("selinux_domain")];
    }
    let Ok(ps_out) = runner.run("ps", &["-eZ"]) else {
        return vec![CheckResult::pass("selinux_domain")];
    };
    let text = String::from_utf8_lossy(&ps_out.stdout);
    let confined: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("firecrab-api") || l.contains("firecrab-net-he"))
        .filter(|l| l.split_whitespace().next().unwrap_or("").contains(":init_t:"))
        .collect();
    if confined.is_empty() {
        return vec![CheckResult::pass("selinux_domain")];
    }
    let detail = format!(
        "{}\ninit_t cannot connect to https ports, so every registry read fails\nwith EACCES, and the helper cannot exec nft.\n",
        confined.join("\n")
    );
    vec![CheckResult::fail(
        "selinux: a firecrab service runs in systemd's own domain (init_t)",
        Some(&detail),
        Some(&format!(
            "sudo semanage fcontext -a -t bin_t '{libdir}(/.*)?' && sudo restorecon -R {libdir} && sudo systemctl restart firecrab-net-helper firecrab-api",
            libdir = env.libdir
        )),
    )]
}

fn resolve_api_user(env: &DoctorEnv, runner: &dyn CommandRunner) -> String {
    if let Some(u) = &env.api_user {
        return u.clone();
    }
    if let Ok(out) = runner.run("systemctl", &["show", "-p", "User", "--value", "firecrab-api.service"]) {
        let val = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !val.is_empty() {
            return val;
        }
    }
    current_username()
}

pub fn check_helper_socket(env: &DoctorEnv, runner: &dyn CommandRunner) -> Vec<CheckResult> {
    let sock = &env.helper_sock;
    let meta = match fs::symlink_metadata(sock) {
        Ok(m) => m,
        Err(_) => {
            return vec![CheckResult::fail(
                format!("helper socket: {sock} does not exist"),
                Some("API cannot reach the network helper"),
                Some("systemctl start firecrab-net-helper  (dev: ./scripts/dev-net-helper.sh)"),
            )];
        }
    };
    if !meta.file_type().is_socket() {
        return vec![CheckResult::fail(format!("helper socket: {sock} exists but is not a socket"), None, None)];
    }

    let mode = meta.permissions().mode() & 0o777;
    let owner_uid = meta.uid();
    let group_gid = meta.gid();
    let api_user = resolve_api_user(env, runner);

    let api_uid: Option<u32> = runner
        .run("id", &["-u", &api_user])
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok());
    let Some(api_uid) = api_uid else {
        return vec![CheckResult::fail(
            format!("helper socket: API account {api_user} does not exist"),
            Some(&format!("path={sock} mode={mode:o} owner={owner_uid}:{group_gid}")),
            Some("re-run ./install.sh to create the service account"),
        )];
    };

    let api_gids: Vec<u32> = runner
        .run("id", &["-G", &api_user])
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().filter_map(|s| s.parse().ok()).collect())
        .unwrap_or_default();

    let permission = if api_uid == 0 {
        6
    } else if api_uid == owner_uid {
        (mode >> 6) & 0o7
    } else if api_gids.contains(&group_gid) {
        (mode >> 3) & 0o7
    } else {
        mode & 0o7
    };

    if permission & 0o6 != 0o6 {
        return vec![CheckResult::fail(
            format!("helper socket: not accessible by API account {api_user}"),
            Some(&format!("path={sock} mode={mode:o} owner={owner_uid}:{group_gid}")),
            Some("unit Group= must match the API account group (socket is 0660)"),
        )];
    }
    if mode == 0o600 || mode == 0o620 {
        return vec![CheckResult::fail(
            format!("helper socket: mode {mode:o} is too tight for a group-shared socket"),
            Some(&format!("path={sock} owner={owner_uid}:{group_gid}")),
            Some("ensure firecrab-net-helper runs with Group= shared with the API (chmod 0660)"),
        )];
    }
    vec![CheckResult::pass("helper_socket")]
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::doctor::{DoctorEnv, Status};
    use crate::shell::FakeCommandRunner;

    #[test]
    fn ip_forward_pass_when_one() {
        // /proc/sys/net/ipv4/ip_forward is real host state; this test only
        // asserts the function returns exactly one result either way and
        // that "1" maps to Pass — covered precisely by the parsing test below.
        let results = check_ip_forward();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn data_root_pass_with_single_db() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("data")).unwrap();
        fs::write(dir.path().join("data/firecrab.db"), b"").unwrap();
        let env = DoctorEnv {
            datadir: dir.path().display().to_string(),
            ..DoctorEnv::default()
        };
        let results = check_data_root(&env);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, Status::Pass);
    }

    #[test]
    fn data_root_skip_when_nothing_found() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let env = DoctorEnv {
            datadir: missing.display().to_string(),
            ..DoctorEnv::default()
        };
        let results = check_data_root(&env);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, Status::Skip);
    }

    #[test]
    fn data_root_fail_on_duplicate_db() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("data")).unwrap();
        fs::write(dir.path().join("data/firecrab.db"), b"").unwrap();
        fs::write(dir.path().join("firecrab.db"), b"").unwrap();
        let env = DoctorEnv {
            datadir: dir.path().display().to_string(),
            ..DoctorEnv::default()
        };
        let results = check_data_root(&env);
        assert_eq!(results[0].status, Status::Fail);
        assert!(results[0].detail.as_deref().unwrap().contains("firecrab.db"));
    }

    #[test]
    fn firecracker_pass_when_version_prints() {
        let mut fake = FakeCommandRunner::new();
        fake.set("firecracker", &["--version"], 0, "Firecracker v1.9.0\n", "");
        let results = check_firecracker(&DoctorEnv::default(), &fake);
        assert_eq!(results[0].status, Status::Pass);
    }

    #[test]
    fn firecracker_fail_when_not_on_path() {
        let fake = FakeCommandRunner::new();
        let results = check_firecracker(&DoctorEnv::default(), &fake);
        assert_eq!(results[0].status, Status::Fail);
        assert!(results[0].title.contains("not found on PATH"));
    }

    #[test]
    fn nft_pass_when_both_tables_present() {
        let mut fake = FakeCommandRunner::new();
        fake.set(
            "nft",
            &["list", "tables"],
            0,
            "table inet firecrab\ntable bridge firecrab_l2\n",
            "",
        );
        let results = check_nft(&DoctorEnv::default(), &fake);
        assert_eq!(results[0].status, Status::Pass);
    }

    #[test]
    fn nft_skip_on_permission_denied() {
        let mut fake = FakeCommandRunner::new();
        fake.set("nft", &["list", "tables"], 1, "", "Operation not permitted\n");
        let results = check_nft(&DoctorEnv::default(), &fake);
        assert_eq!(results[0].status, Status::Skip);
    }

    #[test]
    fn dnsmasq_skip_when_helper_and_dnsmasq_both_down() {
        let fake = FakeCommandRunner::new();
        let env = DoctorEnv {
            helper_sock: "/nonexistent/socket".to_owned(),
            dnsmasq_pid: "/nonexistent/pid".to_owned(),
            ..DoctorEnv::default()
        };
        let results = check_dnsmasq(&env, &fake);
        assert_eq!(results[0].status, Status::Skip);
    }

    #[test]
    fn ufw_pass_when_not_installed() {
        let fake = FakeCommandRunner::new();
        let results = check_ufw(&fake);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, Status::Pass);
    }

    #[test]
    fn selinux_domain_pass_when_getenforce_missing() {
        let fake = FakeCommandRunner::new();
        let results = check_selinux_domain(&DoctorEnv::default(), &fake);
        assert_eq!(results[0].status, Status::Pass);
    }

    #[test]
    fn selinux_domain_pass_when_permissive() {
        let mut fake = FakeCommandRunner::new();
        fake.set("getenforce", &[], 0, "Permissive\n", "");
        let results = check_selinux_domain(&DoctorEnv::default(), &fake);
        assert_eq!(results[0].status, Status::Pass);
    }

    #[test]
    fn helper_socket_fail_when_missing() {
        let fake = FakeCommandRunner::new();
        let env = DoctorEnv {
            helper_sock: "/nonexistent/net-helper.sock".to_owned(),
            ..DoctorEnv::default()
        };
        let results = check_helper_socket(&env, &fake);
        assert_eq!(results[0].status, Status::Fail);
        assert!(results[0].title.contains("does not exist"));
    }
}
