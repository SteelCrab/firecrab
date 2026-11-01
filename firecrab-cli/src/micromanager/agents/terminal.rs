//! Attach to the recorded Herdr PTY. Disconnecting releases control, never the agent.
use super::*;
use axum::extract::ws::{Message, WebSocket};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::SinkExt;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};

const MAX_RECORD: usize = 4 * 1024 * 1024;
const MAX_INPUT: usize = 256 * 1024;
const BUSY: &str = "FIRECRAB_TERMINAL_BUSY";
const BUSY_REASON: &str = "Verification is running in this workspace. Terminal control was released; input was not retried.";

#[cfg(test)]
mod qa;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attach {
    pub controller: String,
    pub cols: u16,
    pub rows: u16,
}
impl Attach {
    pub fn validate(&self) -> Result<(), Error> {
        if !(2..=400).contains(&self.cols) || !(2..=200).contains(&self.rows) {
            return Err(invalid("Terminal dimensions are out of range."));
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum Input {
    #[serde(rename = "terminal.input")]
    Bytes { bytes: String },
    #[serde(rename = "terminal.resize")]
    Resize { cols: u16, rows: u16 },
    #[serde(rename = "terminal.scroll")]
    Scroll {
        direction: String,
        lines: u16,
        column: u16,
        row: u16,
        modifiers: u8,
    },
    #[serde(rename = "terminal.mouse")]
    Mouse {
        action: String,
        button: String,
        column: u16,
        row: u16,
        modifiers: u8,
    },
}
impl Input {
    fn parse(raw: &str) -> Result<Self, Error> {
        let value: Self =
            serde_json::from_str(raw).map_err(|_| invalid("Invalid terminal input."))?;
        let valid = match &value {
            Self::Bytes { bytes } => {
                bytes.len() <= MAX_INPUT.div_ceil(3) * 4
                    && STANDARD
                        .decode(bytes)
                        .is_ok_and(|b| !b.is_empty() && b.len() <= MAX_INPUT)
            }
            Self::Resize { cols, rows } => (2..=400).contains(cols) && (2..=200).contains(rows),
            Self::Scroll {
                direction,
                lines,
                column,
                row,
                modifiers,
            } => {
                ["up", "down"].contains(&direction.as_str())
                    && (1..=100).contains(lines)
                    && *column < 400
                    && *row < 200
                    && *modifiers < 8
            }
            Self::Mouse {
                action,
                button,
                column,
                row,
                modifiers,
            } => {
                ["down", "up", "drag", "move"].contains(&action.as_str())
                    && ["left", "right", "middle"].contains(&button.as_str())
                    && *column < 400
                    && *row < 200
                    && *modifiers < 8
            }
        };
        if !valid {
            return Err(invalid("Terminal input is out of range."));
        }
        Ok(value)
    }
}

impl Runtime {
    pub fn terminal_command(&self, id: Uuid, attach: &Attach) -> Result<Command, Error> {
        attach.validate()?;
        let guest = self
            .guest
            .as_ref()
            .ok_or_else(|| invalid("Select a MicroVM first."))?;
        let agent = self.agent(id)?;
        if agent.closed {
            return Err(invalid("This agent session is closed."));
        }
        self.pane(&agent)?;
        if self.automation_busy(id)? {
            return Err(invalid(
                "Verification is running in this workspace. Wait before controlling its terminal.",
            ));
        }
        let command = self.guest_script(&[
            "terminal",
            "session",
            "control",
            &agent.terminal_id,
            "--cols",
            &attach.cols.to_string(),
            "--rows",
            &attach.rows.to_string(),
        ]);
        // Check inside the guest for each input, including runs started after attach.
        // Never turn an interrupted or rejected keystroke into a replayed instruction.
        let root = self.automation_root(id)?;
        let script = format!(
            r#"
root={root}
boot=$(cat /proc/sys/kernel/random/boot_id)
busy() {{
  [ -f "$root/latest" ] || return 1
  job=''
  IFS= read -r job < "$root/latest" || [ -n "$job" ] || return 1
  case "$job" in *[!a-f0-9-]*|'') return 1;; esac
  [ -f "$root/$job/state" ] && [ -f "$root/$job/boot" ] || return 1
  IFS= read -r state < "$root/$job/state" || [ -n "$state" ] || return 1
  IFS= read -r job_boot < "$root/$job/boot" || [ -n "$job_boot" ] || return 1
  [ "$state" = running ] && [ "$job_boot" = "$boot" ]
}}
while IFS= read -r line; do
  case "$line" in
    '{{"type":"terminal.input",'*|'{{"type":"terminal.mouse",'*)
      if busy; then printf '{BUSY}\n' >&2; break; fi ;;
  esac
  printf '%s\n' "$line"
done | ( {command} )
"#
        );
        let mut command = guest.command(&script)?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Ok(command)
    }
}

// A record remains owned by one reader task across socket input and resize events.
// Cancelling read_line in select! can otherwise discard half an ANSI frame.
async fn record(
    reader: &mut (impl AsyncBufRead + Unpin),
    limit: usize,
) -> io::Result<Option<String>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Incomplete terminal frame",
                ))
            };
        }
        let count = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |p| p + 1);
        if bytes.len() + count > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Terminal frame is too large",
            ));
        }
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if bytes.last() == Some(&b'\n') {
            return String::from_utf8(bytes)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
        }
    }
}

