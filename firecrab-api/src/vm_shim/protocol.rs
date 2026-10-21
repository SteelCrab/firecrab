//! Framed messages between the shim and the one client (the API) attached to
//! its control socket.
//!
//! A frame is a kind byte, a big-endian `u32` payload length, and the
//! payload. Console bytes travel raw; the few structured payloads are JSON.
//! Event kinds (shim → client) and request kinds (client → shim) never
//! overlap, so a peer that reads the wrong direction fails loudly instead of
//! misinterpreting bytes.

use std::io;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

/// Bumped on any incompatible change. An API that reconnects to a shim
/// started by a different version must refuse it rather than guess.
pub(crate) const PROTOCOL_VERSION: u32 = 1;

/// Largest payload either side accepts. Console chunks are at most a few
/// KiB; this bound only exists so a broken peer cannot make the other side
/// allocate without limit.
pub(crate) const MAX_FRAME_LEN: usize = 1024 * 1024;

const HELLO: u8 = 1;
const OUTPUT: u8 = 2;
const EXITED: u8 = 3;
const INPUT: u8 = 16;
const TERMINATE: u8 = 17;
const KILL: u8 = 18;

/// How Firecracker ended, as the shim observed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExitStatus {
    /// Exit code, when the process exited on its own.
    pub code: Option<i32>,
    /// Terminating signal, when a signal ended it.
    pub signal: Option<i32>,
}

impl ExitStatus {
    /// Whether the VMM exited on its own with status 0 (guest poweroff or a
    /// SIGTERM it handled).
    pub fn clean(&self) -> bool {
        self.code == Some(0)
    }
}

/// How Firecracker ended and whether that end was asked for (`Terminate`,
/// `Kill`, or the shim's own stop). It is both the `Exited` frame and
/// `exit.json`; a shim older than `stop_requested` leaves it out of both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExitReport {
    /// Firecracker's exit status.
    #[serde(flatten)]
    pub status: ExitStatus,
    /// Whether the end was asked for rather than the guest powering off or
    /// the VMM crashing.
    #[serde(default)]
    pub stop_requested: bool,
}

/// Shim → client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShimEvent {
    /// First frame on every connection.
    Hello {
        /// The shim's [`PROTOCOL_VERSION`].
        version: u32,
        /// Firecracker's process id.
        vmm_pid: u32,
        /// The VM this shim runs. Lets a reattaching API confirm it reached
        /// the VM it meant to; absent from shims that predate the field.
        vm_id: Option<Uuid>,
    },
    /// Raw guest console bytes, including `FIRECRAB_USAGE` lines.
    Output(Vec<u8>),
    /// Firecracker exited; the shim exits after sending this.
    Exited(ExitReport),
}

/// Client → shim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShimRequest {
    /// Bytes for the guest console.
    Input(Vec<u8>),
    /// SIGTERM to Firecracker.
    Terminate,
    /// SIGKILL to Firecracker.
    Kill,
}

