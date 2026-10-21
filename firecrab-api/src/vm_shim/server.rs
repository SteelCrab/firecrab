//! The shim side: runs one Firecracker process and serves it on the
//! runtime directory's `shim.sock`.
//!
//! Exactly one client is attached at a time; a newly accepted connection
//! replaces the previous one, because the only legitimate reason for a second
//! connection is an API that restarted and is reattaching. The VM keeps
//! running with no client at all: console output still lands in
//! `console.log` and the backlog, and the exit status is written to
//! `exit.json` so a later client can learn how the VM ended.

use std::collections::VecDeque;
use std::fs;
use std::future::Future;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::protocol::{
    ExitStatus, PROTOCOL_VERSION, ShimEvent, ShimRequest, read_request, write_event,
};
use crate::artifacts::HostRuntimePaths;

/// Scrollback replayed to a client that attaches after boot output already
/// scrolled by — the same budget the API's console broker keeps.
const BACKLOG_BYTES: usize = 256 * 1024;
/// Bytes read off the guest console per `read()`.
const CONSOLE_READ_CHUNK: usize = 4096;
/// Console bytes queued for a client before it counts as stuck and is
/// dropped. The shim must never stop reading the guest console because a
/// client stopped reading the socket (a full serial buffer stalls the
/// guest), but a client that is only briefly slow — an API busy starting
/// other VMs — must survive a boot burst. Counted in bytes, not frames:
/// Firecracker writes the console a few bytes at a time.
const CLIENT_BACKLOG_BYTES: usize = 8 * 1024 * 1024;
/// Queued console output is merged into frames of up to this size.
const OUTPUT_FRAME_BYTES: usize = 64 * 1024;
/// Console bytes queued for Firecracker's stdin.
const INPUT_QUEUE: usize = 256;
/// After Firecracker exits, how long to keep draining its stdout and then to
/// let the final `Exited` frame reach the client.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// What one shim runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShimConfig {
    /// The VM this shim serves; only used for diagnostics.
    pub vm_id: Uuid,
    /// The start's runtime files: Firecracker config and sockets, console
    /// log, shim socket, exit status.
    pub runtime: HostRuntimePaths,
    /// Firecracker binary.
    pub firecracker: PathBuf,
    /// Whether Firecracker gets `--enable-pci`.
    pub enable_pci: bool,
    /// How long `shutdown` waits after SIGTERM before SIGKILL.
    pub stop_grace: Duration,
}

/// How a shim's VM ended: written to `exit.json`, so an API that was down
/// when the VM ended can tell a requested stop from a crash, and sent as the
/// final `Exited` frame, so an API that was up can too.
pub(crate) use super::protocol::ExitReport as ShimExit;

