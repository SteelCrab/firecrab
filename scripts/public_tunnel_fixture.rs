//! Disposable test app for public Tunnel acceptance. It contains no credentials.
use std::env;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

// WebSocket requires SHA-1 only for the public upgrade handshake, not for security.
fn sha1(input: &[u8]) -> [u8; 20] {
    let mut data = input.to_vec();
    let bits = (data.len() as u64) * 8;
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bits.to_be_bytes());
    let mut h: [u32; 5] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];
    for chunk in data.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, &word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5a827999u32),
                20..=39 => (b ^ c ^ d, 0x6ed9eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
                _ => (b ^ c ^ d, 0xca62c1d6),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = next;
        }
        for (slot, value) in h.iter_mut().zip([a, b, c, d, e]) {
            *slot = slot.wrapping_add(value);
        }
    }
    let mut out = [0u8; 20];
    for (chunk, value) in out.chunks_exact_mut(4).zip(h) {
        chunk.copy_from_slice(&value.to_be_bytes());
    }
    out
}

fn base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let n = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | (chunk.get(2).copied().unwrap_or(0) as u32);
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid test request")
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) -> io::Result<()> {
    write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n{body}", body.len())
}

fn handle(mut stream: TcpStream, label: &str) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut raw = Vec::new();
    loop {
        if raw.len() == 8192 {
            return Err(invalid());
        }
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte)?;
        raw.push(byte[0]);
        if raw.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let text = std::str::from_utf8(&raw).map_err(|_| invalid())?;
    let mut lines = text.split("\r\n");
    let request = lines.next().ok_or_else(invalid)?;
    if request == "GET /health HTTP/1.1" {
        return respond(&mut stream, "200 OK", "healthy");
    }
    if request == "GET /app HTTP/1.1" {
        return respond(&mut stream, "200 OK", &format!("fixture-{label}"));
    }
    if request != "GET /ws HTTP/1.1" {
        return respond(&mut stream, "404 Not Found", "not found");
    }
    let mut key = None;
    let mut upgrade = false;
    let mut connection = false;
    let mut version = false;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or_else(invalid)?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("sec-websocket-key") {
            if key.replace(value).is_some() {
                return Err(invalid());
            }
        } else if name.eq_ignore_ascii_case("upgrade") {
            upgrade = value.eq_ignore_ascii_case("websocket");
        } else if name.eq_ignore_ascii_case("connection") {
            connection = value
                .split(',')
                .any(|v| v.trim().eq_ignore_ascii_case("upgrade"));
        } else if name.eq_ignore_ascii_case("sec-websocket-version") {
            version = value == "13";
        }
    }
    let key = key
        .filter(|v| {
            v.len() == 24
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
        })
        .ok_or_else(invalid)?;
    if !upgrade || !connection || !version {
        return Err(invalid());
    }
    let accept = base64(&sha1(
        format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
    ));
    write!(stream, "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n")?;
    let mut header = [0u8; 2];
    stream.read_exact(&mut header)?;
    let length = (header[1] & 0x7f) as usize;
    if header[0] != 0x81 || header[1] & 0x80 == 0 || length >= 126 {
        return Err(invalid());
    }
    let mut mask = [0u8; 4];
    stream.read_exact(&mut mask)?;
    let mut payload = vec![0u8; length];
    stream.read_exact(&mut payload)?;
    for (i, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[i % 4];
    }
    std::str::from_utf8(&payload).map_err(|_| invalid())?;
    stream.write_all(&[0x81, length as u8])?;
    stream.write_all(&payload)
}

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    if !(args.len() == 2 || args.len() == 3) || !matches!(args[1].as_str(), "A" | "B") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "use A or B and optional bind address",
        ));
    }
    let listener = TcpListener::bind(args.get(2).map_or("0.0.0.0:8080", String::as_str))?;
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = stream?;
        if active
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                (count < 16).then_some(count + 1)
            })
            .is_err()
        {
            continue;
        }
        let active = Arc::clone(&active);
        let label = args[1].clone();
        std::thread::spawn(move || {
            let _ = handle(stream, &label);
            active.fetch_sub(1, Ordering::Relaxed);
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn websocket_known_handshake() {
        assert_eq!(
            base64(&sha1(
                b"dGhlIHNhbXBsZSBub25jZQ==258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
            )),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }
}
