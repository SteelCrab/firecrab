//! Applying a downloaded host bundle over this host's install, for
//! `NetworkRequest::ApplySelfUpdate`.
//!
//! Split deliberately into two functions: [`crate::self_update::apply_bundle`]
//! does pure filesystem work and is therefore fully unit-testable against a
//! `tempdir` layout, while [`crate::self_update::restart_units`] does nothing
//! but shell out to `systemctl` and is never called from `dispatch`. That
//! separation is what lets the connection loop write its response frame
//! *before* this process restarts itself (see `AfterResponse` in `main.rs`).
//!
//! Three properties in here are load-bearing and easy to "simplify" away:
//!
//! * the request's `layout` is a **cross-check, never an instruction** — the
//!   helper re-derives the install layout from its own environment and refuses
//!   anything that disagrees (see [`crate::self_update::host_layout`]);
//! * nothing but a regular file or a directory is ever extracted, so no later
//!   `chown`/`chmod`/`rename` can be redirected through a symlink the bundle
//!   planted (see `extract_and_check`);
//! * every replacement is materialized inside the **target's own directory**
//!   before the final `rename(2)`, because each install path is a separate bind
//!   mount and `rename(2)` refuses to cross a mount boundary (see
//!   `materialize_beside`).

use std::fs::{self, File, Metadata};
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
    /// A path or hash in the request failed re-validation, or the `layout` it
    /// carried is not the layout this host actually installs into.
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

