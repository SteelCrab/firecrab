//! Own the source dashboard through a per-session HTTP control endpoint, not PID files.

use std::fs;
use std::io;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use fs2::FileExt;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::micromanager::report;

/// Fixed browser address; Vite must fail rather than silently select another port.
const URL: &str = "http://127.0.0.1:8080";
/// Session-protected ownership and shutdown endpoint installed before Vite listens.
const CONTROL: &str = "/__firecrab_dev/";
/// The server entry point is embedded so source snapshots need no helper script.
const SERVER: &str = include_str!("dashboard.mjs");

/// Development dashboard failures must make service dev fail instead of reporting readiness.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("dashboard: {0}")]
    Io(#[from] io::Error),
    #[error("dashboard: {0}")]
    State(#[from] serde_json::Error),
    #[error("dashboard: {0}")]
    Http(#[from] reqwest::Error),
    #[error("{0}")]
    Failed(String),
}

/// Source checkout and capability used to recognize only our running Vite process.
#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
struct Session {
    /// Canonical checkout used by start/restart even from another working directory.
    source: PathBuf,
    /// Unpredictable credential for ownership checks and graceful shutdown.
    session: Uuid,
}

/// Prepare Node and locked frontend dependencies before changing the guest deployment.
pub fn prepare(source: &Path) -> Result<PathBuf, Error> {
    let source = fs::canonicalize(source)?;
    let frontend = source.join("firecrab-frontend");
    for name in ["package.json", "package-lock.json"] {
        if !frontend.join(name).is_file() {
            return Err(Error::Failed(format!(
                "dashboard source is missing {}",
                frontend.join(name).display()
            )));
        }
    }
    if !Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        return Err(Error::Failed("Node.js is required for the development dashboard; install Node.js and npm, then retry service dev".into()));
    }
    if !frontend.join("node_modules/vite/package.json").is_file() {
        report!("[BUILD] dashboard dependencies: npm ci");
        #[cfg(windows)]
        let mut npm = {
            let mut command = Command::new("cmd.exe");
            command.args(["/d", "/c", "npm.cmd"]);
            command
        };
        #[cfg(not(windows))]
        let mut npm = Command::new("npm");
        let status = npm
            .args(["ci", "--no-audit", "--no-fund"])
            .current_dir(frontend)
            .status()?;
        if !status.success() {
            return Err(Error::Failed(
                "dashboard dependency installation failed; see npm output above".into(),
            ));
        }
    }
    Ok(source)
}

/// Use the last selected checkout for control commands, or the current checkout initially.
pub fn source(home: &Path) -> Result<PathBuf, Error> {
    let selected =
        read_session(home)?.map_or_else(std::env::current_dir, |state| Ok(state.source))?;
    prepare(&selected)
}

/// A short, non-proxied request cannot redirect the session capability elsewhere.
fn client() -> Result<Client, Error> {
    Ok(Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(1))
        .build()?)
}

