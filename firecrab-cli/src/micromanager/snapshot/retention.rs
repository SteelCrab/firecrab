//! Bound disk backups and diagnostic logs independently.
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Local};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::Snapshot;
use crate::micromanager::{managed_home, os_repair::CheckResult, report};

const HISTORY_LIMIT: usize = 10;
const STATE_FILE: &str = "retention.json";
// Host-local time and numeric UTC offset; all characters are valid on Windows.
const DATE_FORMAT: &str = "%Y-%m-%d_%H-%M-%S%z";
const MAC_IMAGES: &[&str] = &[
    "debian-system.before.raw",
    "debian-system.repaired.raw",
    "debian-system.reference.raw",
];
const WINDOWS_IMAGES: &[&str] = &["ext4.before.vhdx", "ext4.repaired.vhdx"];
const DIAGNOSTIC_FILES: &[&str] = &["result.json", "console.log", "error.log"];
const LOG_FILES: &[&str] = &[
    STATE_FILE,
    "snapshots.json",
    "result.json",
    "console.log",
    "error.log",
];

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Repaired,
    Failed,
    Clean,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema: u32,
    outcome: Outcome,
    completed_at_unix_micros: u64,
    // Preserve completeness evidence even after the corresponding logs expire.
    #[serde(default)]
    backup_bytes: Option<u64>,
}

#[derive(Debug)]
pub(in crate::micromanager) struct Repair {
    directory: PathBuf,
    logs: PathBuf,
    unchanged: bool,
    finished: bool,
    // Hold through filesystem checks, service restart, and retention cleanup.
    lock: File,
}

fn private_directory(path: &Path) -> io::Result<PathBuf> {
    managed_home::reject_symlink_components(path).map_err(io::Error::other)?;
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    fs::canonicalize(path)
}

fn log_root(repairs: &Path) -> io::Result<PathBuf> {
    Ok(repairs
        .parent()
        .ok_or_else(|| io::Error::other("repair directory has no parent"))?
        .join("logs/repairs"))
}

impl Repair {
    pub(in crate::micromanager) fn begin(repairs: &Path, prefix: &str) -> io::Result<Self> {
        if !matches!(prefix, "os-" | "vhd-") {
            return Err(io::Error::other("unknown repair snapshot prefix"));
        }
        let repairs = private_directory(repairs)?;
        let logs_root = private_directory(&log_root(&repairs)?)?;
        let lock_path = repairs
            .parent()
            .expect("repair has a parent")
            .join("snapshot.lock");
        managed_home::reject_symlink_components(&lock_path).map_err(io::Error::other)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        FileExt::try_lock_exclusive(&lock)
            .map_err(|_| io::Error::other("another repair owns the snapshots"))?;
        let prefix = format!("{prefix}{}-", Local::now().format(DATE_FORMAT));
        let staged = tempfile::Builder::new()
            .prefix(&prefix)
            .tempdir_in(&repairs)?;
        let logs = logs_root.join(staged.path().file_name().expect("repair has a name"));
        fs::create_dir(&logs)?;
        let logs = private_directory(&logs)?;
        let repair = Self {
            directory: staged.keep(),
            logs,
            unchanged: false,
            finished: false,
            lock,
        };
        // An interrupted repair remains failed until the final service/API checks.
        write_state(&repair.directory, Outcome::Failed, now())?;
        write_state(&repair.logs, Outcome::Failed, now())?;
        report!("[LOGS] repair: {}", repair.logs.display());
        cleanup(&repairs, Some(&repair.directory), None);
        Ok(repair)
    }

    pub(in crate::micromanager) fn directory(&self) -> &Path {
        &self.directory
    }
    pub(in crate::micromanager) fn logs(&self) -> &Path {
        &self.logs
    }

    pub(in crate::micromanager) fn record(
        &self,
        backup: &Snapshot,
        working: &Snapshot,
    ) -> io::Result<()> {
        super::record(&self.directory, backup, working)?;
        super::record(&self.logs, backup, working)
    }

    /// Called only after the full repair verifies the running guest and API.
    pub(in crate::micromanager) fn commit(mut self) {
        let outcome = if self.unchanged {
            Outcome::Clean
        } else {
            Outcome::Repaired
        };
        self.finish(outcome);
    }

