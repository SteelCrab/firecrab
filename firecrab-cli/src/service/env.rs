//! Rust equivalent of install.sh's initial variable block: accounts and installation paths.

use std::path::{Path, PathBuf};

/// Startup order. Stop in reverse order.
pub const UNITS: [&str; 2] = ["firecrab-helper.service", "firecrab-api.service"];
/// Previous installations keep working until the installer migrates their unit.
pub const LEGACY_HELPER_UNIT: &str = "firecrab-net-helper.service";

/// Installation accounts and paths. Resolve `PREFIX`/`FIRECRAB_LIBDIR` using
/// the same rules as `firecrab update`'s `resolve_layout` to match the helper's `host_layout`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEnv {
    /// Service account name (`$FIRECRAB_USER`, default `firecrab`).
    pub user: String,
    /// Service group name (`$FIRECRAB_GROUP`, default `firecrab`).
    pub group: String,
    /// Installation prefix (`$PREFIX`, default `/usr/local`).
    pub prefix: PathBuf,
    /// Executable directory (`$PREFIX/bin`).
    pub bindir: PathBuf,
    /// Library directory (`$FIRECRAB_LIBDIR`, default `$PREFIX/lib/firecrab`).
    pub libdir: PathBuf,
    /// Shared data directory (`$PREFIX/share/firecrab`).
    pub sharedir: PathBuf,
    /// State directory for VM disks, the database, etc. (`$DATADIR`, default `/var/lib/firecrab`).
    pub datadir: PathBuf,
    /// Configuration directory (`$CONFDIR`, default `/etc/firecrab`).
    pub confdir: PathBuf,
    /// systemd unit directory (`$UNITDIR`, default `/etc/systemd/system`).
    pub unitdir: PathBuf,
}

fn env_or(var: &str, default: &str) -> String {
    std::env::var(var)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

impl ServiceEnv {
    /// Read the process environment using the same names and defaults as install.sh.
    pub fn from_process_env() -> Self {
        let layout = crate::update::resolve_layout();
        let prefix = PathBuf::from(env_or("PREFIX", "/usr/local"));
        Self {
            user: env_or("FIRECRAB_USER", "firecrab"),
            group: env_or("FIRECRAB_GROUP", "firecrab"),
            bindir: layout.bindir,
            libdir: layout.libdir,
            sharedir: layout.sharedir,
            prefix,
            datadir: crate::update::datadir(),
            confdir: PathBuf::from(env_or("CONFDIR", "/etc/firecrab")),
            unitdir: PathBuf::from(env_or("UNITDIR", "/etc/systemd/system")),
        }
    }

    /// Standard layout under the prefix for tests and temporary directories.
    pub fn from_values(
        user: &str,
        group: &str,
        prefix: &Path,
        datadir: &Path,
        confdir: &Path,
        unitdir: &Path,
    ) -> Self {
        Self {
            user: user.to_owned(),
            group: group.to_owned(),
            prefix: prefix.to_path_buf(),
            bindir: prefix.join("bin"),
            libdir: prefix.join("lib/firecrab"),
            sharedir: prefix.join("share/firecrab"),
            datadir: datadir.to_path_buf(),
            confdir: confdir.to_path_buf(),
            unitdir: unitdir.to_path_buf(),
        }
    }

    /// `$UNITDIR/<unit>`.
    pub fn unit_path(&self, unit: &str) -> PathBuf {
        self.unitdir.join(unit)
    }

    /// `FIRECRAB_BIND_ADDR`, else the API's default bind.
    pub fn api_bind() -> String {
        env_or("FIRECRAB_BIND_ADDR", "127.0.0.1:5523")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::ENV_LOCK;

    fn clear() {
        for var in [
            "FIRECRAB_USER",
            "FIRECRAB_GROUP",
            "PREFIX",
            "FIRECRAB_LIBDIR",
            "DATADIR",
            "CONFDIR",
            "UNITDIR",
        ] {
            // SAFETY: serialized by ENV_LOCK against every other env-touching test.
            unsafe { std::env::remove_var(var) };
        }
    }

    #[test]
    fn defaults_match_install_sh() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear();
        let env = ServiceEnv::from_process_env();
        assert_eq!(env.user, "firecrab");
        assert_eq!(env.group, "firecrab");
        assert_eq!(env.prefix, Path::new("/usr/local"));
        assert_eq!(env.bindir, Path::new("/usr/local/bin"));
        assert_eq!(env.libdir, Path::new("/usr/local/lib/firecrab"));
        assert_eq!(env.sharedir, Path::new("/usr/local/share/firecrab"));
        assert_eq!(env.datadir, Path::new("/var/lib/firecrab"));
        assert_eq!(env.confdir, Path::new("/etc/firecrab"));
        assert_eq!(env.unitdir, Path::new("/etc/systemd/system"));
        assert_eq!(
            env.unit_path("firecrab-api.service"),
            Path::new("/etc/systemd/system/firecrab-api.service")
        );
    }

    #[test]
    fn prefix_and_libdir_overrides_follow_update() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear();
        // SAFETY: serialized by ENV_LOCK against every other env-touching test.
        unsafe {
            std::env::set_var("PREFIX", "/opt/fc");
            std::env::set_var("FIRECRAB_LIBDIR", "/opt/lib/fc");
            std::env::set_var("FIRECRAB_USER", "fcuser");
        }
        let env = ServiceEnv::from_process_env();
        clear();
        assert_eq!(env.bindir, Path::new("/opt/fc/bin"));
        assert_eq!(env.sharedir, Path::new("/opt/fc/share/firecrab"));
        assert_eq!(env.libdir, Path::new("/opt/lib/fc"));
        assert_eq!(env.user, "fcuser");
        assert_eq!(env.group, "firecrab");
    }

    #[test]
    fn api_bind_defaults_to_loopback() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by ENV_LOCK against every other env-touching test.
        unsafe { std::env::remove_var("FIRECRAB_BIND_ADDR") };
        assert_eq!(ServiceEnv::api_bind(), "127.0.0.1:5523");
        // SAFETY: serialized by ENV_LOCK — see the note above.
        unsafe { std::env::set_var("FIRECRAB_BIND_ADDR", "0.0.0.0:8080") };
        assert_eq!(ServiceEnv::api_bind(), "0.0.0.0:8080");
        // SAFETY: serialized by ENV_LOCK — see the note above.
        unsafe { std::env::remove_var("FIRECRAB_BIND_ADDR") };
    }
}
