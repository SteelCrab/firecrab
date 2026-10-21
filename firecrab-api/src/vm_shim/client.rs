//! The API side of the shim protocol: attach to a VM's shim, receive its
//! console and exit, and send it console input and stop/kill requests.

use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::protocol::{
    ExitReport, FrameError, PROTOCOL_VERSION, ShimEvent, ShimRequest, read_event, write_request,
};

/// Requests queued for the shim before `input` waits for room.
const REQUEST_QUEUE: usize = 256;
/// Console input is sent in frames of at most this many bytes, so a paste of
/// any size stays within the protocol's frame limit.
const INPUT_CHUNK: usize = 64 * 1024;
/// Delay between connection attempts while the shim is still starting.
const CONNECT_RETRY: Duration = Duration::from_millis(20);

/// An attached shim.
#[derive(Debug)]
pub(crate) struct ShimSession {
    /// Firecracker's process id, as the shim reported it.
    pub vmm_pid: u32,
    /// Sends console input and stop/kill requests.
    pub control: ShimControl,
    /// Console output, then exactly one of [`SessionEvent::Exited`] or
    /// [`SessionEvent::Lost`], then the channel closes.
    pub events: mpsc::UnboundedReceiver<SessionEvent>,
}

/// What the attached shim reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionEvent {
    /// Raw guest console bytes.
    Output(Vec<u8>),
    /// Firecracker exited.
    Exited(ExitReport),
    /// The connection closed without an exit report: the shim died, or
    /// another client took the connection over.
    Lost,
}

/// Cheap handle for sending requests to one shim. Every method is
/// best-effort: once the shim is gone, requests are silently dropped, the
/// same way keystrokes to an exited guest always were.
#[derive(Debug, Clone)]
pub(crate) struct ShimControl {
    requests: mpsc::Sender<ShimRequest>,
}

impl ShimControl {
    /// Sends bytes to the guest console, waiting if the queue is full.
    pub async fn input(&self, bytes: &[u8]) {
        for chunk in bytes.chunks(INPUT_CHUNK) {
            if self
                .requests
                .send(ShimRequest::Input(chunk.to_vec()))
                .await
                .is_err()
            {
                return;
            }
        }
    }

    /// Asks the shim to SIGTERM Firecracker.
    pub fn terminate(&self) {
        let _ = self.requests.try_send(ShimRequest::Terminate);
    }

    /// Asks the shim to SIGKILL Firecracker.
    pub fn kill(&self) {
        let _ = self.requests.try_send(ShimRequest::Kill);
    }

    /// A control whose requests land in the returned receiver, for tests of
    /// code that only needs to send.
    #[cfg(test)]
    pub fn for_test() -> (Self, mpsc::Receiver<ShimRequest>) {
        let (requests, received) = mpsc::channel(REQUEST_QUEUE);
        (Self { requests }, received)
    }
}