    pub(in crate::micromanager) fn discard(mut self) -> io::Result<Self> {
        fs::remove_dir_all(&self.directory)?;
        self.unchanged = true;
        // Keep logs and the lock until the final service/API checks finish.
        cleanup(
            self.directory.parent().expect("repair has a parent"),
            Some(&self.directory),
            None,
        );
        Ok(self)
    }

    fn finish(&mut self, outcome: Outcome) {
        self.finished = true;
        let completed = now();
        let result = (|| -> io::Result<()> {
            if !self.unchanged {
                write_state(&self.directory, outcome, completed)?;
            }
            write_state(&self.logs, outcome, completed)
        })();
        if let Err(error) = result {
            report!(
                "[WARN] repair retention state not saved; backups: {}; logs: {}: {error}",
                self.directory.display(),
                self.logs.display()
            );
            return;
        }
        cleanup(
            self.directory.parent().expect("repair has a parent"),
            None,
            Some(&self.directory),
        );
    }
}

impl Drop for Repair {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(Outcome::Failed);
        }
        // Explicit unlock also releases copies briefly inherited by forked probes.
        let _ = FileExt::unlock(&self.lock);
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .min(u64::MAX as u128) as u64
}

fn write_state(
    directory: &Path,
    outcome: Outcome,
    completed_at_unix_micros: u64,
) -> io::Result<()> {
    let backup_bytes =
        image_names(directory).and_then(|images| confirmed_backup_bytes(directory, images));
    save_state(
        directory,
        &State {
            schema: 1,
            outcome,
            completed_at_unix_micros,
            backup_bytes,
        },
    )
}

fn save_state(directory: &Path, state: &State) -> io::Result<()> {
    managed_home::reject_symlink_components(&directory.join(STATE_FILE))
        .map_err(io::Error::other)?;
    let staged = tempfile::Builder::new()
        .prefix(".retention-")
        .tempfile_in(directory)?;
    serde_json::to_writer_pretty(staged.as_file(), state).map_err(io::Error::other)?;
    staged.as_file().sync_all()?;
    staged
        .persist(directory.join(STATE_FILE))
        .map_err(|error| error.error)?;
    #[cfg(unix)]
    File::open(directory)?.sync_all()?;
    Ok(())
}

fn image_names(directory: &Path) -> Option<&'static [&'static str]> {
    let name = directory.file_name()?.to_str()?;
    let (suffix, images) = if let Some(suffix) = name.strip_prefix("os-") {
        (suffix, MAC_IMAGES)
    } else {
        (name.strip_prefix("vhd-")?, WINDOWS_IMAGES)
    };
    let random = if suffix.len() == 6 {
        suffix
    } else {
        let (timestamp, random) = suffix.rsplit_once('-')?;
        let date = DateTime::parse_from_str(timestamp, DATE_FORMAT).ok()?;
        if date.format(DATE_FORMAT).to_string() != timestamp {
            return None;
        }
        random
    };
    (random.len() == 6 && random.bytes().all(|byte| byte.is_ascii_alphanumeric())).then_some(images)
}

/// Never recurse into user files, links, or directories with an unknown layout.
fn managed_files(directory: &Path, images: &[&str]) -> io::Result<Option<Vec<PathBuf>>> {
    if !fs::symlink_metadata(directory)?.is_dir() {
        return Ok(None);
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Ok(None);
        };
        let temporary_state = name.strip_prefix(".retention-").is_some_and(|suffix| {
            suffix.len() == 6 && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
        });
        if !entry.file_type()?.is_file()
            || !(images.contains(&name) || LOG_FILES.contains(&name) || temporary_state)
        {
            return Ok(None);
        }
        files.push(entry.path());
    }
    Ok(Some(files))
}

struct Record {
    directory: PathBuf,
    images: &'static [&'static str],
    state: State,
}

