//! Bounded application output owned by the VM shim, separate from ttyS0.
//!
//! Guest output is untrusted. A guest can forge records for its own VM, but
//! neither another guest nor an operator's console output shares this stream.
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{
    Arc,
    mpsc::{self, SyncSender, TrySendError},
};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::io::AsyncReadExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Semaphore, oneshot};
use tokio::task::{JoinHandle, JoinSet};

use crate::artifacts::HostRuntimePaths;

/// Firecracker forwards guest connections to `v.sock_<port>` in this runtime.
pub(crate) const PORT: u32 = 47101;
/// The guest and host exchange a fixed-size, per-boot key before any records.
pub(crate) const KEY_LEN: usize = 32;
/// Per-file limit; a previous file retains a second bounded window.
const FILE_LIMIT: u64 = 1024 * 1024;
/// A guest can never ask the host to allocate more than one small frame.
const FRAME_LIMIT: usize = 4096;
/// Limit guest connections and pending disk writes independently.
const CONNECTION_LIMIT: usize = 4;
const QUEUE_LIMIT: usize = 64;
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Capture availability for this VM runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Status {
    /// No new shim/key exists for this runtime.
    Unavailable,
    /// The shim is listening but has not accepted the app helper yet.
    Waiting,
    /// At least one authenticated app helper is connected.
    Active,
    /// The last helper disconnected, or the shim finished.
    Inactive,
    /// The listener or local log storage failed.
    Error,
}

/// One captured chunk. The host supplies both sequence and timestamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Record {
    /// Monotonic within one VM runtime, including after shim restart.
    pub sequence: u64,
    /// Host Unix time in milliseconds.
    pub timestamp: i64,
    /// `stdout`, `stderr`, or `system` for an explicit loss notice.
    pub stream: String,
    /// Lossy UTF-8 chunk; it is not necessarily a complete line.
    pub message: String,
}

/// A consistent page from both files; callers persist before advancing `next`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Batch {
    /// Last known capture state; old runtimes return unavailable.
    pub status: Status,
    /// Records strictly newer than the supplied sequence.
    pub records: Vec<Record>,
    /// Last returned sequence, or the original cursor when empty.
    pub next: u64,
    /// Rotation or malformed trailing data removed part of the requested range.
    pub truncated: bool,
}

fn path(runtime: &HostRuntimePaths, name: &str) -> PathBuf {
    runtime.dir.join(name)
}

/// Per-boot key file, created by the API in the private runtime directory.
pub(crate) fn key_path(runtime: &HostRuntimePaths) -> PathBuf {
    path(runtime, "app.key")
}

/// Save the matching per-boot key in the host's private runtime directory.
pub(crate) fn prepare(runtime: &HostRuntimePaths, key: &str) -> io::Result<()> {
    let path = key_path(runtime);
    let result = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut file| file.write_all(key.as_bytes()));
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

/// Report collection setup failure without preventing the application from booting.
pub(crate) fn mark_error(runtime: &HostRuntimePaths) {
    let _ = status(runtime, Status::Error);
}

fn socket_path(runtime: &HostRuntimePaths) -> PathBuf {
    let mut socket = runtime.vsock_socket.as_os_str().to_owned();
    socket.push(format!("_{PORT}"));
    PathBuf::from(socket)
}

fn status_path(runtime: &HostRuntimePaths) -> PathBuf {
    path(runtime, "app.status")
}

fn status(runtime: &HostRuntimePaths, status: Status) -> io::Result<()> {
    let value = match status {
        Status::Unavailable => "unavailable",
        Status::Waiting => "waiting",
        Status::Active => "active",
        Status::Inactive => "inactive",
        Status::Error => "error",
    };
    let temporary = path(runtime, "app.status.tmp");
    fs::write(&temporary, value)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    fs::rename(temporary, status_path(runtime))
}

fn current_status(runtime: &HostRuntimePaths) -> Status {
    match fs::read_to_string(status_path(runtime)).as_deref() {
        Ok("waiting") => Status::Waiting,
        Ok("active") => Status::Active,
        Ok("inactive") => Status::Inactive,
        Ok("error") => Status::Error,
        _ => Status::Unavailable,
    }
}

/// A lock shared by the writer and readers across the shim/API process boundary.
struct Lock(File);

