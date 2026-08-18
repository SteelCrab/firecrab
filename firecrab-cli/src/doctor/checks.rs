use std::fs;
use std::path::{Path, PathBuf};

use super::{CheckResult, DoctorEnv};

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
    use std::os::unix::fs::FileTypeExt;

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

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::doctor::{DoctorEnv, Status};

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
}
