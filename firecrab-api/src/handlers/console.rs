use std::collections::HashMap;
use std::sync::Mutex;

use axum::Extension;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast;
use uuid::Uuid;

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

async fn stream_console(socket: WebSocket, process: VmProcess) {
    let VmProcess {
        mut exited,
        console,
        ..
    } = process;
    let (mut sink, mut inbound) = socket.split();
    let (backlog, mut output) = console.subscribe();

    if !backlog.is_empty() && sink.send(Message::Binary(backlog.into())).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            chunk = output.recv() => match chunk {
                Ok(bytes) => {
                    if sink.send(Message::Binary(bytes.into())).await.is_err() {
                        return;
                    }
                }
                // A slow viewer missed some output; keep streaming what's
                // still coming rather than closing the session over it.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            },
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

    #[test]
    fn a_running_vm_resolves_to_its_process() {
        let id = Uuid::new_v4();
        let processes = Mutex::new(HashMap::from([(id, running_process())]));

        let process = resolve_console_process(&processes, &id.to_string(), Uuid::new_v4()).unwrap();
        assert_eq!(process.pid, 4242);
    }
}
