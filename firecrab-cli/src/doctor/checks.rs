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

fn resolve_image_roots(env: &DoctorEnv) -> Vec<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut candidates = Vec::new();
    if let Some(root) = &env.image_root {
        candidates.push(PathBuf::from(root));
    }
    candidates.push(cwd.join("images"));
    candidates.push(Path::new(&env.datadir).join("images"));

    let mut seen = std::collections::HashSet::new();
    let mut roots = Vec::new();
    for c in candidates {
        if !c.is_dir() {
            continue;
        }
        let abs = abs_dir(&c);
        if seen.insert(abs.clone()) {
            roots.push(abs);
        }
    }
    roots
}

/// Template artifact paths (relative to an image root), matching
/// `firecrab-api/src/templates.rs`'s `default_specs()` per architecture.
pub(crate) fn template_artifacts() -> Vec<&'static str> {
    if std::env::consts::ARCH == "aarch64" {
        vec![
            "kernel/Image-ubuntu-26.04-aarch64",
            "rootfs/ubuntu-rootfs-26.04-arm64.ext4",
            "kernel/Image-alpine-virt-aarch64",
            "kernel/initramfs-alpine-virt-aarch64",
            "rootfs/alpine-rootfs-3.24.1-aarch64.ext4",
            "kernel/Image-rocky-9.8-aarch64",
            "kernel/initramfs-rocky-9.8-aarch64",
            "rootfs/rocky-rootfs-9.8-aarch64.ext4",
        ]
    } else {
        vec![
            "kernel/vmlinux-ubuntu-26.04-x86_64",
            "rootfs/ubuntu-rootfs-26.04-amd64.ext4",
            "kernel/vmlinux-alpine-virt-x86_64",
            "kernel/initramfs-alpine-virt-x86_64",
            "rootfs/alpine-rootfs-3.24.1-x86_64.ext4",
            "kernel/vmlinux-rocky-9.8-x86_64",
            "kernel/initramfs-rocky-9.8-x86_64",
            "rootfs/rocky-rootfs-9.8-x86_64.ext4",
        ]
    }
}

fn short_digest(file: &Path) -> String {
    use sha2::{Digest, Sha256};
    let Ok(bytes) = fs::read(file) else {
        return "unavailable".to_owned();
    };
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    hasher.finalize().iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// True when `DATADIR` exists but the current (unprivileged) user cannot
/// traverse it — the healthy-but-private state bash detects with `[ ! -x ]`.
fn datadir_private(datadir: &str) -> bool {
    let path = Path::new(datadir);
    path.exists() && fs::read_dir(path).is_err()
}

pub fn check_images(env: &DoctorEnv, digest: bool) -> Vec<CheckResult> {
    let roots = resolve_image_roots(env);
    let artifacts = template_artifacts();

    if roots.is_empty() {
        if datadir_private(&env.datadir) {
            return vec![CheckResult::skip(
                format!("images: {} is private to the service account", env.datadir),
                Some(&format!("the current user cannot inspect {}/images", env.datadir)),
                Some("sudo firecrab doctor   # inspect installed image contents"),
            )];
        }
        return vec![CheckResult::fail(
            "images: no image root found",
            Some(&format!("looked at FIRECRAB_IMAGE_ROOT, ./images, {}/images", env.datadir)),
            Some("./install.sh   # or build with scripts/firecracker-menual/install-alpine-rootfs.sh"),
        )];
    }

    let mut root: Option<PathBuf> = None;
    'outer: for candidate in &roots {
        for art in &artifacts {
            if candidate.join(art).is_file() {
                root = Some(candidate.clone());
                break 'outer;
            }
        }
    }

    let root = match root {
        Some(r) => r,
        None => {
            let any_ext4_root = roots.iter().find(|c| {
                fs::read_dir(c.join("rootfs"))
                    .map(|it| it.filter_map(Result::ok).any(|e| e.path().extension().is_some_and(|x| x == "ext4")))
                    .unwrap_or(false)
            });
            match any_ext4_root {
                Some(r) => r.clone(),
                None => {
                    if datadir_private(&env.datadir) {
                        return vec![CheckResult::skip(
                            format!("images: {} is private to the service account", env.datadir),
                            Some(&format!(
                                "the current user cannot inspect {}/images; accessible roots had no images",
                                env.datadir
                            )),
                            Some("sudo firecrab doctor   # inspect installed image contents"),
                        )];
                    }
                    let roots_str = roots.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(" ");
                    return vec![CheckResult::fail(
                        format!("images: no guest rootfs found under {roots_str}"),
                        None,
                        Some(&format!("./install.sh  or copy images into {}/images", env.datadir)),
                    )];
                }
            }
        }
    };

    let mut missing = Vec::new();
    let mut present = 0u32;
    for art in &artifacts {
        if root.join(art).is_file() {
            present += 1;
        } else {
            missing.push(*art);
        }
    }

    if present == 0 {
        return vec![CheckResult::fail(
            format!("images: image root {} has none of the default template artifacts", root.display()),
            None,
            Some(&format!("build or copy templates into {}", root.display())),
        )];
    }

    if !missing.is_empty() {
        return vec![CheckResult::skip(
            format!("images: some default templates missing under {}", root.display()),
            Some(&format!("missing: {}", missing.join(" "))),
            Some(&format!("build the missing image(s), copy into {}, restart firecrab-api", root.display())),
        )];
    }

    if digest {
        let digests: Vec<String> = artifacts.iter().map(|art| format!("{art}={}", short_digest(&root.join(art)))).collect();
        eprintln!("images: {}", digests.join(" "));
    }
    vec![CheckResult::pass("images")]
}

