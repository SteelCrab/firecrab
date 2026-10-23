//! A resident, loopback-only proxy. A request is sent upstream exactly once.
//! The lifecycle gate serializes wake, sleep, and lease acquisition.
use super::settings::{Settings, Store};
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Mutex as AsyncMutex,
};

pub const GUEST_PORT: u16 = 5524;
pub const STATUS_PATH: &str = "/api/micromanager/status";
pub trait Backend: Send + Sync + 'static {
    fn wake(&self, settings: &Settings) -> Result<(), String>;
    fn stop(&self) -> Result<(), String>;
}
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub state: &'static str,
    pub detail: Option<String>,
    pub active_leases: usize,
}
#[derive(Deserialize)]
struct Activity {
    busy: bool,
}

pub struct Controller<B> {
    backend: Arc<B>,
    home: PathBuf,
    upstream: std::net::SocketAddr,
    gate: AsyncMutex<()>,
    status: Mutex<Status>,
    active: AtomicUsize,
    wake_generation: AtomicUsize,
    last_work: Mutex<Instant>,
    client: reqwest::Client,
    shutdown: tokio::sync::Notify,
}
impl<B: Backend> Controller<B> {
    pub fn new(backend: B, home: PathBuf, upstream: std::net::SocketAddr) -> io::Result<Arc<Self>> {
        let stopped = home.join("runtime/manual-stop").exists();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(io::Error::other)?;
        Ok(Arc::new(Self {
            backend: Arc::new(backend),
            home,
            upstream,
            gate: AsyncMutex::new(()),
            status: Mutex::new(Status {
                state: if stopped { "stopped" } else { "sleeping" },
                detail: None,
                active_leases: 0,
            }),
            active: AtomicUsize::new(0),
            wake_generation: AtomicUsize::new(0),
            last_work: Mutex::new(Instant::now()),
            client,
            shutdown: tokio::sync::Notify::new(),
        }))
    }
    fn status(&self) -> Status {
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        status.active_leases = self.active.load(Ordering::SeqCst);
        status
    }
    fn publish(&self, state: &'static str, detail: Option<String>) {
        *self.status.lock().unwrap_or_else(|p| p.into_inner()) = Status {
            state,
            detail,
            active_leases: 0,
        };
    }
    async fn wake_locked(&self) -> Result<(), String> {
        if self.status().state == "running" {
            return Ok(());
        }
        let settings = Store::new(&self.home)
            .load()
            .map_err(|e| e.to_string())?
            .settings;
        self.publish("starting", None);
        let backend = Arc::clone(&self.backend);
        let result = tokio::task::spawn_blocking(move || backend.wake(&settings))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        // Requests queued during this attempt share its failure. A later request
        // may make a new attempt after the underlying problem has been fixed.
        self.wake_generation.fetch_add(1, Ordering::SeqCst);
        match result {
            Ok(()) => {
                let _ = std::fs::remove_file(self.home.join("runtime/sleeping"));
                self.publish("running", None);
                *self.last_work.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
                Ok(())
            }
            Err(error) => {
                self.publish("failed", Some(error.clone()));
                Err(error)
            }
        }
    }
    async fn lease(self: &Arc<Self>, work: bool) -> Result<Lease<B>, String> {
        let generation = self.wake_generation.load(Ordering::SeqCst);
        let _gate = self.gate.lock().await;
        if self.home.join("runtime/manual-stop").exists() {
            self.publish("stopped", None);
            return Err("microManager was stopped manually; run firecrab service start".into());
        }
        if work {
            let status = self.status();
            if status.state == "failed" && self.wake_generation.load(Ordering::SeqCst) != generation
            {
                return Err(status
                    .detail
                    .unwrap_or_else(|| "Management VM failed to wake".into()));
            }
            self.wake_locked().await?;
        }
        if self.status().state != "running" {
            return Err("microManager is sleeping; monitoring does not wake it".into());
        }
        self.active.fetch_add(1, Ordering::SeqCst);
        Ok(Lease {
            controller: Arc::clone(self),
            work,
        })
    }
    async fn idle(&self, timeout: Duration) {
        let _gate = self.gate.lock().await;
        if self.status().state != "running"
            || self.active.load(Ordering::SeqCst) > 0
            || self
                .last_work
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .elapsed()
                < timeout
        {
            return;
        }
        let host_work = self.home.join("runtime/host-work.lock");
        let Ok(_host_guard) = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(host_work)
        else {
            return;
        };
        if fs2::FileExt::try_lock_exclusive(&_host_guard).is_err() {
            return;
        }
        let url = format!("http://{}/api/micromanager/activity", self.upstream);
        match self.client.get(url).send().await {
            Ok(response) if response.status().is_success() => {
                match response.json::<Activity>().await {
                    Ok(Activity { busy: false }) => {}
                    Ok(Activity { busy: true }) => {
                        *self.last_work.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
                        return;
                    }
                    Err(e) => {
                        self.publish(
                            "running",
                            Some(format!("Sleep inhibited: invalid activity report: {e}")),
                        );
                        return;
                    }
                }
            }
            _ => {
                self.publish(
                    "running",
                    Some(
                        "Sleep inhibited: guest activity is unavailable (update the guest API)"
                            .into(),
                    ),
                );
                return;
            }
        }
        // Persist before stopping. A controller crash must not undo an automatic sleep.
        let marker = self.home.join("runtime/sleeping");
        if let Err(e) = std::fs::write(&marker, b"idle\n") {
            self.publish("running", Some(format!("Sleep inhibited: {e}")));
            return;
        }
        self.publish("sleeping", None);
        let backend = Arc::clone(&self.backend);
        match tokio::task::spawn_blocking(move || backend.stop()).await {
            Ok(Ok(())) => {}
            result => {
                let _ = std::fs::remove_file(marker);
                self.publish("failed", Some(format!("Sleep failed: {result:?}")));
            }
        }
    }
    pub async fn serve(self: Arc<Self>, listener: TcpListener) -> io::Result<()> {
        let settings = Store::new(&self.home)
            .load()
            .map_err(io::Error::other)?
            .settings;
        let sleeping = self.home.join("runtime/sleeping").exists();
        let booting = Arc::clone(&self);
        tokio::spawn(async move {
            let _gate = booting.gate.lock().await;
            if booting.home.join("runtime/manual-stop").exists() || sleeping {
                return;
            }
            if booting
                .client
                .get(format!("http://{}/api/host", booting.upstream))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                booting.publish("running", None);
            } else if settings.autostart {
                let _ = booting.wake_locked().await;
            }
        });
        let polling = Arc::clone(&self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            loop {
                interval.tick().await;
                // A malformed settings file inhibits sleep instead of falling back to defaults.
                if let Ok(snapshot) = Store::new(&polling.home).load()
                    && snapshot.settings.sleepy
                {
                    polling
                        .idle(Duration::from_secs(
                            u64::from(snapshot.settings.idle_minutes) * 60,
                        ))
                        .await;
                }
            }
        });
        let limit = Arc::new(tokio::sync::Semaphore::new(128));
        loop {
            let (stream, _) = tokio::select! {
                accepted = listener.accept() => accepted?,
                _ = self.shutdown.notified() => return Ok(()),
            };
            let permit = Arc::clone(&limit)
                .acquire_owned()
                .await
                .map_err(io::Error::other)?;
            let controller = Arc::clone(&self);
            tokio::spawn(async move {
                let _permit = permit;
                let _ = controller.connection(stream).await;
            });
        }
    }
    async fn connection(self: Arc<Self>, mut stream: TcpStream) -> io::Result<()> {
        let mut request =
            match tokio::time::timeout(Duration::from_secs(10), read_header(&mut stream)).await {
                Ok(Ok(request)) => request,
                _ => return reply(&mut stream, 400, "Invalid or incomplete HTTP request").await,
            };
        if request.path == STATUS_PATH {
            if request.method != "GET" {
                return reply(&mut stream, 405, "Use GET").await;
            }
            return json_reply(
                &mut stream,
                200,
                &serde_json::to_string(&self.status()).map_err(io::Error::other)?,
            )
            .await;
        }
        if request.path == "/api/micromanager/hold" {
            if request.method != "POST" || request.origin {
                return reply(&mut stream, 403, "Local CLI control only").await;
            }
            let _lease = match self.lease(true).await {
                Ok(lease) => lease,
                Err(e) => return reply(&mut stream, 503, &e).await,
            };
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").await?;
            let mut buffer = [0; 1024];
            while stream.read(&mut buffer).await? != 0 {}
            return Ok(());
        }
        if matches!(
            request.path.as_str(),
            "/api/micromanager/start"
                | "/api/micromanager/stop"
                | "/api/micromanager/shutdown"
                | "/api/micromanager/reconfigure"
        ) {
            if request.method != "POST" || request.origin {
                return reply(&mut stream, 403, "Local CLI control only").await;
            }
            let _gate = self.gate.lock().await;
            if request.path.ends_with("/reconfigure") {
                if self.active.load(Ordering::SeqCst) != 0 {
                    return reply(&mut stream, 503, "Close active API/console/shell connections before applying resource changes").await;
                }
                if !matches!(self.status().state, "sleeping" | "stopped") {
                    let url = format!("http://{}/api/micromanager/activity", self.upstream);
                    let idle = match self.client.get(url).send().await {
                        Ok(r) if r.status().is_success() => r
                            .json::<Activity>()
                            .await
                            .is_ok_and(|activity| !activity.busy),
                        _ => false,
                    };
                    if !idle {
                        return reply(
                            &mut stream,
                            503,
                            "Resource changes need a verified idle guest",
                        )
                        .await;
                    }
                }
            }
            if request.path.ends_with("/start") {
                let marker = self.home.join("runtime/manual-stop");
                if marker.exists() {
                    std::fs::remove_file(marker)?;
                }
                if let Err(e) = self.wake_locked().await {
                    return reply(&mut stream, 503, &e).await;
                }
            } else {
                std::fs::write(self.home.join("runtime/manual-stop"), b"manual\n")?;
                let backend = Arc::clone(&self.backend);
                let result = tokio::task::spawn_blocking(move || backend.stop())
                    .await
                    .map_err(io::Error::other)?;
                if let Err(e) = result {
                    self.publish("failed", Some(e.clone()));
                    return reply(&mut stream, 503, &e).await;
                }
                self.publish("stopped", None);
            }
            json_reply(&mut stream, 200, "{\"ok\":true}").await?;
            if request.path.ends_with("/shutdown") || request.path.ends_with("/reconfigure") {
                self.shutdown.notify_one();
            }
            return Ok(());
        }
        // Keep wake serialized to completion even if the client leaves, but do
        // not execute a timed-out client's buffered mutation after that boot.
        let acquiring = self.lease(request.work());
        tokio::pin!(acquiring);
        let mut disconnected = false;
        let mut pending = [0; 4096];
        let acquired = loop {
            tokio::select! {
                result = &mut acquiring => break result,
                read = stream.read(&mut pending), if !disconnected => match read {
                    Ok(0) | Err(_) => disconnected = true,
                    Ok(n) => {
                        if request.bytes.len() + n > 128 * 1024 { disconnected = true; }
                        else { request.bytes.extend_from_slice(&pending[..n]); }
                    }
                }
            }
        };
        if disconnected {
            return Ok(());
        }
        let _lease = match acquired {
            Ok(lease) => lease,
            Err(e) => return reply(&mut stream, 503, &e).await,
        };
        let mut upstream =
            match tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(self.upstream))
                .await
            {
                Ok(Ok(stream)) => stream,
                _ => {
                    self.publish("failed", Some("Guest API connection failed".into()));
                    return reply(
                        &mut stream,
                        503,
                        "Guest API unavailable; request was not sent",
                    )
                    .await;
                }
            };
        upstream.write_all(&request.bytes).await?;
        // Includes request/response bodies, SSE and console WebSocket upgrades.
        // After write_all succeeds we never retry, even if the response is lost.
        tokio::io::copy_bidirectional(&mut stream, &mut upstream).await?;
        Ok(())
    }
}
struct Lease<B: Backend> {
    controller: Arc<Controller<B>>,
    work: bool,
}
impl<B: Backend> Drop for Lease<B> {
    fn drop(&mut self) {
        if self.work {
            *self
                .controller
                .last_work
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = Instant::now();
        }
        self.controller.active.fetch_sub(1, Ordering::SeqCst);
    }
}
struct Request {
    method: String,
    path: String,
    origin: bool,
    bytes: Vec<u8>,
}
impl Request {
    fn work(&self) -> bool {
        !matches!(self.method.as_str(), "GET" | "HEAD" | "OPTIONS") || self.path.starts_with("/ws/")
    }
}
async fn read_header(stream: &mut TcpStream) -> io::Result<Request> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    let end = loop {
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break end + 4;
        }
        if bytes.len() >= 65536 {
            return Err(io::Error::other("Header too large"));
        }
        let n = stream.read(&mut buffer).await?;
        if n == 0 {
            return Err(io::Error::other("Incomplete header"));
        }
        bytes.extend_from_slice(&buffer[..n]);
    };
    let header = std::str::from_utf8(&bytes[..end]).map_err(io::Error::other)?;
    let mut lines = header.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split_whitespace();
    let method = first.next().unwrap_or_default().to_string();
    let target = first.next().unwrap_or_default();
    if !target.starts_with('/')
        || !matches!(first.next(), Some("HTTP/1.1" | "HTTP/1.0"))
        || first.next().is_some()
        || method.is_empty()
        || !method.bytes().all(|b| b.is_ascii_uppercase())
    {
        return Err(io::Error::other("Invalid request line"));
    }
    let path = target.split('?').next().unwrap_or(target).to_string();
    let headers: Vec<_> = lines
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.split_once(':')
                .ok_or_else(|| io::Error::other("Invalid header"))
        })
        .collect::<Result<_, _>>()?;
    let origin = headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("origin"));
    let upgrade = headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("upgrade") && value.trim().eq_ignore_ascii_case("websocket")
    });
    if !upgrade {
        let mut rewritten = format!("{method} {target} HTTP/1.1\r\n");
        for (name, value) in headers {
            if !name.eq_ignore_ascii_case("connection")
                && !name.eq_ignore_ascii_case("proxy-connection")
            {
                rewritten.push_str(&format!("{name}:{value}\r\n"));
            }
        }
        rewritten.push_str("Connection: close\r\n\r\n");
        let mut forwarded = rewritten.into_bytes();
        forwarded.extend_from_slice(&bytes[end..]);
        bytes = forwarded;
    }
    Ok(Request {
        method,
        path,
        origin,
        bytes,
    })
}
async fn reply(stream: &mut TcpStream, status: u16, message: &str) -> io::Result<()> {
    let body = serde_json::json!({"error": {"code":"micromanager_unavailable", "message":message}})
        .to_string();
    json_reply(stream, status, &body).await
}
async fn json_reply(stream: &mut TcpStream, status: u16, body: &str) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        405 => "Method Not Allowed",
        _ => "Service Unavailable",
    };
    stream.write_all(format!("HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
    stream.shutdown().await
}

#[cfg(test)]
#[path = "sleepy_tests.rs"]
mod tests;
