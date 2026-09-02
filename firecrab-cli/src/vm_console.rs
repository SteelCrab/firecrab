//! Interactive client for a running MicroVM's serial-console WebSocket.

use std::io::IsTerminal;

use futures_util::{SinkExt, StreamExt};
use nix::sys::termios::{self, SetArg, Termios};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
use uuid::Uuid;

/// ASCII Group Separator, conventionally entered as Ctrl+].
const DETACH_BYTE: u8 = 0x1d;

/// Failures while attaching to a VM serial console.
#[derive(Debug, Error)]
pub enum Error {
    /// The configured REST base cannot be converted to a WebSocket URL.
    #[error("invalid API base URL: {0}")]
    InvalidApiUrl(String),
    /// The API rejected the WebSocket upgrade.
    #[error("console handshake HTTP {status}: {body}")]
    Handshake {
        /// HTTP response status.
        status: u16,
        /// API error response body.
        body: String,
    },
    /// Network, TLS, or WebSocket framing failure.
    #[error("console connection failed: {0}")]
    Connection(String),
    /// Reading stdin or writing stdout failed.
    #[error("console I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// The local terminal could not enter raw mode.
    #[error("terminal mode failed: {0}")]
    Terminal(String),
}

/// Attaches the process terminal to the VM's ttyS0 stream until the VM stops,
/// stdin closes, or the operator types Ctrl+].
pub fn attach(api_base: &str, id: Uuid) -> Result<(), Error> {
    let url = console_url(api_base, id)?;
    eprintln!("attaching to VM {id} serial console (Ctrl+] to detach)");

    let raw_terminal = RawTerminal::enter()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Error::Io)?;
    let result = runtime.block_on(stream(
        url.as_str(),
        tokio::io::stdin(),
        tokio::io::stdout(),
    ));

    // Restore canonical input before printing a local status line. This also
    // runs on every error path; Drop remains a second safety net for panics.
    drop(raw_terminal);
    if result.is_ok() {
        eprintln!("\r\nconsole detached");
    }
    result
}