/// Load remembered checkout information; a stopped server deliberately retains it.
fn read_session(home: &Path) -> Result<Option<Session>, Error> {
    match fs::read(home.join("runtime/dev-dashboard.json")) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Check both session and source so another server cannot be mistaken for our dashboard.
fn running(client: &Client, expected: &Session, url: &str) -> bool {
    client
        .get(format!("{url}{CONTROL}"))
        .header("X-Firecrab-Dev-Session", expected.session.to_string())
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(|response| response.json::<Session>())
        .is_ok_and(|actual| actual == *expected)
}

/// Serialize dashboard transitions for this installation.
fn lock(home: &Path) -> Result<fs::File, Error> {
    fs::create_dir_all(home.join("runtime"))?;
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(home.join("runtime/dev-dashboard.lock"))?;
    file.try_lock_exclusive()
        .map_err(|_| Error::Failed("another development dashboard operation is running".into()))?;
    Ok(file)
}

/// Only a successful connection proves that a server is listening.
fn listening(url: &str) -> bool {
    let Ok(address) = url.trim_start_matches("http://").parse::<SocketAddr>() else {
        return true;
    };
    // Windows can time out before a closed loopback port reports connection refused.
    // Vite's strictPort still rejects any bind conflict when starting the server.
    TcpStream::connect_timeout(&address, Duration::from_millis(250)).is_ok()
}

/// Stop only a server that proves ownership; stale state never authorizes killing a PID.
fn stop_session(client: &Client, state: &Session, url: &str) -> Result<(), Error> {
    if !running(client, state, url) {
        return Ok(());
    }
    client
        .post(format!("{url}{CONTROL}stop"))
        .header("X-Firecrab-Dev-Session", state.session.to_string())
        .send()?
        .error_for_status()?;
    let started = Instant::now();
    while listening(url) {
        if started.elapsed() > Duration::from_secs(10) {
            return Err(Error::Failed("development dashboard did not stop".into()));
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// Shut down the owned dashboard and preserve its checkout for service dev start.
pub fn stop(home: &Path) -> Result<(), Error> {
    stop_at(home, URL)
}

/// Stop with the same endpoint used at startup; tests use isolated loopback ports.
fn stop_at(home: &Path, url: &str) -> Result<(), Error> {
    if read_session(home)?.is_none() {
        return Ok(());
    }
    let _lock = lock(home)?;
    if let Some(state) = read_session(home)? {
        stop_session(&client()?, &state, url)?;
    }
    Ok(())
}

/// Launch one detached Node process, or reuse the matching healthy dashboard.
pub fn start(source: &Path, home: &Path) -> Result<(), Error> {
    start_at(source, home, 8080)
}

/// Execute the complete startup transaction on one strict loopback port.
fn start_at(source: &Path, home: &Path, port: u16) -> Result<(), Error> {
    let url = format!("http://127.0.0.1:{port}");
    let _lock = lock(home)?;
    let client = client()?;
    if let Some(previous) = read_session(home)? {
        if previous.source == source && running(&client, &previous, &url) {
            return report_ready(&client, home, &url);
        }
        stop_session(&client, &previous, &url)?;
    }
    if listening(&url) {
        return Err(Error::Failed(format!(
            "dashboard port {port} is already in use by another server; stop that server and retry service dev"
        )));
    }
    let runtime = home.join("runtime");
    let script = runtime.join("dev-dashboard.mjs");
    let log_path = runtime.join("dev-dashboard.log");
    fs::write(&script, SERVER)?;
    let state = Session {
        source: source.to_owned(),
        session: Uuid::new_v4(),
    };
    let log = fs::File::create(&log_path)?;
    let mut command = Command::new("node");
    command
        .arg(script)
        .arg(source)
        .arg(port.to_string())
        .env("FIRECRAB_DEV_SESSION", state.session.to_string())
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x00000008 | 0x00000200 | 0x08000000);
    }
    let mut child = command.spawn()?;
    let started = Instant::now();
    while !running(&client, &state, &url) {
        if child.try_wait()?.is_some() || started.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Failed(format!(
                "development dashboard failed to start; inspect {}",
                log_path.display()
            )));
        }
        thread::sleep(Duration::from_millis(100));
    }
    let mut record = tempfile::NamedTempFile::new_in(&runtime)?;
    use std::io::Write;
    record.write_all(&serde_json::to_vec(&state)?)?;
    if let Err(error) = record.persist(runtime.join("dev-dashboard.json")) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error.error.into());
    }
    report_ready(&client, home, &url)
}

/// Report success only after both the HTML and API proxy answer through port 8080.
fn report_ready(client: &Client, home: &Path, url: &str) -> Result<(), Error> {
    for path in ["/", "/api/host"] {
        client
            .get(format!("{url}{path}"))
            .send()?
            .error_for_status()?;
    }
    report!(
        "[PASS] Dashboard: {}/",
        url.replace("127.0.0.1", "localhost")
    );
    report!(
        "  dashboard logs: {}",
        home.join("runtime/dev-dashboard.log").display()
    );
    Ok(())
}

#[cfg(test)]
#[path = "dashboard_tests.rs"]
mod tests;