async fn error(socket: &mut WebSocket, message: &str) {
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        socket.send(Message::Text(
            json!({"type":"terminal.error", "message":message})
                .to_string()
                .into(),
        )),
    )
    .await;
}

pub async fn bridge(mut socket: WebSocket, command: Command) {
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            error(&mut socket, &e.to_string()).await;
            return;
        }
    };
    let mut stdin = child.stdin.take().expect("piped terminal input");
    let mut stdout = BufReader::new(child.stdout.take().expect("piped terminal output"));
    let mut stderr = BufReader::new(child.stderr.take().expect("piped terminal diagnostics"));
    let (frames, mut output) = tokio::sync::mpsc::channel(2);
    let reader = tokio::spawn(async move {
        loop {
            match record(&mut stdout, MAX_RECORD).await {
                Ok(Some(line)) => {
                    if frames.send(Ok(line)).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    let _ = frames.send(Err(e.to_string())).await;
                    break;
                }
            }
        }
    });
    let (diagnostics, mut errors) = tokio::sync::mpsc::channel(8);
    let diagnostic_reader = tokio::spawn(async move {
        while let Ok(Some(line)) = record(&mut stderr, 8192).await {
            if diagnostics.send(line).await.is_err() {
                break;
            }
        }
    });
    let mut diagnostic = String::new();
    let mut diagnostics_done = false;
    loop {
        tokio::select! {
            message = socket.recv() => {
                match message {
                    Some(Ok(Message::Text(raw))) => {
                        let input = match Input::parse(&raw) {
                            Ok(input) => input,
                            Err(e) => { error(&mut socket, &e.to_string()).await; break; }
                        };
                        let line = format!("{}\n", serde_json::to_string(&input).expect("serializable terminal input"));
                        if !matches!(tokio::time::timeout(Duration::from_secs(3), stdin.write_all(line.as_bytes())).await, Ok(Ok(()))) { break; }
                    }
                    Some(Ok(Message::Ping(data))) => { if socket.send(Message::Pong(data)).await.is_err() { break; } }
                    Some(Ok(Message::Pong(_))) => {}
                    _ => break,
                }
            }
            line = errors.recv(), if !diagnostics_done => {
                match line {
                    Some(line) if line.trim() == BUSY => {
                        error(&mut socket, BUSY_REASON).await;
                        break;
                    }
                    Some(line) => { if diagnostic.len() < 8192 { diagnostic.push_str(&line); } }
                    None => diagnostics_done = true,
                }
            }
            line = output.recv() => {
                let raw = match line {
                    Some(Ok(line)) => line,
                    Some(Err(e)) => { error(&mut socket, &e).await; break; }
                    None => {
                        // A rejected controller can close stdout before its diagnostic arrives.
                        let _ = tokio::time::timeout(Duration::from_millis(250), async {
                            while let Some(line) = errors.recv().await { if diagnostic.len() < 8192 { diagnostic.push_str(&line); } }
                        }).await;
                        let message = if diagnostic.lines().any(|line| line.trim() == BUSY) { BUSY_REASON }
                            else if diagnostic.trim().is_empty() { "Terminal connection ended. Reconnect to the existing session." }
                            else { diagnostic.trim() };
                        error(&mut socket, message).await;
                        break;
                    }
                };
                let Ok(frame) = serde_json::from_str::<Value>(&raw) else { error(&mut socket, "Invalid terminal frame. Update Herdr in this MicroVM.").await; break; };
                let closed = frame["type"] == "terminal.closed";
                if !closed && !(frame["type"] == "terminal.frame" && frame["encoding"] == "ansi" && frame["bytes"].is_string()) {
                    error(&mut socket, "Unsupported terminal stream. Update Herdr in this MicroVM.").await; break;
                }
                if !matches!(tokio::time::timeout(Duration::from_secs(5), socket.send(Message::Text(raw.into()))).await, Ok(Ok(()))) || closed { break; }
            }
        }
    }
    // Detach only this controller. The Herdr-owned PTY and CLI remain alive.
    let _ = tokio::time::timeout(
        Duration::from_millis(250),
        stdin.write_all(b"{\"type\":\"terminal.release\"}\n"),
    )
    .await;
    drop(stdin);
    if tokio::time::timeout(Duration::from_secs(1), child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    reader.abort();
    diagnostic_reader.abort();
    let _ = socket.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_preserves_raw_keys_and_rejects_scope_changes_and_oversized_payloads() {
        let keys = b"\x1b[A\t\x03\x1b[200~hello\nworld\x1b[201~";
        let raw = json!({"type":"terminal.input","bytes":STANDARD.encode(keys)}).to_string();
        let Input::Bytes { bytes } = Input::parse(&raw).unwrap() else {
            panic!("input");
        };
        assert_eq!(STANDARD.decode(bytes).unwrap(), keys);
        assert!(
            Input::parse(
                &json!({"type":"terminal.input","bytes":STANDARD.encode(vec![b'x'; MAX_INPUT])})
                    .to_string()
            )
            .is_ok()
        );
        for invalid in [
            json!({"type":"terminal.input","bytes":"YQ==","target":"another-terminal"}),
            json!({"type":"terminal.input","bytes":STANDARD.encode(vec![0;MAX_INPUT + 1])}),
            json!({"type":"terminal.input","bytes":"bad base64"}),
            json!({"type":"terminal.resize","cols":0,"rows":20}),
            json!({"type":"terminal.resize","cols":100,"rows":201}),
            json!({"type":"terminal.mouse","action":"down","button":"left","column":400,"row":0,"modifiers":0}),
        ] {
            assert!(Input::parse(&invalid.to_string()).is_err());
        }
    }
    #[tokio::test]
    async fn records_preserve_split_utf8_and_reject_truncation_or_unbounded_frames() {
        let (mut writer, stream) = tokio::io::duplex(8);
        let task = tokio::spawn(async move {
            writer.write_all("한글\nnext\n".as_bytes()).await.unwrap();
        });
        let mut reader = BufReader::new(stream);
        assert_eq!(record(&mut reader, 16).await.unwrap().unwrap(), "한글\n");
        assert_eq!(record(&mut reader, 16).await.unwrap().unwrap(), "next\n");
        assert!(record(&mut reader, 16).await.unwrap().is_none());
        task.await.unwrap();
        assert!(
            record(&mut BufReader::new(&b"truncated"[..]), 16)
                .await
                .is_err()
        );
        assert!(
            record(&mut BufReader::new(&b"oversized\n"[..]), 4)
                .await
                .is_err()
        );
    }
}
