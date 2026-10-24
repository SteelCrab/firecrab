//! Shared ttyS0 dimensions and the private host-to-guest resize channel.
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::watch;

/// Separates resize JSON from legacy raw console text.
pub(crate) const WS_PROTOCOL: &str = "firecrab.console.v1";
/// Collect the final viewport during a browser resize burst.
const COALESCE: Duration = Duration::from_millis(50);
/// Retry unsupported or not-yet-started guest listeners without busy-looping.
const RETRY: Duration = Duration::from_secs(3);
/// Bound the whole connect, handshake, ioctl and acknowledgement round trip.
const TIMEOUT: Duration = Duration::from_millis(750);

/// A bounded tty grid, independent of browser pixel measurements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Dimensions {
    /// Visible character rows.
    pub rows: u16,
    /// Visible character columns.
    pub cols: u16,
}

impl Dimensions {
    /// Reject zero-sized, excessive and otherwise unusable terminal grids.
    pub fn valid(self) -> bool {
        (2..=1000).contains(&self.rows) && (2..=1000).contains(&self.cols)
    }
}

/// Whether the current shared target has been acknowledged by the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ResizeStatus {
    /// A new target is waiting for the coalescing timer or guest reply.
    Pending,
    /// The helper confirmed the current shared grid.
    Applied,
    /// The last attempt failed; terminal input and output remain available.
    Unavailable,
}

/// Published state shared by WebSocket bridges and resize workers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Snapshot {
    /// Minimum reported rows and columns across the current viewers.
    target: Option<Dimensions>,
    /// Last grid actually acknowledged, retained while another target is pending.
    applied: Option<Dimensions>,
    /// Result of applying the current target.
    status: ResizeStatus,
    /// Number of negotiated viewers, including those not yet measured.
    viewers: usize,
    /// Oldest live viewer; its worker owns guest I/O.
    coordinator: Option<u64>,
    /// Changes when the target or coordinator changes, invalidating old replies.
    revision: u64,
}

impl Snapshot {
    /// Serialize only the public status and last acknowledged grid.
    pub fn message(self) -> String {
        serde_json::json!({
            "type": "resize", "status": self.status, "size": self.applied,
            "viewers": self.viewers,
        })
        .to_string()
    }
}

/// Mutable membership and private socket identity for one VM process generation.
#[derive(Debug)]
struct State {
    /// Local viewport reports, ordered by viewer arrival.
    viewers: BTreeMap<u64, Option<Dimensions>>,
    /// Next connection identity within this VM generation.
    next: u64,
    /// Firecracker's private vsock UDS, never an IP endpoint.
    socket: Option<PathBuf>,
    /// Current published state.
    snapshot: Snapshot,
}

/// Coordinates the single shared tty while keeping guest I/O out of console forwarding.
#[derive(Debug)]
pub(crate) struct ResizeHub {
    /// Short synchronous updates; this lock is never held over guest I/O.
    state: Mutex<State>,
    /// Retains the newest state so bursts cannot accumulate queued resize jobs.
    changes: watch::Sender<Snapshot>,
}

impl ResizeHub {
    /// Start without viewers or an acknowledged grid.
    pub fn new() -> Self {
        let snapshot = Snapshot {
            target: None,
            applied: None,
            status: ResizeStatus::Pending,
            viewers: 0,
            coordinator: None,
            revision: 0,
        };
        let (changes, _) = watch::channel(snapshot);
        Self {
            state: Mutex::new(State {
                viewers: BTreeMap::new(),
                next: 0,
                socket: None,
                snapshot,
            }),
            changes,
        }
    }

