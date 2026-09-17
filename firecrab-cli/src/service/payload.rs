//! install.sh `resolve_payload` / `download_host_bundle`: 설치할 파일 묶음을
//! `--bin-dir` 체크아웃 또는 다운로드한 릴리스 번들 중 하나로 확정한다.

use std::path::{Path, PathBuf};

use crate::shell::CommandRunner;
use crate::update::{bundle, check};

use super::Error;

const STEP: &str = "payload";

/// 번들과 `--bin-dir` 양쪽에서 반드시 있어야 하는 바이너리.
pub const BUNDLE_BINARIES: [&str; 3] = ["firecrab-api", "firecrab-net-helper", "firecrab"];

/// API가 런타임에 커널을 변환할 때 쓰는 셸 스크립트.
pub const EXTRACT_HELPERS: [&str; 2] = ["extract-vmlinux", "extract-arm64-image"];

/// `firecrab_elf_arch`: 64비트 리틀엔디언 ELF의 `e_machine`을 릴리스 arch 이름으로.
pub fn elf_arch(path: &Path) -> Option<&'static str> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 20 || &bytes[0..4] != b"\x7fELF" {
        return None;
    }
    if bytes[4] != 2 || bytes[5] != 1 {
        return None;
    }
    match u16::from_le_bytes([bytes[18], bytes[19]]) {
        62 => Some("x86_64"),
        183 => Some("aarch64"),
        _ => None,
    }
}