fn confirmed_backup_bytes(directory: &Path, images: &[&str]) -> Option<u64> {
    let bytes = fs::metadata(directory.join(images[0])).ok()?.len();
    if bytes == 0 {
        return None;
    }
    match fs::read(directory.join("snapshots.json")) {
        Ok(text) => serde_json::from_slice::<serde_json::Value>(&text)
            .ok()
            .and_then(|value| value["backup"]["logical_bytes"].as_u64())
            .filter(|expected| *expected == bytes),
        // Older helpers create diagnostics only after both copies exist.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            (directory.join("result.json").is_file() || directory.join("console.log").is_file())
                .then_some(bytes)
        }
        Err(_) => None,
    }
}

fn complete_backup(record: &Record) -> bool {
    record.state.backup_bytes.is_some_and(|bytes| {
        bytes > 0
            && fs::metadata(record.directory.join(record.images[0]))
                .is_ok_and(|metadata| metadata.len() == bytes)
    })
}

fn read_record(directory: PathBuf) -> io::Result<Option<Record>> {
    let Some(images) = image_names(&directory) else {
        return Ok(None);
    };
    if managed_files(&directory, images)?.is_none() {
        return Ok(None);
    }
    let (mut state, mut needs_save) = match fs::read(directory.join(STATE_FILE)) {
        Ok(bytes) => match serde_json::from_slice::<State>(&bytes) {
            Ok(state) if state.schema == 1 => (state, false),
            _ => return Ok(None),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if !directory.join(images[0]).is_file() {
                return Ok(None);
            }
            let outcome = fs::read(directory.join("result.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<CheckResult>(&bytes).ok())
                .filter(|result| {
                    result.verified() && result.changed && !directory.join(images[1]).exists()
                })
                .map_or(Outcome::Failed, |_| Outcome::Repaired);
            let completed_at_unix_micros = fs::metadata(&directory)?
                .modified()?
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros()
                .min(u64::MAX as u128) as u64;
            (
                State {
                    schema: 1,
                    outcome,
                    completed_at_unix_micros,
                    backup_bytes: None,
                },
                true,
            )
        }
        Err(error) => return Err(error),
    };
    if state.backup_bytes.is_none() {
        state.backup_bytes = confirmed_backup_bytes(&directory, images);
        needs_save |= state.backup_bytes.is_some();
    }
    // Save legacy completeness evidence before moving logs or expiring them.
    if needs_save {
        save_state(&directory, &state)?;
    }
    Ok(Some(Record {
        directory,
        images,
        state,
    }))
}

fn migrate_logs(record: &Record, logs_root: &Path) -> io::Result<()> {
    if !DIAGNOSTIC_FILES
        .iter()
        .any(|name| record.directory.join(name).exists())
    {
        return Ok(());
    }
    let logs = logs_root.join(record.directory.file_name().expect("repair has a name"));
    private_directory(&logs)?;
    if managed_files(&logs, &[])?.is_none() {
        return Err(io::Error::other("unrecognized repair log directory"));
    }
    match fs::read(logs.join(STATE_FILE)) {
        Ok(bytes) => {
            if !serde_json::from_slice::<State>(&bytes).is_ok_and(|state| state.schema == 1) {
                return Err(io::Error::other("unrecognized repair log state"));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    save_state(&logs, &record.state)?;
    for name in DIAGNOSTIC_FILES {
        let source = record.directory.join(name);
        if !source.exists() {
            continue;
        }
        let target = logs.join(name);
        match fs::symlink_metadata(&target) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
            Ok(_) => {
                return Err(io::Error::other(format!(
                    "repair log already exists: {}",
                    target.display()
                )));
            }
        }
        fs::rename(source, target)?;
    }
    #[cfg(unix)]
    {
        File::open(&logs)?.sync_all()?;
        File::open(&record.directory)?.sync_all()?;
    }
    Ok(())
}

fn sort_records(records: &mut [Record], completed: Option<&Path>) {
    let completed = completed.and_then(Path::file_name);
    records.sort_by(|a, b| {
        (completed == b.directory.file_name())
            .cmp(&(completed == a.directory.file_name()))
            .then_with(|| {
                b.state
                    .completed_at_unix_micros
                    .cmp(&a.state.completed_at_unix_micros)
            })
            .then_with(|| b.directory.cmp(&a.directory))
    });
}

fn remove_record(record: &Record) -> io::Result<()> {
    let files = managed_files(&record.directory, record.images)?
        .ok_or_else(|| io::Error::other("repair files changed during cleanup"))?;
    for file in files {
        fs::remove_file(file)?;
    }
    fs::remove_dir(&record.directory)
}

fn cleanup(repairs: &Path, active: Option<&Path>, completed: Option<&Path>) {
    if let Err(error) = prune(repairs, active, completed) {
        report!(
            "[WARN] repair cleanup incomplete at {}: {error}; retained files are available for inspection",
            repairs.display()
        );
    }
}

fn prune(repairs: &Path, active: Option<&Path>, completed: Option<&Path>) -> io::Result<()> {
    managed_home::reject_symlink_components(repairs).map_err(io::Error::other)?;
    let logs_root = log_root(repairs)?;
    managed_home::reject_symlink_components(&logs_root).map_err(io::Error::other)?;
    let mut records = Vec::new();
    for entry in fs::read_dir(repairs)? {
        let entry = entry?;
        if active == Some(entry.path().as_path()) || !entry.file_type()?.is_dir() {
            continue;
        }
        if let Some(record) = read_record(entry.path())? {
            migrate_logs(&record, &logs_root)?;
            records.push(record);
        }
    }
    sort_records(&mut records, completed);
    let rollback = records.iter().position(|record| {
        record.state.outcome == Outcome::Repaired
            && record.directory.join(record.images[0]).is_file()
    });
    let failed = records
        .iter()
        .position(|record| record.state.outcome == Outcome::Failed && complete_backup(record))
        .or_else(|| {
            records
                .iter()
                .position(|record| record.state.outcome == Outcome::Failed)
        });
    let mut removed_images = 0;
    for (index, record) in records.iter().enumerate() {
        let keep = rollback == Some(index) || failed == Some(index);
        for name in record.images {
            if keep && !name.ends_with(".reference.raw") {
                continue;
            }
            match fs::remove_file(record.directory.join(name)) {
                Ok(()) => removed_images += 1,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        if !keep {
            remove_record(record)?;
        }
    }
    let removed_logs = prune_logs(&logs_root, active, completed)?;
    if removed_images > 0 || removed_logs > 0 {
        report!(
            "[CLEANUP] removed {removed_images} obsolete disk files and {removed_logs} old log records; keeping the latest rollback and failed backup, up to {HISTORY_LIMIT} separate log records"
        );
    }
    Ok(())
}

fn prune_logs(
    logs_root: &Path,
    active: Option<&Path>,
    completed: Option<&Path>,
) -> io::Result<usize> {
    let mut records = Vec::new();
    for entry in fs::read_dir(logs_root)? {
        let entry = entry?;
        let directory = entry.path();
        if active.and_then(Path::file_name) == directory.file_name()
            || !entry.file_type()?.is_dir()
            || image_names(&directory).is_none()
            || managed_files(&directory, &[])?.is_none()
        {
            continue;
        }
        let state = match fs::read(directory.join(STATE_FILE)) {
            Ok(bytes) => match serde_json::from_slice::<State>(&bytes) {
                Ok(state) if state.schema == 1 => state,
                _ => continue,
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        records.push(Record {
            directory,
            images: &[],
            state,
        });
    }
    sort_records(&mut records, completed);
    let mut removed = 0;
    for record in records.iter().skip(HISTORY_LIMIT) {
        remove_record(record)?;
        removed += 1;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn staged(repairs: &Path, prefix: &str) -> Repair {
        let repair = Repair::begin(repairs, prefix).unwrap();
        for name in image_names(repair.directory()).unwrap() {
            fs::write(repair.directory().join(name), b"disk contents").unwrap();
        }
        fs::write(repair.logs().join("console.log"), b"diagnostic log").unwrap();
        fs::write(
            repair.directory().join("snapshots.json"),
            r#"{"backup":{"logical_bytes":13}}"#,
        )
        .unwrap();
        repair
    }

    fn repaired(repairs: &Path, prefix: &str) -> PathBuf {
        let repair = staged(repairs, prefix);
        let directory = repair.directory().to_path_buf();
        let images = image_names(&directory).unwrap();
        fs::remove_file(directory.join(images[1])).unwrap();
        repair.commit();
        directory
    }

    fn directories(repairs: &Path) -> Vec<PathBuf> {
        fs::read_dir(repairs)
            .unwrap()
            .map(|entry| fs::canonicalize(entry.unwrap().path()).unwrap())
            .collect()
    }

    fn logs_for(repairs: &Path, directory: &Path) -> PathBuf {
        log_root(repairs)
            .unwrap()
            .join(directory.file_name().unwrap())
    }

    #[test]
    fn backup_directories_record_creation_time_with_a_windows_safe_utc_offset() {
        for prefix in ["os-", "vhd-"] {
            let root = tempfile::tempdir().unwrap();
            let repairs = root.path().join("repairs");
            let started = Local::now();
            let repair = Repair::begin(&repairs, prefix).unwrap();
            let ended = Local::now();
            let name = repair.directory().file_name().unwrap().to_str().unwrap();
            let (timestamp, random) = name.strip_prefix(prefix).unwrap().rsplit_once('-').unwrap();
            let created = DateTime::parse_from_str(timestamp, DATE_FORMAT).unwrap();
            assert!((started.timestamp()..=ended.timestamp()).contains(&created.timestamp()));
            assert_eq!(random.len(), 6);
            assert!(!name.contains([':', '/', '\\', '*', '?', '"', '<', '>', '|']));
            assert!(image_names(repair.directory()).is_some());
            repair.discard().unwrap();
        }
    }

    #[test]
    fn cleanup_accepts_dated_and_legacy_names_but_rejects_invalid_dates_and_suffixes() {
        for prefix in ["os-", "vhd-"] {
            for suffix in [
                "Ab12Cd",
                "2026-10-15_23-45-12+0900-Ab12Cd",
                "2024-02-29_00-00-00-0700-Ab12Cd",
                "2026-10-15_14-45-12+0000-Ab12Cd",
            ] {
                assert!(
                    image_names(Path::new(&format!("{prefix}{suffix}"))).is_some(),
                    "{suffix}"
                );
            }
            for suffix in [
                "2026-02-29_23-45-12+0900-Ab12Cd",
                "2026-10-15_24-45-12+0900-Ab12Cd",
                "2026-10-15_23-45-12-Ab12Cd",
                "2026-10-15_23-45-12+0900-",
                "2026-10-15_23-45-12+0900-Ab12Cd-extra",
                "backup-Ab12Cd",
            ] {
                assert!(
                    image_names(Path::new(&format!("{prefix}{suffix}"))).is_none(),
                    "{suffix}"
                );
            }
        }
    }

    #[test]
    fn repeated_repairs_bound_disk_storage_and_log_history_on_both_platforms() {
        for prefix in ["os-", "vhd-"] {
            let root = tempfile::tempdir().unwrap();
            let repairs = root.path().join("repairs");
            let mut rollback = PathBuf::new();
            let mut failed = PathBuf::new();
            for _ in 0..15 {
                rollback = repaired(&repairs, prefix);
                let failure = staged(&repairs, prefix);
                failed = failure.directory().to_path_buf();
                drop(failure);
            }
            let records = directories(&repairs);
            assert_eq!(records.len(), 2);
            assert_eq!(
                directories(&log_root(&repairs).unwrap()).len(),
                HISTORY_LIMIT
            );
            let images = image_names(&rollback).unwrap();
            assert!(rollback.join(images[0]).is_file());
            assert!(failed.join(images[0]).is_file());
            assert!(failed.join(images[1]).is_file());
            let mut disk_count = 0;
            for directory in records {
                assert!(!directory.join("console.log").exists());
                for name in images {
                    disk_count += directory.join(name).exists() as usize;
                }
                assert!(!directory.join("debian-system.reference.raw").exists());
            }
            assert_eq!(disk_count, 3);
        }
    }

    #[test]
    fn uncommitted_repairs_preserve_the_last_healthy_rollback_even_after_disk_install() {
        let root = tempfile::tempdir().unwrap();
        let repairs = root.path().join("repairs");
        let rollback = repaired(&repairs, "os-");
        let pending = staged(&repairs, "os-");
        let failed = pending.directory().to_path_buf();
        fs::remove_file(failed.join(MAC_IMAGES[1])).unwrap();
        assert!(rollback.join(MAC_IMAGES[0]).is_file());
        drop(pending); // The guest/API did not become healthy.
        assert!(rollback.join(MAC_IMAGES[0]).is_file());
        assert!(failed.join(MAC_IMAGES[0]).is_file());
        let latest = repaired(&repairs, "os-");
        assert!(latest.join(MAC_IMAGES[0]).is_file());
        assert!(!rollback.join(MAC_IMAGES[0]).exists());
        assert!(logs_for(&repairs, &rollback).join("console.log").is_file());
        assert!(failed.join(MAC_IMAGES[0]).is_file());
    }

    #[test]
    fn an_old_rollback_is_kept_even_when_newer_failures_fill_the_history_limit() {
        let root = tempfile::tempdir().unwrap();
        let repairs = root.path().join("repairs");
        let rollback = repaired(&repairs, "vhd-");
        for _ in 0..20 {
            drop(staged(&repairs, "vhd-"));
        }
        assert_eq!(directories(&repairs).len(), 2);
        assert_eq!(
            directories(&log_root(&repairs).unwrap()).len(),
            HISTORY_LIMIT
        );
        assert!(!logs_for(&repairs, &rollback).exists());
        assert!(rollback.join(WINDOWS_IMAGES[0]).is_file());
    }

    #[test]
    fn a_clean_attempt_is_removed_without_rotating_the_rollback() {
        let root = tempfile::tempdir().unwrap();
        let repairs = root.path().join("repairs");
        let rollback = repaired(&repairs, "os-");
        let clean = staged(&repairs, "os-");
        let directory = clean.directory().to_path_buf();
        clean.discard().unwrap().commit();
        assert!(!directory.exists());
        assert_eq!(
            directories(&repairs).as_slice(),
            std::slice::from_ref(&rollback)
        );
        assert!(rollback.join(MAC_IMAGES[0]).is_file());
        let logs = logs_for(&repairs, &directory);
        assert!(logs.join("console.log").is_file());
        let state: State =
            serde_json::from_slice(&fs::read(logs.join(STATE_FILE)).unwrap()).unwrap();
        assert_eq!(state.outcome, Outcome::Clean);
    }

    #[test]
    fn clean_checks_keep_separate_logs_without_retaining_disk_copies() {
        let root = tempfile::tempdir().unwrap();
        let repairs = root.path().join("repairs");
        for _ in 0..15 {
            staged(&repairs, "os-").discard().unwrap().commit();
        }
        assert!(directories(&repairs).is_empty());
        let records = directories(&log_root(&repairs).unwrap());
        assert_eq!(records.len(), HISTORY_LIMIT);
        for directory in records {
            assert!(directory.join("console.log").is_file());
            assert!(MAC_IMAGES.iter().all(|name| !directory.join(name).exists()));
        }
    }

    #[test]
    fn legacy_failure_backup_remains_complete_after_its_logs_expire() {
        let root = tempfile::tempdir().unwrap();
        let repairs = root.path().join("repairs");
        let legacy = repairs.join("os-old001");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join(MAC_IMAGES[0]), b"complete backup").unwrap();
        fs::write(legacy.join(MAC_IMAGES[1]), b"failed working disk").unwrap();
        fs::write(legacy.join("console.log"), b"legacy console").unwrap();
        fs::write(legacy.join("error.log"), b"legacy error").unwrap();
        for _ in 0..12 {
            staged(&repairs, "os-").discard().unwrap().commit();
        }
        assert!(!logs_for(&repairs, &legacy).exists());
        assert!(!legacy.join("console.log").exists());
        let partial = Repair::begin(&repairs, "os-").unwrap();
        fs::write(partial.directory().join(MAC_IMAGES[0]), b"partial").unwrap();
        drop(partial);
        assert_eq!(
            fs::read(legacy.join(MAC_IMAGES[0])).unwrap(),
            b"complete backup"
        );
        assert_eq!(
            fs::read(legacy.join(MAC_IMAGES[1])).unwrap(),
            b"failed working disk"
        );
    }

    #[test]
    fn legacy_log_migration_refuses_to_overwrite_existing_diagnostics() {
        let root = tempfile::tempdir().unwrap();
        let repairs = root.path().join("repairs");
        let legacy = repairs.join("os-old001");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join(MAC_IMAGES[0]), b"rollback").unwrap();
        fs::write(legacy.join("console.log"), b"original console").unwrap();
        let logs = logs_for(&repairs, &legacy);
        fs::create_dir_all(&logs).unwrap();
        fs::write(logs.join("console.log"), b"existing console").unwrap();
        Repair::begin(&repairs, "os-")
            .unwrap()
            .discard()
            .unwrap()
            .commit();
        assert_eq!(
            fs::read(legacy.join("console.log")).unwrap(),
            b"original console"
        );
        assert_eq!(
            fs::read(logs.join("console.log")).unwrap(),
            b"existing console"
        );
        assert_eq!(fs::read(legacy.join(MAC_IMAGES[0])).unwrap(), b"rollback");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_log_root_is_rejected_before_creating_an_attempt() {
        let root = tempfile::tempdir().unwrap();
        let repairs = root.path().join("repairs");
        let outside = root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.path().join("logs")).unwrap();
        assert!(Repair::begin(&repairs, "os-").is_err());
        assert!(directories(&repairs).is_empty());
        assert!(directories(&outside).is_empty());
    }

    #[test]
    fn a_missing_latest_backup_does_not_retire_the_last_available_rollback() {
        let root = tempfile::tempdir().unwrap();
        let repairs = root.path().join("repairs");
        let rollback = repaired(&repairs, "os-");
        let pending = staged(&repairs, "os-");
        fs::remove_file(pending.directory().join(MAC_IMAGES[0])).unwrap();
        fs::remove_file(pending.directory().join(MAC_IMAGES[1])).unwrap();
        pending.commit();
        assert!(rollback.join(MAC_IMAGES[0]).is_file());
    }

    #[test]
    fn the_just_completed_backup_wins_after_the_wall_clock_moves_backwards() {
        let root = tempfile::tempdir().unwrap();
        let repairs = root.path().join("repairs");
        let old = repaired(&repairs, "os-");
        write_state(&old, Outcome::Repaired, u64::MAX).unwrap();
        let latest = repaired(&repairs, "os-");
        assert!(latest.join(MAC_IMAGES[0]).is_file());
        assert!(!old.join(MAC_IMAGES[0]).exists());
        Repair::begin(&repairs, "os-").unwrap().discard().unwrap();
        assert!(latest.join(MAC_IMAGES[0]).is_file());
    }

    #[test]
    fn snapshot_creation_failure_does_not_displace_a_complete_failure_backup() {
        let root = tempfile::tempdir().unwrap();
        let repairs = root.path().join("repairs");
        let first = staged(&repairs, "os-");
        let preserved = first.directory().to_path_buf();
        drop(first);
        for partial in [None, Some(b"partial".as_slice())] {
            let next = Repair::begin(&repairs, "os-").unwrap();
            let directory = next.directory().to_path_buf();
            if let Some(bytes) = partial {
                fs::write(directory.join(MAC_IMAGES[0]), bytes).unwrap();
            }
            drop(next);
            assert_eq!(
                fs::read(preserved.join(MAC_IMAGES[0])).unwrap(),
                b"disk contents"
            );
            assert_eq!(
                fs::read(preserved.join(MAC_IMAGES[1])).unwrap(),
                b"disk contents"
            );
            assert!(!directory.join(MAC_IMAGES[0]).exists());
        }
    }

    #[test]
    fn old_native_layouts_are_migrated_but_unknown_files_and_future_schemas_are_preserved() {
        let root = tempfile::tempdir().unwrap();
        let repairs = root.path().join("repairs");
        fs::create_dir(&repairs).unwrap();
        for (name, changed) in [("os-old001", false), ("os-old002", true)] {
            let directory = repairs.join(name);
            fs::create_dir(&directory).unwrap();
            fs::write(directory.join(MAC_IMAGES[0]), b"rollback").unwrap();
            fs::write(directory.join(MAC_IMAGES[2]), b"recreatable reference").unwrap();
            fs::write(directory.join("console.log"), b"legacy log").unwrap();
            fs::write(
                directory.join("result.json"),
                if changed {
                    r#"{"check_exit":4,"repair_exit":1,"verify_exit":0,"changed":true}"#
                } else {
                    "invalid result"
                },
            )
            .unwrap();
        }
        let unknown = repairs.join("os-user01");
        fs::create_dir(&unknown).unwrap();
        fs::write(unknown.join("user-notes.txt"), b"keep user content").unwrap();
        fs::write(unknown.join(MAC_IMAGES[0]), b"keep user backup").unwrap();
        let future = repairs.join("os-future");
        fs::create_dir(&future).unwrap();
        fs::write(
            future.join(STATE_FILE),
            r#"{"schema":2,"outcome":"repaired","completed_at_unix_micros":1}"#,
        )
        .unwrap();
        fs::write(future.join(MAC_IMAGES[0]), b"keep future backup").unwrap();
        let latest = repaired(&repairs, "os-");
        assert!(latest.join(MAC_IMAGES[0]).is_file());
        assert!(!repairs.join("os-old002").join(MAC_IMAGES[0]).exists());
        assert!(
            log_root(&repairs)
                .unwrap()
                .join("os-old002/console.log")
                .is_file()
        );
        assert!(!repairs.join("os-old001/console.log").exists());
        assert!(repairs.join("os-old001").join(MAC_IMAGES[0]).is_file());
        assert!(!repairs.join("os-old001").join(MAC_IMAGES[2]).exists());
        assert_eq!(
            fs::read(unknown.join(MAC_IMAGES[0])).unwrap(),
            b"keep user backup"
        );
        assert_eq!(
            fs::read(future.join(MAC_IMAGES[0])).unwrap(),
            b"keep future backup"
        );
    }

    #[test]
    fn retention_lock_lasts_until_the_full_repair_finishes() {
        let root = tempfile::tempdir().unwrap();
        let repairs = root.path().join("repairs");
        let first = staged(&repairs, "os-");
        assert!(Repair::begin(&repairs, "os-").is_err());
        assert_eq!(directories(&repairs).len(), 1);
        drop(first);
        Repair::begin(&repairs, "os-").unwrap().discard().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn state_save_failure_preserves_both_backups_without_failing_a_healthy_repair() {
        let root = tempfile::tempdir().unwrap();
        let repairs = root.path().join("repairs");
        let rollback = repaired(&repairs, "os-");
        let pending = staged(&repairs, "os-");
        let current = pending.directory().to_path_buf();
        let outside = root.path().join("outside-state");
        fs::write(&outside, b"keep outside state").unwrap();
        fs::remove_file(current.join(STATE_FILE)).unwrap();
        std::os::unix::fs::symlink(&outside, current.join(STATE_FILE)).unwrap();
        pending.commit();
        assert!(rollback.join(MAC_IMAGES[0]).is_file());
        assert!(current.join(MAC_IMAGES[0]).is_file());
        assert_eq!(fs::read(outside).unwrap(), b"keep outside state");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_records_images_roots_and_locks_do_not_delete_outside_files() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let repairs = root.path().join("repairs");
        fs::create_dir(&repairs).unwrap();
        let outside = root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join(MAC_IMAGES[0]), b"outside disk").unwrap();
        symlink(&outside, repairs.join("os-link01")).unwrap();
        let linked = repairs.join("os-link02");
        fs::create_dir(&linked).unwrap();
        symlink(outside.join(MAC_IMAGES[0]), linked.join(MAC_IMAGES[0])).unwrap();
        for _ in 0..12 {
            repaired(&repairs, "os-");
        }
        assert!(linked.join(MAC_IMAGES[0]).is_symlink());
        assert!(repairs.join("os-link01").is_symlink());
        let bad_root = root.path().join("linked-root");
        symlink(&outside, &bad_root).unwrap();
        assert!(Repair::begin(&bad_root, "os-").is_err());
        let lock = root.path().join("snapshot.lock");
        fs::remove_file(&lock).unwrap();
        symlink(outside.join(MAC_IMAGES[0]), &lock).unwrap();
        assert!(Repair::begin(&repairs, "os-").is_err());
        assert_eq!(
            fs::read(outside.join(MAC_IMAGES[0])).unwrap(),
            b"outside disk"
        );
    }
}
