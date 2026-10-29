//! State for one shared terminal's output and input controller.
//!
//! This library does not start a shell or authorize users. The transport must
//! authenticate and authorize every operation, including each input frame.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard};

use tokio::sync::watch;
use uuid::Uuid;

/// Output returned after a requested byte cursor.
#[derive(Debug, PartialEq, Eq)]
pub struct Replay {
    /// Cursor of the first returned byte.
    pub start: u64,
    /// Cursor immediately after the last returned byte.
    pub end: u64,
    /// The requested cursor was older than the retained output.
    pub truncated: bool,
    /// Retained bytes, in their original order.
    pub bytes: Vec<u8>,
}

/// A request could not be applied to the current terminal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The connection has not joined, or has already disconnected.
    UnknownParticipant,
    /// The connection cannot hold terminal input control.
    ControlDenied,
    /// Another connection already holds terminal input control.
    ControlTaken,
    /// The connection does not hold terminal input control.
    NotController,
    /// The requested output cursor is ahead of the terminal.
    FutureCursor,
    /// The output cursor cannot represent additional bytes.
    CursorOverflow,
}

struct State {
    participants: HashMap<Uuid, bool>,
    controller: Option<Uuid>,
    output: VecDeque<u8>,
    end: u64,
}

/// In-memory state for a single terminal process generation.
///
/// Participant IDs must identify individual authenticated connections. The
/// caller supplies `can_control` only after applying current server-side
/// permissions. Removing a participant releases its control immediately.
pub struct Terminal {
    capacity: usize,
    state: Mutex<State>,
    changes: watch::Sender<u64>,
}

impl Terminal {
    /// Create a terminal with a bounded output history measured in bytes.
    pub fn new(capacity: usize) -> Self {
        let (changes, _) = watch::channel(0);
        Self {
            capacity,
            state: Mutex::new(State {
                participants: HashMap::new(),
                controller: None,
                output: VecDeque::new(),
                end: 0,
            }),
            changes,
        }
    }

    /// Register or update an authenticated connection's control permission.
    pub fn set_participant(&self, participant: Uuid, can_control: bool) {
        let mut state = self.lock();
        state.participants.insert(participant, can_control);
        if !can_control && state.controller == Some(participant) {
            state.controller = None;
        }
    }

    /// Remove a connection without stopping the terminal process.
    pub fn disconnect(&self, participant: Uuid) {
        let mut state = self.lock();
        state.participants.remove(&participant);
        if state.controller == Some(participant) {
            state.controller = None;
        }
    }

    /// Acquire control if no other connection currently holds it.
    pub fn acquire_control(&self, participant: Uuid) -> Result<(), Error> {
        let mut state = self.lock();
        match state.participants.get(&participant) {
            None => return Err(Error::UnknownParticipant),
            Some(false) => return Err(Error::ControlDenied),
            Some(true) => {}
        }
        if state.controller.is_some_and(|owner| owner != participant) {
            return Err(Error::ControlTaken);
        }
        state.controller = Some(participant);
        Ok(())
    }

    /// Release control only when held by this connection.
    pub fn release_control(&self, participant: Uuid) -> Result<(), Error> {
        let mut state = self.lock();
        if state.controller != Some(participant) {
            return Err(Error::NotController);
        }
        state.controller = None;
        Ok(())
    }

    /// Enqueue input while holding the controller lock.
    ///
    /// `enqueue` must be nonblocking. Keeping authorization and enqueue under
    /// one lock prevents a released controller from sending a later frame.
    pub fn with_control<T>(
        &self,
        participant: Uuid,
        enqueue: impl FnOnce() -> T,
    ) -> Result<T, Error> {
        let state = self.lock();
        if state.controller != Some(participant) {
            return Err(Error::NotController);
        }
        Ok(enqueue())
    }

    /// Append terminal output and notify subscribers of the new end cursor.
    pub fn append(&self, bytes: &[u8]) -> Result<(), Error> {
        if bytes.is_empty() {
            return Ok(());
        }
        let mut state = self.lock();
        let end = state
            .end
            .checked_add(bytes.len() as u64)
            .ok_or(Error::CursorOverflow)?;
        if bytes.len() >= self.capacity {
            state.output.clear();
            state.output.extend(&bytes[bytes.len() - self.capacity..]);
        } else {
            let overflow = (state.output.len() + bytes.len()).saturating_sub(self.capacity);
            state.output.drain(..overflow);
            state.output.extend(bytes);
        }
        state.end = end;
        self.changes.send_replace(end);
        Ok(())
    }