/// Runs Firecracker until it exits and serves it on `runtime.shim_socket`.
///
/// `shutdown` resolving (the shim itself was asked to stop) sends SIGTERM and
/// escalates to SIGKILL after `stop_grace`. Returns how the VM ended once
/// that has been written to `runtime.exit_status` and offered to the
/// attached client. Fails only if Firecracker cannot be started at all,
/// including when another shim already holds this VM's lock.
pub(crate) async fn serve(
    config: ShimConfig,
    shutdown: impl Future<Output = ()>,
) -> io::Result<ShimExit> {
    let runtime = &config.runtime;
    // Held until this function returns: while it is, no other shim can start
    // Firecracker on this VM's disk.
    let _vm_lock = match lock_vm(&runtime.vm_lock) {
        Ok(lock) => lock,
        Err(error) => {
            let _ = remove_if_present(&runtime.shim_error);
            record_startup_error(
                runtime,
                &format!(
                    "another firecrab-vm shim is already running this VM ({}): {error}",
                    runtime.vm_lock.display()
                ),
            );
            return Err(error);
        }
    };
    for stale in [
        &runtime.api_socket,
        &runtime.shim_socket,
        &runtime.exit_status,
        &runtime.shim_error,
    ] {
        remove_if_present(stale).inspect_err(|error| {
            record_startup_error(
                runtime,
                &format!("failed to clear {}: {error}", stale.display()),
            );
        })?;
    }
    let log = fs::File::create(&runtime.console_log).inspect_err(|error| {
        record_startup_error(
            runtime,
            &format!(
                "failed to create {}: {error}",
                runtime.console_log.display()
            ),
        );
    })?;
    // Firecracker's own diagnostics go straight to the log; its stdout is the
    // guest's ttyS0 and is filtered on the way in.
    let mut child = match spawn_firecracker(&config, log.try_clone()?).await {
        Ok(child) => child,
        Err(error) => {
            record_startup_error(
                runtime,
                &format!(
                    "failed to start Firecracker {}: {error}",
                    config.firecracker.display()
                ),
            );
            return Err(error);
        }
    };
    let vmm_pid = child.id().unwrap_or_default();
    let listener = match bind_private(&runtime.shim_socket) {
        Ok(listener) => listener,
        Err(error) => {
            record_startup_error(
                runtime,
                &format!("failed to bind {}: {error}", runtime.shim_socket.display()),
            );
            let _ = child.kill().await;
            return Err(error);
        }
    };
    tracing::info!(vm_id = %config.vm_id, vmm_pid, "vm shim serving");

    let stdin = child.stdin.take().expect("stdin was piped");
    let stdout = child.stdout.take().expect("stdout was piped");
    let (output_tx, mut output_rx) = mpsc::channel(256);
    tokio::spawn(read_console(stdout, output_tx));
    let (requests_tx, mut requests_rx) = mpsc::channel(64);
    let mut shim = Shim {
        vm_id: config.vm_id,
        vmm_pid,
        stop_requested: false,
        input: Some(spawn_input_writer(stdin)),
        log: tokio::fs::File::from_std(log),
        log_filter: Vec::new(),
        backlog: VecDeque::new(),
        client: None,
        next_client_id: 0,
        requests_tx,
    };

    let mut shutdown = std::pin::pin!(shutdown);
    let mut shutdown_requested = false;
    let mut kill_at: Option<tokio::time::Instant> = None;
    let mut output_open = true;
    let status = loop {
        tokio::select! {
            status = child.wait() => break status,
            chunk = output_rx.recv(), if output_open => match chunk {
                Some(chunk) => shim.on_output(chunk).await,
                None => output_open = false,
            },
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => shim.attach(stream),
                Err(error) => tracing::warn!(%error, "shim accept failed"),
            },
            Some((id, request)) = requests_rx.recv() => shim.on_request(id, request),
            () = &mut shutdown, if !shutdown_requested => {
                shutdown_requested = true;
                shim.stop_requested = true;
                signal(vmm_pid, libc::SIGTERM);
                kill_at = Some(tokio::time::Instant::now() + config.stop_grace);
            }
            () = sleep_until(kill_at), if kill_at.is_some() => {
                kill_at = None;
                signal(vmm_pid, libc::SIGKILL);
            }
        }
    };
    let status = match status {
        Ok(status) => ExitStatus {
            code: status.code(),
            signal: status.signal(),
        },
        // `wait` only fails if the child was already reaped, which nothing
        // else here does; report it as an abnormal end rather than hang.
        Err(error) => {
            tracing::warn!(%error, "waiting for Firecracker failed");
            ExitStatus {
                code: None,
                signal: None,
            }
        }
    };

    // Output Firecracker wrote just before exiting is still in the pipe;
    // deliver it before the Exited frame so nothing after it is lost.
    while output_open {
        match tokio::time::timeout(DRAIN_TIMEOUT, output_rx.recv()).await {
            Ok(Some(chunk)) => shim.on_output(chunk).await,
            Ok(None) | Err(_) => output_open = false,
        }
    }
    let _ = shim.log.flush().await;
    let exit = ShimExit {
        status,
        stop_requested: shim.stop_requested,
    };
    if let Err(error) = write_exit_status(&runtime.exit_status, &exit) {
        tracing::warn!(%error, "failed to record the VM exit status");
    }
    shim.finish(exit).await;
    let _ = remove_if_present(&runtime.shim_socket);
    let _ = remove_if_present(&runtime.api_socket);
    tracing::info!(vm_id = %config.vm_id, ?exit, "vm shim exiting");
    Ok(exit)
}

/// The attached client: its outgoing queue, the console bytes waiting in it,
/// and the tasks that own the two halves of its socket.
struct Client {
    id: u64,
    events: mpsc::UnboundedSender<ShimEvent>,
    queued_bytes: Arc<AtomicUsize>,
    writer: JoinHandle<()>,
    reader: JoinHandle<()>,
}

/// State the serve loop mutates, kept apart from the Firecracker child so the
/// loop can wait on the child while handling everything else.
struct Shim {
    vm_id: Uuid,
    vmm_pid: u32,
    stop_requested: bool,
    input: Option<mpsc::Sender<Vec<u8>>>,
    log: tokio::fs::File,
    log_filter: Vec<u8>,
    backlog: VecDeque<u8>,
    client: Option<Client>,
    next_client_id: u64,
    requests_tx: mpsc::Sender<(u64, Option<ShimRequest>)>,
}

impl Shim {
    async fn on_output(&mut self, chunk: Vec<u8>) {
        let visible = crate::console::filter_guest_usage_for_terminal(&mut self.log_filter, &chunk);
        if !visible.is_empty() {
            if let Err(error) = self.log.write_all(&visible).await {
                tracing::warn!(%error, "failed to tee console output to the log");
            }
            self.backlog.extend(visible.iter().copied());
            let overflow = self.backlog.len().saturating_sub(BACKLOG_BYTES);
            self.backlog.drain(..overflow);
        }
        // The client gets raw bytes: the API's metrics read FIRECRAB_USAGE
        // lines before its own console broker filters them.
        self.send(ShimEvent::Output(chunk));
    }