    /// Bind this hub to the socket for its VM's current runtime generation.
    pub fn set_socket(&self, path: PathBuf) {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).socket = Some(path);
    }

    /// Observe both membership and guest acknowledgement changes.
    pub fn subscribe(&self) -> watch::Receiver<Snapshot> {
        self.changes.subscribe()
    }

    /// Join the shared grid; dropping the returned guard removes this viewer.
    pub fn register(self: &Arc<Self>) -> Viewer {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let id = state.next;
        state.next += 1;
        state.viewers.insert(id, None);
        self.recompute(&mut state);
        Viewer {
            id,
            hub: Arc::clone(self),
        }
    }

    /// Elect the oldest viewer and independently minimize each reported dimension.
    fn recompute(&self, state: &mut State) {
        let target = state
            .viewers
            .values()
            .flatten()
            .copied()
            .reduce(|a, b| Dimensions {
                rows: a.rows.min(b.rows),
                cols: a.cols.min(b.cols),
            });
        let coordinator = state.viewers.first_key_value().map(|(id, _)| *id);
        let snapshot = &mut state.snapshot;
        if snapshot.target != target || snapshot.coordinator != coordinator {
            snapshot.revision += 1;
            snapshot.status = ResizeStatus::Pending;
        }
        snapshot.target = target;
        snapshot.coordinator = coordinator;
        snapshot.viewers = state.viewers.len();
        if state.viewers.is_empty() {
            snapshot.applied = None;
        }
        self.changes.send_if_modified(|current| {
            if current == snapshot {
                return false;
            }
            *current = *snapshot;
            true
        });
    }

    /// Accept a result only while its target and coordinator revision is current.
    fn publish(&self, revision: u64, size: Dimensions, success: bool) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.snapshot.revision != revision {
            return;
        }
        state.snapshot.status = if success {
            ResizeStatus::Applied
        } else {
            ResizeStatus::Unavailable
        };
        if success {
            state.snapshot.applied = Some(size);
        }
        self.changes.send_replace(state.snapshot);
    }

    /// Every viewer has a worker; only the first live viewer performs guest I/O.
    pub async fn synchronize(self: Arc<Self>, id: u64, mut exited: watch::Receiver<bool>) {
        let mut changes = self.subscribe();
        loop {
            if *exited.borrow() {
                return;
            }
            let snapshot = *changes.borrow_and_update();
            let target = snapshot.target.filter(|_| snapshot.coordinator == Some(id));
            let waiting = target.is_none()
                || (snapshot.status == ResizeStatus::Applied && snapshot.applied == target);
            if waiting {
                tokio::select! { biased;
                    _ = exited.changed() => return,
                    changed = changes.changed() => { if changed.is_err() { return; } }
                }
                continue;
            }
            // A watch channel retains only the final size during a resize burst.
            tokio::select! { biased;
                _ = exited.changed() => return,
                _ = changes.changed() => continue,
                _ = tokio::time::sleep(COALESCE) => {}
            }
            let path = self
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .socket
                .clone();
            let size = target.expect("checked target");
            let apply = async {
                let Some(path) = path else {
                    return false;
                };
                tokio::time::timeout(TIMEOUT, apply_size(path, size))
                    .await
                    .is_ok_and(|r| r.is_ok())
            };
            let success = tokio::select! { biased;
                _ = exited.changed() => return,
                _ = changes.changed() => continue,
                result = apply => result,
            };
            self.publish(snapshot.revision, size, success);
            let latest = *changes.borrow_and_update();
            if latest.revision != snapshot.revision {
                continue;
            }
            tokio::select! { biased;
                _ = exited.changed() => return,
                _ = changes.changed() => {},
                _ = tokio::time::sleep(RETRY), if !success => {},
            }
        }
    }
}

/// Connection-scoped membership guard for a negotiated browser viewer.
pub(crate) struct Viewer {
    /// Identity used to elect one of the live connection workers.
    pub id: u64,
    /// Shared process-generation state to release on disconnect.
    hub: Arc<ResizeHub>,
}

impl Viewer {
    /// Update a valid viewport without waking workers for duplicate reports.
    pub fn report(&self, size: Dimensions) {
        let mut state = self.hub.state.lock().unwrap_or_else(|p| p.into_inner());
        if !size.valid() || state.viewers.get(&self.id) == Some(&Some(size)) {
            return;
        }
        state.viewers.insert(self.id, Some(size));
        self.hub.recompute(&mut state);
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        let mut state = self.hub.state.lock().unwrap_or_else(|p| p.into_inner());
        state.viewers.remove(&self.id);
        self.hub.recompute(&mut state);
    }
}

/// Negotiate Firecracker's host-vsock transport and verify the exact applied grid.
async fn apply_size(path: PathBuf, size: Dimensions) -> io::Result<()> {
    let mut stream = UnixStream::connect(path).await?;
    stream.write_all(b"CONNECT 47100\n").await?;
    let mut greeting = Vec::new();
    let mut terminated = false;
    for _ in 0..64 {
        let byte = stream.read_u8().await?;
        if byte == b'\n' {
            terminated = true;
            break;
        }
        greeting.push(byte);
    }
    if !terminated
        || !greeting.starts_with(b"OK ")
        || greeting.len() <= 3
        || !greeting[3..].iter().all(u8::is_ascii_digit)
    {
        return Err(io::Error::other("invalid guest vsock handshake"));
    }
    let [rh, rl] = size.rows.to_be_bytes();
    let [ch, cl] = size.cols.to_be_bytes();
    stream
        .write_all(&[b'F', b'C', b'R', b'1', rh, rl, ch, cl])
        .await?;
    let mut reply = [0; 8];
    stream.read_exact(&mut reply).await?;
    if reply != [b'F', b'C', b'A', b'1', rh, rl, ch, cl] {
        return Err(io::Error::other("guest did not apply terminal dimensions"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