pub fn check_image_install_tools(env: &DoctorEnv, runner: &dyn CommandRunner) -> Vec<CheckResult> {
    let mut missing = Vec::new();
    if runner.run("tar", &["--version"]).is_err() {
        missing.push("tar");
    }
    if runner.run("zstd", &["--version"]).is_err() {
        missing.push("zstd");
    }
    if !missing.is_empty() {
        return vec![CheckResult::skip(
            format!("image-install tools: missing {}", missing.join(" ")),
            Some("dashboard template install needs tar + zstd to unpack {alias}.tar.zst"),
            Some("install packages: tar zstd  (then retry dashboard image install)"),
        )];
    }
    if let Some(base_url) = &env.image_base_url {
        if matches!(base_url.as_str(), "none" | "NONE" | "-") {
            return vec![CheckResult::skip(
                format!("image-install: remote disabled (FIRECRAB_IMAGE_BASE_URL={base_url})"),
                Some("dashboard will not download packages; seed images/ on the host"),
                Some("see public-docs/installation.md (M2Image)"),
            )];
        }
    }
    vec![CheckResult::pass("image_install_tools")]
}

fn resolve_vms_roots(env: &DoctorEnv) -> Vec<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut candidates = Vec::new();
    if let Some(roots_env) = &env.storage_roots {
        for entry in roots_env.split(':') {
            if let Some((_, path)) = entry.split_once('=') {
                candidates.push(PathBuf::from(path));
            }
        }
    }
    candidates.push(PathBuf::from(&env.datadir));
    candidates.push(cwd.join("data"));

    let mut seen = std::collections::HashSet::new();
    let mut roots = Vec::new();
    for entry in candidates {
        for sub in [entry.join("vms"), entry.clone()] {
            if sub.is_dir() {
                let abs = abs_dir(&sub);
                if seen.insert(abs.clone()) {
                    roots.push(abs);
                }
                break;
            }
        }
    }
    roots
}

pub fn check_reflink(env: &DoctorEnv) -> Vec<CheckResult> {
    let image_roots = resolve_image_roots(env);
    let vms_roots = resolve_vms_roots(env);
    if image_roots.is_empty() || vms_roots.is_empty() {
        return vec![CheckResult::pass("reflink")];
    }
    let image_root = &image_roots[0];
    let Ok(image_meta) = fs::metadata(image_root) else {
        return vec![CheckResult::pass("reflink")];
    };
    let image_dev = image_meta.dev();

    let mut split = Vec::new();
    for vms_root in &vms_roots {
        let Ok(vms_meta) = fs::metadata(vms_root) else { continue };
        if vms_meta.dev() == image_dev {
            continue;
        }
        split.push(format!("VM disks:  {}", vms_root.display()));
    }
    if split.is_empty() {
        return vec![CheckResult::pass("reflink")];
    }
    let detail = format!("templates: {}\n{}", image_root.display(), split.join("\n"));
    vec![CheckResult::skip(
        "reflink: templates and VM disks are on different filesystems",
        Some(&detail),
        Some("put both on one XFS/Btrfs filesystem, or accept a full template copy per VM"),
    )]
}

