//! Remove old shim output only from verified, stopped service runtimes.
//!
//! Directory descriptors and `O_NOFOLLOW` prevent symlink traversal. A runtime
//! must have a valid exit report and its private log lock must be available.
//! Anything uncertain is retained.

use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};
use std::time::{Duration, SystemTime};

use uuid::Uuid;

use crate::artifacts::VmArtifactPaths;
use crate::state::AppState;

const RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Open one directory without resolving a symbolic link.
fn open_dir_at(parent: i32, name: &CStr) -> io::Result<OwnedFd> {
    // SAFETY: name is a NUL-terminated string and parent is an open directory.
    let fd = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful openat returns a fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Resolve an absolute path one non-symlink component at a time.
fn absolute_directory(path: &Path) -> io::Result<OwnedFd> {
    if !path.is_absolute() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "relative path"));
    }
    // SAFETY: the root path is a static NUL-terminated string.
    let fd = unsafe {
        libc::open(
            c"/".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful open returns a fresh owned descriptor.
    let mut current = unsafe { OwnedFd::from_raw_fd(fd) };
    for part in path.components() {
        match part {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                let name = CString::new(name.as_bytes())
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid path"))?;
                current = open_dir_at(current.as_raw_fd(), &name)?;
            }
            _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, "unsafe path")),
        }
    }
    Ok(current)
}

/// Check that untrusted accounts cannot alter a runtime directory.
fn private_directory(fd: &OwnedFd) -> bool {
    // SAFETY: zero is a valid initial bit pattern for stat; fstat fills it.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: fd is open and stat points to valid writable memory.
    unsafe {
        libc::fstat(fd.as_raw_fd(), &mut stat) == 0
            && stat.st_uid == libc::geteuid()
            && stat.st_mode & 0o077 == 0
    }
}

/// Open a service-owned ordinary file without following links.
fn open_file_at(parent: i32, name: &CStr, write: bool) -> Option<File> {
    let flags = if write { libc::O_RDWR } else { libc::O_RDONLY };
    // SAFETY: name is NUL-terminated and parent is an open directory.
    let fd = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return None;
    }
    // SAFETY: a successful openat returns a fresh owned descriptor.
    let file = unsafe { File::from_raw_fd(fd) };
    let meta = file.metadata().ok()?;
    // Only service-owned ordinary files with no external hard links qualify.
    // Group/other read bits are harmless inside a private runtime directory.
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o022 != 0
    {
        return None;
    }
    Some(file)
}