/// `firecrab_resolve_binary`: `--bin-dir`에 있으면 그것, 없으면 이미 설치된 것.
///
/// `scripts/firecrab-release.sh`는 `-x`(실행 비트)로 검사하지만, 여기서는
/// `is_file()`로 충분하다: 설치는 `install -o root -g root -m 0755`로 복사돼
/// 실행 비트를 무조건 다시 세팅하므로, 원본의 실행 비트 유무는 이후 설치
/// 성공 여부에 영향을 주지 않는다.
pub fn resolve_binary(name: &str, src_dir: Option<&Path>, dest_dir: &Path) -> Option<PathBuf> {
    if let Some(src) = src_dir {
        let candidate = src.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let installed = dest_dir.join(name);
    installed.is_file().then_some(installed)
}

/// `start`에서 위로 올라가며 firecrab 체크아웃 루트를 찾는다
/// (install.sh `is_checkout`과 같은 판정).
pub fn find_checkout(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(current) = dir {
        if current.join("Cargo.toml").is_file()
            && current
                .join("packaging/systemd/firecrab-api.service")
                .is_file()
        {
            return Some(current.to_path_buf());
        }
        dir = current.parent();
    }
    None
}

/// 추출된 릴리스 번들이 install.sh가 요구하는 멤버를 모두 갖췄는지 확인한다.
///
/// `install.sh`는 `[ -x "$PAYLOAD_ROOT/$name" ]`로 검사하지만, 여기서도
/// [`resolve_binary`]와 같은 이유로 `is_file()`이면 충분하다 — 이후 설치
/// 단계가 `install -m 0755`로 실행 비트를 다시 세팅한다.
pub fn verify_bundle_members(root: &Path, arch: &str) -> Result<(), Error> {
    for name in BUNDLE_BINARIES {
        let path = root.join(name);
        if !path.is_file() {
            return Err(Error::step(
                STEP,
                format!("release bundle is missing {name}"),
            ));
        }
        match elf_arch(&path) {
            Some(found) if found == arch => {}
            _ => {
                return Err(Error::step(
                    STEP,
                    format!("release bundle: {name} is not a {arch} binary"),
                ));
            }
        }
    }
    for name in [
        "LICENSE",
        "THIRD_PARTY_NOTICES.txt",
        "release-license-inventory.json",
        "licenses/GPL-2.0-only.txt",
    ] {
        if !root.join(name).is_file() {
            return Err(Error::step(
                STEP,
                format!("release bundle is missing compliance artifact {name}"),
            ));
        }
    }
    Ok(())
}

/// 명시적으로 주어진 버전을 정규화한다. `None`은 "아직 확정된 태그가 없다 —
/// GitHub의 `releases/latest`를 조회해서 알아내야 한다"는 뜻으로, 미지정
/// (`None`), 빈 문자열, `"latest"` 모두 이 경우로 합쳐진다. 그 외에는
/// `scripts/firecrab-release.sh`의 `firecrab_normalize_tag`와 같은 규칙으로
/// `v` 접두를 보정한 `Some(tag)`를 돌려준다.
///
/// 순수 함수로 분리해 둔 이유는 네트워크 없이 태그 정규화 규칙만 테스트하기
/// 위해서다 — 실제 "latest" 조회는 [`Payload::from_release`]에서
/// `check::fetch_latest_tag`로 한다.
fn normalize_tag(version: Option<&str>) -> Option<String> {
    match version {
        None => None,
        Some(v) if v.is_empty() || v == "latest" => None,
        Some(v) if v.starts_with('v') => Some(v.to_owned()),
        Some(v) => Some(format!("v{v}")),
    }
}

/// 설치할 파일들의 출처.
#[derive(Debug)]
pub struct Payload {
    /// 바이너리가 있는 디렉터리.
    pub bin: PathBuf,
    /// `*.service` 템플릿 디렉터리.
    pub units: PathBuf,
    /// `extract-vmlinux` / `extract-arm64-image`가 있는 디렉터리.
    pub extract: PathBuf,
    /// 대시보드 빌드 (없으면 `--no-frontend`이거나 기존 설치 유지).
    pub dashboard: Option<PathBuf>,
    /// FireCrab LICENSE.
    pub license: PathBuf,
    /// GPL-2.0 전문.
    pub gpl: PathBuf,
    /// 서드파티 고지 (체크아웃에서는 없을 수 있음).
    pub third_party: Option<PathBuf>,
    /// 라이선스 인벤토리 (체크아웃에서는 없을 수 있음).
    pub inventory: Option<PathBuf>,
    /// 체크아웃 루트 (firecracker 스크립트 탐색에 쓰임).
    pub checkout: Option<PathBuf>,
    /// 다운로드 번들의 수명을 payload에 묶는다.
    pub(crate) _temp: Option<tempfile::TempDir>,
}

impl Payload {
    /// install.sh `resolve_payload`의 `dir` 분기.
    pub fn from_bin_dir(
        bin_dir: &Path,
        dashboard_dir: Option<&Path>,
        checkout: &Path,
    ) -> Result<Self, Error> {
        if !bin_dir.is_dir() {
            return Err(Error::step(
                STEP,
                format!("--bin-dir is not a directory: {}", bin_dir.display()),
            ));
        }
        for name in BUNDLE_BINARIES {
            if !bin_dir.join(name).is_file() {
                return Err(Error::step_fix(
                    STEP,
                    format!("no {name} in {}", bin_dir.display()),
                    "cargo build --release -p firecrab-api -p firecrab-net-helper -p firecrab-cli",
                ));
            }
        }
        let license = checkout.join("LICENSE");
        let gpl = checkout.join("licenses/GPL-2.0-only.txt");
        for path in [&license, &gpl] {
            if !path.is_file() {
                return Err(Error::step(
                    STEP,
                    format!("missing {} in the checkout", path.display()),
                ));
            }
        }
        let compliance = checkout.join("dist/compliance");
        let (third_party, inventory) = {
            let notices = compliance.join("THIRD_PARTY_NOTICES.txt");
            let inv = compliance.join("release-license-inventory.json");
            if notices.is_file() && inv.is_file() {
                (Some(notices), Some(inv))
            } else {
                (None, None)
            }
        };
        let dashboard = match dashboard_dir {
            Some(dir) => Some(dir.to_path_buf()),
            None => {
                let default = checkout.join("firecrab-frontend/dist");
                default.join("index.html").is_file().then_some(default)
            }
        };
        Ok(Self {
            bin: bin_dir.to_path_buf(),
            units: checkout.join("packaging/systemd"),
            extract: checkout.join("scripts/firecracker-menual"),
            dashboard,
            license,
            gpl,
            third_party,
            inventory,
            checkout: Some(checkout.to_path_buf()),
            _temp: None,
        })
    }

    /// install.sh `download_host_bundle`: 번들과 `SHA256SUMS`를 받아 검증하고 푼다.
    pub fn from_release(
        runner: &dyn CommandRunner,
        version: Option<&str>,
        libc: Option<&str>,
    ) -> Result<Self, Error> {
        let arch = bundle::host_arch()?;
        let libc = bundle::host_libc(libc)?;
        let tarball = bundle::host_tarball(arch, libc);
        // GitHub only serves the "latest" alias at `releases/latest/download/…`,
        // never `releases/download/latest/…` (see `bundle::asset_url`'s doc
        // comment). So an unspecified/"latest" version must be resolved to a
        // real tag first, the same way `firecrab update` does via
        // `check::fetch_latest_tag`.
        let tag = match normalize_tag(version) {
            Some(tag) => tag,
            None => check::fetch_latest_tag(&check::release_api_url(&bundle::release_repo()))?,
        };
        let base = bundle::release_base();

        let temp = tempfile::tempdir().map_err(|e| {
            Error::step(STEP, format!("could not create a temporary directory: {e}"))
        })?;
        let archive = temp.path().join(&tarball);
        let sums = temp.path().join("SHA256SUMS");

        super::output::step(&format!("downloading {tarball}"));
        bundle::download_to(&bundle::asset_url(&base, &tag, &tarball), &archive)?;
        bundle::download_to(&bundle::asset_url(&base, &tag, "SHA256SUMS"), &sums)?;

        let sums_text = std::fs::read_to_string(&sums).unwrap_or_default();
        let expected = bundle::expected_sha256(&sums_text, &tarball)
            .ok_or_else(|| Error::step(STEP, format!("{tarball} is not listed in SHA256SUMS")))?;
        let actual = bundle::file_sha256(&archive)?;
        if actual != expected {
            return Err(Error::step(
                STEP,
                format!("checksum mismatch for {tarball}: expected {expected}, got {actual}"),
            ));
        }

        let root = temp.path().join("root");
        std::fs::create_dir_all(&root)
            .map_err(|e| Error::step(STEP, format!("could not create {}: {e}", root.display())))?;
        let (archive_s, root_s) = (
            archive.to_string_lossy().into_owned(),
            root.to_string_lossy().into_owned(),
        );
        let out = runner
            .run("tar", &["-xzf", &archive_s, "-C", &root_s])
            .map_err(|e| Error::step(STEP, format!("tar: {e}")))?;
        if !out.status.success() {
            return Err(Error::step(
                STEP,
                format!(
                    "tar failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            ));
        }

        verify_bundle_members(&root, arch)?;
        super::output::log(&format!("host bundle {arch}/{libc}"));
        Ok(Self {
            bin: root.clone(),
            units: root.join("systemd"),
            extract: root.clone(),
            dashboard: Some(root.join("dashboard")),
            license: root.join("LICENSE"),
            gpl: root.join("licenses/GPL-2.0-only.txt"),
            third_party: Some(root.join("THIRD_PARTY_NOTICES.txt")),
            inventory: Some(root.join("release-license-inventory.json")),
            checkout: None,
            _temp: Some(temp),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Minimal 64-bit little-endian ELF header with the given e_machine.
    fn write_elf(path: &Path, machine: u16) {
        let mut header = vec![0u8; 64];
        header[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        header[4] = 2; // ELFCLASS64
        header[5] = 1; // ELFDATA2LSB
        header[18..20].copy_from_slice(&machine.to_le_bytes());
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(&header).unwrap();
    }

    #[test]
    fn normalize_tag_resolves_absent_and_latest_to_none_and_prefixes_bare_versions() {
        assert_eq!(normalize_tag(None), None);
        assert_eq!(normalize_tag(Some("latest")), None);
        assert_eq!(normalize_tag(Some("")), None);
        assert_eq!(normalize_tag(Some("0.3.0")), Some("v0.3.0".to_owned()));
        assert_eq!(normalize_tag(Some("v0.3.0")), Some("v0.3.0".to_owned()));
    }

    #[test]
    fn elf_arch_reads_the_machine_field() {
        let dir = tempfile::tempdir().unwrap();
        let x86 = dir.path().join("x86");
        let arm = dir.path().join("arm");
        let junk = dir.path().join("junk");
        write_elf(&x86, 62);
        write_elf(&arm, 183);
        std::fs::write(&junk, b"not an elf").unwrap();
        assert_eq!(elf_arch(&x86), Some("x86_64"));
        assert_eq!(elf_arch(&arm), Some("aarch64"));
        assert_eq!(elf_arch(&junk), None);
        assert_eq!(elf_arch(&dir.path().join("absent")), None);
    }

    #[test]
    fn resolve_binary_prefers_the_source_directory_then_the_installed_copy() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dest = dir.path().join("dest");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("firecrab-api"), b"old").unwrap();
        assert_eq!(
            resolve_binary("firecrab-api", Some(&src), &dest),
            Some(dest.join("firecrab-api"))
        );
        std::fs::write(src.join("firecrab-api"), b"new").unwrap();
        assert_eq!(
            resolve_binary("firecrab-api", Some(&src), &dest),
            Some(src.join("firecrab-api"))
        );
        assert_eq!(resolve_binary("absent", Some(&src), &dest), None);
    }

    #[test]
    fn find_checkout_walks_up_to_the_workspace_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("Cargo.toml"), b"[workspace]\n").unwrap();
        std::fs::create_dir_all(root.join("packaging/systemd")).unwrap();
        std::fs::write(
            root.join("packaging/systemd/firecrab-api.service"),
            b"[Unit]\n",
        )
        .unwrap();
        let nested = root.join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(find_checkout(&nested).as_deref(), Some(root));
        let elsewhere = tempfile::tempdir().unwrap();
        assert_eq!(find_checkout(elsewhere.path()), None);
    }

    #[test]
    fn from_bin_dir_requires_the_binaries_and_units() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("packaging/systemd")).unwrap();
        std::fs::create_dir_all(root.join("scripts/firecracker-menual")).unwrap();
        std::fs::create_dir_all(root.join("licenses")).unwrap();
        std::fs::write(root.join("LICENSE"), b"x").unwrap();
        std::fs::write(root.join("licenses/GPL-2.0-only.txt"), b"x").unwrap();
        let bin = root.join("target/release");
        std::fs::create_dir_all(&bin).unwrap();

        let err = Payload::from_bin_dir(&bin, None, root).unwrap_err();
        assert!(
            matches!(
                err,
                Error::Step {
                    step: "payload",
                    ..
                }
            ),
            "{err:?}"
        );

        for name in BUNDLE_BINARIES {
            std::fs::write(bin.join(name), b"x").unwrap();
        }
        let dash = root.join("firecrab-frontend/dist");
        std::fs::create_dir_all(&dash).unwrap();
        std::fs::write(dash.join("index.html"), b"<html>").unwrap();

        let payload = Payload::from_bin_dir(&bin, None, root).unwrap();
        assert_eq!(payload.bin, bin);
        assert_eq!(payload.units, root.join("packaging/systemd"));
        assert_eq!(payload.extract, root.join("scripts/firecracker-menual"));
        assert_eq!(payload.dashboard.as_deref(), Some(dash.as_path()));
        assert_eq!(payload.checkout.as_deref(), Some(root));
    }

    #[test]
    fn from_bin_dir_accepts_an_explicit_dashboard_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("packaging/systemd")).unwrap();
        std::fs::create_dir_all(root.join("scripts/firecracker-menual")).unwrap();
        std::fs::create_dir_all(root.join("licenses")).unwrap();
        std::fs::write(root.join("LICENSE"), b"x").unwrap();
        std::fs::write(root.join("licenses/GPL-2.0-only.txt"), b"x").unwrap();
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for name in BUNDLE_BINARIES {
            std::fs::write(bin.join(name), b"x").unwrap();
        }
        let dash = root.join("custom-dash");
        std::fs::create_dir_all(&dash).unwrap();
        std::fs::write(dash.join("index.html"), b"<html>").unwrap();

        let payload = Payload::from_bin_dir(&bin, Some(&dash), root).unwrap();
        assert_eq!(payload.dashboard.as_deref(), Some(dash.as_path()));
    }

    #[test]
    fn verify_bundle_members_rejects_a_missing_binary() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("licenses")).unwrap();
        std::fs::write(root.join("LICENSE"), b"x").unwrap();
        std::fs::write(root.join("licenses/GPL-2.0-only.txt"), b"x").unwrap();
        write_elf(&root.join("firecrab-api"), 62);
        let err = verify_bundle_members(root, "x86_64").unwrap_err();
        assert!(format!("{err}").contains("firecrab-net-helper"), "{err}");
    }

    #[test]
    fn verify_bundle_members_rejects_a_foreign_architecture() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("licenses")).unwrap();
        std::fs::create_dir_all(root.join("systemd")).unwrap();
        std::fs::create_dir_all(root.join("dashboard")).unwrap();
        std::fs::write(root.join("LICENSE"), b"x").unwrap();
        std::fs::write(root.join("THIRD_PARTY_NOTICES.txt"), b"x").unwrap();
        std::fs::write(root.join("release-license-inventory.json"), b"{}").unwrap();
        std::fs::write(root.join("licenses/GPL-2.0-only.txt"), b"x").unwrap();
        for name in BUNDLE_BINARIES {
            write_elf(&root.join(name), 183); // aarch64
        }
        let err = verify_bundle_members(root, "x86_64").unwrap_err();
        assert!(format!("{err}").contains("x86_64"), "{err}");
    }
}