/// Converts an HTTP API base to the console WebSocket endpoint while
/// preserving an optional reverse-proxy path prefix.
fn console_url(api_base: &str, id: Uuid) -> Result<reqwest::Url, Error> {
    let mut url =
        reqwest::Url::parse(api_base).map_err(|error| Error::InvalidApiUrl(error.to_string()))?;
    let websocket_scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        other => {
            return Err(Error::InvalidApiUrl(format!(
                "unsupported scheme {other:?}; expected http or https"
            )));
        }
    };
    url.set_scheme(websocket_scheme)
        .map_err(|_| Error::InvalidApiUrl("could not set WebSocket scheme".to_owned()))?;

    let prefix = url.path().trim_end_matches('/');
    url.set_path(&format!("{prefix}/ws/vms/{id}/console"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

async fn stream<R, W>(url: &str, mut input: R, mut output: W) -> Result<(), Error>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (socket, _) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(map_websocket_error)?;
    let (mut sink, mut source) = socket.split();
    let mut buffer = [0_u8; 4096];

    loop {
        tokio::select! {
            read = input.read(&mut buffer) => {
                let read = read?;
                if read == 0 {
                    let _ = sink.send(Message::Close(None)).await;
                    return Ok(());
                }

                let (guest_input, detach) = split_at_detach(&buffer[..read]);
                if !guest_input.is_empty() {
                    sink.send(Message::Binary(guest_input.to_vec().into()))
                        .await
                        .map_err(map_websocket_error)?;
                }
                if detach {
                    let _ = sink.send(Message::Close(None)).await;
                    return Ok(());
                }
            }
            frame = source.next() => {
                let Some(frame) = frame else {
                    return Ok(());
                };
                match frame.map_err(map_websocket_error)? {
                    Message::Binary(bytes) => {
                        output.write_all(&bytes).await?;
                        output.flush().await?;
                    }
                    Message::Text(text) => {
                        output.write_all(text.as_bytes()).await?;
                        output.flush().await?;
                    }
                    Message::Close(_) => return Ok(()),
                    Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
                }
            }
        }
    }
}

fn split_at_detach(input: &[u8]) -> (&[u8], bool) {
    match input.iter().position(|byte| *byte == DETACH_BYTE) {
        Some(position) => (&input[..position], true),
        None => (input, false),
    }
}

fn map_websocket_error(error: WebSocketError) -> Error {
    if let WebSocketError::Http(response) = error {
        let body = response
            .body()
            .as_deref()
            .map(String::from_utf8_lossy)
            .unwrap_or_default()
            .into_owned();
        return Error::Handshake {
            status: response.status().as_u16(),
            body,
        };
    }
    Error::Connection(error.to_string())
}

/// Restores the exact input terminal attributes when the console exits.
struct RawTerminal {
    original: Termios,
}

impl RawTerminal {
    fn enter() -> Result<Option<Self>, Error> {
        let input = std::io::stdin();
        if !input.is_terminal() {
            return Ok(None);
        }

        let original =
            termios::tcgetattr(&input).map_err(|error| Error::Terminal(error.to_string()))?;
        let mut raw = original.clone();
        termios::cfmakeraw(&mut raw);
        termios::tcsetattr(&input, SetArg::TCSANOW, &raw)
            .map_err(|error| Error::Terminal(error.to_string()))?;
        Ok(Some(Self { original }))
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        let _ = termios::tcsetattr(std::io::stdin(), SetArg::TCSANOW, &self.original);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::time::Duration;

    use tokio::net::TcpListener;
    use tokio::time::sleep;
    use tokio_tungstenite::accept_async;

    use super::*;

    const ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    fn id() -> Uuid {
        Uuid::parse_str(ID).unwrap()
    }

    #[test]
    fn console_url_maps_http_https_and_proxy_prefixes() {
        assert_eq!(
            console_url("http://127.0.0.1:5523", id()).unwrap().as_str(),
            format!("ws://127.0.0.1:5523/ws/vms/{ID}/console")
        );
        assert_eq!(
            console_url("https://cloud.example/firecrab/", id())
                .unwrap()
                .as_str(),
            format!("wss://cloud.example/firecrab/ws/vms/{ID}/console")
        );
    }

    #[test]
    fn console_url_rejects_non_http_schemes() {
        let error = console_url("file:///tmp/firecrab.sock", id()).unwrap_err();
        assert!(error.to_string().contains("expected http or https"));
    }

    #[test]
    fn detach_byte_is_local_and_discards_everything_after_it() {
        assert_eq!(
            split_at_detach(b"echo ok\r"),
            (b"echo ok\r".as_slice(), false)
        );
        assert_eq!(
            split_at_detach(b"echo ok\r\x1dignored"),
            (b"echo ok\r".as_slice(), true)
        );
    }

    #[test]
    fn non_tty_input_does_not_require_terminal_mode_changes() {
        if !std::io::stdin().is_terminal() {
            assert!(RawTerminal::enter().unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn websocket_stream_forwards_bytes_in_both_directions() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(tcp).await.unwrap();
            socket
                .send(Message::Binary(b"booted\r\n$ ".to_vec().into()))
                .await
                .unwrap();
            let message = socket.next().await.unwrap().unwrap();
            assert_eq!(message.into_data(), b"echo ok\r".as_slice());
            socket.close(None).await.unwrap();
        });

        let (mut input_writer, input_reader) = tokio::io::duplex(64);
        let input_task = tokio::spawn(async move {
            sleep(Duration::from_millis(20)).await;
            input_writer.write_all(b"echo ok\r").await.unwrap();
        });
        let mut output = Vec::new();
        stream(
            &format!("ws://{address}/ws/vms/{ID}/console"),
            input_reader,
            &mut output,
        )
        .await
        .unwrap();

        input_task.await.unwrap();
        server.await.unwrap();
        assert_eq!(output, b"booted\r\n$ ");
    }

    #[tokio::test]
    async fn handshake_error_keeps_status_and_body() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            tcp.write_all(
                b"HTTP/1.1 409 Conflict\r\nContent-Type: application/json\r\nContent-Length: 25\r\nConnection: close\r\n\r\n{\"code\":\"vm_not_running\"}",
            )
            .await
            .unwrap();
        });

        let error = stream(
            &format!("ws://{address}/ws/vms/{ID}/console"),
            Cursor::new(Vec::<u8>::new()),
            Vec::<u8>::new(),
        )
        .await
        .unwrap_err();
        server.await.unwrap();
        assert!(matches!(error, Error::Handshake { status: 409, .. }));
        assert!(error.to_string().contains("vm_not_running"), "{error}");
    }
}
