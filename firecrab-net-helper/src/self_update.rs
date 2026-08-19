//! Applying a downloaded host bundle over this host's install, for
//! `NetworkRequest::ApplySelfUpdate`.
//!
//! Split deliberately into two functions: [`apply_bundle`] does pure
//! filesystem work and is therefore fully unit-testable against a `tempdir`
//! layout, while [`restart_units`] does nothing but shell out to `systemctl`
//! and is never called from `dispatch`. That separation is what lets the
//! connection loop write its response frame *before* this process restarts
//! itself (see `AfterResponse` in `main.rs`).

use std::fs::{self, File};
use std::io::{Read, Seek};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use firecrab_helper_protocol::network::InstallLayout;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::process::Command;
use uuid::Uuid;

/// Binaries a release bundle must contain; a bundle missing any of them is
/// rejected before a single target is touched (the same three names
/// `install.sh`'s `download_host_bundle` insists on).
const REQUIRED_BINARIES: [&str; 3] = ["firecrab-api", "firecrab-net-helper", "firecrab"];

/// Suffix the pre-update copy of every replaced target is parked under, so a
/// failure halfway through the swap can be rolled back in reverse order.
const BACKUP_SUFFIX: &str = ".firecrab-bak";

/// Why an `ApplySelfUpdate` couldn't be carried out. Mapped to
/// `HelperFailure::UpdateChecksumMismatch` / `UpdateApplyFailed` /
/// `InvalidRequest` by `dispatch`; `error_chain` flattens the cause chain
/// into `detail` the same way every other helper operation does.
#[derive(Debug, Error)]
pub enum SelfUpdateError {
    /// A path or hash in the request failed re-validation.
    #[error("{0}")]
    Invalid(String),
    /// The file on disk hashed to something else.
    #[error("bundle checksum mismatch")]
    Checksum {
        /// Hash the request carried.
        expected: String,
        /// Hash the helper computed.
        actual: String,
    },
    /// Extraction or the swap failed; `restored` records whether the
    /// pre-update binaries were put back.
    #[error("failed to apply the bundle")]
    Apply {
        /// Whether every replaced target was restored from its `.firecrab-bak`.
        restored: bool,
        #[source]
        source: std::io::Error,
    },
}

/// Streams `reader` through SHA-256 and returns the lowercase hex digest.
fn hash_reader<R: Read>(reader: &mut R) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Every path this update replaces, in swap order: `$LIBDIR` first, then the
/// CLI, then the dashboard. The order matters only for the rollback loop,
/// which walks it backwards.
fn swap_plan(layout: &InstallLayout, staging: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut plan = Vec::new();
    for name in [
        "firecrab-api",
        "firecrab-net-helper",
        "extract-vmlinux",
        "extract-arm64-image",
    ] {
        plan.push((staging.join(name), layout.libdir.join(name)));
    }
    plan.push((staging.join("firecrab"), layout.bindir.join("firecrab")));
    plan.push((staging.join("dashboard"), layout.sharedir.join("dashboard")));
    plan
}

/// Re-validates everything the request carried before a single byte is read.
/// The helper is the trust boundary and never assumes the (unprivileged) CLI
/// already checked — the same reasoning as `validate_prefix` and the
/// `egress_policy` allowlist lookup.
fn validate(
    layout: &InstallLayout,
    tarball_path: &Path,
    sha256: &str,
) -> Result<(), SelfUpdateError> {
    if !tarball_path.is_absolute() {
        return Err(SelfUpdateError::Invalid(format!(
            "tarball path {} is not absolute",
            tarball_path.display()
        )));
    }
    let escapes = |path: &Path| {
        path.components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    };
    if escapes(tarball_path) {
        return Err(SelfUpdateError::Invalid(
            "tarball path may not contain '..'".to_owned(),
        ));
    }
    for dir in [&layout.bindir, &layout.libdir, &layout.sharedir] {
        if !dir.is_absolute() || escapes(dir) {
            return Err(SelfUpdateError::Invalid(format!(
                "layout path {} must be absolute and free of '..'",
                dir.display()
            )));
        }
        if !dir.is_dir() {
            return Err(SelfUpdateError::Invalid(format!(
                "layout path {} is not an existing directory",
                dir.display()
            )));
        }
    }
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(SelfUpdateError::Invalid(
            "sha256 must be 64 lowercase hex characters".to_owned(),
        ));
    }
    Ok(())
}