    /// Queues an event for the client, dropping a client that has let more
    /// than [`CLIENT_BACKLOG_BYTES`] of console output pile up: it has
    /// stopped reading, and waiting for it would stall the console.
    fn send(&mut self, event: ShimEvent) {
        let Some(client) = &self.client else {
            return;
        };
        let bytes = match &event {
            ShimEvent::Output(output) => output.len(),
            _ => 0,
        };
        let queued = client.queued_bytes.fetch_add(bytes, Ordering::Relaxed) + bytes;
        if queued > CLIENT_BACKLOG_BYTES {
            tracing::warn!(
                client = client.id,
                queued,
                "dropping a shim client that stopped reading"
            );
            self.drop_stuck_client();
        } else if client.events.send(event).is_err() {
            self.detach();
        }
    }

    fn attach(&mut self, stream: UnixStream) {
        self.detach();
        self.next_client_id += 1;
        let id = self.next_client_id;
        let (read_half, write_half) = stream.into_split();
        let (events, queued) = mpsc::unbounded_channel();
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        let writer = tokio::spawn(write_events(write_half, queued, Arc::clone(&queued_bytes)));
        let reader = tokio::spawn(read_requests(id, read_half, self.requests_tx.clone()));
        self.client = Some(Client {
            id,
            events,
            queued_bytes,
            writer,
            reader,
        });
        self.send(ShimEvent::Hello {
            version: PROTOCOL_VERSION,
            vmm_pid: self.vmm_pid,
            vm_id: Some(self.vm_id),
        });
        if !self.backlog.is_empty() {
            let backlog = self.backlog.iter().copied().collect();
            self.send(ShimEvent::Output(backlog));
        }
    }

    /// Lets the client's writer flush what is queued, then close: for a
    /// client that left or was replaced.
    fn detach(&mut self) {
        if let Some(client) = self.client.take() {
            // Dropping `events` ends the writer once the queue is written,
            // which shuts the socket's write side; aborting the reader drops
            // the read side.
            client.reader.abort();
        }
    }

    /// Closes a client that stopped reading right away: its writer may be
    /// blocked on a full socket forever, holding the queue with it.
    fn drop_stuck_client(&mut self) {
        if let Some(client) = self.client.take() {
            client.writer.abort();
            client.reader.abort();
        }
    }

    fn on_request(&mut self, id: u64, request: Option<ShimRequest>) {
        if self.client.as_ref().is_none_or(|client| client.id != id) {
            return; // a replaced client's last words
        }
        match request {
            None => self.detach(),
            Some(ShimRequest::Input(bytes)) => {
                if let Some(input) = &self.input
                    && let Err(error) = input.try_send(bytes)
                {
                    if matches!(error, mpsc::error::TrySendError::Closed(_)) {
                        self.input = None;
                    } else {
                        tracing::warn!("console input queue full; dropping keystrokes");
                    }
                }
            }
            Some(ShimRequest::Terminate) => {
                self.stop_requested = true;
                signal(self.vmm_pid, libc::SIGTERM);
            }
            Some(ShimRequest::Kill) => {
                self.stop_requested = true;
                signal(self.vmm_pid, libc::SIGKILL);
            }
        }
    }

    /// Offers the exit status to the client and waits briefly for it to be
    /// written, then closes the connection.
    async fn finish(&mut self, exit: ShimExit) {
        self.send(ShimEvent::Exited(exit));
        if let Some(client) = self.client.take() {
            let Client {
                events,
                writer,
                reader,
                ..
            } = client;
            drop(events);
            let _ = tokio::time::timeout(DRAIN_TIMEOUT, writer).await;
            reader.abort();
        }
    }
}

/// Writes queued events to the client, merging consecutive console output
/// into frames of up to [`OUTPUT_FRAME_BYTES`] so a burst of tiny writes
/// costs a few frames instead of one per byte.
async fn write_events(
    mut writer: OwnedWriteHalf,
    mut events: mpsc::UnboundedReceiver<ShimEvent>,
    queued_bytes: Arc<AtomicUsize>,
) {
    let mut held = None;
    loop {
        let event = match held.take() {
            Some(event) => event,
            None => match events.recv().await {
                Some(event) => event,
                None => return,
            },
        };
        let event = match event {
            ShimEvent::Output(mut output) => {
                while output.len() < OUTPUT_FRAME_BYTES {
                    match events.try_recv() {
                        Ok(ShimEvent::Output(more)) => output.extend_from_slice(&more),
                        Ok(other) => {
                            held = Some(other);
                            break;
                        }
                        Err(_) => break,
                    }
                }
                ShimEvent::Output(output)
            }
            other => other,
        };
        let written = match &event {
            ShimEvent::Output(output) => output.len(),
            _ => 0,
        };
        if write_event(&mut writer, &event).await.is_err() {
            return;
        }
        queued_bytes.fetch_sub(written, Ordering::Relaxed);
    }
}