/// Read a small verified metadata file within a fixed size limit.
fn read_file(parent: i32, name: &CStr, limit: u64) -> Option<Vec<u8>> {
    let file = open_file_at(parent, name, false)?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

/// Require both a complete exit report and an inactive capture status.
fn stopped_runtime(fd: i32) -> bool {
    let Some(bytes) = read_file(fd, c"exit.json", 4096) else {
        return false;
    };
    let Ok(exit) = serde_json::from_slice::<crate::vm_shim::protocol::ExitReport>(&bytes) else {
        return false;
    };
    if exit.status.code.is_some() == exit.status.signal.is_some() {
        return false;
    }
    matches!(
        read_file(fd, c"app.status", 32).as_deref(),
        Some(b"inactive" | b"error")
    )
}

#[allow(clippy::unnecessary_cast)] // dev_t has different widths on Linux and macOS.
/// Unlink only the unchanged, single-link file after it exceeds retention.
fn remove_expired(fd: i32, name: &CStr, before: SystemTime) {
    let Some(file) = open_file_at(fd, name, false) else {
        return;
    };
    let Ok(meta) = file.metadata() else {
        return;
    };
    if !meta.modified().is_ok_and(|modified| modified < before) {
        return;
    }
    // Recheck the directory entry without following a replacement symlink.
    // The containing runtime directory is private and its writer lock is held.
    // SAFETY: stat is initialized by fstatat and the name is NUL-terminated.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatat(fd, name.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) } != 0
        || stat.st_dev as u64 != meta.dev()
        || stat.st_ino as u64 != meta.ino()
        || stat.st_mode & libc::S_IFMT != libc::S_IFREG
        || stat.st_nlink != 1
    {
        return;
    }
    // SAFETY: fd names the locked private runtime directory; unlinkat never follows links.
    let _ = unsafe { libc::unlinkat(fd, name.as_ptr(), 0) };
}

/// Own a directory stream and close its transferred descriptor on drop.
struct Directory(*mut libc::DIR);
impl Drop for Directory {
    fn drop(&mut self) {
        // SAFETY: this object owns the DIR returned by fdopendir.
        unsafe { libc::closedir(self.0) };
    }
}

/// Inspect one service VM's known runtimes while holding each writer lock.
fn prune_vm(
    vm_dir: &Path,
    current: Uuid,
    current_stopped: bool,
    now: SystemTime,
) -> io::Result<()> {
    let Some(before) = now.checked_sub(RETENTION) else {
        return Ok(());
    };
    let vm = absolute_directory(vm_dir)?;
    if !private_directory(&vm) {
        return Ok(());
    }
    let runtimes = open_dir_at(vm.as_raw_fd(), c"r")?;
    if !private_directory(&runtimes) {
        return Ok(());
    }
    // SAFETY: dup creates a separate descriptor transferred to fdopendir.
    let copy = unsafe { libc::dup(runtimes.as_raw_fd()) };
    if copy < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: copy is an open directory descriptor; fdopendir takes ownership on success.
    let directory = unsafe { libc::fdopendir(copy) };
    if directory.is_null() {
        // SAFETY: fdopendir failed, so copy is still ours to close.
        unsafe { libc::close(copy) };
        return Err(io::Error::last_os_error());
    }
    let directory = Directory(directory);
    loop {
        // SAFETY: directory is owned and remains alive while the entry is read.
        let entry = unsafe { libc::readdir(directory.0) };
        if entry.is_null() {
            break;
        }
        // SAFETY: d_name is NUL-terminated and valid until the next readdir.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        let bytes = name.to_bytes();
        if bytes.len() != 32
            || !bytes
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        {
            continue;
        }
        let Ok(text) = std::str::from_utf8(bytes) else {
            continue;
        };
        let Ok(id) = Uuid::parse_str(text) else {
            continue;
        };
        if id == current && !current_stopped {
            continue;
        }
        let Ok(runtime) = open_dir_at(runtimes.as_raw_fd(), name) else {
            continue;
        };
        if !private_directory(&runtime) || !stopped_runtime(runtime.as_raw_fd()) {
            continue;
        }
        let Some(lock) = open_file_at(runtime.as_raw_fd(), c"app.lock", true) else {
            continue;
        };
        // SAFETY: lock is an open private file. A nonblocking exclusive flock
        // coordinates with the shim writer and any concurrent reader.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            continue;
        }
        remove_expired(runtime.as_raw_fd(), c"app.jsonl", before);
        remove_expired(runtime.as_raw_fd(), c"app.jsonl.1", before);
    }
    Ok(())
}

/// Only examine VMs still owned by a service and with a known current runtime.
pub(crate) fn prune(state: &AppState) {
    let vms: Vec<_> = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
        .cloned()
        .collect();
    for vm in vms {
        let Some(current) = vm.last_runtime_id else {
            continue;
        };
        if !matches!(state.store.vm_has_service_owner(vm.id), Ok(true)) {
            continue;
        }
        let path = VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id);
        let current_stopped = vm.state == firecrab_api_types::VmState::Stopped;
        if prune_vm(&path.dir, current, current_stopped, SystemTime::now()).is_err() {
            tracing::warn!(vm_id = %vm.id, "Could not prune expired application log files");
        }
    }
}

