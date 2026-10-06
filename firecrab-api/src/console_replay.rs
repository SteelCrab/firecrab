//! Scrollback must not ask the viewer's terminal to answer old queries.
//! Those replies would go to the current guest shell as keystrokes, so an
//! Ubuntu console can receive `RRRexit` instead of `exit` on reattachment.

/// Keep display sequences, but remove complete terminal queries from history.
/// Incomplete sequences pass through: the live stream may still finish one
/// whose guest program is waiting for an answer. Only 7-bit ESC forms are
/// parsed, so continuation bytes in UTF-8 text cannot become control codes.
pub(crate) fn without_terminal_queries(history: &[u8]) -> Vec<u8> {
    let mut replay = Vec::with_capacity(history.len());
    let mut rest = history;
    while let Some(start) = rest.iter().position(|&byte| byte == 0x1b) {
        replay.extend_from_slice(&rest[..start]);
        rest = &rest[start..];
        let Some((length, query)) = escape_sequence(rest) else {
            break;
        };
        if !query {
            replay.extend_from_slice(&rest[..length]);
        }
        rest = &rest[length..];
    }
    replay.extend_from_slice(rest);
    replay
}

fn escape_sequence(bytes: &[u8]) -> Option<(usize, bool)> {
    match *bytes.get(1)? {
        b'[' => {
            let end = bytes[2..]
                .iter()
                .position(|byte| !(0x20..=0x3f).contains(byte))?
                + 2;
            let body = &bytes[2..end];
            let query = match bytes[end] {
                // Device status (including cursor position) and attributes.
                b'n' | b'c' => body.iter().all(|byte| (0x30..=0x3f).contains(byte)),
                b'p' => body.ends_with(b"$"), // Request mode (DECRQM).
                // Window reports; preserve actions such as resizing (8).
                b't' => matches!(
                    body,
                    b"11" | b"13" | b"14" | b"15" | b"16" | b"18" | b"19" | b"20" | b"21"
                ),
                _ => false,
            };
            Some((end + 1, query))
        }
        kind @ (b'P' | b']' | b'X' | b'^' | b'_') => {
            let (end, terminator_len) = string_end(bytes, kind == b']')?;
            let body = &bytes[2..end];
            let query = match kind {
                b'P' => body.starts_with(b"$q") || body.starts_with(b"+q"),
                b']' => osc_query(body),
                _ => false,
            };
            Some((end + terminator_len, query))
        }
        _ => Some((2, false)),
    }
}

fn string_end(bytes: &[u8], allow_bell: bool) -> Option<(usize, usize)> {
    (2..bytes.len()).find_map(|end| {
        if allow_bell && bytes[end] == 0x07 {
            return Some((end, 1));
        }
        (bytes[end..].starts_with(b"\x1b\\")).then_some((end, 2))
    })
}

fn osc_query(body: &[u8]) -> bool {
    let mut fields = body.split(|&byte| byte == b';');
    let command = fields.next().unwrap_or_default();
    matches!(
        command,
        b"4" | b"5"
            | b"10"
            | b"11"
            | b"12"
            | b"13"
            | b"14"
            | b"15"
            | b"16"
            | b"17"
            | b"18"
            | b"19"
            | b"52"
    ) && fields.any(|field| field == b"?")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ubuntu_logout_queries_cannot_send_cursor_replies_to_the_next_shell() {
        let history = b"logout\r\n\x1b[!p\x1b]104\x1b\\\x1b[0m\x1b[?7h\x1b[1G\x1b[0J\x1b[6n\x1b[32766;32766H\x1b[6nroot# ";
        assert_eq!(
            without_terminal_queries(history),
            b"logout\r\n\x1b[!p\x1b]104\x1b\\\x1b[0m\x1b[?7h\x1b[1G\x1b[0J\x1b[32766;32766Hroot# "
        );
    }

    #[test]
    fn terminal_queries_are_removed_without_replaying_their_payloads() {
        for query in [
            b"\x1b[5n".as_slice(),
            b"\x1b[?6n",
            b"\x1b[c",
            b"\x1b[>0c",
            b"\x1b[?25$p",
            b"\x1b[18t",
            b"\x1bP+q6E616D65\x1b\\",
            b"\x1bP$qm\x1b\\",
            b"\x1b]10;?\x07",
            b"\x1b]4;1;?\x1b\\",
            b"\x1b]52;c;?\x07",
        ] {
            let history = [b"before".as_slice(), query, b"after"].concat();
            assert_eq!(
                without_terminal_queries(&history),
                b"beforeafter",
                "{query:?}"
            );
        }
    }

    #[test]
    fn display_sequences_titles_utf8_and_incomplete_queries_are_preserved() {
        for history in [
            "한글 🦀\x1b[31mred\x1b[0m\x1b[8;24;80t".as_bytes(),
            b"\x1b]0;title with [6n\x07\x1b]10;#ffffff\x1b\\",
            b"\x1b",
            b"\x1b[",
            b"\x1b[6",
            b"\x1bP+q6E616D65\x1b",
        ] {
            assert_eq!(without_terminal_queries(history), history);
        }
    }
}
