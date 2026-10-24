use std::collections::HashMap;
use std::sync::{Arc, Mutex};
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
    let upgrade = upgrade.protocols([crate::console_resize::WS_PROTOCOL]);
    let modern = upgrade.selected_protocol().is_some();
    Ok(upgrade.on_upgrade(move |socket| stream_console(socket, process, state, id, modern)))
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

async fn stream_console(
    socket: WebSocket,
    process: VmProcess,
    state: AppState,
    id: String,
    modern: bool,
) {
    let (sink, inbound) = socket.split();
    bridge_console(
        sink,
        inbound,
        process,
        Some((state, id.parse().expect("validated VM id"))),
        modern,
    )
    .await;
}

/// Pumps console output to `sink` and viewer input from `inbound` until the
/// VM exits, the viewer leaves, or the guest's login session ends (#303).
async fn bridge_console<S, I, E>(
    mut sink: S,
    mut inbound: I,
    process: VmProcess,
    vm: Option<(AppState, Uuid)>,
    modern: bool,
) where
    S: Sink<Message> + Unpin,
    I: Stream<Item = Result<Message, E>> + Unpin,
{
    let VmProcess {
        mut exited,
        console,
        ..
    } = process;
    let mut sizes = console.resize.subscribe();
    let viewer = modern.then(|| console.resize.register());
    let _worker = viewer.as_ref().map(|viewer| {
        ResizeWorker(tokio::spawn(
            Arc::clone(&console.resize).synchronize(viewer.id, exited.clone()),
        ))
    });
    if modern {
        let greeting = sizes.borrow_and_update().message();
        if sink.send(Message::Text(greeting.into())).await.is_err() {
            return;
        }
    }
    let (backlog, mut output) = console.subscribe();
    let mut scanner = SessionEndScanner::default();

    let replay = crate::console_replay::without_terminal_queries(&scanner.backlog(&backlog));
    if !replay.is_empty() && sink.send(Message::Binary(replay.into())).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            changed = sizes.changed(), if modern => {
                if changed.is_err() { return; }
                let message = sizes.borrow_and_update().message();
                if sink.send(Message::Text(message.into())).await.is_err() { return; }
            }
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
                Some(Ok(Message::Binary(bytes))) => { write_console_input(&console, vm.as_ref(), &bytes).await; },
                Some(Ok(Message::Text(text))) if modern => {
                    let size = (text.len() <= 128).then(|| serde_json::from_str::<ResizeRequest>(&text).ok()).flatten();
                    match (viewer.as_ref(), size) {
                        (Some(viewer), Some(ResizeRequest::Resize { rows, cols })) if (crate::console_resize::Dimensions { rows, cols }).valid() => {
                            viewer.report(crate::console_resize::Dimensions { rows, cols });
                        }
                        _ => {
                            let _ = sink.send(Message::Close(Some(CloseFrame { code: 1008, reason: "Invalid console control frame".into() }))).await;
                            return;
                        }
                    }
                },
                Some(Ok(Message::Text(text))) => { write_console_input(&console, vm.as_ref(), text.as_bytes()).await; },
                // Ping/Pong and anything else: nothing to act on.
                Some(Ok(_)) => {}
            },
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
enum ResizeRequest {
    Resize { rows: u16, cols: u16 },
}

struct ResizeWorker(tokio::task::JoinHandle<()>);
impl Drop for ResizeWorker {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn write_console_input(
    console: &crate::console::ConsoleBroker,
    vm: Option<&(AppState, Uuid)>,
    bytes: &[u8],
) {
    let Some((state, id)) = vm else {
        console.write_input(bytes).await;
        return;
    };
    let _guard = state.lock_vm(*id).await;
    let running = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(id)
        .is_some_and(|vm| vm.state == crate::model::VmState::Running);
    if running {
        console.write_input(bytes).await;
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
            control: crate::vm_shim::client::ShimControl::for_test().0,
        }
    }