impl Lock {
    fn open(runtime: &HostRuntimePaths, create: bool, exclusive: bool) -> io::Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).mode(0o600).create(create);
        let file = options.open(path(runtime, "app.lock"))?;
        let mode = if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        };
        // SAFETY: the descriptor remains owned by `Lock` until unlock/drop.
        if unsafe { libc::flock(file.as_raw_fd(), mode) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(file))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // SAFETY: this descriptor belongs to the still-live file.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn records(runtime: &HostRuntimePaths) -> io::Result<(Vec<Record>, bool)> {
    let mut output = Vec::new();
    let mut malformed = false;
    for name in ["app.jsonl.1", "app.jsonl"] {
        let file = match File::open(path(runtime, name)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for line in BufReader::new(file).lines() {
            match line
                .ok()
                .and_then(|line| serde_json::from_str::<Record>(&line).ok())
            {
                Some(record) => output.push(record),
                None => malformed = true,
            }
        }
    }
    Ok((output, malformed))
}

/// Read a stable page from a runtime's bounded files. Run this on a blocking worker.
pub(crate) fn read(runtime: &HostRuntimePaths, after: u64, limit: usize) -> io::Result<Batch> {
    let _lock = match Lock::open(runtime, false, false) {
        Ok(lock) => lock,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Batch {
                status: current_status(runtime),
                records: vec![],
                next: after,
                truncated: false,
            });
        }
        Err(error) => return Err(error),
    };
    let (all, malformed) = records(runtime)?;
    let lost_range = all
        .first()
        .is_some_and(|first| first.sequence > after.saturating_add(1));
    let records: Vec<_> = all
        .into_iter()
        .filter(|record| record.sequence > after)
        .take(limit.clamp(1, 512))
        .collect();
    let next = records.last().map_or(after, |record| record.sequence);
    let truncated =
        malformed || lost_range || records.iter().any(|record| record.stream == "system");
    Ok(Batch {
        status: current_status(runtime),
        records,
        next,
        truncated,
    })
}

enum Event {
    Data(&'static str, Vec<u8>),
}

/// State updates cannot be dropped when the bounded record queue fills.
#[derive(Default)]
struct Health {
    active: AtomicUsize,
    seen: AtomicBool,
    lost: AtomicU64,
}

impl Health {
    fn lost(&self, bytes: u64) {
        let _ = self
            .lost
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                Some(old.saturating_add(bytes))
            });
    }
}

struct Active(Arc<Health>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}

struct Writer {
    runtime: HostRuntimePaths,
    sequence: u64,
    health: Arc<Health>,
    failed: bool,
}

impl Writer {
    fn new(runtime: HostRuntimePaths, health: Arc<Health>) -> io::Result<Self> {
        let _lock = Lock::open(&runtime, true, true)?;
        let (saved, _) = records(&runtime)?;
        let sequence = saved.last().map_or(0, |record| record.sequence);
        status(&runtime, Status::Waiting)?;
        Ok(Self {
            runtime,
            sequence,
            health,
            failed: false,
        })
    }

    fn append(&mut self, stream: &str, bytes: &[u8]) -> io::Result<()> {
        let _lock = Lock::open(&self.runtime, false, true)?;
        let record = Record {
            sequence: self.sequence.saturating_add(1),
            timestamp: chrono::Utc::now().timestamp_millis(),
            stream: stream.into(),
            message: String::from_utf8_lossy(bytes).into_owned(),
        };
        let mut line = serde_json::to_vec(&record).map_err(io::Error::other)?;
        line.push(b'\n');
        let current = path(&self.runtime, "app.jsonl");
        let previous = path(&self.runtime, "app.jsonl.1");
        let size = fs::metadata(&current).map_or(0, |metadata| metadata.len());
        if size.saturating_add(line.len() as u64) > FILE_LIMIT {
            match fs::remove_file(&previous) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            match fs::rename(&current, &previous) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(current)?
            .write_all(&line)?;
        self.sequence = record.sequence;
        Ok(())
    }

    fn run(mut self, rx: mpsc::Receiver<Event>) {
        let mut last_status = Status::Waiting;
        let mut last_health = std::time::Instant::now();
        loop {
            let closed = match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Event::Data(stream, bytes)) => {
                    if self.append(stream, &bytes).is_err() {
                        self.failed = true;
                    }
                    false
                }
                Err(mpsc::RecvTimeoutError::Timeout) => false,
                Err(mpsc::RecvTimeoutError::Disconnected) => true,
            };
            if closed || last_health.elapsed() >= Duration::from_millis(250) {
                let lost = self.health.lost.swap(0, Ordering::Relaxed);
                if lost > 0
                    && self
                        .append(
                            "system",
                            format!("{lost} application output bytes were dropped").as_bytes(),
                        )
                        .is_err()
                {
                    self.failed = true;
                }
                let state = if self.failed {
                    Status::Error
                } else if self.health.active.load(Ordering::Relaxed) > 0 {
                    Status::Active
                } else if closed || self.health.seen.load(Ordering::Relaxed) {
                    Status::Inactive
                } else {
                    Status::Waiting
                };
                if state != last_status {
                    if status(&self.runtime, state).is_err() {
                        self.failed = true;
                    }
                    last_status = state;
                }
                last_health = std::time::Instant::now();
            }
            if closed {
                break;
            }
        }
    }
}