async fn read_requests(
    id: u64,
    mut reader: OwnedReadHalf,
    requests: mpsc::Sender<(u64, Option<ShimRequest>)>,
) {
    loop {
        match read_request(&mut reader).await {
            Ok(Some(request)) => {
                if requests.send((id, Some(request))).await.is_err() {
                    return;
                }
            }
            // End of stream, or a peer that does not speak the protocol:
            // either way this client is done.
            Ok(None) | Err(_) => {
                let _ = requests.send((id, None)).await;
                return;
            }
        }
    }
}

async fn read_console(mut stdout: ChildStdout, output: mpsc::Sender<Vec<u8>>) {
    let mut buffer = [0_u8; CONSOLE_READ_CHUNK];
    loop {
        match stdout.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                if output.send(buffer[..read].to_vec()).await.is_err() {
                    return;
                }
            }
        }
    }
}

fn spawn_input_writer(mut stdin: ChildStdin) -> mpsc::Sender<Vec<u8>> {
    let (input, mut queued) = mpsc::channel::<Vec<u8>>(INPUT_QUEUE);
    tokio::spawn(async move {
        while let Some(bytes) = queued.recv().await {
            if stdin.write_all(&bytes).await.is_err() {
                return; // the guest console closed; later input is dropped
            }
        }
    });
    input
}

async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

fn signal(pid: u32, signal: libc::c_int) {
    if pid == 0 {
        return;
    }
    // SAFETY: sending a signal is memory-safe. The pid is Firecracker's, and
    // the serve loop stops signalling once it has reaped it.
    unsafe {
        libc::kill(pid as i32, signal);
    }
}

fn bind_private(path: &Path) -> io::Result<UnixListener> {
    let listener = UnixListener::bind(path)?;
    // The runtime directory is already 0700; this keeps the socket private
    // even if it is ever moved somewhere less strict.
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

fn write_exit_status(path: &Path, exit: &ShimExit) -> io::Result<()> {
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec(exit)?)?;
    fs::rename(&temporary, path)
}