/// The install layout **this host actually has**, derived from the helper's own
/// process environment and never from the request.
///
/// Must stay in sync with `firecrab-cli/src/update/mod.rs::resolve_layout`,
/// which resolves the same three paths the same way on the caller's side. The
/// duplication is deliberate: the alternative is a new shared crate for ten
/// lines, and the helper — the privileged half — must not take a dependency on
/// the unprivileged binary it is about to overwrite.
///
/// `PREFIX` reaches this process from `Environment=PREFIX=@PREFIX@` in
/// `packaging/systemd/firecrab-net-helper.service`, which `install.sh` renders.
/// A unit file predating that line resolves `/usr/local`, which is
/// `install.sh`'s own default — so only a non-default `PREFIX` needs the units
/// re-rendered (a plain `install.sh` re-run) before a self-update is accepted.
pub fn host_layout() -> InstallLayout {
    let prefix = PathBuf::from(std::env::var("PREFIX").unwrap_or_else(|_| "/usr/local".to_owned()));
    let libdir = std::env::var("FIRECRAB_LIBDIR")
        .ok()
        .filter(|value| !value.is_empty())
        .map_or_else(|| prefix.join("lib/firecrab"), PathBuf::from);
    InstallLayout {
        bindir: prefix.join("bin"),
        libdir,
        sharedir: prefix.join("share/firecrab"),
    }
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
///
/// Two different things are checked, and both matter:
///
/// * **shape** — the tarball path is absolute and `..`-free, the three layout
///   paths are absolute, `..`-free and already-existing directories, and the
///   hash is 64 lowercase hex characters;
/// * **legitimacy** — the layout is *byte-for-byte* the one [`host_layout`]
///   derives from this process's own environment.
///
/// The second check is not redundant. The helper socket admits root and the
/// `firecrab-api` service account, and that account is what answers
/// `POST /api/update`; without it, anything that compromised that one
/// unprivileged account could aim root-owned `0755` writes at any existing
/// directory on the host. Every other helper operation already refuses to take
/// a filesystem path from the wire at all (`validate_prefix`, the
/// `egress_policy` allowlist, the derived TAP/bridge names); this keeps
/// `ApplySelfUpdate` in the same posture by treating `layout` as a cross-check
/// the caller must get right, not as an instruction the helper obeys.
fn validate(
    host: &InstallLayout,
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
    if layout != host {
        return Err(SelfUpdateError::Invalid(format!(
            "layout is not this host's install: the request asked for bindir={} \
             libdir={} sharedir={}, but this host installs to bindir={} libdir={} \
             sharedir={} (re-run install.sh if the install prefix changed)",
            layout.bindir.display(),
            layout.libdir.display(),
            layout.sharedir.display(),
            host.bindir.display(),
            host.libdir.display(),
            host.sharedir.display(),
        )));
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

/// Validates the request against this host's own layout, re-verifies the bundle
/// from a single open file descriptor, extracts all of it into a staging
/// directory, then replaces every target with `rename(2)`.
///
/// Three ordering rules carry the safety here:
/// * the layout is checked against [`host_layout`] before anything is opened,
///   so a caller cannot aim this at a directory of its choosing;
/// * the tarball is opened **once**, and both the hash and the extraction read
///   that same descriptor, so a file swapped between the two steps cannot be
///   verified as one thing and installed as another;
/// * nothing is replaced until everything has been extracted and every
///   extracted entry confirmed to be a regular file or a directory, so neither
///   a disk-full part-way through extraction nor a hostile bundle member leaves
///   the install half-written.
///
/// `rename(2)` (rather than writing in place) is what makes replacing a
/// *running* binary legal at all: writing to one returns `ETXTBSY`, while a
/// rename swaps the directory entry and leaves the running inode alone.
pub async fn apply_bundle(
    layout: &InstallLayout,
    tarball_path: &Path,
    sha256: &str,
) -> Result<(), SelfUpdateError> {
    apply_bundle_against(&host_layout(), layout, tarball_path, sha256).await
}

/// [`apply_bundle`] with the host's own layout passed in, so tests can drive
/// the whole apply against a `tempdir` without mutating the process
/// environment. Production always reaches this through [`apply_bundle`].
async fn apply_bundle_against(
    host: &InstallLayout,
    layout: &InstallLayout,
    tarball_path: &Path,
    sha256: &str,
) -> Result<(), SelfUpdateError> {
    validate(host, layout, tarball_path, sha256)?;

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

    // Scratch space for the extracted bundle, nothing more. It is deliberately
    // *not* the source of the final renames: see `materialize_beside` for why
    // each target's replacement has to be written into the target's own
    // directory first.
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
/// binaries arrived.
///
/// Entries are walked one at a time rather than handed to `Archive::unpack`
/// wholesale, because **only regular files and directories may be created**.
/// The `tar` crate path-checks hardlink targets but not symlink targets, so an
/// `unpack` of a hostile bundle would happily create a symlink pointing at
/// `/etc/shadow` — which every following `chown`/`set_permissions` in
/// [`swap_all`] would then follow, handing root's authority to whatever the
/// link named. Refusing the entry outright is the fix; [`reject_irregular_entries`]
/// re-asserts it over the tree that actually landed.
fn extract_and_check(file: &mut File, staging: &Path) -> std::io::Result<()> {
    fs::create_dir_all(staging)?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    archive.set_overwrite(true);
    archive.set_preserve_mtime(false);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        let name = entry
            .path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "<unreadable path>".to_owned());
        if !(kind.is_file() || kind.is_dir()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "release bundle entry {name} is a {kind:?}, not a regular file or directory"
                ),
            ));
        }
        if !entry.unpack_in(staging)? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("release bundle entry {name} tried to escape the staging directory"),
            ));
        }
    }
    reject_irregular_entries(staging)?;
    for name in REQUIRED_BINARIES {
        let path = staging.join(name);
        if !fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("release bundle is missing {name}"),
            ));
        }
    }
    Ok(())
}

/// Walks the freshly extracted tree and fails on anything that is not a regular
/// file or a directory.
///
/// Deliberately uses `symlink_metadata`, not `metadata`/`Path::is_file` — those
/// follow symlinks and would report a link to `/etc/shadow` as a perfectly good
/// regular file. This is the single place that establishes the invariant every
/// later step relies on, so [`swap_all`] never has to wonder what it is
/// chowning.
fn reject_irregular_entries(root: &Path) -> std::io::Result<()> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            let meta = fs::symlink_metadata(&path)?;
            if meta.is_dir() {
                pending.push(path);
            } else if !meta.is_file() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "release bundle entry {} is not a regular file or directory",
                        path.display()
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Puts a copy of every staged entry next to its target, renames the live
/// target aside to `{target}.firecrab-bak`, and renames the copy into place. On
/// the first failure everything already done is rolled back in reverse order;
/// the returned `bool` says whether that rollback fully succeeded.
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
        let Ok(staged_meta) = fs::symlink_metadata(&staged) else {
            continue;
        };
        let replacement = match materialize_beside(&staged, &staged_meta, &target) {
            Ok(path) => path,
            Err(source) => return Err((source, rollback(&done))),
        };
        let backup = with_backup_suffix(&target);
        if exists(&target)
            && let Err(source) = fs::rename(&target, &backup)
        {
            let _ = remove_any(&replacement);
            return Err((source, rollback(&done)));
        }
        if let Err(source) = fs::rename(&replacement, &target) {
            let _ = remove_any(&replacement);
            // Put this one's own backup back before unwinding the rest.
            if exists(&backup) {
                let _ = fs::rename(&backup, &target);
            }
            return Err((source, rollback(&done)));
        }
        done.push((target, backup.clone()));
        backups.push(backup);
    }
    Ok(backups)
}