/// Keeps a bounded listener and writer alive with the Firecracker process.
pub(super) struct Collector {
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
    writer: Option<std::thread::JoinHandle<()>>,
    writer_done: oneshot::Receiver<()>,
    socket: PathBuf,
}

impl Collector {
    /// Drain accepted sockets and pending records before the shim records exit.
    pub(super) async fn finish(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = tokio::time::timeout(DRAIN_TIMEOUT + Duration::from_secs(1), &mut self.task).await;
        self.task.abort();
        let _ = self.task.await;
        if matches!(
            tokio::time::timeout(DRAIN_TIMEOUT, &mut self.writer_done).await,
            Ok(Ok(()))
        ) {
            if let Some(writer) = self.writer.take() {
                let _ = writer.join();
            }
        } else {
            // Dropping a standard thread handle detaches it. A hung disk must
            // not hold the VM process or API shutdown indefinitely.
            tracing::warn!("Application log drain did not finish before VM exit");
        }
        let _ = fs::remove_file(&self.socket);
    }
}

/// Start only when the API prepared this service VM's per-boot key.
pub(super) fn start(runtime: &HostRuntimePaths) -> Option<Collector> {
    let key = match fs::read(key_path(runtime)) {
        Ok(key) if key.len() == KEY_LEN => key,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        _ => {
            let _ = status(runtime, Status::Error);
            return None;
        }
    };
    let health = Arc::new(Health::default());
    let writer = match Writer::new(runtime.clone(), health.clone()) {
        Ok(writer) => writer,
        Err(_) => {
            let _ = status(runtime, Status::Error);
            return None;
        }
    };
    let socket = socket_path(runtime);
    if fs::remove_file(&socket).is_err_and(|error| error.kind() != io::ErrorKind::NotFound) {
        let _ = status(runtime, Status::Error);
        return None;
    }
    let listener = match UnixListener::bind(&socket) {
        Ok(listener) => listener,
        Err(_) => {
            let _ = status(runtime, Status::Error);
            return None;
        }
    };
    if fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).is_err() {
        let _ = status(runtime, Status::Error);
        let _ = fs::remove_file(&socket);
        return None;
    }
    let (tx, rx) = mpsc::sync_channel(QUEUE_LIMIT);
    let (writer_done, done) = oneshot::channel();
    let writer_task = match std::thread::Builder::new()
        .name("app-log-writer".into())
        .spawn(move || {
            writer.run(rx);
            let _ = writer_done.send(());
        }) {
        Ok(task) => task,
        Err(_) => {
            let _ = status(runtime, Status::Error);
            let _ = fs::remove_file(&socket);
            return None;
        }
    };
    let (stop, mut stopping) = oneshot::channel();
    let task = tokio::spawn(async move {
        let semaphore = Arc::new(Semaphore::new(CONNECTION_LIMIT));
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! {
                _ = &mut stopping => break,
                Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        if let Ok(permit) = semaphore.clone().try_acquire_owned() {
                            let tx = tx.clone(); let key = key.clone(); let health = health.clone();
                            tasks.spawn(async move { let _permit = permit; connection(stream, &key, tx, health).await; });
                        }
                    },
                    Err(_) => break,
                }
            }
        }
        let drain = async { while tasks.join_next().await.is_some() {} };
        if tokio::time::timeout(DRAIN_TIMEOUT, drain).await.is_err() {
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        }
        drop(tx);
    });
    Some(Collector {
        stop: Some(stop),
        task,
        writer: Some(writer_task),
        writer_done: done,
        socket,
    })
}

