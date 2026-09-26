use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use axum::Extension;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::Response;
use firecrab_api_types::{CONSOLE_SESSION_ENDED_CLOSE_CODE, CONSOLE_SESSION_ENDED_CLOSE_REASON};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::console_session::{ScannedOutput, SessionEndScanner};
use crate::error::AppError;
use crate::firecracker::VmProcess;
use crate::server::RequestId;
use crate::state::AppState;

/// Upgrades to a WebSocket bridging the VM's serial console (ttyS0):
/// guest -> broker -> socket for output, and socket -> broker -> guest's
/// stdin for keystrokes.
pub async fn console_ws(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, AppError> {
    let process = resolve_console_process(&state.processes, &id, request_id.0)?;
    Ok(upgrade.on_upgrade(move |socket| stream_console(socket, process)))
}

/// Parses the path id and looks up its live process — the same map entry
/// that exists only while a Firecracker process backs the VM, so this is
/// simultaneously the "is it running" and "does it have a console" check.
/// Kept separate from the handler so it's testable without a WS handshake.
fn resolve_console_process(
    processes: &Mutex<HashMap<Uuid, VmProcess>>,
    id: &str,
    request_id: Uuid,
) -> Result<VmProcess, AppError> {
    let id = Uuid::parse_str(id).map_err(|_| AppError::not_found(request_id))?;
    processes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&id)
        .cloned()
        .ok_or_else(|| AppError::vm_not_running(request_id))
}

/// How long bytes held back as a possible session-ended marker start wait for
/// the rest of it before they are forwarded anyway.
const MARKER_HOLD: Duration = Duration::from_millis(50);

async fn stream_console(socket: WebSocket, process: VmProcess) {
    let (sink, inbound) = socket.split();
    bridge_console(sink, inbound, process).await;
}

/// Pumps console output to `sink` and viewer input from `inbound` until the
/// VM exits, the viewer leaves, or the guest's login session ends (#303).
async fn bridge_console<S, I, E>(mut sink: S, mut inbound: I, process: VmProcess)
where
    S: Sink<Message> + Unpin,
    I: Stream<Item = Result<Message, E>> + Unpin,
{
    let VmProcess {
        mut exited,
        console,
        ..
    } = process;
    let (backlog, mut output) = console.subscribe();
    let mut scanner = SessionEndScanner::default();

    let replay = scanner.backlog(&backlog);
    if !replay.is_empty() && sink.send(Message::Binary(replay.into())).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            chunk = output.recv() => match chunk {
                Ok(bytes) => match scanner.push(&bytes) {
                    ScannedOutput::Output(visible) => {
                        if !visible.is_empty()
                            && sink.send(Message::Binary(visible.into())).await.is_err()
                        {
                            return;
                        }
                    }
                    ScannedOutput::SessionEnded(visible) => {
                        if !visible.is_empty() {
                            let _ = sink.send(Message::Binary(visible.into())).await;
                        }
                        let _ = sink.send(session_ended_close()).await;
                        return;
                    }
                },
                // A slow viewer missed some output; keep streaming what's
                // still coming rather than closing the session over it.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            },
            () = tokio::time::sleep(MARKER_HOLD), if scanner.is_holding() => {
                if sink.send(Message::Binary(scanner.flush().into())).await.is_err() {
                    return;
                }
            }
            changed = exited.changed() => {
                if changed.is_err() || *exited.borrow() {
                    let _ = sink.send(Message::Close(None)).await;
                    return;
                }
            }
            frame = inbound.next() => match frame {
                None | Some(Ok(Message::Close(_))) => return,
                Some(Err(_)) => return,
                Some(Ok(Message::Binary(bytes))) => console.write_input(&bytes).await,
                Some(Ok(Message::Text(text))) => console.write_input(text.as_bytes()).await,
                // Ping/Pong and anything else: nothing to act on.
                Some(Ok(_)) => {}
            },
        }
    }
}