    /// Subscribe before reading a replay to avoid missing concurrent output.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// Read retained bytes after a cursor, reporting any truncated history.
    pub fn replay(&self, after: u64) -> Result<Replay, Error> {
        let state = self.lock();
        if after > state.end {
            return Err(Error::FutureCursor);
        }
        let first = state.end - state.output.len() as u64;
        let start = after.max(first);
        let skip = (start - first) as usize;
        Ok(Replay {
            start,
            end: state.end,
            truncated: after < first,
            bytes: state.output.iter().skip(skip).copied().collect(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn controller_is_exclusive_and_disconnect_or_revocation_releases_it() {
        let terminal = Terminal::new(16);
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let viewer = Uuid::from_u128(3);
        terminal.set_participant(a, true);
        terminal.set_participant(b, true);
        terminal.set_participant(viewer, false);
        assert_eq!(terminal.acquire_control(viewer), Err(Error::ControlDenied));
        assert_eq!(terminal.acquire_control(a), Ok(()));
        assert_eq!(terminal.acquire_control(b), Err(Error::ControlTaken));
        assert_eq!(terminal.with_control(b, || 1), Err(Error::NotController));
        assert_eq!(terminal.with_control(a, || 2), Ok(2));
        terminal.set_participant(a, false);
        assert_eq!(terminal.with_control(a, || 1), Err(Error::NotController));
        assert_eq!(terminal.acquire_control(b), Ok(()));
        terminal.disconnect(b);
        assert_eq!(terminal.with_control(b, || 1), Err(Error::NotController));
        terminal.set_participant(a, true);
        assert_eq!(terminal.acquire_control(a), Ok(()));
    }

    #[test]
    fn replay_reports_truncation_and_rejects_future_cursors() {
        let terminal = Terminal::new(5);
        terminal.append(b"abc").unwrap();
        terminal.append(b"defg").unwrap();
        assert_eq!(
            terminal.replay(0).unwrap(),
            Replay {
                start: 2,
                end: 7,
                truncated: true,
                bytes: b"cdefg".to_vec(),
            }
        );
        assert_eq!(
            terminal.replay(5).unwrap(),
            Replay {
                start: 5,
                end: 7,
                truncated: false,
                bytes: b"fg".to_vec(),
            }
        );
        assert_eq!(terminal.replay(8), Err(Error::FutureCursor));
        terminal.append(b"0123456789").unwrap();
        assert_eq!(terminal.replay(7).unwrap().bytes, b"56789");
    }

    #[test]
    fn only_one_of_two_simultaneous_control_requests_succeeds() {
        let terminal = Arc::new(Terminal::new(16));
        let ready = Arc::new(Barrier::new(3));
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        terminal.set_participant(a, true);
        terminal.set_participant(b, true);
        let threads = [a, b].map(|participant| {
            let terminal = Arc::clone(&terminal);
            let ready = Arc::clone(&ready);
            std::thread::spawn(move || {
                ready.wait();
                terminal.acquire_control(participant)
            })
        });
        ready.wait();
        let results = threads.map(|thread| thread.join().unwrap());
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| **result == Err(Error::ControlTaken))
                .count(),
            1
        );
    }

    #[test]
    fn zero_capacity_still_reports_the_missing_output() {
        let terminal = Terminal::new(0);
        terminal.append(b"output").unwrap();
        assert_eq!(
            terminal.replay(0).unwrap(),
            Replay {
                start: 6,
                end: 6,
                truncated: true,
                bytes: vec![],
            }
        );
    }

    #[tokio::test]
    async fn subscribers_observe_output_written_before_or_after_replay() {
        let terminal = Terminal::new(32);
        let mut changes = terminal.subscribe();
        terminal.append(b"first").unwrap();
        let first = terminal.replay(0).unwrap();
        terminal.append(b" second").unwrap();
        changes.changed().await.unwrap();
        assert_eq!(*changes.borrow_and_update(), 12);
        assert_eq!(terminal.replay(first.end).unwrap().bytes, b" second");
    }
}
