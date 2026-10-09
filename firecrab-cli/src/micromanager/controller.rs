//! Native service installation stays outside the portable proxy/state machine.
use super::{
    settings::{Settings, Store},
    sleepy,
};
use std::{
    fs, io,
    io::{Read, Write},
    path::Path,
    time::{Duration, Instant},
};
#[cfg(target_os = "windows")]
#[path = "windows/controller.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "macos/controller.rs"]
mod platform;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Settings(#[from] super::settings::Error),
    #[error("Controller request failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("{0}")]
    Native(String),
}
pub fn configured(home: &Path) -> bool {
    home.join("settings.json").is_file() || active(home)
}
pub fn active(home: &Path) -> bool {
    home.join("runtime/controlled").is_file()
}
fn port(home: &Path) -> Result<u16, Error> {
    let path = home.join("runtime/controller-port");
    fs::read_to_string(path)?
        .trim()
        .parse()
        .map_err(|_| Error::Native("Invalid controller port marker".into()))
}
pub fn local_api_base(home: &Path) -> Result<Option<String>, Error> {
    if !active(home) {
        return Ok(None);
    }
    Ok(Some(format!("http://127.0.0.1:{}", port(home)?)))
}
fn client() -> Result<reqwest::blocking::Client, Error> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(360))
        .build()
        .map_err(|e| Error::Native(e.to_string()))
}
pub fn control(home: &Path, action: &str) -> Result<(), Error> {
    let url = format!("http://127.0.0.1:{}/api/micromanager/{action}", port(home)?);
    let response = client()?.post(url).send()?;
    if !response.status().is_success() {
        return Err(Error::Native(response.text().unwrap_or_default()));
    }
    Ok(())
}
pub fn start(home: &Path) -> Result<(), Error> {
    super::managed_home::reject_symlink_components(home)
        .map_err(|e| Error::Native(e.to_string()))?;
    let settings = Store::new(home).load()?.settings;
    fs::create_dir_all(home.join("runtime"))?;
    // Serialize native reconfiguration; settings writes themselves use a separate CAS lock.
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join("runtime/controller-install.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock)
        .map_err(|_| Error::Native("Another controller operation is in progress".into()))?;
    let previous = fs::read(home.join("runtime/applied-settings.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok());
    let resources_changed = previous.as_ref().is_none_or(|old| {
        old.cpu != settings.cpu
            || old.memory_mib != settings.memory_mib
            || old.api_port != settings.api_port
    });
    use sha2::Digest;
    let revision = format!(
        "{:x}",
        sha2::Sha256::digest(fs::read(std::env::current_exe()?)?)
    );
    let revision_changed = fs::read_to_string(home.join("runtime/controller-revision"))
        .ok()
        .as_deref()
        != Some(&revision);
    if !resources_changed && !revision_changed && active(home) && control(home, "start").is_ok() {
        return Ok(());
    }
    let host_work_path = home.join("runtime/host-work.lock");
    let host_work = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(host_work_path)?;
    fs2::FileExt::try_lock_exclusive(&host_work).map_err(|_| {
        Error::Native(
            "Wait for host-side builds/shell work before applying resource changes".into(),
        )
    })?;
    if active(home) && (resources_changed || revision_changed) {
        platform::pause().map_err(Error::Native)?;
        match control(home, "reconfigure") {
            Ok(()) => {}
            Err(Error::Transport(error)) if error.is_connect() => {
                platform::quiesce(home).map_err(Error::Native)?;
            }
            Err(error) => {
                let _ = platform::unpause();
                return Err(error);
            }
        }
    }
    platform::prepare(home, &settings, resources_changed).map_err(Error::Native)?;
    for marker in ["manual-stop", "sleeping"] {
        let path = home.join("runtime").join(marker);
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    fs::write(home.join("runtime/controlled"), b"1\n")?;
    fs::write(
        home.join("runtime/controller-port"),
        settings.api_port.to_string(),
    )?;
    fs::write(home.join("runtime/controller-revision"), revision)?;
    platform::install(home).map_err(Error::Native)?;
    let started = Instant::now();
    loop {
        if control(home, "start").is_ok() {
            break;
        }
        if started.elapsed() >= Duration::from_secs(360) {
            return Err(Error::Native("Controller did not become ready; inspect runtime/controller.log and controller.err.log".into()));
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    fs::write(
        home.join("runtime/applied-settings.json"),
        serde_json::to_vec(&settings).map_err(|e| Error::Native(e.to_string()))?,
    )?;
    Ok(())
}
pub fn require_idle(api_port: u16) -> Result<(), Error> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| Error::Native(e.to_string()))?;
    let response = client.get(format!("http://127.0.0.1:{api_port}/api/micromanager/activity")).send()
        .map_err(|e| Error::Native(format!("Cannot verify guest activity: {e}. Update the guest API with firecrab service dev first")))?;
    if !response.status().is_success() {
        return Err(Error::Native("Guest API lacks the activity endpoint. Run firecrab service dev before activating settings".into()));
    }
    let activity: serde_json::Value = response.json().map_err(|e| Error::Native(e.to_string()))?;
    if activity.get("busy").and_then(|v| v.as_bool()) != Some(false) {
        return Err(Error::Native("Resource/controller changes need an idle guest. Stop workloads and wait for background jobs first".into()));
    }
    Ok(())
}
pub fn stop(home: &Path) -> Result<(), Error> {
    fs::write(home.join("runtime/manual-stop"), b"manual\n")?;
    match control(home, "stop") {
        Ok(()) => Ok(()),
        Err(_) => platform::Backend::new(home)
            .map_err(Error::Native)?
            .stop_native()
            .map_err(Error::Native),
    }
}
pub fn uninstall(home: &Path) -> Result<(), Error> {
    if !active(home) {
        return Ok(());
    }
    stop(home)?;
    let _ = control(home, "shutdown");
    platform::uninstall(home).map_err(Error::Native)?;
    platform::restore(home).map_err(Error::Native)?;
    for marker in [
        "controlled",
        "controller-port",
        "applied-settings.json",
        "controller-revision",
        "sleeping",
        "manual-stop",
    ] {
        let path = home.join("runtime").join(marker);
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}
pub fn print_status(home: &Path) -> Result<i32, Error> {
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| Error::Native(e.to_string()))?
        .get(format!(
            "http://127.0.0.1:{}/{}",
            port(home)?,
            sleepy::STATUS_PATH.trim_start_matches('/')
        ))
        .send()
        .map_err(|e| Error::Native(e.to_string()))?;
    let status: serde_json::Value = response.json().map_err(|e| Error::Native(e.to_string()))?;
    println!(
        "microManager: {}",
        status["state"].as_str().unwrap_or("unknown")
    );
    if let Some(detail) = status["detail"].as_str() {
        println!("  {detail}");
    }
    Ok(i32::from(!matches!(
        status["state"].as_str(),
        Some("running" | "sleeping" | "starting")
    )))
}
pub fn run(home: &Path) -> Result<i32, Error> {
    super::managed_home::reject_symlink_components(home)
        .map_err(|e| Error::Native(e.to_string()))?;
    let settings = Store::new(home).load()?.settings;
    let lock_path = home.join("runtime/controller.lock");
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    fs2::FileExt::try_lock_exclusive(&lock)
        .map_err(|_| Error::Native("Controller is already running".into()))?;
    // A restart in the same login must preserve automatic sleep; a new login
    // honors autostart. The explicit manual-stop marker always takes priority.
    if let Ok(session) = platform::session() {
        let path = home.join("runtime/controller-session");
        let changed = fs::read_to_string(&path).ok().as_deref() != Some(&session);
        if changed && settings.autostart && !home.join("runtime/manual-stop").exists() {
            let _ = fs::remove_file(home.join("runtime/sleeping"));
        }
        fs::write(path, session)?;
    }
    let backend = platform::Backend::new(home).map_err(Error::Native)?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let listener =
                tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, settings.api_port))
                    .await?;
            let controller = sleepy::Controller::new(
                backend,
                home.into(),
                (std::net::Ipv4Addr::LOCALHOST, sleepy::GUEST_PORT).into(),
            )?;
            controller.serve(listener).await
        })?;
    Ok(0)
}

/// Holds a persistent proxy lease for host shells; socket EOF releases it.
pub fn hold(home: &Path) -> Result<std::net::TcpStream, Error> {
    let mut stream = std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port(home)?))?;
    stream.set_read_timeout(Some(Duration::from_secs(360)))?;
    stream.write_all(
        b"POST /api/micromanager/hold HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
    )?;
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") && header.len() < 65536 {
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        header.push(byte[0]);
    }
    if !header.starts_with(b"HTTP/1.1 200 ") {
        return Err(Error::Native(
            "microManager is manually stopped or failed to wake; run firecrab service start".into(),
        ));
    }
    Ok(stream)
}
pub struct HostWork(fs::File);
impl Drop for HostWork {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}
impl HostWork {
    pub fn begin(home: &Path) -> Result<Self, Error> {
        fs::create_dir_all(home.join("runtime"))?;
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(home.join("runtime/host-work.lock"))?;
        fs2::FileExt::lock_shared(&file)?;
        Ok(Self(file))
    }
}