    #[tokio::test]
    async fn resize_control_never_becomes_shell_input_and_the_viewer_is_released() {
        let (exit, exited) = watch::channel(false);
        let console = Arc::new(ConsoleBroker::new());
        let (control, mut input) = crate::vm_shim::client::ShimControl::for_test();
        console.attach_control(control.clone());
        let process = VmProcess {
            pid: 42,
            exited,
            console: Arc::clone(&console),
            control,
        };
        let frames: Vec<Result<Message, std::convert::Infallible>> = vec![
            Ok(Message::Text(
                r#"{"type":"resize","rows":30,"cols":100}"#.into(),
            )),
            Ok(Message::Binary(b"hello\r".to_vec().into())),
            Ok(Message::Text(
                r#"{"type":"resize","rows":0,"cols":80}"#.into(),
            )),
        ];
        let mut sent = Vec::new();
        bridge_console(
            &mut sent,
            futures_util::stream::iter(frames),
            process,
            None,
            true,
        )
        .await;
        assert!(
            matches!(input.try_recv(), Ok(crate::vm_shim::protocol::ShimRequest::Input(bytes)) if bytes == b"hello\r")
        );
        assert!(input.try_recv().is_err());
        assert!(
            sent.iter().any(
                |message| matches!(message, Message::Close(Some(frame)) if frame.code == 1008)
            )
        );
        assert!(
            sent.iter()
                .any(|message| matches!(message, Message::Text(text) if text.contains("resize")))
        );
        assert!(
            console
                .resize
                .subscribe()
                .borrow()
                .message()
                .contains("\"viewers\":0")
        );
        drop(exit);
    }

    #[tokio::test]
    async fn paused_console_input_is_discarded_and_resume_accepts_new_input() {
        let root = tempfile::tempdir().unwrap();
        let templates =
            crate::templates::TemplateRegistry::from_specs(root.path(), std::iter::empty())
                .unwrap();
        let state = AppState::with_db_file(templates, root.path().join("state.db"))
            .await
            .unwrap();
        let id = Uuid::new_v4();
        let mut vm = crate::handlers::vms::test_support::record("paused", id);
        vm.state = crate::model::VmState::Paused;
        state.vms.lock().unwrap().insert(id, vm);
        let broker = ConsoleBroker::new();
        let (control, mut received) = crate::vm_shim::client::ShimControl::for_test();
        broker.attach_control(control);
        let context = (state.clone(), id);
        write_console_input(&broker, Some(&context), b"discard").await;
        assert!(received.try_recv().is_err());
        state.vms.lock().unwrap().get_mut(&id).unwrap().state = crate::model::VmState::Running;
        write_console_input(&broker, Some(&context), b"new input").await;
        assert!(
            matches!(received.recv().await, Some(crate::vm_shim::protocol::ShimRequest::Input(bytes)) if bytes == b"new input")
        );
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
            tokio::join!(
                bridge_console(&mut sent, inbound, process, None, false),
                drive
            )
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
            control: crate::vm_shim::client::ShimControl::for_test().0,
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
                b";1R;145R\r\n-bash: syntax error near unexpected token `;'\r\n".as_slice(),
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
                Message::Binary(b"new\r\n".to_vec().into()),
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

    #[tokio::test]
    async fn past_terminal_queries_are_removed_but_live_queries_still_reach_the_viewer() {
        let (_exit_tx, process) = live_process();
        let console = process.console.clone();
        console.push_output(b"old logout\r\n\x1b[6n\x1bP+q6E616D65\x1b\\root# ");

        let sent = bridge_until_closed(process, async {
            console.push_output(
                &[
                    b"\x1b[6nlogout\r\n".as_slice(),
                    crate::console_session::SESSION_ENDED_MARKER,
                ]
                .concat(),
            );
        })
        .await;

        assert_eq!(
            sent,
            vec![
                Message::Binary(b"old logout\r\nroot# ".to_vec().into()),
                Message::Binary(b"\x1b[6nlogout\r\n".to_vec().into()),
                expected_session_ended_close(),
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
