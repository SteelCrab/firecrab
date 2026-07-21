use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Upper bound for one length-prefixed frame; larger frames are a protocol
/// violation and the connection must be closed.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("connection I/O failed")]
    Io(#[from] std::io::Error),
    #[error("frame of {len} bytes exceeds the {MAX_FRAME_BYTES}-byte limit")]
    TooLarge { len: usize },
    #[error("frame payload is not valid protocol JSON")]
    Malformed(#[source] serde_json::Error),
}

/// Write `message` as a `u32` big-endian length prefix followed by JSON.
pub async fn write_frame<W, T>(writer: &mut W, message: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let payload = serde_json::to_vec(message).map_err(FrameError::Malformed)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge { len: payload.len() });
    }
    writer.write_all(&(payload.len() as u32).to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

/// Read one frame written by [`write_frame`]. The length prefix is checked
/// against [`MAX_FRAME_BYTES`] before any payload byte is read.
pub async fn read_frame<R, T>(reader: &mut R) -> Result<T, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len_bytes = [0_u8; 4];
    reader.read_exact(&mut len_bytes).await?;
    let len = u32::from_be_bytes(len_bytes) as usize;
    if len == 0 || len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge { len });
    }

    let mut payload = vec![0_u8; len];
    reader.read_exact(&mut payload).await?;
    serde_json::from_slice(&payload).map_err(FrameError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_round_trip() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &vec!["a".to_owned(), "b".to_owned()])
            .await
            .expect("write frame");

        let decoded: Vec<String> = read_frame(&mut buffer.as_slice()).await.expect("read frame");
        assert_eq!(decoded, ["a", "b"]);
    }

    #[tokio::test]
    async fn oversized_length_prefix_is_rejected_before_reading_payload() {
        let mut frame = ((MAX_FRAME_BYTES + 1) as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(b"ignored");

        let result = read_frame::<_, String>(&mut frame.as_slice()).await;
        assert!(matches!(result, Err(FrameError::TooLarge { len }) if len == MAX_FRAME_BYTES + 1));
    }

    #[tokio::test]
    async fn garbage_payload_is_malformed() {
        let mut frame = 3_u32.to_be_bytes().to_vec();
        frame.extend_from_slice(b"{{{");

        let result = read_frame::<_, String>(&mut frame.as_slice()).await;
        assert!(matches!(result, Err(FrameError::Malformed(_))));
    }
}
