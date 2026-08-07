//! Wait for a fixed console sentinel with an exit code — the pattern
//! bootstrap (and formerly guest package actions) use when there is no
//! guest agent to ask directly (`public-docs/networking.md`).

use std::time::Duration;

use tokio::sync::broadcast;

/// Bytes of command output kept for the caller's `output_tail` — enough
/// for the last several dozen lines without holding a full transcript in
/// memory for the life of the session.
pub(crate) const OUTPUT_TAIL_CAP: usize = 8 * 1024;

/// Reads console output until `sentinel` appears as `<sentinel>:<code>`,
/// the console closes, or `timeout` elapses — `Ok` carries the guest-
/// reported exit code plus the output seen so far (capped at
/// [`OUTPUT_TAIL_CAP`]).
pub(crate) async fn wait_for_completion_with_sentinel(
    receiver: &mut broadcast::Receiver<Vec<u8>>,
    timeout: Duration,
    sentinel: &str,
) -> Result<(i32, String), String> {
    let mut tail = Vec::new();
    let wait = async {
        loop {
            match receiver.recv().await {
                Ok(chunk) => {
                    tail.extend_from_slice(&chunk);
                    if tail.len() > OUTPUT_TAIL_CAP {
                        let excess = tail.len() - OUTPUT_TAIL_CAP;
                        tail.drain(..excess);
                    }
                    if let Some(code) = find_sentinel(&tail, sentinel) {
                        return Ok((code, String::from_utf8_lossy(&tail).into_owned()));
                    }
                }
                // A lagged receiver just missed some buffered output — the
                // command is still running fine, so keep reading forward
                // instead of treating it as the console having closed (see
                // `wait_for_network_ready`'s identical handling).
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => {
                    return Err("console closed before the command finished".to_owned());
                }
            }
        }
    };

    tokio::time::timeout(timeout, wait)
        .await
        .unwrap_or_else(|_| Err("timed out waiting for the command to finish".to_owned()))
}

/// Finds the last complete `<sentinel>:<code>` line in `buffer` and parses
/// its exit code. The literal command text itself (echoed back as it's
/// typed, containing the unexpanded `$?`) never parses as a number, so it
/// can't be mistaken for the real result.
pub(crate) fn find_sentinel(buffer: &[u8], sentinel: &str) -> Option<i32> {
    let text = String::from_utf8_lossy(buffer);
    text.lines().rev().find_map(|line| {
        let (_, rest) = line.split_once(sentinel)?;
        rest.trim_start_matches(':').trim().parse().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::ConsoleBroker;

    const TEST_SENTINEL: &str = "FIRECRAB_TEST_DONE";

    #[test]
    fn find_done_sentinel_ignores_the_echoed_command_and_finds_the_real_result() {
        let buffer = b"Reading package lists...\n\
            echo \"FIRECRAB_TEST_DONE:$?\"\n\
            FIRECRAB_TEST_DONE:0\n";
        assert_eq!(find_sentinel(buffer, TEST_SENTINEL), Some(0));
    }

    #[test]
    fn find_done_sentinel_reports_a_nonzero_exit_code() {
        assert_eq!(
            find_sentinel(b"FIRECRAB_TEST_DONE:100\n", TEST_SENTINEL),
            Some(100)
        );
    }

    #[test]
    fn find_done_sentinel_is_none_before_the_command_finishes() {
        assert_eq!(
            find_sentinel(b"Reading package lists...\n", TEST_SENTINEL),
            None
        );
    }

    #[tokio::test]
    async fn wait_for_completion_survives_a_lagged_receiver() {
        let console = ConsoleBroker::new();
        let (_backlog, mut receiver) = console.subscribe();

        let waiter = tokio::spawn(async move {
            wait_for_completion_with_sentinel(&mut receiver, Duration::from_secs(5), TEST_SENTINEL)
                .await
        });
        tokio::task::yield_now().await;

        for _ in 0..300 {
            console.push_output(b"Unpacking...\n");
        }
        console.push_output(b"FIRECRAB_TEST_DONE:0\n");

        let (code, tail) = waiter.await.expect("waiter task panicked").unwrap();
        assert_eq!(code, 0);
        assert!(tail.contains("FIRECRAB_TEST_DONE:0"));
    }

    #[tokio::test]
    async fn wait_for_completion_caps_the_tail_at_output_tail_cap() {
        let console = ConsoleBroker::new();
        let (_backlog, mut receiver) = console.subscribe();

        let waiter = tokio::spawn(async move {
            wait_for_completion_with_sentinel(&mut receiver, Duration::from_secs(5), TEST_SENTINEL)
                .await
        });
        tokio::task::yield_now().await;

        // One line short of OUTPUT_TAIL_CAP by itself is well past it once
        // repeated — enough to force the rolling-buffer trim, not just fill
        // it exactly.
        let line = "x".repeat(100) + "\n";
        for _ in 0..(OUTPUT_TAIL_CAP / line.len() + 10) {
            console.push_output(line.as_bytes());
        }
        console.push_output(b"FIRECRAB_TEST_DONE:0\n");

        let (code, tail) = waiter.await.expect("waiter task panicked").unwrap();
        assert_eq!(code, 0);
        assert!(tail.len() <= OUTPUT_TAIL_CAP + 64);
        assert!(tail.contains("FIRECRAB_TEST_DONE:0"));
    }
}