/// Validates the request, re-verifies the bundle from a single open file
/// descriptor, extracts all of it into a staging directory on the target
/// filesystem, then swaps every target with `rename(2)`.
///
/// Two ordering rules carry the safety here:
/// * the tarball is opened **once**, and both the hash and the extraction read
///   that same descriptor, so a file swapped between the two steps cannot be
///   verified as one thing and installed as another;
/// * nothing is replaced until everything has been extracted, so a disk-full
///   part-way through extraction leaves the install completely untouched.
///
/// `rename(2)` (rather than writing in place) is what makes replacing a
/// *running* binary legal at all: writing to one returns `ETXTBSY`, while a
/// rename swaps the directory entry and leaves the running inode alone.
pub async fn apply_bundle(
    layout: &InstallLayout,
    tarball_path: &Path,
    sha256: &str,
) -> Result<(), SelfUpdateError> {
    validate(layout, tarball_path, sha256)?;

    let mut file = File::open(tarball_path).map_err(|source| {
        SelfUpdateError::Invalid(format!("cannot open {}: {source}", tarball_path.display()))
    })?;
    let actual = hash_reader(&mut file).map_err(|source| SelfUpdateError::Apply {
        restored: true,
        source,
    })?;
    if actual != sha256 {
        return Err(SelfUpdateError::Checksum {
            expected: sha256.to_owned(),
            actual,
        });
    }
    file.rewind().map_err(|source| SelfUpdateError::Apply {
        restored: true,
        source,
    })?;

    // Staging sits inside $LIBDIR so every rename below stays on one
    // filesystem — a cross-device rename returns EXDEV and is not atomic.
    let staging = layout.libdir.join(format!(".update-{}", Uuid::new_v4()));
    let extracted = extract_and_check(&mut file, &staging);
    if let Err(source) = extracted {
        let _ = fs::remove_dir_all(&staging);
        return Err(SelfUpdateError::Apply {
            restored: true,
            source,
        });
    }

    match swap_all(layout, &staging) {
        Ok(backups) => {
            for backup in backups {
                let _ = remove_any(&backup);
            }
            let _ = fs::remove_dir_all(&staging);
            cleanup_download_dir(tarball_path);
            Ok(())
        }
        Err((source, restored)) => {
            let _ = fs::remove_dir_all(&staging);
            Err(SelfUpdateError::Apply { restored, source })
        }
    }
}

/// Extracts the whole bundle into `staging` and confirms the three required
/// binaries arrived. `tar`'s own unpack refuses entries that would escape the
/// destination directory, so a hostile bundle cannot write outside `staging`.
fn extract_and_check(file: &mut File, staging: &Path) -> std::io::Result<()> {
    fs::create_dir_all(staging)?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    archive.set_overwrite(true);
    archive.set_preserve_mtime(false);
    archive.unpack(staging)?;
    for name in REQUIRED_BINARIES {
        let path = staging.join(name);
        if !path.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("release bundle is missing {name}"),
            ));
        }
    }
    Ok(())
}

