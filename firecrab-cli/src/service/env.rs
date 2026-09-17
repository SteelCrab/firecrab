//! install.sh 상단 변수 블록의 Rust 대응: 계정과 설치 경로.

use std::path::{Path, PathBuf};

/// 시작 순서. 정지는 역순.
pub const UNITS: [&str; 2] = ["firecrab-net-helper.service", "firecrab-api.service"];

/// 설치 계정과 경로 집합. `firecrab update`의 `resolve_layout`과 같은 규칙으로
/// `PREFIX`/`FIRECRAB_LIBDIR`를 해석해 helper의 `host_layout`과 어긋나지 않는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEnv {
    /// 서비스 계정 이름 (`$FIRECRAB_USER`, 기본 `firecrab`).
    pub user: String,
    /// 서비스 그룹 이름 (`$FIRECRAB_GROUP`, 기본 `firecrab`).
    pub group: String,
    /// 설치 접두 경로 (`$PREFIX`, 기본 `/usr/local`).
    pub prefix: PathBuf,
    /// 실행 파일 디렉터리 (`$PREFIX/bin`).
    pub bindir: PathBuf,
    /// 라이브러리 디렉터리 (`$FIRECRAB_LIBDIR`, 기본 `$PREFIX/lib/firecrab`).
    pub libdir: PathBuf,
    /// 공유 데이터 디렉터리 (`$PREFIX/share/firecrab`).
    pub sharedir: PathBuf,
    /// VM 디스크·DB 등 상태 디렉터리 (`$DATADIR`, 기본 `/var/lib/firecrab`).
    pub datadir: PathBuf,
    /// 설정 디렉터리 (`$CONFDIR`, 기본 `/etc/firecrab`).
    pub confdir: PathBuf,
    /// systemd 유닛 디렉터리 (`$UNITDIR`, 기본 `/etc/systemd/system`).
    pub unitdir: PathBuf,
}

fn env_or(var: &str, default: &str) -> String {
    std::env::var(var)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

impl ServiceEnv {
    /// install.sh와 같은 이름·기본값으로 프로세스 환경을 읽는다.
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

    /// 테스트·임시 디렉터리용: prefix 아래 표준 배치.
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