/// Copies one staged entry into a uniquely named sibling of `target` and
/// returns that path, ready for an atomic same-directory `rename(2)`.
///
/// **This detour is a bug fix, not a style choice.** `firecrab-net-helper.service`
/// punches holes through `ProtectSystem=full` with
/// `ReadWritePaths=/run/firecrab @LIBDIR@ @SHAREDIR@ @PREFIX@/bin @DATADIR@`,
/// and systemd implements each of those as its own bind mount. `rename(2)` (and
/// `link(2)`, so hardlinking is no shortcut) refuses to cross a **mount**
/// boundary with `EXDEV` even when both sides sit on the same block device —
/// the kernel checks the vfsmount, not the device. Renaming out of one shared
/// staging directory into `$LIBDIR`, `$PREFIX/bin` *and* `$SHAREDIR` therefore
/// fails on every real install while passing every `tempdir` test, which have no
/// bind mounts. Writing the replacement bytes into `target.parent()` first
/// keeps the one rename that has to be atomic inside a single mount.
///
/// Do not "simplify" this back into renaming straight out of the staging
/// directory.
fn materialize_beside(
    staged: &Path,
    staged_meta: &Metadata,
    target: &Path,
) -> std::io::Result<PathBuf> {
    let parent = target.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} has no parent directory", target.display()),
        )
    })?;
    let name = target
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let replacement = parent.join(format!(".{name}.new-{}", Uuid::new_v4()));

    let copied = if staged_meta.is_dir() {
        copy_tree(staged, &replacement)
    } else if staged_meta.is_file() {
        copy_regular_file(staged, &replacement)
            .and_then(|()| fs::set_permissions(&replacement, fs::Permissions::from_mode(0o755)))
    } else {
        // Unreachable in practice: `reject_irregular_entries` already refused
        // anything else. Kept so this function is safe on its own terms.
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "staged {} is neither a regular file nor a directory",
                staged.display()
            ),
        ))
    };
    if copied.is_err() {
        let _ = remove_any(&replacement);
    }
    copied.map(|()| replacement)
}

/// Copies one regular file and gives the copy root's ownership.
///
/// Ownership is best-effort: in production the helper is uid 0 and the copy is
/// root-owned the moment it is created, while an unprivileged test run must not
/// fail on an `EPERM` it can do nothing about.
fn copy_regular_file(source: &Path, dest: &Path) -> std::io::Result<()> {
    fs::copy(source, dest)?;
    let _ = std::os::unix::fs::chown(dest, Some(0), Some(0));
    Ok(())
}

/// Recursively copies a staged directory (today only `dashboard/`) into `dest`,
/// preserving each entry's mode so the dashboard keeps the permissions the
/// release bundle shipped.
///
/// Only regular files and directories are copied. [`reject_irregular_entries`]
/// already guaranteed the staged tree holds nothing else; refusing again here
/// keeps that guarantee next to the code that acts on it.
fn copy_tree(source: &Path, dest: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let meta = fs::symlink_metadata(&from)?;
        if meta.is_dir() {
            copy_tree(&from, &to)?;
        } else if meta.is_file() {
            copy_regular_file(&from, &to)?;
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "staged {} is not a regular file or directory",
                    from.display()
                ),
            ));
        }
    }
    let mode = fs::symlink_metadata(source)?.permissions().mode() & 0o777;
    let _ = std::os::unix::fs::chown(dest, Some(0), Some(0));
    fs::set_permissions(dest, fs::Permissions::from_mode(mode))
}

/// `{path}.firecrab-bak`, keeping the file name intact. Always a sibling of
/// `target`, so parking a target aside is a same-mount rename too.
fn with_backup_suffix(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(BACKUP_SUFFIX);
    target.with_file_name(name)
}