/// Renames every target aside to `{target}.firecrab-bak` and moves the staged
/// copy into its place. On the first failure everything already done is rolled
/// back in reverse order; the returned `bool` says whether that rollback fully
/// succeeded.
///
/// A staged source that does not exist is skipped rather than treated as a
/// failure: the three names in [`REQUIRED_BINARIES`] were already checked, and
/// an older bundle without, say, `extract-arm64-image` should still install.
fn swap_all(
    layout: &InstallLayout,
    staging: &Path,
) -> Result<Vec<PathBuf>, (std::io::Error, bool)> {
    let mut done: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut backups: Vec<PathBuf> = Vec::new();

    for (staged, target) in swap_plan(layout, staging) {
        if !staged.exists() {
            continue;
        }
        if staged.is_file() {
            // Ownership is best-effort: in production the helper is uid 0, so
            // the extracted files are already root-owned; unprivileged test
            // runs must not fail on an EPERM they can do nothing about.
            let _ = std::os::unix::fs::chown(&staged, Some(0), Some(0));
            if let Err(source) = fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)) {
                let restored = rollback(&done);
                return Err((source, restored));
            }
        }
        let backup = with_backup_suffix(&target);
        if target.exists()
            && let Err(source) = fs::rename(&target, &backup)
        {
            let restored = rollback(&done);
            return Err((source, restored));
        }
        if let Err(source) = fs::rename(&staged, &target) {
            // Put this one's own backup back before unwinding the rest.
            if backup.exists() {
                let _ = fs::rename(&backup, &target);
            }
            let restored = rollback(&done);
            return Err((source, restored));
        }
        done.push((target, backup.clone()));
        backups.push(backup);
    }
    Ok(backups)
}

/// `{path}.firecrab-bak`, keeping the file name intact.
fn with_backup_suffix(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(BACKUP_SUFFIX);
    target.with_file_name(name)
}

/// Puts every already-swapped target back from its backup, newest first.
/// Returns whether all of them made it.
fn rollback(done: &[(PathBuf, PathBuf)]) -> bool {
    let mut all = true;
    for (target, backup) in done.iter().rev() {
        if !backup.exists() {
            continue;
        }
        if remove_any(target).is_err() {
            all = false;
            continue;
        }
        if fs::rename(backup, target).is_err() {
            all = false;
        }
    }
    all
}

