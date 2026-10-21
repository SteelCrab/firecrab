//! install.sh `resolve_payload` / `download_host_bundle`: select the installation
//! files from a `--bin-dir` checkout or a downloaded release bundle.

use std::path::{Path, PathBuf};

use crate::shell::CommandRunner;
use crate::update::{bundle, check};

use super::Error;

const STEP: &str = "payload";

/// Binaries required in both release bundles and `--bin-dir` installations.
pub const BUNDLE_BINARIES: [&str; 3] = ["firecrab-api", "firecrab-helper", "firecrab"];

/// Shell scripts the API uses to convert kernels at runtime.
pub const EXTRACT_HELPERS: [&str; 2] = ["extract-vmlinux", "extract-arm64-image"];

/// `firecrab_elf_arch`: map a 64-bit little-endian ELF `e_machine` to a release architecture name.
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

/// `firecrab_resolve_binary`: prefer `--bin-dir`, then fall back to the installed binary.
///
/// `scripts/firecrab-release.sh` checks `-x` (the executable bit), but
/// `is_file()` is enough here: installation copies files with
/// `install -o root -g root -m 0755`, which always sets the executable bits.
/// The source file's executable bits therefore do not affect installation.
pub fn resolve_binary(name: &str, src_dir: Option<&Path>, dest_dir: &Path) -> Option<PathBuf> {
    for dir in src_dir.into_iter().chain(std::iter::once(dest_dir)) {
        let names = if name == "firecrab-helper" {
            vec![name, "firecrab-net-helper"]
        } else {
            vec![name]
        };
        for candidate in names {
            let path = dir.join(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

/// Walk upward from `start` to find the firecrab checkout root
/// using the same checks as install.sh `is_checkout`.
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

/// Check that an extracted release bundle contains every member required by install.sh.
///
/// `install.sh` checks `[ -x "$PAYLOAD_ROOT/$name" ]`, but `is_file()` is enough
/// for the same reason as in [`resolve_binary`]: the later installation step
/// sets the executable bits with `install -m 0755`.
pub fn verify_bundle_members(root: &Path, arch: &str) -> Result<(), Error> {
    for name in BUNDLE_BINARIES {
        let path = resolve_binary(name, None, root)
            .ok_or_else(|| Error::step(STEP, format!("release bundle is missing {name}")))?;
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

/// Normalize an explicitly supplied version. `None` means the tag is not yet
/// known and must be fetched from GitHub's `releases/latest`. An omitted value
/// (`None`), an empty string, and `"latest"` all resolve to this case. Otherwise,
/// return `Some(tag)` with the `v` prefix normalized using the same rules as
/// `scripts/firecrab-release.sh`'s `firecrab_normalize_tag`.
///
/// Keep this as a pure function so tag normalization can be tested without
/// network access. [`Payload::from_release`] performs the actual "latest"
/// lookup through `check::fetch_latest_tag`.
fn normalize_tag(version: Option<&str>) -> Option<String> {
    match version {
        None => None,
        Some(v) if v.is_empty() || v == "latest" => None,
        Some(v) if v.starts_with('v') => Some(v.to_owned()),
        Some(v) => Some(format!("v{v}")),
    }
}

/// Sources of the files to install.
#[derive(Debug)]
pub struct Payload {
    /// Directory containing the binaries.
    pub bin: PathBuf,
    /// Directory containing `*.service` templates.
    pub units: PathBuf,
    /// Directory containing `extract-vmlinux` / `extract-arm64-image`.
    pub extract: PathBuf,
    /// Dashboard build (absent with `--no-frontend` or when keeping the existing installation).
    pub dashboard: Option<PathBuf>,
    /// FireCrab LICENSE.
    pub license: PathBuf,
    /// Full GPL-2.0 license text.
    pub gpl: PathBuf,
    /// Third-party notices (may be absent in a checkout).
    pub third_party: Option<PathBuf>,
    /// License inventory (may be absent in a checkout).
    pub inventory: Option<PathBuf>,
    /// Checkout root (used to find the Firecracker script).
    pub checkout: Option<PathBuf>,
    /// Keep the downloaded bundle alive for the lifetime of the payload.
    pub(crate) _temp: Option<tempfile::TempDir>,
}

impl Payload {
    /// The `dir` branch of install.sh `resolve_payload`.
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
            if resolve_binary(name, None, bin_dir).is_none() {
                return Err(Error::step_fix(
                    STEP,
                    format!("no {name} in {}", bin_dir.display()),
                    "cargo build --release -p firecrab-api -p firecrab-helper -p firecrab-cli",
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

    /// install.sh `download_host_bundle`: download the bundle and `SHA256SUMS`, verify, then extract.
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
        assert!(format!("{err}").contains("firecrab-helper"), "{err}");
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
    #[test]
    fn a_legacy_source_helper_wins_over_the_installed_canonical_binary() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("source");
        let dest = dir.path().join("installed");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(src.join("firecrab-net-helper"), b"new").unwrap();
        std::fs::write(dest.join("firecrab-helper"), b"old").unwrap();
        assert_eq!(
            resolve_binary("firecrab-helper", Some(&src), &dest),
            Some(src.join("firecrab-net-helper"))
        );
        std::fs::write(src.join("firecrab-helper"), b"newest").unwrap();
        assert_eq!(
            resolve_binary("firecrab-helper", Some(&src), &dest),
            Some(src.join("firecrab-helper"))
        );
    }
}
