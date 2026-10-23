//! Ephemeral PTY for `claude auth login`. No shell, replay buffer or transcript.
use std::{
    fs::File,
    io::{self, Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio::{io::unix::AsyncFd, process::Child};
use uuid::Uuid;

use super::claude_code::{ClaudeCode, Error, LOGIN_SECONDS};

struct Pty(AsyncFd<File>);

impl Pty {
    fn open() -> io::Result<(Self, File)> {
        let (mut master, mut slave) = (-1, -1);
        let size = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                &size,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // Own both descriptors immediately, including on configuration failure.
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        for file in [&master, &slave] {
            if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
                return Err(io::Error::last_os_error());
            }
        }
        if unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: File owns this descriptor until AsyncFd drops it; it is never replaced.
        Ok((Self(unsafe { AsyncFd::register(master)? }), slave))
    }

    async fn read(&self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut ready = self.0.readable().await?;
            match ready.try_io(|fd| fd.get_ref().read(bytes)) {
                Ok(result) => return result,
                Err(_) => continue,
            }
        }
    }

    async fn write(&self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let mut ready = self.0.writable().await?;
            match ready.try_io(|fd| fd.get_ref().write(bytes)) {
                Ok(Ok(0)) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(Ok(n)) => bytes = &bytes[n..],
                Ok(Err(error)) => return Err(error),
                Err(_) => continue,
            }
        }
        Ok(())
    }

    fn resize(&self, cols: u16, rows: u16) {
        let size = libc::winsize {
            ws_row: rows.clamp(8, 100),
            ws_col: cols.clamp(20, 240),
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe {
            libc::ioctl(self.0.get_ref().as_raw_fd(), libc::TIOCSWINSZ, &size);
        }
    }
}

struct LoginProcess {
    child: Child,
    pty: Pty,
}

impl LoginProcess {
    fn spawn(cli: &ClaudeCode, id: Uuid) -> Result<Self, Error> {
        let (pty, slave) = Pty::open().map_err(|_| Error::Failed)?;
        let mut command = cli.command(id, "login")?;
        command
            .stdin(Stdio::from(slave.try_clone().map_err(|_| Error::Failed)?))
            .stdout(Stdio::from(slave.try_clone().map_err(|_| Error::Failed)?))
            .stderr(Stdio::from(slave));
        // A new session confines cancellation to this login process group.
        #[cfg(target_os = "linux")]
        let parent = unsafe { libc::getpid() };
        unsafe {
            command.pre_exec(move || {
                // An API crash must not leave a detached account-login process.
                #[cfg(target_os = "linux")]
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1
                    || libc::getppid() != parent
                {
                    return Err(io::Error::other(
                        "login parent changed or could not be monitored",
                    ));
                }
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().map_err(|_| Error::Failed)?;
        Ok(Self { child, pty })
    }
}

impl Drop for LoginProcess {
    fn drop(&mut self) {
        if let Some(pid) = self.child.id() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
            let _ = self.child.start_kill();
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Attachment {
    ticket: Uuid,
}

#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Input {
    Resize { cols: u16, rows: u16 },
}

pub async fn serve(cli: Arc<ClaudeCode>, id: Uuid, mut socket: WebSocket) {
    // The capability travels in a frame, never in a URL or request log.
    let ticket = match tokio::time::timeout(Duration::from_secs(5), socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => serde_json::from_str::<Attachment>(&text)
            .ok()
            .map(|value| value.ticket),
        _ => None,
    };
    let Some(ticket) = ticket else {
        return;
    };
    let operation = cli.operation(id);
    let Ok(guard) = operation.try_lock() else {
        return;
    };
    let Ok(mut cancel) = cli.claim(id, ticket) else {
        return;
    };
    let success = if *cancel.borrow() {
        false
    } else {
        tokio::select! {
            _ = cancel.changed() => false,
            result = tokio::time::timeout(Duration::from_secs(LOGIN_SECONDS), run(&cli, id, &mut socket)) => result.ok().and_then(Result::ok).unwrap_or(false),
        }
    };
    cli.finish(id, ticket);
    drop(guard);
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        socket.send(Message::Text(
            serde_json::json!({"type":"exit","success":success})
                .to_string()
                .into(),
        )),
    )
    .await;
    let _ = tokio::time::timeout(Duration::from_secs(2), socket.close()).await;
}

async fn run(cli: &ClaudeCode, id: Uuid, socket: &mut WebSocket) -> Result<bool, Error> {
    let mut process = LoginProcess::spawn(cli, id)?;
    let deadline = tokio::time::sleep(Duration::from_secs(LOGIN_SECONDS));
    tokio::pin!(deadline);
    let mut output = [0u8; 4096];
    loop {
        tokio::select! {
            _ = &mut deadline => return Ok(false),
            exit = process.child.wait() => {
                // Drain the final prompt before reporting exit. Linux PTYs end with EIO.
                while let Ok(Ok(n)) = tokio::time::timeout(Duration::from_millis(100), process.pty.read(&mut output)).await {
                    if n == 0 { break; }
                    if socket.send(Message::Binary(output[..n].to_vec().into())).await.is_err() { break; }
                }
                return Ok(exit.map_err(|_| Error::Failed)?.success());
            },
            read = process.pty.read(&mut output) => match read {
                Ok(0) => return Ok(process.child.wait().await.map_err(|_| Error::Failed)?.success()),
                Ok(n) => if socket.send(Message::Binary(output[..n].to_vec().into())).await.is_err() { return Ok(false); },
                Err(error) if error.raw_os_error() == Some(libc::EIO) => return Ok(process.child.wait().await.map_err(|_| Error::Failed)?.success()),
                Err(_) => return Err(Error::Failed),
            },
            message = socket.next() => match message {
                Some(Ok(Message::Binary(bytes))) if bytes.len() <= 4096 => {
                    tokio::time::timeout(Duration::from_secs(2), process.pty.write(&bytes)).await.map_err(|_| Error::Failed)?.map_err(|_| Error::Failed)?;
                },
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<Input>(&text) {
                    Ok(Input::Resize { cols, rows }) => process.pty.resize(cols, rows),
                    Err(_) => return Ok(false),
                },
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {},
                _ => return Ok(false),
            }
        }
    }
}

pub use serve as login_terminal;