/// Periodically remove old files independently of database retention.
pub(crate) fn start(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let state = state.clone();
            if tokio::task::spawn_blocking(move || prune(&state))
                .await
                .is_err()
            {
                tracing::warn!("Could not prune expired application log files");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::PathBuf;

    fn private_dir(path: &Path) {
        fs::create_dir_all(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn runtime(parent: &Path, id: Uuid) -> PathBuf {
        let path = parent.join(id.simple().to_string());
        private_dir(&path);
        fs::write(
            path.join("exit.json"),
            b"{\"code\":0,\"signal\":null,\"stop_requested\":true}",
        )
        .unwrap();
        fs::write(path.join("app.status"), b"inactive").unwrap();
        fs::write(path.join("app.lock"), b"").unwrap();
        path
    }

    fn old_file(path: &Path) {
        fs::write(path, b"application output").unwrap();
        filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(1, 0)).unwrap();
    }

    #[test]
    fn removes_only_expired_logs_from_verified_inactive_runtimes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let vm = root.join("vm");
        let runtimes = vm.join("r");
        private_dir(&vm);
        private_dir(&runtimes);
        let current_id = Uuid::new_v4();
        let current = runtime(&runtimes, current_id);
        let old = runtime(&runtimes, Uuid::new_v4());
        let unknown = runtime(&runtimes, Uuid::new_v4());
        let running = runtime(&runtimes, Uuid::new_v4());
        let locked = runtime(&runtimes, Uuid::new_v4());
        for dir in [&current, &old, &unknown, &running, &locked] {
            old_file(&dir.join("app.jsonl"));
        }
        old_file(&old.join("app.jsonl.1"));
        old_file(&old.join("console.log"));
        fs::remove_file(unknown.join("exit.json")).unwrap();
        fs::write(running.join("app.status"), "active").unwrap();
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(locked.join("app.lock"))
            .unwrap();
        // SAFETY: lock owns an open file descriptor for the duration of the test.
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );

        prune_vm(&vm, current_id, false, SystemTime::now()).unwrap();
        assert!(!old.join("app.jsonl").exists());
        assert!(!old.join("app.jsonl.1").exists());
        assert!(old.join("console.log").exists());
        for dir in [&current, &unknown, &running, &locked] {
            assert!(dir.join("app.jsonl").exists());
        }
        // The latest runtime may be pruned only after the VM is stopped and
        // the same exit, status and lock checks succeed.
        prune_vm(&vm, current_id, true, SystemTime::now()).unwrap();
        assert!(!current.join("app.jsonl").exists());
        for dir in [&unknown, &running, &locked] {
            assert!(dir.join("app.jsonl").exists());
        }
    }

    #[test]
    fn retains_recent_files_symlinks_hardlinks_and_unknown_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let vm = root.join("vm");
        let runtimes = vm.join("r");
        private_dir(&vm);
        private_dir(&runtimes);
        let current = Uuid::new_v4();
        let valid = runtime(&runtimes, Uuid::new_v4());
        fs::write(valid.join("app.jsonl"), b"recent").unwrap();
        let victim = root.join("user-file");
        old_file(&victim);
        symlink(&victim, valid.join("app.jsonl.1")).unwrap();
        let hardlinked = runtime(&runtimes, Uuid::new_v4());
        fs::hard_link(&victim, hardlinked.join("app.jsonl")).unwrap();
        let unknown = runtimes.join("not-a-runtime");
        private_dir(&unknown);
        old_file(&unknown.join("app.jsonl"));
        let external = root.join("outside");
        private_dir(&external);
        old_file(&external.join("app.jsonl"));
        symlink(
            &external,
            runtimes.join(Uuid::new_v4().simple().to_string()),
        )
        .unwrap();

        prune_vm(&vm, current, false, SystemTime::now()).unwrap();
        assert!(valid.join("app.jsonl").exists());
        assert!(
            fs::symlink_metadata(valid.join("app.jsonl.1"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(hardlinked.join("app.jsonl").exists());
        assert!(unknown.join("app.jsonl").exists());
        assert!(victim.exists() && external.join("app.jsonl").exists());
        let symlink_vm = root.join("symlink-vm");
        private_dir(&symlink_vm);
        symlink(&runtimes, symlink_vm.join("r")).unwrap();
        assert!(prune_vm(&symlink_vm, current, false, SystemTime::now()).is_err());
        assert!(valid.join("app.jsonl").exists());
    }
}