/// Removes a path whether it is a file or a directory.
fn remove_any(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Clears `$DATADIR/updates/<uuid>`: the bundle, its `SHA256SUMS` sibling, and
/// then the directory itself with a **non-recursive** `remove_dir`. Recursive
/// deletion of a caller-supplied parent path is deliberately avoided — the
/// directory only disappears when nothing else is left in it.
fn cleanup_download_dir(tarball_path: &Path) {
    let _ = fs::remove_file(tarball_path);
    if let Some(parent) = tarball_path.parent() {
        let _ = fs::remove_file(parent.join("SHA256SUMS"));
        let _ = fs::remove_dir(parent);
    }
}

/// Restarts both units, as the very last thing this process does.
///
/// `firecrab-api` first, blocking, so the new API is already coming up when
/// the helper goes away. The helper's own restart must be `--no-block`:
/// stopping this unit tears down its whole control group, which would kill the
/// `systemctl` child before it could register the job. `--no-block` hands the
/// job to PID 1 and returns immediately, so the restart completes even though
/// the caller is about to be killed.
pub async fn restart_units() {
    match Command::new("systemctl")
        .args(["restart", "firecrab-api.service"])
        .status()
        .await
    {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!("[ERROR] systemctl restart firecrab-api.service: {status}"),
        Err(error) => eprintln!("[ERROR] systemctl restart firecrab-api.service: {error}"),
    }
    if let Err(error) = Command::new("systemctl")
        .args(["--no-block", "restart", "firecrab-net-helper.service"])
        .status()
        .await
    {
        eprintln!("[ERROR] systemctl --no-block restart firecrab-net-helper.service: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Write;

    use flate2::Compression;
    use flate2::write::GzEncoder;

    /// Builds a layout of three real directories plus the pre-update files a
    /// live install would already have there.
    fn seed_layout(root: &Path) -> InstallLayout {
        let layout = InstallLayout {
            bindir: root.join("usr/local/bin"),
            libdir: root.join("usr/local/lib/firecrab"),
            sharedir: root.join("usr/local/share/firecrab"),
        };
        for dir in [&layout.bindir, &layout.libdir, &layout.sharedir] {
            fs::create_dir_all(dir).expect("create layout dir");
        }
        for name in REQUIRED_BINARIES {
            fs::write(layout.libdir.join(name), b"old").expect("seed lib binary");
        }
        for name in ["extract-vmlinux", "extract-arm64-image"] {
            fs::write(layout.libdir.join(name), b"old").expect("seed extract helper");
        }
        fs::write(layout.bindir.join("firecrab"), b"old").expect("seed cli");
        fs::create_dir_all(layout.sharedir.join("dashboard")).expect("seed dashboard");
        fs::write(layout.sharedir.join("dashboard/index.html"), b"old").expect("seed index");
        layout
    }

    /// Writes a release-shaped bundle into `{root}/updates/job/<name>` and
    /// returns (path, lowercase hex sha256). The tarball deliberately lives in
    /// its own directory: `apply_bundle` cleans up its parent on success.
    fn write_bundle(root: &Path, members: &[(&str, &[u8])]) -> (PathBuf, String) {
        let dir = root.join("updates/job");
        fs::create_dir_all(&dir).expect("create staging dir");
        let path = dir.join("firecrab-host-x86_64-gnu.tar.gz");
        {
            let file = File::create(&path).expect("create tarball");
            let encoder = GzEncoder::new(file, Compression::fast());
            let mut builder = tar::Builder::new(encoder);
            for (name, bytes) in members {
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, name, *bytes)
                    .expect("append member");
            }
            let encoder = builder.into_inner().expect("finish tar");
            let mut file = encoder.finish().expect("finish gzip");
            file.flush().expect("flush");
        }
        let mut opened = File::open(&path).expect("reopen tarball");
        let digest = hash_reader(&mut opened).expect("hash tarball");
        (path, digest)
    }

    fn release_members() -> Vec<(&'static str, &'static [u8])> {
        vec![
            ("firecrab-api", b"new-api" as &[u8]),
            ("firecrab-net-helper", b"new-helper"),
            ("firecrab", b"new-cli"),
            ("extract-vmlinux", b"new-extract"),
            ("extract-arm64-image", b"new-extract-arm"),
            ("dashboard/index.html", b"new-index"),
            ("systemd/firecrab-api.service", b"[Unit]\n"),
        ]
    }

    #[tokio::test]
    async fn apply_bundle_rejects_a_relative_tarball_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = seed_layout(dir.path());
        let result = apply_bundle(&layout, Path::new("bundle.tar.gz"), &"a".repeat(64)).await;
        assert!(
            matches!(result, Err(SelfUpdateError::Invalid(_))),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn apply_bundle_rejects_a_non_hex_sha256() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = seed_layout(dir.path());
        let (path, _) = write_bundle(dir.path(), &release_members());
        for sha in ["", "abc", &"A".repeat(64), &"z".repeat(64)] {
            let result = apply_bundle(&layout, &path, sha).await;
            assert!(
                matches!(result, Err(SelfUpdateError::Invalid(_))),
                "{sha}: {result:?}"
            );
        }
    }

    #[tokio::test]
    async fn apply_bundle_rejects_a_layout_directory_that_does_not_exist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut layout = seed_layout(dir.path());
        layout.sharedir = dir.path().join("nope");
        let (path, sha) = write_bundle(dir.path(), &release_members());
        let result = apply_bundle(&layout, &path, &sha).await;
        assert!(
            matches!(result, Err(SelfUpdateError::Invalid(_))),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn apply_bundle_reports_a_checksum_mismatch_without_touching_the_layout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = seed_layout(dir.path());
        let (path, real) = write_bundle(dir.path(), &release_members());
        let wrong = "0".repeat(64);

        let result = apply_bundle(&layout, &path, &wrong).await;
        match result {
            Err(SelfUpdateError::Checksum { expected, actual }) => {
                assert_eq!(expected, wrong);
                assert_eq!(actual, real);
            }
            other => panic!("expected a checksum mismatch, got {other:?}"),
        }
        assert_eq!(
            fs::read(layout.libdir.join("firecrab-api")).unwrap(),
            b"old"
        );
        assert_eq!(fs::read(layout.bindir.join("firecrab")).unwrap(), b"old");
    }

    #[tokio::test]
    async fn apply_bundle_rejects_a_bundle_missing_a_required_binary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = seed_layout(dir.path());
        let members = vec![("firecrab-api", b"new-api" as &[u8])];
        let (path, sha) = write_bundle(dir.path(), &members);

        let result = apply_bundle(&layout, &path, &sha).await;
        assert!(
            matches!(result, Err(SelfUpdateError::Apply { .. })),
            "{result:?}"
        );
        assert_eq!(
            fs::read(layout.libdir.join("firecrab-api")).unwrap(),
            b"old"
        );
    }

    #[tokio::test]
    async fn apply_bundle_swaps_every_binary_in_a_temp_layout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = seed_layout(dir.path());
        let (path, sha) = write_bundle(dir.path(), &release_members());

        apply_bundle(&layout, &path, &sha).await.expect("apply");

        assert_eq!(
            fs::read(layout.libdir.join("firecrab-api")).unwrap(),
            b"new-api"
        );
        assert_eq!(
            fs::read(layout.libdir.join("firecrab-net-helper")).unwrap(),
            b"new-helper"
        );
        assert_eq!(
            fs::read(layout.libdir.join("extract-vmlinux")).unwrap(),
            b"new-extract"
        );
        assert_eq!(
            fs::read(layout.bindir.join("firecrab")).unwrap(),
            b"new-cli"
        );
        assert_eq!(
            fs::read(layout.sharedir.join("dashboard/index.html")).unwrap(),
            b"new-index"
        );

        let mode = fs::metadata(layout.libdir.join("firecrab-api"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755, "swapped binaries must stay executable");

        // MVP ignores the bundle's systemd/ directory entirely.
        assert!(!layout.libdir.join("systemd").exists());
        // Backups, staging and the download directory are all cleaned up.
        assert!(!layout.libdir.join("firecrab-api.firecrab-bak").exists());
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
        let leftovers: Vec<_> = fs::read_dir(&layout.libdir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".update-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "staging dirs left behind: {leftovers:?}"
        );
    }

    #[tokio::test]
    async fn apply_bundle_restores_the_previous_binaries_when_a_swap_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = seed_layout(dir.path());
        let (path, sha) = write_bundle(dir.path(), &release_members());

        // rename(2) of a regular file onto an existing directory fails with
        // EISDIR for every uid, root included — so this injection is stable
        // whether or not the test runner happens to be privileged. The CLI is
        // swapped after the four $LIBDIR targets, so those four are already
        // done and must be rolled back.
        let blocked = layout.bindir.join("firecrab.firecrab-bak");
        fs::create_dir_all(&blocked).expect("create blocking dir");
        fs::write(blocked.join("keep"), b"x").expect("make it non-empty");

        let result = apply_bundle(&layout, &path, &sha).await;
        match result {
            Err(SelfUpdateError::Apply { restored, .. }) => {
                assert!(restored, "rollback must succeed")
            }
            other => panic!("expected an apply failure, got {other:?}"),
        }
        assert_eq!(
            fs::read(layout.libdir.join("firecrab-api")).unwrap(),
            b"old"
        );
        assert_eq!(
            fs::read(layout.libdir.join("firecrab-net-helper")).unwrap(),
            b"old"
        );
        assert_eq!(fs::read(layout.bindir.join("firecrab")).unwrap(), b"old");
    }
}