/// Takes this VM's exclusive lock without waiting. The lock lives as long as
/// the returned file stays open.
fn lock_vm(path: &Path) -> io::Result<fs::File> {
    use std::os::fd::AsRawFd;

    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    // SAFETY: `flock` on a file descriptor this function owns.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

/// Leaves the reason a start failed where the API can read it. Best effort:
/// the same message also goes to stderr.
fn record_startup_error(runtime: &HostRuntimePaths, message: &str) {
    tracing::error!(%message, "vm shim could not start the VM");
    let _ = fs::write(&runtime.shim_error, message);
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

async fn spawn_firecracker(config: &ShimConfig, stderr: fs::File) -> io::Result<Child> {
    let mut command = Command::new(&config.firecracker);
    command
        .arg("--api-sock")
        .arg(&config.runtime.api_socket)
        .arg("--config-file")
        .arg(&config.runtime.config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true);
    // Firecracker defaults to virtio-mmio. Templates whose distro kernels
    // need the PCI device model opt in through `requires_pci_transport`.
    if config.enable_pci {
        command.arg("--enable-pci");
    }
    die_with_shim(&mut command);

    // `execve` of a script that was just written can fail with `ETXTBSY`
    // when many tests spawn at once; production Firecracker is a stable
    // binary, so the retry is test-only.
    const BUSY_ATTEMPTS: u32 = 8;
    let mut attempt = 0;
    loop {
        match command.spawn() {
            Err(error)
                if cfg!(test)
                    && error.kind() == io::ErrorKind::ExecutableFileBusy
                    && attempt < BUSY_ATTEMPTS =>
            {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(10 * u64::from(attempt))).await;
            }
            result => return result,
        }
    }
}

/// The shim is Firecracker's owner: if the shim dies, so does the VMM, rather
/// than becoming a VM nothing can reach. Linux only; elsewhere this binary
/// never runs VMs.
#[cfg(target_os = "linux")]
fn die_with_shim(command: &mut Command) {
    // SAFETY: `pre_exec` runs only in the forked child before exec, and the
    // closure makes a single async-signal-safe syscall.
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn die_with_shim(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::time::{Duration, Instant};

    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixStream;
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;
    use uuid::Uuid;

    use super::*;
    use crate::artifacts::HostRuntimePaths;
    use crate::firecracker::test_support::{SERVE_LOOP, fake_firecracker, short_tempdir};
    use crate::vm_shim::protocol::{
        PROTOCOL_VERSION, ShimEvent, ShimRequest, read_event, write_request,
    };

    const HONOR_SIGTERM: &str = "signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))\n";
    const IGNORE_SIGTERM: &str = "signal.signal(signal.SIGTERM, signal.SIG_IGN)\n";
    const ECHO_STDIN: &str = r#"
import threading
def _echo():
    for line in sys.stdin:
        print("echo:" + line.strip(), flush=True)
threading.Thread(target=_echo, daemon=True).start()
"#;

    const PRINT_USAGE_ON_GO: &str = r#"
import threading
def _on_go():
    for line in sys.stdin:
        if line.strip() == "go":
            print("FIRECRAB_USAGE cpu=1 mem=2", flush=True)
            print("visible line", flush=True)
threading.Thread(target=_on_go, daemon=True).start()
"#;

    /// Firecracker's serial device writes the guest console a few bytes at
    /// a time; this reproduces that as one write per byte, on request.
    const BURST_ON_GO: &str = r#"
import threading
def _burst():
    for line in sys.stdin:
        if line.strip() == "go":
            for _ in range(200000):
                sys.stdout.write("x")
                sys.stdout.flush()
            print("burst-done", flush=True)
threading.Thread(target=_burst, daemon=True).start()
"#;

    struct Running {
        _directory: tempfile::TempDir,
        runtime: HostRuntimePaths,
        shim: JoinHandle<io::Result<ShimExit>>,
        shutdown: Option<oneshot::Sender<()>>,
    }

    fn launch(body: &str, stop_grace: Duration) -> Running {
        let directory = short_tempdir();
        let binary = fake_firecracker(directory.path(), body);
        launch_with(directory, binary, stop_grace)
    }

    fn launch_with(
        directory: tempfile::TempDir,
        firecracker: std::path::PathBuf,
        stop_grace: Duration,
    ) -> Running {
        let runtime = HostRuntimePaths::in_dir(directory.path().join("vm/r/one"));
        fs::create_dir_all(&runtime.dir).unwrap();
        fs::write(&runtime.config, "{}").unwrap();
        let (shutdown, requested) = oneshot::channel::<()>();
        let config = ShimConfig {
            vm_id: Uuid::new_v4(),
            runtime: runtime.clone(),
            firecracker,
            enable_pci: false,
            stop_grace,
        };
        let shim = tokio::spawn(serve(config, async move {
            let _ = requested.await;
        }));
        Running {
            _directory: directory,
            runtime,
            shim,
            shutdown: Some(shutdown),
        }
    }

    async fn connect(socket: &Path) -> UnixStream {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(stream) = UnixStream::connect(socket).await {
                return stream;
            }
            assert!(Instant::now() < deadline, "shim socket never accepted");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn next_event(stream: &mut UnixStream) -> Option<ShimEvent> {
        tokio::time::timeout(Duration::from_secs(5), read_event(stream))
            .await
            .expect("timed out waiting for a shim event")
            .expect("malformed shim frame")
    }

    /// Reads until the accumulated console output contains `needle`; returns
    /// everything read.
    async fn output_until(stream: &mut UnixStream, needle: &str) -> String {
        let mut seen = String::new();
        while !seen.contains(needle) {
            match next_event(stream).await {
                Some(ShimEvent::Output(bytes)) => seen.push_str(&String::from_utf8_lossy(&bytes)),
                Some(ShimEvent::Hello { .. }) => {}
                other => panic!(
                    "expected console output containing {needle:?}, got {other:?} after {} bytes ending {:?}",
                    seen.len(),
                    &seen[seen.len().saturating_sub(80)..]
                ),
            }
        }
        seen
    }

    async fn exited(stream: &mut UnixStream) -> ExitStatus {
        loop {
            match next_event(stream).await {
                Some(ShimEvent::Exited(exit)) => return exit.status,
                Some(_) => {}
                None => panic!("stream ended without an Exited event"),
            }
        }
    }

    /// Waits for the shim to return. The temp directory travels with the
    /// result so files the shim wrote are still there to inspect.
    async fn finish(running: Running) -> (tempfile::TempDir, HostRuntimePaths, ExitStatus) {
        let Running {
            _directory,
            runtime,
            shim,
            ..
        } = running;
        let exit = tokio::time::timeout(Duration::from_secs(10), shim)
            .await
            .expect("shim did not return")
            .unwrap()
            .unwrap();
        (_directory, runtime, exit.status)
    }

    fn fake_pid(runtime: &HostRuntimePaths) -> u32 {
        fs::read_to_string(format!("{}.pid", runtime.api_socket.display()))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    #[tokio::test]
    async fn a_client_receives_hello_then_console_output() {
        let running = launch(
            &format!("{HONOR_SIGTERM}{SERVE_LOOP}"),
            Duration::from_secs(5),
        );
        let mut client = connect(&running.runtime.shim_socket).await;

        let Some(ShimEvent::Hello {
            version,
            vmm_pid,
            vm_id,
        }) = next_event(&mut client).await
        else {
            panic!("the first frame must be Hello");
        };
        assert_eq!(version, PROTOCOL_VERSION);
        assert!(vm_id.is_some(), "the shim must name its VM");
        // The fake records its pid before it prints anything.
        output_until(&mut client, "booted").await;
        assert_eq!(vmm_pid, fake_pid(&running.runtime));

        write_request(&mut client, &ShimRequest::Terminate)
            .await
            .unwrap();
        exited(&mut client).await;
        finish(running).await;
    }

    #[tokio::test]
    async fn console_output_is_teed_to_the_log_without_usage_lines() {
        // Printed on request, so the client is attached and sees it live
        // rather than through the (filtered) backlog.
        let running = launch(
            &format!("{HONOR_SIGTERM}{PRINT_USAGE_ON_GO}{SERVE_LOOP}"),
            Duration::from_secs(5),
        );
        let mut client = connect(&running.runtime.shim_socket).await;
        output_until(&mut client, "booted").await;
        write_request(&mut client, &ShimRequest::Input(b"go\n".to_vec()))
            .await
            .unwrap();
        let seen = output_until(&mut client, "visible line").await;
        assert!(
            seen.contains("FIRECRAB_USAGE"),
            "the client must see raw bytes, usage lines included, for metrics"
        );

        write_request(&mut client, &ShimRequest::Terminate)
            .await
            .unwrap();
        exited(&mut client).await;
        let (_directory, runtime, _) = finish(running).await;
        let log = fs::read_to_string(&runtime.console_log).unwrap();
        assert!(log.contains("visible line"));
        assert!(!log.contains("FIRECRAB_USAGE"));
    }

    #[tokio::test]
    async fn a_new_client_gets_the_backlog_and_replaces_the_previous_one() {
        let running = launch(
            &format!("{HONOR_SIGTERM}{SERVE_LOOP}"),
            Duration::from_secs(5),
        );
        let mut first = connect(&running.runtime.shim_socket).await;
        output_until(&mut first, "booted").await;

        let mut second = connect(&running.runtime.shim_socket).await;
        assert!(matches!(
            next_event(&mut second).await,
            Some(ShimEvent::Hello { .. })
        ));
        output_until(&mut second, "booted").await;

        // The first client is closed once the second one takes over.
        loop {
            match next_event(&mut first).await {
                None => break,
                Some(ShimEvent::Output(_)) => {}
                other => panic!("the replaced client must only see EOF, got {other:?}"),
            }
        }

        write_request(&mut second, &ShimRequest::Terminate)
            .await
            .unwrap();
        exited(&mut second).await;
        finish(running).await;
    }

    /// A client that stops reading for a moment while the guest prints a
    /// burst of tiny writes (boot output) must not be dropped: dropping the
    /// API mid-start would fail an otherwise healthy VM.
    #[tokio::test]
    async fn a_burst_of_tiny_writes_does_not_drop_a_briefly_slow_client() {
        let running = launch(
            &format!("{HONOR_SIGTERM}{BURST_ON_GO}{SERVE_LOOP}"),
            Duration::from_secs(5),
        );
        let mut client = connect(&running.runtime.shim_socket).await;
        output_until(&mut client, "booted").await;

        write_request(&mut client, &ShimRequest::Input(b"go\n".to_vec()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(3)).await;
        let seen = tokio::time::timeout(
            Duration::from_secs(30),
            output_until(&mut client, "burst-done"),
        )
        .await
        .expect("the burst never fully arrived");
        assert_eq!(seen.matches('x').count(), 200_000);

        write_request(&mut client, &ShimRequest::Terminate)
            .await
            .unwrap();
        exited(&mut client).await;
        finish(running).await;
    }

    #[tokio::test]
    async fn input_reaches_the_guest_console() {
        let running = launch(
            &format!("{HONOR_SIGTERM}{ECHO_STDIN}{SERVE_LOOP}"),
            Duration::from_secs(5),
        );
        let mut client = connect(&running.runtime.shim_socket).await;
        output_until(&mut client, "booted").await;

        write_request(&mut client, &ShimRequest::Input(b"ping\n".to_vec()))
            .await
            .unwrap();
        output_until(&mut client, "echo:ping").await;

        write_request(&mut client, &ShimRequest::Terminate)
            .await
            .unwrap();
        exited(&mut client).await;
        finish(running).await;
    }

    #[tokio::test]
    async fn terminate_ends_the_vm_and_reports_a_clean_exit() {
        let running = launch(
            &format!("{HONOR_SIGTERM}{SERVE_LOOP}"),
            Duration::from_secs(5),
        );
        let mut client = connect(&running.runtime.shim_socket).await;
        output_until(&mut client, "booted").await;

        write_request(&mut client, &ShimRequest::Terminate)
            .await
            .unwrap();
        let reported = exited(&mut client).await;
        let (_directory, runtime, returned) = finish(running).await;

        assert!(
            reported.clean(),
            "SIGTERM honored with exit 0: {reported:?}"
        );
        assert_eq!(reported, returned);
        let recorded: ExitStatus =
            serde_json::from_slice(&fs::read(&runtime.exit_status).unwrap()).unwrap();
        assert_eq!(recorded, returned);
        assert!(!runtime.shim_socket.exists());
        assert!(!runtime.api_socket.exists());
    }

    #[tokio::test]
    async fn kill_ends_a_vm_that_ignores_sigterm() {
        let running = launch(
            &format!("{IGNORE_SIGTERM}{SERVE_LOOP}"),
            Duration::from_secs(5),
        );
        let mut client = connect(&running.runtime.shim_socket).await;
        output_until(&mut client, "booted").await;

        write_request(&mut client, &ShimRequest::Kill)
            .await
            .unwrap();
        let status = exited(&mut client).await;
        finish(running).await;
        assert_eq!(status.signal, Some(libc::SIGKILL));
    }

    #[tokio::test]
    async fn shutdown_escalates_to_sigkill_when_sigterm_is_ignored() {
        let mut running = launch(
            &format!("{IGNORE_SIGTERM}{SERVE_LOOP}"),
            Duration::from_millis(300),
        );
        let mut client = connect(&running.runtime.shim_socket).await;
        output_until(&mut client, "booted").await;

        let started = Instant::now();
        running.shutdown.take().unwrap().send(()).unwrap();
        let (_directory, runtime, status) = finish(running).await;

        assert!(started.elapsed() >= Duration::from_millis(300));
        assert_eq!(status.signal, Some(libc::SIGKILL));
        assert!(runtime.exit_status.exists());
    }

    #[tokio::test]
    async fn shutdown_stops_a_vm_that_honors_sigterm_without_waiting() {
        let mut running = launch(
            &format!("{HONOR_SIGTERM}{SERVE_LOOP}"),
            Duration::from_secs(30),
        );
        let mut client = connect(&running.runtime.shim_socket).await;
        output_until(&mut client, "booted").await;

        running.shutdown.take().unwrap().send(()).unwrap();
        let (_directory, _, status) = finish(running).await;
        assert!(status.clean());
    }

    /// Like `finish`, but keeps the whole `ShimExit`; the temp directory is
    /// returned so `exit.json` is still there to read.
    async fn finish_exit(running: Running) -> (tempfile::TempDir, ShimExit) {
        let Running {
            _directory, shim, ..
        } = running;
        let exit = tokio::time::timeout(Duration::from_secs(10), shim)
            .await
            .expect("shim did not return")
            .unwrap()
            .unwrap();
        (_directory, exit)
    }

    fn recorded_stop_request(runtime: &HostRuntimePaths) -> bool {
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(&runtime.exit_status).unwrap()).unwrap();
        record["stop_requested"].as_bool().unwrap()
    }

    #[tokio::test]
    async fn a_terminate_request_is_recorded_as_a_requested_stop() {
        let running = launch(
            &format!("{HONOR_SIGTERM}{SERVE_LOOP}"),
            Duration::from_secs(5),
        );
        let runtime = running.runtime.clone();
        let mut client = connect(&runtime.shim_socket).await;
        output_until(&mut client, "booted").await;

        write_request(&mut client, &ShimRequest::Terminate)
            .await
            .unwrap();
        let reported = exited(&mut client).await;
        let (_directory, exit) = finish_exit(running).await;

        assert_eq!(reported, exit.status);
        assert!(exit.stop_requested);
        assert!(recorded_stop_request(&runtime));
    }

    #[tokio::test]
    async fn shutdown_counts_as_a_requested_stop() {
        let mut running = launch(SERVE_LOOP, Duration::from_secs(5));
        let runtime = running.runtime.clone();
        let mut client = connect(&runtime.shim_socket).await;
        output_until(&mut client, "booted").await;

        running.shutdown.take().unwrap().send(()).unwrap();
        let (_directory, exit) = finish_exit(running).await;

        // Killed by the SIGTERM it did not handle, as real Firecracker is:
        // not a clean exit, but one that was asked for.
        assert_eq!(exit.status.signal, Some(libc::SIGTERM));
        assert!(exit.stop_requested);
        assert!(recorded_stop_request(&runtime));
    }

    #[tokio::test]
    async fn a_guest_initiated_exit_is_not_a_requested_stop() {
        let running = launch("time.sleep(0.2)\nsys.exit(0)\n", Duration::from_secs(5));
        let runtime = running.runtime.clone();
        let (_directory, exit) = finish_exit(running).await;

        assert!(exit.status.clean());
        assert!(!exit.stop_requested);
        assert!(!recorded_stop_request(&runtime));
    }

    #[tokio::test]
    async fn a_second_shim_for_the_same_vm_is_refused() {
        let first = launch(
            &format!("{HONOR_SIGTERM}{SERVE_LOOP}"),
            Duration::from_secs(5),
        );
        let mut client = connect(&first.runtime.shim_socket).await;
        output_until(&mut client, "booted").await;

        // Same VM directory, new runtime directory: what a retried start
        // after a lost record would look like.
        let second_runtime =
            HostRuntimePaths::in_dir(first.runtime.dir.parent().unwrap().join("two"));
        fs::create_dir_all(&second_runtime.dir).unwrap();
        fs::write(&second_runtime.config, "{}").unwrap();
        let firecracker = first
            .runtime
            .dir
            .ancestors()
            .nth(3)
            .unwrap()
            .join("fake-firecracker");
        let second = serve(
            ShimConfig {
                vm_id: Uuid::new_v4(),
                runtime: second_runtime.clone(),
                firecracker,
                enable_pci: false,
                stop_grace: Duration::from_secs(5),
            },
            std::future::pending(),
        )
        .await;

        assert!(second.is_err());
        let reason = fs::read_to_string(&second_runtime.shim_error).unwrap();
        assert!(reason.contains("already running"), "{reason}");

        write_request(&mut client, &ShimRequest::Terminate)
            .await
            .unwrap();
        exited(&mut client).await;
        finish(first).await;
    }

    #[tokio::test]
    async fn the_exit_status_is_recorded_with_no_client_attached() {
        let running = launch("time.sleep(0.2)\nsys.exit(3)\n", Duration::from_secs(5));
        let (_directory, runtime, status) = finish(running).await;

        assert_eq!(status.code, Some(3));
        let recorded: ExitStatus =
            serde_json::from_slice(&fs::read(&runtime.exit_status).unwrap()).unwrap();
        assert_eq!(recorded, status);
    }

    #[tokio::test]
    async fn an_oversized_request_drops_only_that_client() {
        let running = launch(
            &format!("{HONOR_SIGTERM}{SERVE_LOOP}"),
            Duration::from_secs(5),
        );
        let mut bad = connect(&running.runtime.shim_socket).await;
        output_until(&mut bad, "booted").await;

        let too_long = u32::try_from(crate::vm_shim::protocol::MAX_FRAME_LEN + 1).unwrap();
        bad.write_all(&[16]).await.unwrap();
        bad.write_all(&too_long.to_be_bytes()).await.unwrap();
        loop {
            match next_event(&mut bad).await {
                None => break,
                Some(ShimEvent::Output(_)) => {}
                other => panic!("a misbehaving client must be closed, got {other:?}"),
            }
        }

        let mut good = connect(&running.runtime.shim_socket).await;
        assert!(matches!(
            next_event(&mut good).await,
            Some(ShimEvent::Hello { .. })
        ));
        write_request(&mut good, &ShimRequest::Terminate)
            .await
            .unwrap();
        exited(&mut good).await;
        finish(running).await;
    }

    #[tokio::test]
    async fn a_firecracker_that_cannot_be_spawned_is_an_error() {
        let directory = short_tempdir();
        let missing = directory.path().join("no-such-firecracker");
        let running = launch_with(directory, missing, Duration::from_secs(1));

        let result = tokio::time::timeout(Duration::from_secs(5), running.shim)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err());
        assert!(!running.runtime.shim_socket.exists());
    }

    /// Serves a VM whose runtime directory has a directory where `blocked`
    /// expects a file, so preparing it fails before Firecracker starts.
    async fn serve_blocked(
        blocked: fn(&HostRuntimePaths) -> &Path,
    ) -> (tempfile::TempDir, HostRuntimePaths, bool) {
        let directory = short_tempdir();
        let firecracker = fake_firecracker(directory.path(), SERVE_LOOP);
        let runtime = HostRuntimePaths::in_dir(directory.path().join("vm/r/one"));
        fs::create_dir_all(&runtime.dir).unwrap();
        fs::write(&runtime.config, "{}").unwrap();
        let obstacle = blocked(&runtime).to_owned();
        fs::create_dir(&obstacle).unwrap();
        fs::write(obstacle.join("keep"), "").unwrap();
        let config = ShimConfig {
            vm_id: Uuid::new_v4(),
            runtime: runtime.clone(),
            firecracker,
            enable_pci: false,
            stop_grace: Duration::from_secs(5),
        };
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            serve(config, std::future::pending()),
        )
        .await
        .unwrap();
        (directory, runtime, result.is_err())
    }

    #[tokio::test]
    async fn a_console_log_that_cannot_be_created_is_recorded_as_the_start_failure() {
        let (_directory, runtime, failed) = serve_blocked(|runtime| &runtime.console_log).await;

        assert!(failed);
        let reason = fs::read_to_string(&runtime.shim_error).unwrap();
        assert!(reason.contains("console.log"), "{reason}");
    }

    #[tokio::test]
    async fn a_stale_file_that_cannot_be_removed_is_recorded_as_the_start_failure() {
        let (_directory, runtime, failed) = serve_blocked(|runtime| &runtime.exit_status).await;

        assert!(failed);
        let reason = fs::read_to_string(&runtime.shim_error).unwrap();
        assert!(reason.contains("exit.json"), "{reason}");
    }

    #[tokio::test]
    async fn the_control_socket_is_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt;

        let running = launch(
            &format!("{HONOR_SIGTERM}{SERVE_LOOP}"),
            Duration::from_secs(5),
        );
        let mut client = connect(&running.runtime.shim_socket).await;
        let mode = fs::metadata(&running.runtime.shim_socket)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);

        write_request(&mut client, &ShimRequest::Terminate)
            .await
            .unwrap();
        exited(&mut client).await;
        finish(running).await;
    }
}
