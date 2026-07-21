//! Per-VM serial console bridge. Output: one Firecracker process's stdout
//! (the guest's ttyS0) is read once and broadcast to any number of
//! WebSocket viewers, while the raw bytes are still teed to `console.log`
//! on disk. Input: keystrokes from an attached WebSocket are written
//! straight to the guest's stdin.
//!
//! A late-joining viewer (the common case — a user opens the terminal panel
//! after the VM already booted) needs to see what already happened, not just
//! what happens next. [`ConsoleBroker::subscribe`] hands back a backlog
//! snapshot plus a broadcast receiver, taken atomically under one lock so no
//! byte is ever duplicated or dropped between the two.

use std::collections::VecDeque;
use std::sync::Mutex;

use tokio::io::AsyncWriteExt;
use tokio::process::ChildStdin;
use tokio::sync::broadcast;

/// Bytes of scrollback kept for viewers that connect after boot output has
/// already scrolled by. Generous enough for a full systemd boot log without
/// growing unbounded for a long-lived interactive session.
const MAX_BACKLOG_BYTES: usize = 256 * 1024;
/// Buffered chunks per slow subscriber before it starts missing output
/// ([`broadcast::error::RecvError::Lagged`]). Each chunk is one `read()` off
/// the pipe (up to 4 KiB), so this tolerates several hundred KiB of burst.
const BROADCAST_CAPACITY: usize = 256;

#[derive(Debug)]
pub struct ConsoleBroker {
    state: Mutex<ConsoleState>,
    /// The guest's stdin. A `tokio::sync::Mutex` (not `std`) because holding
    /// it spans the `.await` in `write_input`. Every attached WS session may
    /// write; nothing arbitrates between concurrent typists beyond mutual
    /// exclusion of the write itself, matching the single-operator scenario
    /// this is built for.
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
}

#[derive(Debug)]
struct ConsoleState {
    backlog: VecDeque<u8>,
    output: broadcast::Sender<Vec<u8>>,
}

impl ConsoleBroker {
    pub fn new() -> Self {
        let (output, _receiver) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            state: Mutex::new(ConsoleState {
                backlog: VecDeque::new(),
                output,
            }),
            stdin: tokio::sync::Mutex::new(None),
        }
    }

    /// Hands the broker the write half of the guest's console, once, right
    /// after the process is spawned.
    pub async fn attach_stdin(&self, stdin: ChildStdin) {
        *self.stdin.lock().await = Some(stdin);
    }

    /// Writes keystrokes to the guest's console. A closed or never-attached
    /// stdin is not an error — bytes typed before the pipe exists, or after
    /// the VM has already exited, are silently dropped rather than killing
    /// the WS session over it.
    pub async fn write_input(&self, bytes: &[u8]) {
        let mut guard = self.stdin.lock().await;
        if let Some(stdin) = guard.as_mut()
            && stdin.write_all(bytes).await.is_err()
        {
            *guard = None; // pipe closed (e.g. the guest exited mid-keystroke)
        }
    }

    /// Called from the single console-reader task with each chunk read off
    /// the guest's console. No subscribers is not an error — the bytes still
    /// join the backlog for whoever connects next.
    pub fn push_output(&self, chunk: &[u8]) {
        let mut state = self.lock();
        state.backlog.extend(chunk.iter().copied());
        let overflow = state.backlog.len().saturating_sub(MAX_BACKLOG_BYTES);
        if overflow > 0 {
            state.backlog.drain(..overflow);
        }
        // No receivers yet is expected (no viewer connected) and not an error.
        let _ = state.output.send(chunk.to_vec());
    }

    /// Backlog-so-far plus a receiver for everything from this point on.
    /// Held under one lock so nothing sent between the snapshot and the
    /// subscribe call can be missed or double-delivered.
    pub fn subscribe(&self) -> (Vec<u8>, broadcast::Receiver<Vec<u8>>) {
        let state = self.lock();
        (state.backlog.iter().copied().collect(), state.output.subscribe())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ConsoleState> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for ConsoleBroker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_subscriber_gets_the_backlog() {
        let broker = ConsoleBroker::new();
        broker.push_output(b"boot line 1\n");
        broker.push_output(b"boot line 2\n");

        let (backlog, _receiver) = broker.subscribe();
        assert_eq!(backlog, b"boot line 1\nboot line 2\n");
    }

    #[test]
    fn backlog_is_capped_and_drops_the_oldest_bytes() {
        let broker = ConsoleBroker::new();
        let chunk = vec![b'x'; MAX_BACKLOG_BYTES / 2 + 1];
        broker.push_output(&chunk);
        broker.push_output(&chunk);
        broker.push_output(b"tail");

        let (backlog, _receiver) = broker.subscribe();
        assert!(backlog.len() <= MAX_BACKLOG_BYTES);
        assert!(backlog.ends_with(b"tail"));
    }

    #[tokio::test]
    async fn subscriber_receives_output_pushed_after_it_joins() {
        let broker = ConsoleBroker::new();
        let (backlog, mut receiver) = broker.subscribe();
        assert!(backlog.is_empty());

        broker.push_output(b"live output");
        let received = receiver.recv().await.unwrap();
        assert_eq!(received, b"live output");
    }

    #[tokio::test]
    async fn two_subscribers_both_see_the_same_live_output() {
        let broker = ConsoleBroker::new();
        let (_backlog_a, mut a) = broker.subscribe();
        let (_backlog_b, mut b) = broker.subscribe();

        broker.push_output(b"shared");
        assert_eq!(a.recv().await.unwrap(), b"shared");
        assert_eq!(b.recv().await.unwrap(), b"shared");
    }

    #[test]
    fn push_output_with_no_subscribers_is_not_an_error() {
        let broker = ConsoleBroker::new();
        broker.push_output(b"nobody is listening");
        let (backlog, _receiver) = broker.subscribe();
        assert_eq!(backlog, b"nobody is listening");
    }

    #[tokio::test]
    async fn write_input_before_stdin_is_attached_is_silently_dropped() {
        let broker = ConsoleBroker::new();
        // Must not panic or block forever with no stdin ever attached.
        broker.write_input(b"echo hi\n").await;
    }

    #[tokio::test]
    async fn write_input_forwards_bytes_to_the_attached_pipe() {
        // A real ChildStdin can only come from a real Child, so exercise the
        // write path through a tiny `cat`-like process instead of mocking.
        let mut child = tokio::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn cat");
        let stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();

        let broker = ConsoleBroker::new();
        broker.attach_stdin(stdin).await;
        broker.write_input(b"hello broker\n").await;

        let mut buffer = [0_u8; 32];
        let read = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::io::AsyncReadExt::read(&mut stdout, &mut buffer).await.unwrap()
        })
        .await
        .expect("cat echoed input back before the timeout");
        assert_eq!(&buffer[..read], b"hello broker\n");

        let _ = child.kill().await;
    }
}