async fn connection(
    mut stream: UnixStream,
    key: &[u8],
    tx: SyncSender<Event>,
    health: Arc<Health>,
) {
    let auth = async {
        let mut header = [0_u8; 5];
        stream.read_exact(&mut header).await?;
        if header[0] != 0 || u32::from_be_bytes(header[1..].try_into().unwrap()) as usize != KEY_LEN
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid app log handshake",
            ));
        }
        let mut received = [0_u8; KEY_LEN];
        stream.read_exact(&mut received).await?;
        if received.as_slice().ct_eq(key).unwrap_u8() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "invalid app log key",
            ));
        }
        Ok::<(), io::Error>(())
    };
    if !matches!(tokio::time::timeout(DRAIN_TIMEOUT, auth).await, Ok(Ok(()))) {
        return;
    }
    health.seen.store(true, Ordering::Relaxed);
    health.active.fetch_add(1, Ordering::Relaxed);
    let _active = Active(health.clone());
    loop {
        let mut header = [0_u8; 5];
        if stream.read_exact(&mut header).await.is_err() {
            break;
        }
        let length = u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
        if length == 0 || length > FRAME_LIMIT {
            break;
        }
        let mut bytes = vec![0; length];
        if stream.read_exact(&mut bytes).await.is_err() {
            health.lost(length as u64);
            break;
        }
        let event = match header[0] {
            1 => Event::Data("stdout", bytes),
            2 => Event::Data("stderr", bytes),
            3 if length == 8 => {
                health.lost(u64::from_be_bytes(bytes.try_into().unwrap()));
                continue;
            }
            _ => break,
        };
        match tx.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => health.lost(length as u64),
            Err(TrySendError::Disconnected(_)) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn pages_only_app_records_and_reports_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = HostRuntimePaths::in_dir(dir.path().to_owned());
        fs::write(&runtime.console_log, "operator secret\n").unwrap();
        let empty = read(&runtime, 0, 128).unwrap();
        assert_eq!(empty.status, Status::Unavailable);
        assert!(empty.records.is_empty());

        let mut writer = Writer::new(runtime.clone(), Arc::new(Health::default())).unwrap();
        writer.append("stdout", b"first output").unwrap();
        writer.append("stderr", b"second output").unwrap();
        let first = read(&runtime, 0, 1).unwrap();
        assert_eq!(first.records[0].message, "first output");
        assert_eq!(first.next, 1);
        let second = read(&runtime, first.next, 4).unwrap();
        assert_eq!(second.records[0].stream, "stderr");
        assert_eq!(second.records[0].message, "second output");
        assert!(!second.truncated);
        for _ in 0..520 {
            writer.append("stdout", &[b'x'; 4096]).unwrap();
        }
        let rotated = read(&runtime, 0, 4).unwrap();
        assert!(rotated.truncated);
        assert!(rotated.records[0].sequence > 1);
        assert!(
            rotated
                .records
                .iter()
                .all(|item| !item.message.contains("operator secret"))
        );
    }

    #[tokio::test]
    async fn rejects_wrong_key_and_accepts_separate_streams() {
        let key = b"0123456789abcdef0123456789abcdef";
        let health = Arc::new(Health::default());
        let (tx, rx) = mpsc::sync_channel(8);
        let (mut guest, host) = UnixStream::pair().unwrap();
        let task = tokio::spawn(connection(host, key, tx.clone(), health.clone()));
        guest.write_all(&[0, 0, 0, 0, 32]).await.unwrap();
        guest
            .write_all(b"bad-key-01234567bad-key-01234567")
            .await
            .unwrap();
        drop(guest);
        task.await.unwrap();
        assert!(rx.try_recv().is_err());
        assert!(!health.seen.load(Ordering::Relaxed));

        let (mut guest, host) = UnixStream::pair().unwrap();
        let task = tokio::spawn(connection(host, key, tx, health.clone()));
        guest.write_all(&[0, 0, 0, 0, 32]).await.unwrap();
        guest.write_all(key).await.unwrap();
        guest
            .write_all(&[1, 0, 0, 0, 3, b'o', b'u', b't'])
            .await
            .unwrap();
        guest
            .write_all(&[2, 0, 0, 0, 3, b'e', b'r', b'r'])
            .await
            .unwrap();
        drop(guest);
        task.await.unwrap();
        assert!(matches!(rx.try_recv(), Ok(Event::Data("stdout", value)) if value == b"out"));
        assert!(matches!(rx.try_recv(), Ok(Event::Data("stderr", value)) if value == b"err"));
        assert!(health.seen.load(Ordering::Relaxed));
        assert_eq!(health.active.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn excessive_socket_path_reports_error_without_starting_collector() {
        let directory = tempfile::tempdir().unwrap();
        let long = directory.path().join("x".repeat(120));
        fs::create_dir_all(&long).unwrap();
        let runtime = HostRuntimePaths::in_dir(long);
        prepare(&runtime, "0123456789abcdef0123456789abcdef").unwrap();
        assert!(start(&runtime).is_none());
        assert_eq!(read(&runtime, 0, 1).unwrap().status, Status::Error);
    }
}