fn session_ended_close() -> Message {
    Message::Close(Some(CloseFrame {
        code: CONSOLE_SESSION_ENDED_CLOSE_CODE,
        reason: CONSOLE_SESSION_ENDED_CLOSE_REASON.into(),
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use tokio::sync::watch;

    use super::*;
    use crate::console::ConsoleBroker;

    fn running_process() -> VmProcess {
        let (_tx, exited) = watch::channel(false);
        VmProcess {
            pid: 4242,
            exited,
            console: Arc::new(ConsoleBroker::new()),
        }
    }

    #[test]
    fn malformed_vm_id_is_not_found_not_conflict() {
        let processes = Mutex::new(HashMap::new());

        let error = resolve_console_process(&processes, "not-a-uuid", Uuid::new_v4()).unwrap_err();
        // not_found (unparseable id) must be distinguishable from
        // vm_not_running (a real VM that simply isn't running).
        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn unknown_vm_id_is_reported_as_not_running() {
        let processes = Mutex::new(HashMap::new());

        let error =
            resolve_console_process(&processes, &Uuid::new_v4().to_string(), Uuid::new_v4())
                .unwrap_err();
        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    }

    /// Runs the bridge against an in-memory socket until it returns, while
    /// `feed` drives the console once the bridge has subscribed.
    async fn bridge_until_closed(
        process: VmProcess,
        feed: impl std::future::Future<Output = ()>,
    ) -> Vec<Message> {
        let console = process.console.clone();
        let mut sent = Vec::new();
        let inbound = futures_util::stream::pending::<Result<Message, std::convert::Infallible>>();
        let drive = async {
            while console.subscriber_count() == 0 {
                tokio::task::yield_now().await;
            }
            feed.await;
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(bridge_console(&mut sent, inbound, process), drive)
        })
        .await
        .expect("the bridge must close the socket");
        sent
    }

    /// A process whose exit sender the test keeps, so the bridge only sees
    /// the VM exit when the test says so.
    fn live_process() -> (watch::Sender<bool>, VmProcess) {
        let (exit_tx, exited) = watch::channel(false);
        let process = VmProcess {
            pid: 4242,
            exited,
            console: Arc::new(ConsoleBroker::new()),
        };
        (exit_tx, process)
    }

    fn expected_session_ended_close() -> Message {
        Message::Close(Some(axum::extract::ws::CloseFrame {
            code: firecrab_api_types::CONSOLE_SESSION_ENDED_CLOSE_CODE,
            reason: firecrab_api_types::CONSOLE_SESSION_ENDED_CLOSE_REASON.into(),
        }))
    }

    #[tokio::test]
    async fn a_live_session_ended_marker_closes_with_the_session_ended_code() {
        let (_exit_tx, process) = live_process();
        let console = process.console.clone();

        let sent = bridge_until_closed(process, async {
            console.push_output(
                &[
                    b"logout\r\n".as_slice(),
                    crate::console_session::SESSION_ENDED_MARKER,
                    b"login: root (automatic login)",
                ]
                .concat(),
            );
        })
        .await;

        assert_eq!(
            sent,
            vec![
                Message::Binary(b"logout\r\n".to_vec().into()),
                expected_session_ended_close()
            ]
        );
    }

    #[tokio::test]
    async fn a_session_ended_marker_in_the_backlog_does_not_close_the_socket() {
        let (exit_tx, process) = live_process();
        let console = process.console.clone();
        console.push_output(
            &[
                b"old\r\n".as_slice(),
                crate::console_session::SESSION_ENDED_MARKER,
                b"new\r\n",
            ]
            .concat(),
        );

        let sent = bridge_until_closed(process, async {
            console.push_output(b"typing");
            // Let the bridge forward the live chunk before the VM exits.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            exit_tx.send(true).unwrap();
        })
        .await;

        assert_eq!(
            sent,
            vec![
                Message::Binary(b"old\r\nnew\r\n".to_vec().into()),
                Message::Binary(b"typing".to_vec().into()),
                Message::Close(None),
            ]
        );
    }

    #[tokio::test]
    async fn a_held_marker_prefix_is_forwarded_once_output_goes_idle() {
        let (exit_tx, process) = live_process();
        let console = process.console.clone();

        let sent = bridge_until_closed(process, async {
            console.push_output(b"prompt# \x1b");
            tokio::time::sleep(MARKER_HOLD * 4).await;
            exit_tx.send(true).unwrap();
        })
        .await;

        assert_eq!(
            sent,
            vec![
                Message::Binary(b"prompt# ".to_vec().into()),
                Message::Binary(b"\x1b".to_vec().into()),
                Message::Close(None),
            ]
        );
    }

    #[test]
    fn a_running_vm_resolves_to_its_process() {
        let id = Uuid::new_v4();
        let processes = Mutex::new(HashMap::from([(id, running_process())]));

        let process = resolve_console_process(&processes, &id.to_string(), Uuid::new_v4()).unwrap();
        assert_eq!(process.pid, 4242);
    }
}
