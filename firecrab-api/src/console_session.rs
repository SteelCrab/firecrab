//! Guest serial-console session boundary (issue #303).
//!
//! `ttyS0` is a respawned root autologin, so `exit` only starts a new shell
//! on the same terminal. The guest prints [`SESSION_ENDED_MARKER`] right
//! before that respawned session starts, and the console WebSocket turns it
//! into a close frame (`firecrab_api_types::CONSOLE_SESSION_ENDED_CLOSE_CODE`)
//! so the CLI and the dashboard can end the session instead of silently
//! reattaching to the next shell.

/// Written to `ttyS0` right before a respawned login session starts. An OSC
/// string with a non-numeric identifier, which terminals discard, so the raw
/// `console.log` shows nothing where it was.
pub(crate) const SESSION_ENDED_MARKER: &[u8] = b"\x1b]firecrab;session-ended\x07";

/// printf(1) format that writes [`SESSION_ENDED_MARKER`]. Octal escapes,
/// which every guest printf (busybox, coreutils, dash) understands. A macro
/// so guest scripts can `concat!` it into their `const` bodies.
macro_rules! session_ended_marker_printf {
    () => {
        r"\033]firecrab;session-ended\007"
    };
}
pub(crate) use session_ended_marker_printf;

/// What one live chunk of console output means for an attached viewer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ScannedOutput {
    /// Forward these bytes; the session continues.
    Output(Vec<u8>),
    /// Forward these bytes, then end the viewer's session.
    SessionEnded(Vec<u8>),
}

/// Finds [`SESSION_ENDED_MARKER`] in one viewer's live console output,
/// including a marker split across chunks.
#[derive(Debug, Default)]
pub(crate) struct SessionEndScanner {
    /// Trailing bytes that could still be the start of a marker.
    held: Vec<u8>,
    /// Marker prefix already released by [`Self::flush`]. A later chunk can
    /// still finish that marker; anything else clears the memory.
    flushed_prefix: usize,
}

impl SessionEndScanner {
    /// Removes every marker from the backlog replayed on attach: those
    /// sessions ended before this viewer arrived, so they must not end its
    /// session. A trailing partial marker is held for the first live chunk.
    pub(crate) fn backlog(&mut self, backlog: &[u8]) -> Vec<u8> {
        let mut replay = Vec::with_capacity(backlog.len());
        let mut rest = backlog;
        while let Some(position) = find_marker(rest) {
            replay.extend_from_slice(&rest[..position]);
            rest = &rest[position + SESSION_ENDED_MARKER.len()..];
        }
        let (visible, held) = rest.split_at(rest.len() - partial_marker_len(rest));
        replay.extend_from_slice(visible);
        self.flushed_prefix = 0;
        self.held = held.to_vec();
        replay
    }

    /// Scans one live chunk. Bytes that could still start a marker are held
    /// back until the next chunk or [`Self::flush`].
    pub(crate) fn push(&mut self, chunk: &[u8]) -> ScannedOutput {
        let mut data = std::mem::take(&mut self.held);
        data.extend_from_slice(chunk);
        if data.is_empty() {
            return ScannedOutput::Output(Vec::new());
        }
        if self.flushed_prefix != 0 {
            let remaining = &SESSION_ENDED_MARKER[self.flushed_prefix..];
            let matched = data
                .iter()
                .zip(remaining)
                .take_while(|(actual, expected)| actual == expected)
                .count();
            if matched == remaining.len() {
                self.flushed_prefix = 0;
                return ScannedOutput::SessionEnded(Vec::new());
            }
            if matched == data.len() {
                self.held = data;
                return ScannedOutput::Output(Vec::new());
            }
            self.flushed_prefix = 0;
        }
        if let Some(position) = find_marker(&data) {
            data.truncate(position);
            return ScannedOutput::SessionEnded(data);
        }
        self.held = data.split_off(data.len() - partial_marker_len(&data));
        ScannedOutput::Output(data)
    }

    /// Whether bytes are being held back as a possible marker start.
    pub(crate) fn is_holding(&self) -> bool {
        !self.held.is_empty()
    }

    /// Releases held-back bytes once output has gone idle without the rest of
    /// a marker arriving.
    pub(crate) fn flush(&mut self) -> Vec<u8> {
        let flushed = std::mem::take(&mut self.held);
        let continued = SESSION_ENDED_MARKER
            .get(self.flushed_prefix..self.flushed_prefix + flushed.len())
            == Some(flushed.as_slice());
        if continued {
            self.flushed_prefix += flushed.len();
        } else {
            self.flushed_prefix = 0;
        }
        flushed
    }
}

