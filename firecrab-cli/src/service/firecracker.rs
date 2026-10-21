//! install.sh `ensure_firecracker`: firecracker는 배포판 패키지가 아니라
//! 업스트림 스크립트로 설치한다. 체크아웃이 없으면 같은 스크립트를 내려받는다.

use std::path::{Path, PathBuf};

use super::Error;
use super::env::ServiceEnv;
use super::output;
use super::pkg::have;
use super::privileged::Privileged;
use crate::update::bundle;

const STEP: &str = "firecracker";

/// install.sh 스크립트 상대 경로.
const SCRIPT_PATH: &str = "scripts/install-firecracker.sh";

/// `firecrab_repo_raw_url`: 태그의 raw GitHub 파일. `latest`(=None)는 `main`.
pub fn repo_raw_url(repo: &str, version: Option<&str>, path: &str) -> String {
    let tag = match version {
        None => "main".to_owned(),
        Some(v) if v.is_empty() || v == "latest" => "main".to_owned(),
        Some(v) if v.starts_with('v') => v.to_owned(),
        Some(v) => format!("v{v}"),
    };
    format!("https://raw.githubusercontent.com/{repo}/{tag}/{path}")
}

/// firecracker가 없으면 업스트림 스크립트로 설치한다.
///
/// `checkout`은 `--bin-dir` 설치에서 넘어오는 저장소 루트. 없으면 릴리스
/// 태그(또는 main)의 raw 스크립트를 임시 파일로 내려받아 실행한다.
pub fn ensure_firecracker(
    privileged: &Privileged<'_>,
    env: &ServiceEnv,
    checkout: Option<&Path>,
    version: Option<&str>,
    install_deps: bool,
    check_only: bool,
) -> Result<(), Error> {
    if have(privileged.runner(), "firecracker") {
        output::log("firecracker present");
        return Ok(());
    }
    if check_only {
        output::warn("missing: firecracker (would install the upstream Firecracker binary)");
        return Err(Error::step(STEP, "firecracker is required"));
    }
    if !install_deps {
        return Err(Error::step_fix(
            STEP,
            "firecracker is missing (--no-deps)",
            "install it, or re-run without --no-deps",
        ));
    }

    output::step("installing firecracker");
    let notice_dir = env.sharedir.join("firecracker");
    let notice_dir = notice_dir.to_string_lossy().into_owned();

    let checkout_script = checkout.map(|root| root.join(SCRIPT_PATH));
    let (script, temp): (PathBuf, Option<tempfile::TempDir>) = match checkout_script {
        Some(path) if path.is_file() => (path, None),
        _ => {
            let dir = tempfile::tempdir().map_err(|e| {
                Error::step(STEP, format!("could not create a temporary directory: {e}"))
            })?;
            let dest = dir.path().join("install-firecracker.sh");
            let url = repo_raw_url(&bundle::release_repo(), version, SCRIPT_PATH);
            bundle::download_to(&url, &dest)?;
            (dest, Some(dir))
        }
    };

    let script_s = script.to_string_lossy().into_owned();
    let result = privileged.run_env(
        STEP,
        &[("FIRECRACKER_NOTICE_DIR", &notice_dir)],
        "bash",
        &[&script_s],
    );
    drop(temp);
    result?;

    if have(privileged.runner(), "firecracker") {
        Ok(())
    } else {
        Err(Error::step(
            STEP,
            "firecracker install ran but the binary is still not on PATH",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::FakeCommandRunner;
    use std::path::Path;

    fn env_for(prefix: &Path) -> ServiceEnv {
        ServiceEnv::from_values(
            "firecrab",
            "firecrab",
            prefix,
            Path::new("/var/lib/firecrab"),
            Path::new("/etc/firecrab"),
            Path::new("/etc/systemd/system"),
        )
    }

    #[test]
    fn raw_url_maps_latest_to_main_and_normalizes_tags() {
        assert_eq!(
            repo_raw_url("SteelCrab/firecrab", None, "scripts/install-firecracker.sh"),
            "https://raw.githubusercontent.com/SteelCrab/firecrab/main/scripts/install-firecracker.sh"
        );
        assert_eq!(
            repo_raw_url(
                "SteelCrab/firecrab",
                Some("0.3.0"),
                "scripts/install-firecracker.sh"
            ),
            "https://raw.githubusercontent.com/SteelCrab/firecrab/v0.3.0/scripts/install-firecracker.sh"
        );
        assert_eq!(
            repo_raw_url("SteelCrab/firecrab", Some("v0.3.0"), "x"),
            "https://raw.githubusercontent.com/SteelCrab/firecrab/v0.3.0/x"
        );
    }

    #[test]
    fn present_firecracker_is_left_alone() {
        let mut fake = FakeCommandRunner::permissive();
        fake.set(
            "sh",
            &["-c", "command -v firecracker"],
            0,
            "/usr/bin/firecracker\n",
            "",
        );
        let privileged = Privileged::with_sudo(&fake, true);
        ensure_firecracker(
            &privileged,
            &env_for(Path::new("/usr/local")),
            None,
            None,
            true,
            false,
        )
        .unwrap();
        assert!(
            !fake
                .calls()
                .iter()
                .any(|c| c.contains("install-firecracker")),
            "{:?}",
            fake.calls()
        );
    }

    #[test]
    fn checkout_script_is_run_with_the_notice_dir() {
        let dir = tempfile::tempdir().unwrap();
        let scripts = dir.path().join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(scripts.join("install-firecracker.sh"), "#!/bin/sh\n").unwrap();

        let mut fake = FakeCommandRunner::permissive();
        fake.set("sh", &["-c", "command -v firecracker"], 1, "", "");
        let privileged = Privileged::with_sudo(&fake, true);
        let env = env_for(Path::new("/usr/local"));
        // The second `have` probe (after the install) must succeed or the call fails.
        let result = ensure_firecracker(&privileged, &env, Some(dir.path()), None, true, false);
        assert!(
            result.is_err(),
            "the probe after install still reports it missing"
        );
        let script = scripts.join("install-firecracker.sh");
        assert!(
            fake.calls().iter().any(|c| c
                == &format!(
                    "sudo env FIRECRACKER_NOTICE_DIR=/usr/local/share/firecrab/firecracker bash {}",
                    script.display()
                )),
            "{:?}",
            fake.calls()
        );
    }

    #[test]
    fn check_only_reports_without_installing() {
        let mut fake = FakeCommandRunner::permissive();
        fake.set("sh", &["-c", "command -v firecracker"], 1, "", "");
        let privileged = Privileged::with_sudo(&fake, true);
        let err = ensure_firecracker(
            &privileged,
            &env_for(Path::new("/usr/local")),
            None,
            None,
            true,
            true,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            Error::Step {
                step: "firecracker",
                ..
            }
        ));
        assert!(
            !fake.calls().iter().any(|c| c.contains("bash")),
            "{:?}",
            fake.calls()
        );
    }

    #[test]
    fn no_deps_refuses_to_install() {
        let mut fake = FakeCommandRunner::permissive();
        fake.set("sh", &["-c", "command -v firecracker"], 1, "", "");
        let privileged = Privileged::with_sudo(&fake, true);
        let err = ensure_firecracker(
            &privileged,
            &env_for(Path::new("/usr/local")),
            None,
            None,
            false,
            false,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            Error::Step {
                step: "firecracker",
                ..
            }
        ));
        assert!(
            !fake.calls().iter().any(|c| c.contains("bash")),
            "{:?}",
            fake.calls()
        );
    }
}