/// Why attaching to a shim failed.
#[derive(Debug, Error)]
pub(crate) enum ShimConnectError {
    /// Nothing accepted on the socket in time.
    #[error("VM shim socket {path} did not accept a connection within {timeout:?}")]
    Timeout {
        /// The shim socket.
        path: PathBuf,
        /// The deadline that passed.
        timeout: Duration,
    },
    /// The shim closed the connection or sent garbage before its greeting.
    #[error("VM shim handshake failed: {0}")]
    Handshake(#[from] FrameError),
    /// The connection closed before any frame arrived.
    #[error("VM shim closed the connection before its greeting")]
    Closed,
    /// The first frame was not a greeting.
    #[error("VM shim sent {0:?} before its greeting")]
    UnexpectedFrame(ShimEvent),
    /// The shim runs a different VM than the one asked for.
    #[error("VM shim runs VM {found}, expected {expected}")]
    WrongVm {
        /// The VM the caller meant to reach.
        expected: Uuid,
        /// The VM the shim reported.
        found: Uuid,
    },
    /// The shim speaks a different protocol version.
    #[error("VM shim speaks protocol version {found}, this API speaks {expected}")]
    Version {
        /// The shim's version.
        found: u32,
        /// This API's version.
        expected: u32,
    },
}

/// Attaches to the shim on `socket`, retrying until it accepts or `timeout`
/// passes, and checks its greeting: the protocol version, and that it runs
/// `vm_id`. A shim that is still starting is the normal case right after
/// launch, hence the retry.
pub(crate) async fn connect(
    socket: &Path,
    vm_id: Uuid,
    timeout: Duration,
) -> Result<ShimSession, ShimConnectError> {
    let attach = async {
        let mut stream = loop {
            match UnixStream::connect(socket).await {
                Ok(stream) => break stream,
                Err(_) => tokio::time::sleep(CONNECT_RETRY).await,
            }
        };
        let greeting = read_event(&mut stream).await?;
        Ok::<_, ShimConnectError>((stream, greeting))
    };
    let (stream, greeting) =
        tokio::time::timeout(timeout, attach)
            .await
            .map_err(|_| ShimConnectError::Timeout {
                path: socket.to_owned(),
                timeout,
            })??;
    let vmm_pid = match greeting {
        Some(ShimEvent::Hello {
            version,
            vm_id: Some(found),
            ..
        }) if version == PROTOCOL_VERSION && found != vm_id => {
            return Err(ShimConnectError::WrongVm {
                expected: vm_id,
                found,
            });
        }
        Some(ShimEvent::Hello {
            version, vmm_pid, ..
        }) if version == PROTOCOL_VERSION => vmm_pid,
        Some(ShimEvent::Hello { version, .. }) => {
            return Err(ShimConnectError::Version {
                found: version,
                expected: PROTOCOL_VERSION,
            });
        }
        Some(other) => return Err(ShimConnectError::UnexpectedFrame(other)),
        None => return Err(ShimConnectError::Closed),
    };

    let (read_half, write_half) = stream.into_split();
    let (requests, queued) = mpsc::channel(REQUEST_QUEUE);
    let (events, received) = mpsc::unbounded_channel();
    tokio::spawn(send_requests(write_half, queued));
    tokio::spawn(receive_events(read_half, events));
    Ok(ShimSession {
        vmm_pid,
        control: ShimControl { requests },
        events: received,
    })
}

/// Writes requests until the socket fails. A request that cannot be framed
/// is dropped on its own: ending the writer would silently drop every later
/// stop and kill too.
async fn send_requests(mut writer: OwnedWriteHalf, mut requests: mpsc::Receiver<ShimRequest>) {
    while let Some(request) = requests.recv().await {
        match write_request(&mut writer, &request).await {
            Ok(()) => {}
            Err(FrameError::Io(_)) => return,
            Err(error) => tracing::warn!(%error, "dropping a shim request that cannot be sent"),
        }
    }
}

/// Forwards events until the shim reports the exit or the connection ends.
/// Events are unbounded on this side on purpose: back-pressure here would
/// reach the shim, which drops a client that stops reading.
async fn receive_events(mut reader: OwnedReadHalf, events: mpsc::UnboundedSender<SessionEvent>) {
    loop {
        let event = match read_event(&mut reader).await {
            Ok(Some(ShimEvent::Output(bytes))) => SessionEvent::Output(bytes),
            Ok(Some(ShimEvent::Exited(exit))) => {
                let _ = events.send(SessionEvent::Exited(exit));
                return;
            }
            // A second greeting is a protocol violation; treat the session
            // as gone rather than trust anything after it.
            Ok(Some(ShimEvent::Hello { .. })) | Ok(None) | Err(_) => {
                let _ = events.send(SessionEvent::Lost);
                return;
            }
        };
        if events.send(event).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use tokio::net::UnixListener;
    use tokio::sync::oneshot;
    use uuid::Uuid;

    use super::*;
    use crate::artifacts::HostRuntimePaths;
    use crate::firecracker::test_support::{SERVE_LOOP, fake_firecracker, short_tempdir};
    use crate::vm_shim::protocol::{PROTOCOL_VERSION, ShimEvent, write_event};
    use crate::vm_shim::server::{ShimConfig, serve};

    const HONOR_SIGTERM: &str = "signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))\n";
    const ECHO_STDIN: &str = r#"
import threading
def _echo():
    for line in sys.stdin:
        print("echo:" + line.strip(), flush=True)
threading.Thread(target=_echo, daemon=True).start()
"#;

    struct Shim {
        _directory: tempfile::TempDir,
        vm_id: Uuid,
        runtime: HostRuntimePaths,
        _stop: oneshot::Sender<()>,
    }

    fn start_shim(body: &str) -> Shim {
        let directory = short_tempdir();
        let firecracker = fake_firecracker(directory.path(), body);
        let runtime = HostRuntimePaths::in_dir(directory.path().join("vm/r/one"));
        fs::create_dir_all(&runtime.dir).unwrap();
        fs::write(&runtime.config, "{}").unwrap();
        let (stop, stopped) = oneshot::channel::<()>();
        let vm_id = Uuid::new_v4();
        tokio::spawn(serve(
            ShimConfig {
                vm_id,
                runtime: runtime.clone(),
                firecracker,
                enable_pci: false,
                stop_grace: Duration::from_secs(5),
            },
            async move {
                let _ = stopped.await;
            },
        ));
        Shim {
            _directory: directory,
            vm_id,
            runtime,
            _stop: stop,
        }
    }

    async fn next(events: &mut mpsc::UnboundedReceiver<SessionEvent>) -> Option<SessionEvent> {
        tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("timed out waiting for a session event")
    }

    async fn output_until(events: &mut mpsc::UnboundedReceiver<SessionEvent>, needle: &str) {
        let mut seen = String::new();
        while !seen.contains(needle) {
            match next(events).await {
                Some(SessionEvent::Output(bytes)) => {
                    seen.push_str(&String::from_utf8_lossy(&bytes))
                }
                other => panic!("expected output containing {needle:?}, got {other:?}"),
            }
        }
    }

    async fn exit_of(events: &mut mpsc::UnboundedReceiver<SessionEvent>) -> SessionEvent {
        loop {
            match next(events).await {
                Some(SessionEvent::Output(_)) => {}
                Some(other) => return other,
                None => panic!("session ended without an exit event"),
            }
        }
    }

    #[tokio::test]
    async fn connecting_reports_the_vmm_pid_and_streams_console_output() {
        let shim = start_shim(&format!("{HONOR_SIGTERM}{SERVE_LOOP}"));
        let mut session = connect(
            &shim.runtime.shim_socket,
            shim.vm_id,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert!(session.vmm_pid > 0);
        output_until(&mut session.events, "booted").await;
        session.control.terminate();
        exit_of(&mut session.events).await;
    }

    #[tokio::test]
    async fn input_reaches_the_guest_console() {
        let shim = start_shim(&format!("{HONOR_SIGTERM}{ECHO_STDIN}{SERVE_LOOP}"));
        let mut session = connect(
            &shim.runtime.shim_socket,
            shim.vm_id,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        output_until(&mut session.events, "booted").await;

        session.control.input(b"hello shim\n").await;
        output_until(&mut session.events, "echo:hello shim").await;

        session.control.terminate();
        exit_of(&mut session.events).await;
    }

    #[tokio::test]
    async fn terminate_yields_a_clean_exit_and_then_the_session_ends() {
        let shim = start_shim(&format!("{HONOR_SIGTERM}{SERVE_LOOP}"));
        let mut session = connect(
            &shim.runtime.shim_socket,
            shim.vm_id,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        output_until(&mut session.events, "booted").await;

        session.control.terminate();
        match exit_of(&mut session.events).await {
            SessionEvent::Exited(exit) => assert!(exit.status.clean(), "{exit:?}"),
            other => panic!("expected a clean exit, got {other:?}"),
        }
        assert!(next(&mut session.events).await.is_none());
    }

    #[tokio::test]
    async fn kill_ends_the_vm_with_sigkill() {
        let shim = start_shim(&format!(
            "signal.signal(signal.SIGTERM, signal.SIG_IGN)\n{SERVE_LOOP}"
        ));
        let mut session = connect(
            &shim.runtime.shim_socket,
            shim.vm_id,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        output_until(&mut session.events, "booted").await;

        session.control.kill();
        match exit_of(&mut session.events).await {
            SessionEvent::Exited(exit) => assert_eq!(exit.status.signal, Some(libc::SIGKILL)),
            other => panic!("expected SIGKILL, got {other:?}"),
        }
    }

    /// A paste bigger than one frame (e.g. a base64 file over the serial
    /// console) must neither be rejected nor take the stop path down with it.
    #[tokio::test]
    async fn input_larger_than_a_frame_leaves_stop_working() {
        let shim = start_shim(&format!("{HONOR_SIGTERM}{SERVE_LOOP}"));
        let mut session = connect(
            &shim.runtime.shim_socket,
            shim.vm_id,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        output_until(&mut session.events, "booted").await;

        let paste = vec![b'a'; crate::vm_shim::protocol::MAX_FRAME_LEN * 2];
        tokio::time::timeout(Duration::from_secs(5), session.control.input(&paste))
            .await
            .expect("a large paste must not block the caller");
        session.control.terminate();

        match exit_of(&mut session.events).await {
            SessionEvent::Exited(exit) => assert!(exit.status.clean(), "{exit:?}"),
            other => panic!("terminate after a large paste must still stop the VM, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn input_after_the_vm_exited_is_silently_dropped() {
        let shim = start_shim(&format!("{HONOR_SIGTERM}{SERVE_LOOP}"));
        let mut session = connect(
            &shim.runtime.shim_socket,
            shim.vm_id,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        output_until(&mut session.events, "booted").await;
        session.control.terminate();
        exit_of(&mut session.events).await;

        tokio::time::timeout(Duration::from_secs(1), session.control.input(b"too late\n"))
            .await
            .expect("input to a finished shim must not block");
        session.control.terminate();
        session.control.kill();
    }

    #[tokio::test]
    async fn a_shim_that_vanishes_without_an_exit_is_lost() {
        let directory = short_tempdir();
        let socket = directory.path().join("shim.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            write_event(
                &mut stream,
                &ShimEvent::Hello {
                    version: PROTOCOL_VERSION,
                    vmm_pid: 7,
                    vm_id: None,
                },
            )
            .await
            .unwrap();
            // dropped: the connection closes with no Exited frame
        });

        let mut session = connect(&socket, Uuid::new_v4(), Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(session.vmm_pid, 7);
        assert!(matches!(
            next(&mut session.events).await,
            Some(SessionEvent::Lost)
        ));
        assert!(next(&mut session.events).await.is_none());
    }

    #[tokio::test]
    async fn a_greeting_for_another_vm_is_refused() {
        let directory = short_tempdir();
        let socket = directory.path().join("shim.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let other = Uuid::new_v4();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            write_event(
                &mut stream,
                &ShimEvent::Hello {
                    version: PROTOCOL_VERSION,
                    vmm_pid: 7,
                    vm_id: Some(other),
                },
            )
            .await
            .unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let expected = Uuid::new_v4();
        let error = connect(&socket, expected, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ShimConnectError::WrongVm { found, .. } if found == other
        ));
    }

    #[tokio::test]
    async fn a_shim_speaking_another_protocol_version_is_refused() {
        let directory = short_tempdir();
        let socket = directory.path().join("shim.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            write_event(
                &mut stream,
                &ShimEvent::Hello {
                    version: PROTOCOL_VERSION + 1,
                    vmm_pid: 7,
                    vm_id: None,
                },
            )
            .await
            .unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let error = connect(&socket, Uuid::new_v4(), Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ShimConnectError::Version { found, .. } if found == PROTOCOL_VERSION + 1
        ));
    }

    #[tokio::test]
    async fn connecting_to_a_socket_nobody_serves_times_out() {
        let directory = short_tempdir();
        let socket = directory.path().join("shim.sock");
        let started = std::time::Instant::now();
        let error = connect(&socket, Uuid::new_v4(), Duration::from_millis(200))
            .await
            .unwrap_err();
        assert!(matches!(error, ShimConnectError::Timeout { .. }));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
