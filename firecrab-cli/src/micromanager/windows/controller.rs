use super::super::{
    settings::Settings,
    sleepy,
    windows::{daemon, lifecycle::Layout, wsl},
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
const TASK: &str = r"Firecrab\microManager-controller";
pub fn session() -> Result<String, String> {
    let text = wsl::run_program("whoami.exe", &["/logonid"]).map_err(|e| e.to_string())?;
    text.split_whitespace()
        .find(|word| word.starts_with("S-1-5-5-"))
        .map(str::to_owned)
        .ok_or("Logon SID unavailable".into())
}
pub struct Backend {
    home: PathBuf,
}
impl Backend {
    pub fn new(home: &Path) -> Result<Self, String> {
        Ok(Self { home: home.into() })
    }
    pub fn stop_native(&self) -> Result<(), String> {
        daemon::stop().map_err(|e| e.to_string())
    }
}
impl sleepy::Backend for Backend {
    fn wake(&self, _settings: &Settings) -> Result<(), String> {
        if !self.home.join("runtime/controlled").is_file() {
            return Err("Controller is not configured".into());
        }
        daemon::resume().map_err(|e| e.to_string())?;
        wait_api(sleepy::GUEST_PORT)
    }
    fn stop(&self) -> Result<(), String> {
        self.stop_native()
    }
}
fn wait_api(port: u16) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| e.to_string())?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(300) {
        if client
            .get(format!("http://127.0.0.1:{port}/api/host"))
            .send()
            .is_ok_and(|response| response.status().is_success())
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err("Guest API did not become ready within 300 seconds".into())
}
pub fn prepare(home: &Path, settings: &Settings, resources_changed: bool) -> Result<(), String> {
    if daemon::task_state() == daemon::TaskState::Missing {
        return Err("microManager is not installed; run firecrab service install".into());
    }
    let controlled = super::active(home);
    daemon::resume().map_err(|e| e.to_string())?;
    if !controlled {
        wait_api(5523)?;
        super::require_idle(5523).map_err(|e| e.to_string())?;
        wsl::root_shell("mkdir -p /etc/systemd/system/firecrab-api.service.d; printf '%s\\n' '[Service]' 'Environment=FIRECRAB_BIND_ADDR=127.0.0.1:5524' > /etc/systemd/system/firecrab-api.service.d/30-micromanager-controller.conf; systemctl daemon-reload; systemctl restart firecrab-api").map_err(|e| e.to_string())?;
    }
    wait_api(sleepy::GUEST_PORT)?;
    if resources_changed {
        write_resources(settings)?;
    }
    daemon::install_controlled(&Layout {
        managed_home: home.into(),
    })
    .map_err(|e| e.to_string())?;
    Ok(())
}
fn write_resources(settings: &Settings) -> Result<(), String> {
    let profile = std::env::var_os("USERPROFILE").ok_or("USERPROFILE is missing")?;
    let path = PathBuf::from(profile).join(".wslconfig");
    super::super::managed_home::reject_symlink_components(&path).map_err(|e| e.to_string())?;
    let previous = match fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    let text = previous
        .as_deref()
        .map(std::str::from_utf8)
        .transpose()
        .map_err(|_| ".wslconfig must be UTF-8; existing file was preserved")?
        .unwrap_or_default();
    let merged = super::super::settings::merge_wsl(text, settings).map_err(|e| e.to_string())?;
    if text == merged {
        return Ok(());
    }
    if let Some(bytes) = &previous {
        let backup = path.with_extension(format!("firecrab-{}.bak", uuid::Uuid::new_v4()));
        fs::write(&backup, bytes).map_err(|e| e.to_string())?;
        if fs::read(&path).map_err(|e| e.to_string())? != *bytes {
            return Err(".wslconfig changed during editing; retry".into());
        }
    }
    let mut staged = tempfile::NamedTempFile::new_in(path.parent().ok_or("No profile directory")?)
        .map_err(|e| e.to_string())?;
    staged
        .write_all(merged.as_bytes())
        .map_err(|e| e.to_string())?;
    staged.as_file().sync_all().map_err(|e| e.to_string())?;
    staged.persist(&path).map_err(|e| e.to_string())?;
    println!(
        "[NOTE] CPU/memory limits apply to all WSL2 distributions after the next WSL2 VM restart. Other distributions were not stopped."
    );
    Ok(())
}
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn ps(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}
pub fn install(home: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // Stage immutable binaries: Windows cannot replace an executable that is running.
    let bytes = fs::read(exe).map_err(|e| e.to_string())?;
    use sha2::Digest;
    let name = format!("controller-{:x}.exe", sha2::Sha256::digest(&bytes));
    let staged = home.join("runtime").join(name);
    if !staged.exists() {
        fs::write(&staged, bytes).map_err(|e| e.to_string())?;
    }
    let user = std::env::var("USERNAME").map_err(|e| e.to_string())?;
    let user = std::env::var("USERDOMAIN")
        .map(|domain| format!("{domain}\\{user}"))
        .unwrap_or(user);
    let launcher = home.join("runtime/controller.ps1");
    fs::write(&launcher, format!("$ErrorActionPreference = 'Stop'\r\n& {} service controller --home {} 1>> {} 2>> {}\r\nexit $LASTEXITCODE\r\n", ps(&staged), ps(home), ps(&home.join("runtime/controller.log")), ps(&home.join("runtime/controller.err.log")))).map_err(|e| e.to_string())?;
    let task = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
<Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger>
<TimeTrigger><Enabled>true</Enabled><StartBoundary>2000-01-01T00:00:00</StartBoundary><Repetition><Interval>PT1M</Interval><StopAtDurationEnd>false</StopAtDurationEnd></Repetition></TimeTrigger></Triggers>
<Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
<Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><Enabled>true</Enabled><Hidden>true</Hidden></Settings>
<Actions Context="Author"><Exec><Command>powershell.exe</Command><Arguments>-NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File &quot;{launcher}&quot;</Arguments></Exec></Actions></Task>"#,
        user = xml(&user),
        launcher = xml(&launcher.to_string_lossy())
    );
    daemon::register_named(
        &Layout {
            managed_home: home.into(),
        },
        TASK,
        "microManager-controller-task.xml",
        &task,
    )
    .map_err(|e| e.to_string())?;
    wsl::run_program("schtasks.exe", &["/Run", "/TN", TASK]).map_err(|e| e.to_string())?;
    Ok(())
}
pub fn uninstall(_home: &Path) -> Result<(), String> {
    wsl::run_program("schtasks.exe", &["/Delete", "/TN", TASK, "/F"]).map_err(|e| e.to_string())?;
    Ok(())
}
pub fn pause() -> Result<(), String> {
    wsl::run_program("schtasks.exe", &["/Change", "/TN", TASK, "/DISABLE"])
        .map_err(|e| e.to_string())?;
    Ok(())
}
pub fn unpause() -> Result<(), String> {
    wsl::run_program("schtasks.exe", &["/Change", "/TN", TASK, "/ENABLE"])
        .map_err(|e| e.to_string())?;
    Ok(())
}
pub fn quiesce(home: &Path) -> Result<(), String> {
    if wsl::is_running(wsl::DISTRO_NAME) {
        super::require_idle(sleepy::GUEST_PORT).map_err(|e| e.to_string())?;
    }
    Backend::new(home)?.stop_native()
}
pub fn restore(home: &Path) -> Result<(), String> {
    // No user-wide WSL shutdown. Restore the API listener for subsequent legacy starts.
    wsl::root_shell("rm -f /etc/systemd/system/firecrab-api.service.d/30-micromanager-controller.conf; systemctl daemon-reload").map_err(|e| e.to_string())?;
    daemon::install(&Layout {
        managed_home: home.into(),
    })
    .map_err(|e| e.to_string())?;
    daemon::stop().map_err(|e| e.to_string())
}