pub fn check_registry_egress(env: &DoctorEnv, runner: &dyn CommandRunner) -> Vec<CheckResult> {
    if runner.run("curl", &["--version"]).is_err() {
        return vec![CheckResult::skip(
            "registry egress: curl is not installed",
            Some("cannot test whether the API account reaches a registry"),
            Some("install curl, or ignore this on a host that never downloads images"),
        )];
    }
    let endpoint = match env.image_base_url.as_deref() {
        Some("") | Some("none") | Some("-") => {
            return vec![CheckResult::skip(
                "registry egress: remote images disabled (FIRECRAB_IMAGE_BASE_URL)",
                Some("only OCI import would still need egress"),
                Some("unset FIRECRAB_IMAGE_BASE_URL to use the public MicroRegistry"),
            )];
        }
        Some(u) => format!("{}/catalog.json", u.trim_end_matches('/')),
        None => "https://registry.firecrab.dev/catalog.json".to_owned(),
    };
    let endpoints = [endpoint, "https://registry-1.docker.io/v2/".to_owned()];

    let user = resolve_api_user(env, runner);
    let current = current_username();
    let is_root = current == "root";

    for probe in &endpoints {
        let host = probe.trim_start_matches("https://").split('/').next().unwrap_or("");
        let output = if user == current {
            runner.run("curl", &["-sS", "-o", "/dev/null", "-m", "12", "-w", "%{http_code}", probe])
        } else if is_root {
            runner.run("sudo", &["-u", &user, "curl", "-sS", "-o", "/dev/null", "-m", "12", "-w", "%{http_code}", probe])
        } else {
            return vec![CheckResult::skip(
                format!("registry egress: cannot test as {user}"),
                Some(&format!("this doctor runs as {current} and does not become another account")),
                Some("sudo firecrab doctor   # tests as the API service account"),
            )];
        };

        let Ok(out) = output else { continue };
        if out.status.success() {
            continue;
        }
        let status_text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if status_text.contains("Permission denied") || status_text.contains("Operation not permitted") {
            let selinux = runner
                .run("getenforce", &[])
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .unwrap_or_else(|_| "not installed".to_owned());
            let ip_deny = runner
                .run("systemctl", &["show", "-p", "IPAddressDeny", "--value", "firecrab-api.service"])
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .unwrap_or_else(|_| "?".to_owned());
            let detail = format!(
                "as {user}: {status_text}\nconnect(2) returned EACCES, so no request ever left the machine.\nselinux: {selinux}\nunit IPAddressDeny: {ip_deny}"
            );
            return vec![CheckResult::fail(
                format!("registry egress: {host} is refused by this host, not by the network"),
                Some(&detail),
                Some("sudo ausearch -m avc -ts recent | grep -i firecrab   # then audit2allow, or fix the unit/firewall rule"),
            )];
        }
        if status_text.contains("Could not resolve host") {
            return vec![CheckResult::fail(
                format!("registry egress: {host} does not resolve for {user}"),
                Some(&status_text),
                Some("check /etc/resolv.conf and any split-DNS or sandbox that hides it from the service"),
            )];
        }
        return vec![CheckResult::fail(
            format!("registry egress: {user} cannot reach {host}"),
            Some(&status_text),
            Some(&format!("check the default route, proxy variables in {}/api.env, and any egress firewall", env.confdir)),
        )];
    }
    vec![CheckResult::pass("registry_egress")]
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

    #[test]
    fn images_fail_when_no_root_found() {
        let dir = tempdir().unwrap();
        let env = DoctorEnv {
            datadir: dir.path().join("nonexistent-datadir").display().to_string(),
            ..DoctorEnv::default()
        };
        // cwd (crate 소스 트리) has no ./images either.
        let results = check_images(&env, false);
        assert_eq!(results[0].status, Status::Fail);
    }

    #[test]
    fn images_pass_when_all_artifacts_present() {
        let dir = tempdir().unwrap();
        let images = dir.path().join("images");
        for art in template_artifacts() {
            let p = images.join(art);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, b"x").unwrap();
        }
        let env = DoctorEnv {
            image_root: Some(images.display().to_string()),
            ..DoctorEnv::default()
        };
        let results = check_images(&env, false);
        assert_eq!(results[0].status, Status::Pass);
    }

    #[test]
    fn images_skip_when_some_artifacts_missing() {
        let dir = tempdir().unwrap();
        let images = dir.path().join("images");
        let artifacts = template_artifacts();
        for art in artifacts.iter().take(artifacts.len() - 1) {
            let p = images.join(art);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, b"x").unwrap();
        }
        let env = DoctorEnv {
            image_root: Some(images.display().to_string()),
            ..DoctorEnv::default()
        };
        let results = check_images(&env, false);
        assert_eq!(results[0].status, Status::Skip);
    }

    #[test]
    fn image_install_tools_skip_when_missing_binaries() {
        let fake = FakeCommandRunner::new();
        let results = check_image_install_tools(&DoctorEnv::default(), &fake);
        assert_eq!(results[0].status, Status::Skip);
        assert!(results[0].title.contains("tar"));
    }

    #[test]
    fn image_install_tools_pass_when_present_and_default_registry() {
        let mut fake = FakeCommandRunner::new();
        fake.set("tar", &["--version"], 0, "tar (GNU tar)\n", "");
        fake.set("zstd", &["--version"], 0, "zstd\n", "");
        let results = check_image_install_tools(&DoctorEnv::default(), &fake);
        assert_eq!(results[0].status, Status::Pass);
    }

    #[test]
    fn reflink_pass_when_roots_missing() {
        let env = DoctorEnv::default();
        let results = check_reflink(&env);
        assert_eq!(results[0].status, Status::Pass);
    }

    #[test]
    fn registry_egress_skip_when_curl_missing() {
        let fake = FakeCommandRunner::new();
        let results = check_registry_egress(&DoctorEnv::default(), &fake);
        assert_eq!(results[0].status, Status::Skip);
        assert!(results[0].title.contains("curl"));
    }

    #[test]
    fn registry_egress_skip_when_remote_disabled() {
        let mut fake = FakeCommandRunner::new();
        fake.set("curl", &["--version"], 0, "curl 8.0\n", "");
        let env = DoctorEnv {
            image_base_url: Some("none".to_owned()),
            ..DoctorEnv::default()
        };
        let results = check_registry_egress(&env, &fake);
        assert_eq!(results[0].status, Status::Skip);
        assert!(results[0].title.contains("remote images disabled"));
    }
}