fn find_marker(data: &[u8]) -> Option<usize> {
    data.windows(SESSION_ENDED_MARKER.len())
        .position(|window| window == SESSION_ENDED_MARKER)
}

/// Length of the longest suffix of `data` that is a proper prefix of
/// [`SESSION_ENDED_MARKER`].
fn partial_marker_len(data: &[u8]) -> usize {
    (1..SESSION_ENDED_MARKER.len().min(data.len() + 1))
        .rev()
        .find(|&length| data.ends_with(&SESSION_ENDED_MARKER[..length]))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marked(before: &[u8], after: &[u8]) -> Vec<u8> {
        [before, SESSION_ENDED_MARKER, after].concat()
    }

    #[test]
    fn the_guest_printf_format_writes_exactly_the_marker() {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("printf '{}'", session_ended_marker_printf!()))
            .output()
            .expect("sh must be available");

        assert_eq!(output.stdout, SESSION_ENDED_MARKER);
    }

    #[test]
    fn a_marker_in_a_live_chunk_ends_the_session_after_the_bytes_before_it() {
        let mut scanner = SessionEndScanner::default();

        let scanned = scanner.push(&marked(b"logout\r\n", b"login: root (automatic login)"));

        assert_eq!(scanned, ScannedOutput::SessionEnded(b"logout\r\n".to_vec()));
    }

    #[test]
    fn a_marker_split_across_chunks_still_ends_the_session() {
        let mut scanner = SessionEndScanner::default();
        let (head, tail) = SESSION_ENDED_MARKER.split_at(5);

        let first = scanner.push(&[b"bye".as_slice(), head].concat());
        let second = scanner.push(&[tail, b"new shell".as_slice()].concat());

        assert_eq!(first, ScannedOutput::Output(b"bye".to_vec()));
        assert_eq!(second, ScannedOutput::SessionEnded(Vec::new()));
    }

    #[test]
    fn held_bytes_that_are_not_a_marker_are_released_with_the_next_chunk() {
        let mut scanner = SessionEndScanner::default();

        let first = scanner.push(b"\x1b");
        let second = scanner.push(b"[31mred");

        assert_eq!(first, ScannedOutput::Output(Vec::new()));
        assert_eq!(second, ScannedOutput::Output(b"\x1b[31mred".to_vec()));
    }

    #[test]
    fn flush_releases_a_held_marker_prefix_when_output_goes_idle() {
        let mut scanner = SessionEndScanner::default();

        scanner.push(b"prompt# \x1b]fire");

        assert!(scanner.is_holding());
        assert_eq!(scanner.flush(), b"\x1b]fire");
        assert!(!scanner.is_holding());
    }

    #[test]
    fn a_flushed_prefix_still_ends_the_session_when_the_rest_arrives() {
        let mut scanner = SessionEndScanner::default();
        let (head, tail) = SESSION_ENDED_MARKER.split_at(5);

        assert_eq!(scanner.push(head), ScannedOutput::Output(Vec::new()));
        assert_eq!(scanner.flush(), head);
        assert_eq!(scanner.push(tail), ScannedOutput::SessionEnded(Vec::new()));
    }

    #[test]
    fn a_flushed_prefix_is_ordinary_output_when_the_next_chunk_diverges() {
        let mut scanner = SessionEndScanner::default();

        scanner.push(b"\x1b]f");
        assert_eq!(scanner.flush(), b"\x1b]f");
        assert_eq!(scanner.push(b"oo"), ScannedOutput::Output(b"oo".to_vec()));
    }

    #[test]
    fn markers_in_the_backlog_are_stripped_and_do_not_end_the_session() {
        let mut scanner = SessionEndScanner::default();

        let replay = scanner.backlog(&marked(b"old session\r\n", b"new session\r\n"));
        let live = scanner.push(b"typing");

        assert_eq!(replay, b"old session\r\nnew session\r\n");
        assert_eq!(live, ScannedOutput::Output(b"typing".to_vec()));
    }

    #[test]
    fn a_marker_completed_by_the_first_live_chunk_ends_the_session() {
        let mut scanner = SessionEndScanner::default();
        let (head, tail) = SESSION_ENDED_MARKER.split_at(3);

        let replay = scanner.backlog(&[b"prompt# ".as_slice(), head].concat());
        let live = scanner.push(tail);

        assert_eq!(replay, b"prompt# ");
        assert_eq!(live, ScannedOutput::SessionEnded(Vec::new()));
    }
}