/// Whether `path` names anything at all, symlinks included. `Path::exists`
/// follows links and answers `false` for a dangling one, which would let a swap
/// clobber it instead of backing it up.
fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Puts every already-swapped target back from its backup, newest first.
/// Returns whether all of them made it.
fn rollback(done: &[(PathBuf, PathBuf)]) -> bool {
    let mut all = true;
    for (target, backup) in done.iter().rev() {
        if !exists(backup) {
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

/// Removes a path whether it is a file, a symlink or a directory; an already
/// absent path is success. Uses `symlink_metadata` so a symlink to a directory
/// is unlinked rather than recursed into.
fn remove_any(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
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

/// Shared fixtures for the tests in this module *and* the `ApplySelfUpdate`
/// dispatch tests in `main.rs`, which reach `apply_bundle` (and therefore
/// [`host_layout`]) through the real request path.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;
    use std::sync::{Mutex, MutexGuard};

    /// Serializes every test that points [`super::host_layout`] at a tempdir.
    /// `set_var` is process-wide, so without one shared lock these race under
    /// `cargo test`'s parallel runner (same pattern as `firecrab-cli`'s
    /// `update::ENV_LOCK`).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Points `PREFIX` at `prefix` (and clears `FIRECRAB_LIBDIR`) for as long
    /// as it is alive, so [`super::host_layout`] resolves into a tempdir.
    ///
    /// Hold this across a `Runtime::block_on`, never across a bare `.await` —
    /// it owns a `MutexGuard`.
    pub(crate) struct HostLayoutEnv {
        _guard: MutexGuard<'static, ()>,
    }

    impl HostLayoutEnv {
        /// Installs the override. A poisoned lock is recovered rather than
        /// propagated: one failing test must not cascade into every other.
        pub(crate) fn set(prefix: &Path) -> Self {
            let guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
            // SAFETY: serialized by ENV_LOCK against every other test that
            // reads or writes these two variables.
            unsafe {
                std::env::set_var("PREFIX", prefix);
                std::env::remove_var("FIRECRAB_LIBDIR");
            }
            Self { _guard: guard }
        }
    }

    impl Drop for HostLayoutEnv {
        fn drop(&mut self) {
            // SAFETY: still holding ENV_LOCK.
            unsafe {
                std::env::remove_var("PREFIX");
                std::env::remove_var("FIRECRAB_LIBDIR");
            }
        }
    }

    /// Builds the layout `host_layout()` resolves for `PREFIX=prefix`, creating
    /// all three directories.
    pub(crate) fn layout_for_prefix(
        prefix: &Path,
    ) -> firecrab_helper_protocol::network::InstallLayout {
        let layout = super::host_layout_for(prefix);
        for dir in [&layout.bindir, &layout.libdir, &layout.sharedir] {
            std::fs::create_dir_all(dir).expect("create layout dir");
        }
        layout
    }
}

/// [`host_layout`] with the prefix supplied directly, so tests can build the
/// exact layout the env-driven resolver would produce without going through the
/// environment twice.
#[cfg(test)]
fn host_layout_for(prefix: &Path) -> InstallLayout {
    InstallLayout {
        bindir: prefix.join("bin"),
        libdir: prefix.join("lib/firecrab"),
        sharedir: prefix.join("share/firecrab"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Write;
    use std::os::unix::fs::MetadataExt;

    use flate2::Compression;
    use flate2::write::GzEncoder;

    /// One bundle member: either file bytes or a symlink target.
    enum Member {
        File(&'static str, &'static [u8]),
        Symlink(&'static str, &'static str),
    }

    /// A current-thread runtime, for the tests that must hold
    /// [`test_support::HostLayoutEnv`]'s lock across the whole apply. A
    /// `MutexGuard` may not be held across a bare `.await`, but `block_on` is
    /// a plain blocking call.
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build runtime")
    }

    /// Builds a layout of three real directories plus the pre-update files a
    /// live install would already have there. The paths are exactly the ones
    /// [`host_layout`] resolves for `PREFIX={root}/usr/local`.
    fn seed_layout(root: &Path) -> InstallLayout {
        let layout = host_layout_for(&root.join("usr/local"));
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
    fn write_bundle(root: &Path, members: &[Member]) -> (PathBuf, String) {
        let dir = root.join("updates/job");
        fs::create_dir_all(&dir).expect("create staging dir");
        let path = dir.join("firecrab-host-x86_64-gnu.tar.gz");
        {
            let file = File::create(&path).expect("create tarball");
            let encoder = GzEncoder::new(file, Compression::fast());
            let mut builder = tar::Builder::new(encoder);
            for member in members {
                let mut header = tar::Header::new_gnu();
                match member {
                    Member::File(name, bytes) => {
                        header.set_size(bytes.len() as u64);
                        header.set_mode(0o644);
                        header.set_cksum();
                        builder
                            .append_data(&mut header, name, *bytes)
                            .expect("append member");
                    }
                    Member::Symlink(name, target) => {
                        header.set_size(0);
                        header.set_mode(0o777);
                        header.set_entry_type(tar::EntryType::Symlink);
                        builder
                            .append_link(&mut header, name, target)
                            .expect("append symlink");
                    }
                }
            }
            let encoder = builder.into_inner().expect("finish tar");
            let mut file = encoder.finish().expect("finish gzip");
            file.flush().expect("flush");
        }
        let mut opened = File::open(&path).expect("reopen tarball");
        let digest = hash_reader(&mut opened).expect("hash tarball");
        (path, digest)
    }

    fn release_members() -> Vec<Member> {
        vec![
            Member::File("firecrab-api", b"new-api"),
            Member::File("firecrab-net-helper", b"new-helper"),
            Member::File("firecrab", b"new-cli"),
            Member::File("extract-vmlinux", b"new-extract"),
            Member::File("extract-arm64-image", b"new-extract-arm"),
            Member::File("dashboard/index.html", b"new-index"),
            Member::File("systemd/firecrab-api.service", b"[Unit]\n"),
        ]
    }

    #[tokio::test]
    async fn apply_bundle_rejects_a_relative_tarball_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = seed_layout(dir.path());
        let result = apply_bundle_against(
            &layout,
            &layout,
            Path::new("bundle.tar.gz"),
            &"a".repeat(64),
        )
        .await;
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
            let result = apply_bundle_against(&layout, &layout, &path, sha).await;
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
        let result = apply_bundle_against(&layout, &layout, &path, &sha).await;
        assert!(
            matches!(result, Err(SelfUpdateError::Invalid(_))),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn apply_bundle_rejects_a_layout_that_is_not_this_hosts_install() {
        // Every path here is absolute, '..'-free and a real directory, so the
        // shape checks all pass — only the comparison against the host's own
        // layout stands between a compromised firecrab-api account and
        // root-owned 0755 writes into a directory of its choosing.
        let dir = tempfile::tempdir().expect("tempdir");
        let host = seed_layout(dir.path());
        let elsewhere = seed_layout(&dir.path().join("attacker"));
        let (path, sha) = write_bundle(dir.path(), &release_members());

        let result = apply_bundle_against(&host, &elsewhere, &path, &sha).await;
        match result {
            Err(SelfUpdateError::Invalid(detail)) => {
                assert!(detail.contains("not this host's install"), "{detail}");
                assert!(
                    detail.contains(&host.libdir.display().to_string()),
                    "{detail}"
                );
            }
            other => panic!("expected a rejected layout, got {other:?}"),
        }
        // Nothing at the attacker's target, and nothing at the real one either.
        assert_eq!(fs::read(elsewhere.bindir.join("firecrab")).unwrap(), b"old");
        assert_eq!(fs::read(host.bindir.join("firecrab")).unwrap(), b"old");
    }

    #[test]
    fn host_layout_follows_install_sh_defaults_and_the_libdir_override() {
        // Must stay identical to firecrab-cli's resolve_layout: the two
        // resolvers have to agree or every apply is rejected.
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let _env = test_support::HostLayoutEnv::set(&dir.path().join("opt/fc"));
            let layout = host_layout();
            assert_eq!(layout.bindir, dir.path().join("opt/fc/bin"));
            assert_eq!(layout.libdir, dir.path().join("opt/fc/lib/firecrab"));
            assert_eq!(layout.sharedir, dir.path().join("opt/fc/share/firecrab"));
        }
        {
            let _env = test_support::HostLayoutEnv::set(&dir.path().join("opt/fc"));
            // SAFETY: HostLayoutEnv holds the env lock for this scope.
            unsafe { std::env::set_var("FIRECRAB_LIBDIR", "/opt/other/lib") };
            let layout = host_layout();
            assert_eq!(layout.libdir, Path::new("/opt/other/lib"));
            assert_eq!(layout.bindir, dir.path().join("opt/fc/bin"));
        }
    }

    #[test]
    fn apply_bundle_reads_the_host_layout_from_the_process_environment() {
        // The public entry point takes no host layout: it must derive one from
        // PREFIX, exactly as the systemd unit supplies it.
        let dir = tempfile::tempdir().expect("tempdir");
        let prefix = dir.path().join("usr/local");
        let layout = seed_layout(dir.path());
        let (path, sha) = write_bundle(dir.path(), &release_members());

        let _env = test_support::HostLayoutEnv::set(&prefix);
        runtime()
            .block_on(apply_bundle(&layout, &path, &sha))
            .expect("apply");

        assert_eq!(
            fs::read(layout.libdir.join("firecrab-api")).unwrap(),
            b"new-api"
        );
    }

    #[tokio::test]
    async fn apply_bundle_reports_a_checksum_mismatch_without_touching_the_layout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = seed_layout(dir.path());
        let (path, real) = write_bundle(dir.path(), &release_members());
        let wrong = "0".repeat(64);

        let result = apply_bundle_against(&layout, &layout, &path, &wrong).await;
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
        let members = vec![Member::File("firecrab-api", b"new-api")];
        let (path, sha) = write_bundle(dir.path(), &members);

        let result = apply_bundle_against(&layout, &layout, &path, &sha).await;
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
    async fn apply_bundle_rejects_a_bundle_carrying_a_symlink() {
        // `tar` only path-checks hardlink targets, so without an explicit
        // entry-type filter this symlink would be created, then chowned to
        // root, chmodded 0755 and renamed into $PREFIX/bin — root touching
        // /etc/shadow on a bundle author's say-so.
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = seed_layout(dir.path());
        let victim = dir.path().join("victim");
        fs::write(&victim, b"secret").expect("seed victim");
        let mut permissions = fs::metadata(&victim).unwrap().permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&victim, permissions).expect("lock victim down");

        let mut members = release_members();
        // Leak is fine in a test: the path has to outlive the &'static str.
        let target: &'static str = Box::leak(victim.display().to_string().into_boxed_str());
        members.push(Member::Symlink("evil-link", target));
        let (path, sha) = write_bundle(dir.path(), &members);

        let result = apply_bundle_against(&layout, &layout, &path, &sha).await;
        match result {
            Err(SelfUpdateError::Apply { restored, source }) => {
                assert!(restored, "nothing was swapped, so nothing needed restoring");
                let detail = source.to_string();
                assert!(detail.contains("evil-link"), "{detail}");
            }
            other => panic!("expected the symlink to be refused, got {other:?}"),
        }
        // The bundle's own files never reached the install...
        assert_eq!(
            fs::read(layout.libdir.join("firecrab-api")).unwrap(),
            b"old"
        );
        // ...and the file the link pointed at kept its content and its mode.
        assert_eq!(fs::read(&victim).unwrap(), b"secret");
        assert_eq!(
            fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[tokio::test]
    async fn apply_bundle_swaps_every_binary_in_a_temp_layout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = seed_layout(dir.path());
        let (path, sha) = write_bundle(dir.path(), &release_members());

        apply_bundle_against(&layout, &layout, &path, &sha)
            .await
            .expect("apply");

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
        // Backups, staging, per-target scratch and the download directory are
        // all cleaned up.
        assert!(!layout.libdir.join("firecrab-api.firecrab-bak").exists());
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
        for dir in [&layout.libdir, &layout.bindir, &layout.sharedir] {
            let leftovers: Vec<_> = fs::read_dir(dir)
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with(".update-") || name.contains(".new-"))
                .collect();
            assert!(
                leftovers.is_empty(),
                "scratch left behind in {}: {leftovers:?}",
                dir.display()
            );
        }
    }

    #[test]
    fn every_replacement_is_staged_inside_its_own_target_directory() {
        // The structural guard for the EXDEV bug: `ReadWritePaths=` makes
        // $LIBDIR, $PREFIX/bin and $SHAREDIR three separate bind mounts, and
        // rename(2) refuses to cross a mount boundary. Whatever else changes,
        // the file that gets renamed onto a target must be a sibling of it.
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = seed_layout(dir.path());
        let staging = layout.libdir.join(".update-fixture");
        fs::create_dir_all(staging.join("dashboard")).expect("create staging");
        for name in [
            "firecrab-api",
            "firecrab-net-helper",
            "extract-vmlinux",
            "extract-arm64-image",
            "firecrab",
        ] {
            fs::write(staging.join(name), b"new").expect("stage file");
        }
        fs::write(staging.join("dashboard/index.html"), b"new").expect("stage asset");

        for (staged, target) in swap_plan(&layout, &staging) {
            let meta = fs::symlink_metadata(&staged).expect("staged source");
            let replacement =
                materialize_beside(&staged, &meta, &target).expect("materialize replacement");
            assert_eq!(
                replacement.parent(),
                target.parent(),
                "{} must be staged next to {}",
                replacement.display(),
                target.display()
            );
            assert_ne!(
                replacement.parent(),
                Some(staging.as_path()),
                "the shared staging dir may never be the rename source"
            );
            remove_any(&replacement).expect("clean up");
        }
    }

    #[test]
    fn apply_bundle_survives_a_layout_split_across_filesystems() {
        // A genuine EXDEV reproduction where the runner allows one: /tmp is
        // very often tmpfs while the build tree is on disk, so putting $LIBDIR
        // and $PREFIX/bin on the two of them makes the old
        // "rename straight out of staging" implementation fail with
        // "Invalid cross-device link". Skipped, not failed, when both happen to
        // land on the same device — the structural test above covers that case.
        let on_tmp = tempfile::Builder::new()
            .prefix("fc-xdev")
            .tempdir_in("/tmp")
            .expect("tempdir in /tmp");
        // Next to the test binary rather than in the source tree: that is the
        // build filesystem, whatever the runner mounted where.
        let exe = std::env::current_exe().expect("current exe");
        let build_dir = exe.parent().expect("test binary has a parent");
        let Ok(here) = tempfile::Builder::new()
            .prefix("fc-xdev")
            .tempdir_in(build_dir)
        else {
            eprintln!(
                "skipping: cannot create a tempdir in {}",
                build_dir.display()
            );
            return;
        };
        let tmp_dev = fs::metadata(on_tmp.path()).unwrap().dev();
        let here_dev = fs::metadata(here.path()).unwrap().dev();
        if tmp_dev == here_dev {
            eprintln!("skipping: /tmp and the build tree share device {tmp_dev}");
            return;
        }

        // libdir (and therefore the extraction staging dir) on one filesystem,
        // bindir and sharedir on the other.
        let layout = InstallLayout {
            libdir: on_tmp.path().join("lib/firecrab"),
            bindir: here.path().join("bin"),
            sharedir: here.path().join("share/firecrab"),
        };
        for dir in [&layout.bindir, &layout.libdir, &layout.sharedir] {
            fs::create_dir_all(dir).expect("create layout dir");
        }
        fs::write(layout.bindir.join("firecrab"), b"old").expect("seed cli");
        fs::create_dir_all(layout.sharedir.join("dashboard")).expect("seed dashboard");
        fs::write(layout.sharedir.join("dashboard/index.html"), b"old").expect("seed index");
        let (path, sha) = write_bundle(on_tmp.path(), &release_members());

        runtime()
            .block_on(apply_bundle_against(&layout, &layout, &path, &sha))
            .expect("a layout spanning two filesystems must still apply");

        assert_eq!(
            fs::read(layout.bindir.join("firecrab")).unwrap(),
            b"new-cli"
        );
        assert_eq!(
            fs::read(layout.sharedir.join("dashboard/index.html")).unwrap(),
            b"new-index"
        );
        assert_eq!(
            fs::read(layout.libdir.join("firecrab-api")).unwrap(),
            b"new-api"
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

        let result = apply_bundle_against(&layout, &layout, &path, &sha).await;
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
        // The failed target's own scratch copy is cleaned up too.
        let leftovers: Vec<_> = fs::read_dir(&layout.bindir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".new-"))
            .collect();
        assert!(leftovers.is_empty(), "scratch left behind: {leftovers:?}");
    }
}
