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
use tokio::sync::{Semaphore, oneshot, watch};
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
/// Limit how much one peer can contribute after Firecracker exits.
const SHUTDOWN_DRAIN_LIMIT: usize = 256 * 1024;

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
                        .append("system", b"Application output was dropped")
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
        // A completed JoinHandle must not be polled again. Only abort and
        // await it when the timeout expired before completion.
        if tokio::time::timeout(DRAIN_TIMEOUT + Duration::from_secs(1), &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
            let _ = self.task.await;
        }
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
        let (shutdown, _) = watch::channel(false);
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! {
                _ = &mut stopping => break,
                Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        if let Ok(permit) = semaphore.clone().try_acquire_owned() {
                            let tx = tx.clone(); let key = key.clone(); let health = health.clone();
                            let shutdown = shutdown.subscribe();
                            tasks.spawn(async move { let _permit = permit; connection(stream, &key, tx, health, shutdown).await; });
                        }
                    },
                    Err(_) => break,
                }
            }
        }
        let _ = shutdown.send(true);
        let drain = async { while tasks.join_next().await.is_some() {} };
        if tokio::time::timeout(DRAIN_TIMEOUT, drain).await.is_err() {
            health.lost(1);
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
    mut shutdown: watch::Receiver<bool>,
) {
    let deadline = tokio::time::Instant::now() + DRAIN_TIMEOUT;
    let mut pending = Vec::with_capacity(FRAME_LIMIT + 5);
    let mut input = [0_u8; FRAME_LIMIT];
    let mut active = None;
    let mut stopping = false;
    let mut drained = 0;
    loop {
        let read = if stopping {
            // Firecracker has exited. Consume only currently buffered bytes;
            // a peer that never sends EOF cannot hold up VM recovery.
            if drained >= SHUTDOWN_DRAIN_LIMIT {
                if try_drain(&stream, &mut input[..1]).is_ok_and(|count| count > 0)
                    && active.is_some()
                {
                    health.lost(1);
                }
                break;
            }
            let size = input.len().min(SHUTDOWN_DRAIN_LIMIT - drained);
            match try_drain(&stream, &mut input[..size]) {
                Ok(count) => {
                    drained += count;
                    count
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    if active.is_some() {
                        health.lost(1);
                    }
                    break;
                }
            }
        } else {
            tokio::select! {
                biased;
                _ = shutdown.changed() => { stopping = true; continue; }
                _ = tokio::time::sleep_until(deadline), if active.is_none() => break,
                result = stream.read(&mut input) => match result { Ok(count) => count, Err(_) => break },
            }
        };
        if read == 0 {
            break;
        }
        pending.extend_from_slice(&input[..read]);
        loop {
            if pending.len() < 5 {
                break;
            }
            let length = u32::from_be_bytes(pending[1..5].try_into().unwrap()) as usize;
            if length == 0 || length > FRAME_LIMIT {
                if active.is_some() {
                    health.lost(1);
                }
                return;
            }
            if pending.len() < 5 + length {
                break;
            }
            let tag = pending[0];
            let body = pending[5..5 + length].to_vec();
            pending.drain(..5 + length);
            if active.is_none() {
                if tag != 0 || length != KEY_LEN || body.as_slice().ct_eq(key).unwrap_u8() != 1 {
                    return;
                }
                health.seen.store(true, Ordering::Relaxed);
                health.active.fetch_add(1, Ordering::Relaxed);
                active = Some(Active(health.clone()));
                continue;
            }
            let event = match tag {
                1 => Event::Data("stdout", body),
                2 => Event::Data("stderr", body),
                3 if length == 8 => {
                    health.lost(u64::from_be_bytes(body.try_into().unwrap()));
                    continue;
                }
                _ => {
                    health.lost(1);
                    return;
                }
            };
            match tx.try_send(event) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => health.lost(length as u64),
                Err(TrySendError::Disconnected(_)) => return,
            }
        }
    }
    if active.is_some() && !pending.is_empty() {
        health.lost(pending.len() as u64);
    }
}

/// Read the kernel socket buffer directly without waiting for reactor readiness.
fn try_drain(stream: &UnixStream, buffer: &mut [u8]) -> io::Result<usize> {
    // SAFETY: the descriptor remains owned by stream; buffer is writable for
    // its full length and MSG_DONTWAIT prevents the call from blocking.
    let count = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            libc::MSG_DONTWAIT,
        )
    };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(count as usize)
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
        let (_shutdown, receiver) = watch::channel(false);
        let (mut guest, host) = UnixStream::pair().unwrap();
        let task = tokio::spawn(connection(
            host,
            key,
            tx.clone(),
            health.clone(),
            receiver.clone(),
        ));
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
        let task = tokio::spawn(connection(host, key, tx, health.clone(), receiver));
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

    /// Connect a guest and wait until its authenticated stream is active.
    async fn connected_collector() -> (tempfile::TempDir, HostRuntimePaths, Collector, UnixStream) {
        let directory = tempfile::tempdir_in("/tmp").unwrap();
        let runtime = HostRuntimePaths::in_dir(directory.path().to_owned());
        let key = "0123456789abcdef0123456789abcdef";
        prepare(&runtime, key).unwrap();
        let collector = start(&runtime).unwrap();
        let mut guest = UnixStream::connect(socket_path(&runtime)).await.unwrap();
        guest.write_all(&[0, 0, 0, 0, 32]).await.unwrap();
        guest.write_all(key.as_bytes()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while read(&runtime, 0, 1).unwrap().status != Status::Active {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("guest did not authenticate");
        (directory, runtime, collector, guest)
    }

    #[tokio::test]
    async fn finish_drains_complete_frames_without_waiting_for_peer_eof() {
        let (_directory, runtime, collector, mut guest) = connected_collector().await;
        guest
            .write_all(&[1, 0, 0, 0, 3, b'o', b'u', b't'])
            .await
            .unwrap();
        guest
            .write_all(&[2, 0, 0, 0, 3, b'e', b'r', b'r'])
            .await
            .unwrap();
        // Keep the connection open: waiting for its EOF used to delay VM exit.
        tokio::time::timeout(Duration::from_millis(1500), collector.finish())
            .await
            .expect("collector waited for peer EOF");
        let batch = read(&runtime, 0, 10).unwrap();
        assert!(
            batch
                .records
                .iter()
                .any(|record| record.stream == "stdout" && record.message == "out")
        );
        assert!(
            batch
                .records
                .iter()
                .any(|record| record.stream == "stderr" && record.message == "err")
        );
        assert!(!batch.truncated);
    }

    #[tokio::test]
    async fn finish_reports_an_incomplete_frame_as_lost() {
        let (_directory, runtime, collector, mut guest) = connected_collector().await;
        guest
            .write_all(&[1, 0, 0, 0, 8, b'p', b'a', b'r'])
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_millis(1500), collector.finish())
            .await
            .expect("collector waited for a partial frame");
        let batch = read(&runtime, 0, 10).unwrap();
        assert!(batch.truncated);
        assert!(batch.records.iter().any(|record| record.stream == "system"));
        assert!(!batch.records.iter().any(|record| record.stream == "stdout"));
    }
}
