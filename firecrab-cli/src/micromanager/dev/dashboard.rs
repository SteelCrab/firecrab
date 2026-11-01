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
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::micromanager::report;

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
    /// Installation identity; older dashboard sessions have no dependency stamp.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    dependencies: String,
    /// Older sessions used the fixed dashboard port and did not record it.
    #[serde(default = "default_port")]
    port: u16,
    #[serde(default = "default_api_port")]
    api_port: u16,
    #[serde(default = "default_resource_port")]
    resource_port: u16,
    #[serde(default)]
    isolated: bool,
}

fn default_port() -> u16 {
    8080
}

fn default_api_port() -> u16 {
    5523
}

fn default_resource_port() -> u16 {
    5524
}

impl Session {
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// Resume the selected port for control commands and trust only its browser origin.
pub fn port(home: &Path) -> Result<u16, Error> {
    Ok(read_session(home)?.map_or_else(default_port, |state| state.port))
}

/// Distinguish a stopped dashboard's settings from an instance never configured.
pub fn has_session(home: &Path) -> Result<bool, Error> {
    Ok(read_session(home)?.is_some())
}

/// Track both manifests so branch changes cannot reuse stale installed packages.
fn dependency_revision(frontend: &Path) -> Result<String, Error> {
    let mut revision = Sha256::new();
    for name in ["package.json", "package-lock.json"] {
        revision.update(fs::read(frontend.join(name))?);
        revision.update([0]);
    }
    Ok(format!("{:x}", revision.finalize()))
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
    let revision = dependency_revision(&frontend)?;
    let stamp = frontend.join("node_modules/.firecrab-dev-dependencies");
    let installed = fs::read_to_string(&stamp).unwrap_or_default();
    if !frontend.join("node_modules/vite/package.json").is_file()
        || installed.lines().next() != Some(revision.as_str())
    {
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
            .current_dir(&frontend)
            .status()?;
        if !status.success() {
            return Err(Error::Failed(
                "dashboard dependency installation failed; see npm output above".into(),
            ));
        }
        // Even reinstalling the same lock must invalidate Vite's module cache.
        fs::write(stamp, format!("{revision}\n{}", Uuid::new_v4()))?;
    }
    Ok(source)
}

/// Use the last selected checkout for control commands, or the current checkout initially.
pub fn source(home: &Path) -> Result<PathBuf, Error> {
    let selected = match read_session(home)? {
        Some(state) => state.source,
        None if super::instance::load(home)?.is_some() => {
            return Err(Error::Failed(
                "development dashboard settings are missing; run service dev --port PORT first"
                    .into(),
            ));
        }
        None => std::env::current_dir()?,
    };
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
    let instance = super::instance::load(home)?;
    let state: Session = match fs::read(home.join("runtime/dev-dashboard.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let (api_port, resource_port) = instance
        .as_ref()
        .map_or((default_api_port(), default_resource_port()), |instance| {
            (instance.api_port, instance.resource_port)
        });
    if state.api_port != api_port
        || state.resource_port != resource_port
        || state.isolated != instance.is_some()
        || instance
            .as_ref()
            .is_some_and(|instance| state.source != instance.source)
    {
        return Err(Error::Failed(
            "dashboard ownership does not match the selected development instance".into(),
        ));
    }
    Ok(Some(state))
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

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn agent_terminal_proxy_ready(client: &Client, state: &Session) -> bool {
    client
        .get(format!("{}{CONTROL}", state.url()))
        .header("X-Firecrab-Dev-Session", state.session.to_string())
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .is_ok_and(|response| {
            response
                .headers()
                .get("x-firecrab-agent-terminal")
                .is_some_and(|value| value == "1")
        })
}

/// Serialize dashboard transitions for this installation.
fn lock(home: &Path) -> Result<fs::File, Error> {
    fs::create_dir_all(home.join("runtime"))?;
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(home.join("runtime/dev-dashboard.lock"))?;
    // A process forked elsewhere can briefly share the lock until its exec closes the fd.
    let started = Instant::now();
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(file),
            Err(error) if error.kind() != fs2::lock_contended_error().kind() => {
                return Err(error.into());
            }
            Err(_) if started.elapsed() > Duration::from_secs(1) => {
                return Err(Error::Failed(
                    "another development dashboard operation is running".into(),
                ));
            }
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
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

fn check_listening_port(client: &Client, state: Option<&Session>, port: u16) -> Result<(), Error> {
    let url = format!("http://127.0.0.1:{port}");
    if listening(&url)
        && !state.is_some_and(|state| state.port == port && running(client, state, &url))
    {
        return Err(Error::Failed(format!(
            "dashboard port {port} is already in use by another server; choose --port PORT or stop that server and retry service dev"
        )));
    }
    Ok(())
}

/// Validate ownership and reject foreign listeners without changing dashboard state.
pub fn check_port(home: &Path, port: u16) -> Result<(), Error> {
    let state = read_session(home)?;
    check_listening_port(&client()?, state.as_ref(), port)
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
    if read_session(home)?.is_none() {
        return Ok(());
    }
    let _lock = lock(home)?;
    if let Some(state) = read_session(home)? {
        stop_session(&client()?, &state, &state.url())?;
    }
    Ok(())
}

/// Stop the owned server and remove generated files, preserving its source and port.
pub fn clean(home: &Path) -> Result<(), Error> {
    let _lock = lock(home)?;
    if let Some(state) = read_session(home)? {
        stop_session(&client()?, &state, &state.url())?;
    }
    for name in ["dev-dashboard.mjs", "dev-dashboard.log"] {
        match fs::remove_file(home.join("runtime").join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    // Keep the lock inode so concurrent dashboard operations use the same lock.
    Ok(())
}

/// Read the selected native controller's existing identity without creating state.
#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn check_backend_identity(home: &Path, actual: Option<&str>) -> Result<(), Error> {
    let expected: String =
        serde_json::from_slice(&fs::read(home.join("runtime/resource-controller-id.json"))?)?;
    if Uuid::parse_str(&expected).is_err() || actual != Some(expected.as_str()) {
        return Err(Error::Failed(
            "the development API does not belong to the selected instance; refusing to connect"
                .into(),
        ));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn connected_session(instance: &super::instance::Instance) -> Result<Option<Session>, Error> {
    let path = instance
        .managed_home
        .join("runtime/dev-dashboard-connected.json");
    let state: Session = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !state.isolated
        || !state.source.is_absolute()
        || state.port == 0
        || state.api_port != instance.api_port
        || state.resource_port != instance.resource_port
        || [instance.api_port, instance.resource_port].contains(&state.port)
    {
        return Err(Error::Failed(
            "connected dashboard ownership does not match the selected development instance".into(),
        ));
    }
    Ok(Some(state))
}

/// Foreground frontend development against an explicitly selected existing backend.
/// This does not start, stop, deploy or modify the guest or its workloads.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn serve_existing(
    source: &Path,
    instance: &super::instance::Instance,
    port: u16,
) -> Result<i32, Error> {
    let client = client()?;
    let status = client
        .get(format!("http://127.0.0.1:{}/api/host", instance.api_port))
        .send()?
        .error_for_status()?
        .json::<firecrab_api_types::HostStatusResponse>()?;
    check_backend_identity(
        &instance.managed_home,
        status
            .platform
            .as_ref()
            .and_then(|platform| platform.controller_id.as_deref()),
    )?;
    let source = prepare(source)?;
    let _lock = lock(&instance.managed_home)?;
    let connected = connected_session(instance)?;
    let previous = read_session(&instance.managed_home)?;
    let owner = connected
        .as_ref()
        .filter(|state| state.port == port)
        .or_else(|| previous.as_ref().filter(|state| state.port == port));
    check_listening_port(&client, owner, port)?;
    let dependencies = fs::read_to_string(
        source.join("firecrab-frontend/node_modules/.firecrab-dev-dependencies"),
    )?;
    if let Some(current) = connected.as_ref().filter(|state| {
        state.port == port
            && state.source == source
            && state.dependencies == dependencies
            && agent_terminal_proxy_ready(&client, state)
    }) && running(&client, current, &current.url())
    {
        report!(
            "[DASHBOARD] {} (existing frontend; API and VMs unchanged)",
            current.url()
        );
        return Ok(0);
    }
    if let Some(current) = connected.as_ref() {
        stop_session(&client, current, &current.url())?;
    }
    if let Some(current) = previous.as_ref().filter(|state| state.port == port) {
        stop_session(&client, current, &current.url())?;
    }
    let state = Session {
        source: source.clone(),
        session: Uuid::new_v4(),
        dependencies,
        port,
        api_port: instance.api_port,
        resource_port: instance.resource_port,
        isolated: true,
    };
    let script = tempfile::Builder::new().suffix(".mjs").tempfile()?;
    fs::write(script.path(), SERVER)?;
    let mut child = Command::new("node")
        .current_dir(
            script
                .path()
                .parent()
                .ok_or_else(|| Error::Failed("dashboard script directory is missing".into()))?,
        )
        .arg(
            script
                .path()
                .file_name()
                .ok_or_else(|| Error::Failed("dashboard script name is missing".into()))?,
        )
        .arg(&source)
        .arg(port.to_string())
        .env("FIRECRAB_DEV_SESSION", state.session.to_string())
        .env("FIRECRAB_DEV_DEPENDENCIES", &state.dependencies)
        .env("FIRECRAB_DEV_API_PORT", state.api_port.to_string())
        .env(
            "FIRECRAB_DEV_RESOURCE_PORT",
            state.resource_port.to_string(),
        )
        .env("FIRECRAB_DEV_ISOLATED", "1")
        .spawn()?;
    let started = Instant::now();
    while !running(&client, &state, &state.url()) {
        if child.try_wait()?.is_some() || started.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Failed(
                "connected dashboard failed to start; the API and VMs were not changed".into(),
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
    let recorded = (|| -> Result<(), Error> {
        use std::io::Write;
        let mut record = tempfile::NamedTempFile::new_in(instance.managed_home.join("runtime"))?;
        record.write_all(&serde_json::to_vec(&state)?)?;
        record
            .persist(
                instance
                    .managed_home
                    .join("runtime/dev-dashboard-connected.json"),
            )
            .map_err(|error| error.error)?;
        Ok(())
    })();
    if let Err(error) = recorded {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    drop(_lock);
    report!(
        "[API] existing development instance: http://127.0.0.1:{}/",
        instance.api_port
    );
    report!("[SOURCE] dashboard: {}", source.display());
    report!("[DASHBOARD] http://localhost:{port}/ (Ctrl+C stops only this dashboard)");
    Ok(child.wait()?.code().unwrap_or(1))
}

/// Launch one detached Node process, or reuse the matching healthy dashboard.
pub fn start(source: &Path, home: &Path, port: u16) -> Result<(), Error> {
    let instance = super::instance::load(home)?;
    if instance
        .as_ref()
        .is_some_and(|instance| instance.source != source)
    {
        return Err(Error::Failed(
            "dashboard source does not match the selected development instance".into(),
        ));
    }
    let url = format!("http://127.0.0.1:{port}");
    let _lock = lock(home)?;
    let client = client()?;
    let dependencies = fs::read_to_string(
        source.join("firecrab-frontend/node_modules/.firecrab-dev-dependencies"),
    )?;
    let (api_port, resource_port) = instance
        .as_ref()
        .map_or((default_api_port(), default_resource_port()), |instance| {
            (instance.api_port, instance.resource_port)
        });
    let isolated = instance.is_some();
    let previous = read_session(home)?;
    check_listening_port(&client, previous.as_ref(), port)?;
    if let Some(previous) = previous {
        if previous.source == source
            && previous.port == port
            && previous.dependencies == dependencies
            && previous.api_port == api_port
            && previous.resource_port == resource_port
            && previous.isolated == isolated
            && fs::read_to_string(home.join("runtime/dev-dashboard.mjs"))
                .is_ok_and(|script| script == SERVER)
            && running(&client, &previous, &url)
        {
            return report_ready(&client, home, &url);
        }
        stop_session(&client, &previous, &previous.url())?;
    }
    let runtime = home.join("runtime");
    let script = runtime.join("dev-dashboard.mjs");
    let log_path = runtime.join("dev-dashboard.log");
    fs::write(&script, SERVER)?;
    let state = Session {
        source: source.to_owned(),
        session: Uuid::new_v4(),
        dependencies,
        port,
        api_port,
        resource_port,
        isolated,
    };
    let log = fs::File::create(&log_path)?;
    let mut command = Command::new("node");
    command
        // Resolve the entry inside runtime instead of passing a Windows verbatim
        // path to Node's ESM CLI. Ownership metadata remains canonical.
        .current_dir(&runtime)
        .arg("dev-dashboard.mjs")
        .arg(source)
        .arg(port.to_string())
        .env("FIRECRAB_DEV_SESSION", state.session.to_string())
        .env("FIRECRAB_DEV_DEPENDENCIES", &state.dependencies)
        .env("FIRECRAB_DEV_API_PORT", state.api_port.to_string())
        .env(
            "FIRECRAB_DEV_RESOURCE_PORT",
            state.resource_port.to_string(),
        )
        .env(
            "FIRECRAB_DEV_ISOLATED",
            if state.isolated { "1" } else { "0" },
        )
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
        if child.try_wait()?.is_some() || started.elapsed() > Duration::from_secs(180) {
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

/// Report success only after both the HTML and API proxy answer through the selected port.
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