/// A frame that could not be read or written.
#[derive(Debug, Error)]
pub(crate) enum FrameError {
    /// The underlying stream failed.
    #[error("shim socket I/O failed: {0}")]
    Io(#[from] io::Error),
    /// The stream ended in the middle of a frame.
    #[error("shim frame truncated by end of stream")]
    Truncated,
    /// A payload length above [`MAX_FRAME_LEN`].
    #[error("shim frame of {0} bytes exceeds the limit")]
    TooLong(usize),
    /// A kind byte this direction does not use.
    #[error("unknown shim frame kind {0}")]
    UnknownKind(u8),
    /// A structured payload that is not the expected JSON.
    #[error("malformed shim frame payload: {0}")]
    Payload(#[from] serde_json::Error),
}

#[derive(Serialize, Deserialize)]
struct HelloPayload {
    version: u32,
    vmm_pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vm_id: Option<Uuid>,
}

/// Writes one shim → client frame.
pub(crate) async fn write_event<W: AsyncWrite + Unpin>(
    writer: &mut W,
    event: &ShimEvent,
) -> Result<(), FrameError> {
    match event {
        ShimEvent::Hello {
            version,
            vmm_pid,
            vm_id,
        } => {
            let payload = serde_json::to_vec(&HelloPayload {
                version: *version,
                vmm_pid: *vmm_pid,
                vm_id: *vm_id,
            })?;
            write_frame(writer, HELLO, &payload).await
        }
        ShimEvent::Output(bytes) => write_frame(writer, OUTPUT, bytes).await,
        ShimEvent::Exited(report) => {
            write_frame(writer, EXITED, &serde_json::to_vec(report)?).await
        }
    }
}

/// Reads one shim → client frame; `None` at a clean end of stream.
pub(crate) async fn read_event<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Option<ShimEvent>, FrameError> {
    let Some((kind, payload)) = read_frame(reader).await? else {
        return Ok(None);
    };
    let event = match kind {
        HELLO => {
            let hello: HelloPayload = serde_json::from_slice(&payload)?;
            ShimEvent::Hello {
                version: hello.version,
                vmm_pid: hello.vmm_pid,
                vm_id: hello.vm_id,
            }
        }
        OUTPUT => ShimEvent::Output(payload),
        EXITED => ShimEvent::Exited(serde_json::from_slice(&payload)?),
        other => return Err(FrameError::UnknownKind(other)),
    };
    Ok(Some(event))
}

/// Writes one client → shim frame.
pub(crate) async fn write_request<W: AsyncWrite + Unpin>(
    writer: &mut W,
    request: &ShimRequest,
) -> Result<(), FrameError> {
    match request {
        ShimRequest::Input(bytes) => write_frame(writer, INPUT, bytes).await,
        ShimRequest::Terminate => write_frame(writer, TERMINATE, &[]).await,
        ShimRequest::Kill => write_frame(writer, KILL, &[]).await,
    }
}

/// Reads one client → shim frame; `None` at a clean end of stream.
pub(crate) async fn read_request<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Option<ShimRequest>, FrameError> {
    let Some((kind, payload)) = read_frame(reader).await? else {
        return Ok(None);
    };
    match kind {
        INPUT => Ok(Some(ShimRequest::Input(payload))),
        TERMINATE => Ok(Some(ShimRequest::Terminate)),
        KILL => Ok(Some(ShimRequest::Kill)),
        other => Err(FrameError::UnknownKind(other)),
    }
}

async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    kind: u8,
    payload: &[u8],
) -> Result<(), FrameError> {
    if payload.len() > MAX_FRAME_LEN {
        return Err(FrameError::TooLong(payload.len()));
    }
    let length = u32::try_from(payload.len()).expect("MAX_FRAME_LEN fits in u32");
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(kind);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(payload);
    writer.write_all(&frame).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Option<(u8, Vec<u8>)>, FrameError> {
    let mut header = [0_u8; 5];
    let mut filled = 0;
    while filled < header.len() {
        match reader.read(&mut header[filled..]).await? {
            0 if filled == 0 => return Ok(None),
            0 => return Err(FrameError::Truncated),
            read => filled += read,
        }
    }
    let kind = header[0];
    let length = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if length > MAX_FRAME_LEN {
        return Err(FrameError::TooLong(length));
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).await.map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            FrameError::Truncated
        } else {
            FrameError::Io(error)
        }
    })?;
    Ok(Some((kind, payload)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncWriteExt, duplex};

    async fn round_trip_event(event: ShimEvent) -> ShimEvent {
        let (mut writer, mut reader) = duplex(64 * 1024);
        write_event(&mut writer, &event).await.unwrap();
        read_event(&mut reader).await.unwrap().unwrap()
    }

    async fn round_trip_request(request: ShimRequest) -> ShimRequest {
        let (mut writer, mut reader) = duplex(64 * 1024);
        write_request(&mut writer, &request).await.unwrap();
        read_request(&mut reader).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn every_event_round_trips() {
        for event in [
            ShimEvent::Hello {
                version: PROTOCOL_VERSION,
                vmm_pid: 4242,
                vm_id: Some(uuid::Uuid::from_u128(7)),
            },
            ShimEvent::Hello {
                version: PROTOCOL_VERSION,
                vmm_pid: 4242,
                vm_id: None,
            },
            ShimEvent::Output(b"login: ".to_vec()),
            ShimEvent::Output(Vec::new()),
            ShimEvent::Exited(ExitReport {
                status: ExitStatus {
                    code: Some(0),
                    signal: None,
                },
                stop_requested: false,
            }),
            ShimEvent::Exited(ExitReport {
                status: ExitStatus {
                    code: None,
                    signal: Some(15),
                },
                stop_requested: true,
            }),
        ] {
            assert_eq!(round_trip_event(event.clone()).await, event);
        }
    }

    #[tokio::test]
    async fn every_request_round_trips() {
        for request in [
            ShimRequest::Input(b"uname -a\n".to_vec()),
            ShimRequest::Terminate,
            ShimRequest::Kill,
        ] {
            assert_eq!(round_trip_request(request.clone()).await, request);
        }
    }

    #[tokio::test]
    async fn consecutive_frames_are_read_in_order() {
        let (mut writer, mut reader) = duplex(64 * 1024);
        write_event(&mut writer, &ShimEvent::Output(b"a".to_vec()))
            .await
            .unwrap();
        write_event(&mut writer, &ShimEvent::Output(b"b".to_vec()))
            .await
            .unwrap();
        assert_eq!(
            read_event(&mut reader).await.unwrap(),
            Some(ShimEvent::Output(b"a".to_vec()))
        );
        assert_eq!(
            read_event(&mut reader).await.unwrap(),
            Some(ShimEvent::Output(b"b".to_vec()))
        );
    }

    #[tokio::test]
    async fn a_clean_end_of_stream_before_a_frame_is_none() {
        let (writer, mut reader) = duplex(64);
        drop(writer);
        assert!(read_event(&mut reader).await.unwrap().is_none());
        let (writer, mut reader) = duplex(64);
        drop(writer);
        assert!(read_request(&mut reader).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_end_of_stream_inside_a_frame_is_truncated() {
        let (mut writer, mut reader) = duplex(64);
        // Output kind, length 10, only three payload bytes.
        writer
            .write_all(&[2, 0, 0, 0, 10, b'a', b'b', b'c'])
            .await
            .unwrap();
        drop(writer);
        assert!(matches!(
            read_event(&mut reader).await,
            Err(FrameError::Truncated)
        ));

        let (mut writer, mut reader) = duplex(64);
        writer.write_all(&[2, 0]).await.unwrap();
        drop(writer);
        assert!(matches!(
            read_event(&mut reader).await,
            Err(FrameError::Truncated)
        ));
    }

    #[tokio::test]
    async fn a_frame_longer_than_the_limit_is_rejected_before_reading_it() {
        let (mut writer, mut reader) = duplex(64);
        let too_long = u32::try_from(MAX_FRAME_LEN + 1).unwrap();
        writer.write_all(&[16]).await.unwrap();
        writer.write_all(&too_long.to_be_bytes()).await.unwrap();
        assert!(matches!(
            read_request(&mut reader).await,
            Err(FrameError::TooLong(length)) if length == MAX_FRAME_LEN + 1
        ));
    }

    #[tokio::test]
    async fn writing_an_oversized_frame_fails_without_sending_anything() {
        let (mut writer, mut reader) = duplex(64);
        let error = write_request(&mut writer, &ShimRequest::Input(vec![0; MAX_FRAME_LEN + 1]))
            .await
            .unwrap_err();
        assert!(matches!(error, FrameError::TooLong(_)));
        drop(writer);
        assert!(read_request(&mut reader).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_unknown_kind_is_rejected() {
        let (mut writer, mut reader) = duplex(64);
        writer.write_all(&[99, 0, 0, 0, 0]).await.unwrap();
        assert!(matches!(
            read_event(&mut reader).await,
            Err(FrameError::UnknownKind(99))
        ));
    }

    #[tokio::test]
    async fn a_request_is_not_accepted_where_an_event_is_expected() {
        let (mut writer, mut reader) = duplex(64);
        write_request(&mut writer, &ShimRequest::Kill)
            .await
            .unwrap();
        assert!(matches!(
            read_event(&mut reader).await,
            Err(FrameError::UnknownKind(_))
        ));
    }

    #[test]
    fn only_a_zero_exit_code_is_clean() {
        assert!(
            ExitStatus {
                code: Some(0),
                signal: None
            }
            .clean()
        );
        assert!(
            !ExitStatus {
                code: Some(1),
                signal: None
            }
            .clean()
        );
        assert!(
            !ExitStatus {
                code: None,
                signal: Some(15)
            }
            .clean()
        );
    }
}
