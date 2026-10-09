use super::super::{
    macos::{daemon, lifecycle::Layout},
    settings::Settings,
    sleepy,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
const LABEL: &str = "io.firecrab.micromanager.controller";
pub fn session() -> Result<String, String> {
    let output = Command::new("/bin/launchctl")
        .arg("managerpid")
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("launchd session unavailable".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().into())
}
pub struct Backend {
    layout: Layout,
}
fn layout(home: &Path) -> Result<Layout, String> {
    let mut layout = Layout::from_process_env().map_err(|e| e.to_string())?;
    layout.managed_home = home.into();
    Ok(layout)
}
impl Backend {
    pub fn new(home: &Path) -> Result<Self, String> {
        Ok(Self {
            layout: layout(home)?,
        })
    }
    pub fn stop_native(&self) -> Result<(), String> {
        daemon::stop(&self.layout).map_err(|e| e.to_string())
    }
}
impl sleepy::Backend for Backend {
    fn wake(&self, _settings: &Settings) -> Result<(), String> {
        let existing = daemon::status(&self.layout).map_err(|e| e.to_string())?;
        if existing.success() {
            return Ok(());
        }
        if existing.loaded {
            return Err("Management VM is already active but its API is unavailable; inspect firecrab service debug --logs".into());
        }
        let status = daemon::start(&self.layout).map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("Management VM did not become ready".into());
        }
        Ok(())
    }
    fn stop(&self) -> Result<(), String> {
        self.stop_native()
    }
}
pub fn prepare(home: &Path, _settings: &Settings, _resources_changed: bool) -> Result<(), String> {
    let layout = layout(home)?;
    if !super::active(home) {
        let status = daemon::status(&layout).map_err(|e| e.to_string())?;
        if !status.success() {
            daemon::start(&layout).map_err(|e| e.to_string())?;
        }
        super::require_idle(5523).map_err(|e| e.to_string())?;
        daemon::stop(&layout).map_err(|e| e.to_string())?;
    }
    fs::write(home.join("runtime/controlled"), b"1\n").map_err(|e| e.to_string())?;
    daemon::install(
        &layout,
        &layout.helper_path(),
        &home.join("runtime/manager_ed25519"),
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
fn domain() -> Result<String, String> {
    let output = Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("Could not resolve launchd user".into());
    }
    Ok(format!(
        "gui/{}",
        String::from_utf8_lossy(&output.stdout).trim()
    ))
}
fn plist() -> Result<PathBuf, String> {
    Ok(
        PathBuf::from(std::env::var_os("HOME").ok_or("HOME missing")?)
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist")),
    )
}
fn launch(args: &[&str], required: bool) -> Result<(), String> {
    let output = Command::new("/bin/launchctl")
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if required && !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().into());
    }
    Ok(())
}
fn xml(path: &Path) -> String {
    path.to_string_lossy()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
pub fn install(home: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bytes = fs::read(&exe).map_err(|e| e.to_string())?;
    use sha2::Digest;
    let staged = home
        .join("runtime")
        .join(format!("controller-{:x}", sha2::Sha256::digest(&bytes)));
    if !staged.exists() {
        fs::copy(&exe, &staged).map_err(|e| e.to_string())?;
    }
    let path = plist()?;
    fs::create_dir_all(path.parent().ok_or("No LaunchAgents directory")?)
        .map_err(|e| e.to_string())?;
    let content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>Label</key><string>{LABEL}</string>
<key>ProgramArguments</key><array><string>{exe}</string><string>service</string><string>controller</string><string>--home</string><string>{home}</string></array>
<key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>ThrottleInterval</key><integer>5</integer>
<key>StandardOutPath</key><string>{log}</string><key>StandardErrorPath</key><string>{err}</string></dict></plist>"#,
        exe = xml(&staged),
        home = xml(home),
        log = xml(&home.join("runtime/controller.log")),
        err = xml(&home.join("runtime/controller.err.log"))
    );
    let service = format!("{}/{LABEL}", domain()?);
    launch(&["bootout", &service], false)?;
    fs::write(&path, content).map_err(|e| e.to_string())?;
    launch(&["bootstrap", &domain()?, &path.to_string_lossy()], true)
}
pub fn uninstall(_home: &Path) -> Result<(), String> {
    launch(&["bootout", &format!("{}/{LABEL}", domain()?)], false)?;
    let path = plist()?;
    if path.exists() {
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}
pub fn pause() -> Result<(), String> {
    Ok(())
}
pub fn unpause() -> Result<(), String> {
    Ok(())
}
pub fn quiesce(home: &Path) -> Result<(), String> {
    let backend = Backend::new(home)?;
    if daemon::status(&backend.layout)
        .map_err(|e| e.to_string())?
        .loaded
    {
        super::require_idle(sleepy::GUEST_PORT).map_err(|e| e.to_string())?;
    }
    backend.stop_native()
}
pub fn restore(home: &Path) -> Result<(), String> {
    let layout = layout(home)?;
    fs::remove_file(home.join("runtime/controlled")).map_err(|e| e.to_string())?;
    daemon::install(
        &layout,
        &layout.helper_path(),
        &home.join("runtime/manager_ed25519"),
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
