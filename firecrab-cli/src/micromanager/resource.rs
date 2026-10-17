//! One persisted allocation shared by the CLI, native controller, and launcher.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use firecrab_api_types::{
    HostOs, ManagerHostCapacity, ManagerNetworkMode, ManagerResourceOperation,
    ManagerResourceStatus, ManagerResources,
};
use fs2::FileExt;
use serde::{Serialize, de::DeserializeOwned};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum NetworkMode {
    Nat,
    Bridged,
    Mirrored,
}

#[derive(Args, Default)]
pub struct Options {
    /// Virtual CPU count (bounded by the physical host).
    #[arg(long)]
    pub cpu: Option<u32>,
    /// RAM in MiB; shared across the management OS and all MicroVMs.
    #[arg(long, visible_alias = "memory")]
    pub ram: Option<u64>,
    /// Data disk capacity in GiB. Only growth is supported.
    #[arg(long)]
    pub disk: Option<u64>,
    /// macOS: nat/bridged. Windows: nat/mirrored (global WSL2 setting).
    #[arg(long, value_enum)]
    pub network: Option<NetworkMode>,
    /// macOS physical interface for bridged mode, e.g. en0.
    #[arg(long)]
    pub interface: Option<String>,
    /// Apply the saved allocation and restart the management VM and its workloads.
    #[arg(long)]
    pub apply: bool,
    /// Emit the allocation and physical host capacity as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("resource I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid resource configuration: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("a resource change is already in progress")]
    Busy,
}

/// Native host operations, separated so state tests never touch a host VM.
#[derive(Clone, Copy)]
pub(super) struct Platform {
    pub os: HostOs,
    pub capacity: fn(&Path) -> Result<ManagerHostCapacity, Error>,
    pub defaults: fn(&Path, &ManagerHostCapacity) -> Result<ManagerResources, Error>,
    pub validate: fn(&Path, &ManagerResources) -> Result<(), Error>,
    pub apply: fn(&Path, &ManagerResources) -> Result<(), Error>,
}

impl Platform {
    pub fn native() -> Self {
        Self {
            os: os(),
            capacity: host_capacity,
            defaults,
            validate: |_home, settings| {
                #[cfg(target_os = "macos")]
                return super::macos::resource::validate_configuration(_home, settings);
                #[cfg(target_os = "windows")]
                return super::windows::resource::validate_configuration(settings);
                #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                {
                    let _ = (_home, settings);
                    Err(Error::Invalid("unsupported platform".into()))
                }
            },
            apply: |home, settings| {
                #[cfg(target_os = "macos")]
                return super::macos::resource::apply(home, settings);
                #[cfg(target_os = "windows")]
                return super::windows::resource::apply(home, settings);
                #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                {
                    let _ = (home, settings);
                    Err(Error::Invalid("unsupported platform".into()))
                }
            },
        }
    }
}

pub fn home() -> Result<PathBuf, Error> {
    #[cfg(target_os = "macos")]
    return super::macos::lifecycle::Layout::from_process_env()
        .map(|layout| layout.managed_home)
        .map_err(|error| Error::Invalid(error.to_string()));
    #[cfg(target_os = "windows")]
    return super::windows::lifecycle::Layout::from_process_env()
        .map(|layout| layout.managed_home)
        .map_err(|error| Error::Invalid(error.to_string()));
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    Err(Error::Invalid(
        "microManager requires macOS or Windows".into(),
    ))
}

pub fn os() -> HostOs {
    if cfg!(target_os = "windows") {
        HostOs::Windows
    } else {
        HostOs::Macos
    }
}

pub fn controller_id(home: &Path) -> Result<String, Error> {
    fs::create_dir_all(home.join("runtime"))?;
    let identity_lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join("runtime/resource-controller-id.lock"))?;
    identity_lock.lock_exclusive()?;
    let path = home.join("runtime/resource-controller-id.json");
    if let Some(id) = read::<String>(&path)? {
        uuid::Uuid::parse_str(&id)
            .map_err(|_| Error::Invalid("invalid resource controller identity".into()))?;
        return Ok(id);
    }
    let id = uuid::Uuid::new_v4().to_string();
    write(&path, &id)?;
    Ok(id)
}

pub fn host_capacity(home: &Path) -> Result<ManagerHostCapacity, Error> {
    #[cfg(target_os = "macos")]
    return super::macos::resource::host_capacity(home);
    #[cfg(target_os = "windows")]
    return super::windows::resource::host_capacity(home);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = home;
        Err(Error::Invalid("unsupported platform".into()))
    }
}

pub fn defaults(home: &Path, host: &ManagerHostCapacity) -> Result<ManagerResources, Error> {
    #[cfg(target_os = "windows")]
    return super::windows::resource::defaults(home, host);
    #[cfg(not(target_os = "windows"))]
    {
        let _ = host;
        let disk = fs::metadata(home.join("data/firecrab-data.raw"))
            .map(|metadata| metadata.len().div_ceil(1 << 30))
            .unwrap_or(16);
        Ok(ManagerResources {
            cpu: 2,
            memory_mib: 4096,
            disk_gib: disk,
            network_mode: ManagerNetworkMode::Nat,
            network_interface: None,
        })
    }
}

pub fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, Error> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn write<T: Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    write_bytes(path, &serde_json::to_vec_pretty(value)?)
}

pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid("missing parent directory".into()))?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| Error::Io(error.error))?;
    Ok(())
}

pub fn lock(home: &Path) -> Result<File, Error> {
    fs::create_dir_all(home.join("runtime"))?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join("runtime/resource.lock"))?;
    file.try_lock_exclusive().map_err(|error| {
        if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
            Error::Busy
        } else {
            error.into()
        }
    })?;
    Ok(file)
}

impl Platform {
    pub fn status(&self, home: &Path) -> Result<ManagerResourceStatus, Error> {
        let host = (self.capacity)(home)?;
        let settings = match read(&home.join("resources.json"))? {
            Some(settings) => settings,
            None => (self.defaults)(home, &host)?,
        };
        let current = read(&home.join("runtime/resources-active.json"))?;
        let operation_path = home.join("runtime/resource-operation.json");
        let mut operation: Option<ManagerResourceOperation> = read(&operation_path)?;
        if let Some(active) = operation.as_mut()
            && active.state == "applying"
            && fs::metadata(&operation_path)?
                .modified()?
                .elapsed()
                .is_ok_and(|age| age.as_secs() > 30)
            && let Ok(_lock) = lock(home)
        {
            active.state = "failed".into();
            active.error = Some("the resource apply worker stopped before completion; inspect runtime/resource-apply.log and retry".into());
            write(&operation_path, active)?;
        }
        let pending = current.as_ref().is_none_or(|current| current != &settings)
            || operation
                .as_ref()
                .is_some_and(|operation| operation.state == "failed");
        Ok(ManagerResourceStatus {
            controller_id: controller_id(home)?,
            os: self.os,
            settings,
            current,
            host,
            pending,
            shared_wsl: self.os == HostOs::Windows,
            operation,
        })
    }
}

pub fn validate(
    settings: &ManagerResources,
    host: &ManagerHostCapacity,
    os: HostOs,
    minimum_disk: u64,
) -> Result<(), Error> {
    if settings.cpu == 0 || settings.cpu > host.cpu {
        return Err(Error::Invalid(format!(
            "CPU must be between 1 and {}",
            host.cpu
        )));
    }
    if settings.memory_mib < 512 || settings.memory_mib > host.memory_mib {
        return Err(Error::Invalid(format!(
            "RAM must be between 512 and {} MiB",
            host.memory_mib
        )));
    }
    if settings.disk_gib < minimum_disk || settings.disk_gib == 0 || settings.disk_gib > 65536 {
        return Err(Error::Invalid(format!(
            "disk must be between {minimum_disk} and 65536 GiB; shrinking is not supported"
        )));
    }
    if !host.network_modes.contains(&settings.network_mode) {
        return Err(Error::Invalid(
            "this network mode is unavailable with the installed host/helper capabilities".into(),
        ));
    }
    match (os, settings.network_mode) {
        (HostOs::Macos, ManagerNetworkMode::Nat)
        | (HostOs::Windows, ManagerNetworkMode::Nat | ManagerNetworkMode::Mirrored) => {
            if settings.network_interface.is_some() {
                return Err(Error::Invalid(
                    "a physical interface is only used with macOS bridged networking".into(),
                ));
            }
        }
        (HostOs::Macos, ManagerNetworkMode::Bridged) => {
            if !settings
                .network_interface
                .as_ref()
                .is_some_and(|name| host.network_interfaces.contains(name))
            {
                return Err(Error::Invalid(
                    "select an available physical interface for bridged networking".into(),
                ));
            }
        }
        _ => {
            return Err(Error::Invalid(
                "this network mode is not supported on this host".into(),
            ));
        }
    }
    Ok(())
}

impl Platform {
    pub fn save(&self, home: &Path, settings: &ManagerResources) -> Result<(), Error> {
        let _lock = lock(home)?;
        let status = self.status(home)?;
        if status
            .operation
            .as_ref()
            .is_some_and(|operation| operation.state == "applying")
        {
            return Err(Error::Busy);
        }
        let minimum_disk = match status.current.as_ref() {
            Some(current) => current.disk_gib,
            None => (self.defaults)(home, &status.host)?.disk_gib,
        };
        validate(settings, &status.host, status.os, minimum_disk)?;
        (self.validate)(home, settings)?;
        write(&home.join("resources.json"), settings)
    }

    fn apply_saved(&self, home: &Path) -> Result<(), Error> {
        let status = self.status(home)?;
        let minimum_disk = match status.current.as_ref() {
            Some(current) => current.disk_gib,
            None => (self.defaults)(home, &status.host)?.disk_gib,
        };
        validate(&status.settings, &status.host, status.os, minimum_disk)?;
        (self.apply)(home, &status.settings)?;
        write(
            &home.join("runtime/resources-active.json"),
            &status.settings,
        )
    }
}

pub fn run(options: Options) -> Result<i32, Error> {
    run_in(&home()?, options, Platform::native())
}

fn run_in(home: &Path, options: Options, platform: Platform) -> Result<i32, Error> {
    let mut settings = platform.status(home)?.settings;
    let changed = options.cpu.is_some()
        || options.ram.is_some()
        || options.disk.is_some()
        || options.network.is_some()
        || options.interface.is_some();
    if let Some(cpu) = options.cpu {
        settings.cpu = cpu;
    }
    if let Some(memory) = options.ram {
        settings.memory_mib = memory;
    }
    if let Some(disk) = options.disk {
        settings.disk_gib = disk;
    }
    if let Some(mode) = options.network {
        settings.network_mode = match mode {
            NetworkMode::Nat => ManagerNetworkMode::Nat,
            NetworkMode::Bridged => ManagerNetworkMode::Bridged,
            NetworkMode::Mirrored => ManagerNetworkMode::Mirrored,
        };
        if settings.network_mode != ManagerNetworkMode::Bridged {
            settings.network_interface = None;
        }
    }
    if let Some(interface) = options.interface {
        settings.network_interface = Some(interface);
    }
    if changed {
        platform.save(home, &settings)?;
    }
    if options.apply {
        let id = uuid::Uuid::new_v4().to_string();
        if options.json {
            let output = std::process::Command::new(std::env::current_exe()?)
                .args(["service", "resource-apply", &id])
                .env("FIRECRAB_MICROMANAGER_HOME", home)
                .output()?;
            if !output.status.success() {
                return Err(Error::Invalid(
                    String::from_utf8_lossy(&output.stderr).trim().into(),
                ));
            }
        } else {
            run_apply_in(home, &id, platform)?;
        }
    }
    let status = platform.status(home)?;
    if options.json {
        super::report!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        super::report!(
            "microManager resources\n  CPU: {} / {} vCPU\n  RAM: {} / {} MiB\n  disk: {} GiB\n  physical disk available: {} GiB\n  network: {:?}{}\n  pending restart: {}",
            status.settings.cpu,
            status.host.cpu,
            status.settings.memory_mib,
            status.host.memory_mib,
            status.settings.disk_gib,
            status.host.disk_available_gib,
            status.settings.network_mode,
            status
                .settings
                .network_interface
                .as_ref()
                .map(|name| format!(" ({name})"))
                .unwrap_or_default(),
            status.pending
        );
        if status.shared_wsl {
            super::report!("  scope: CPU, RAM, and networking apply to all WSL2 distributions");
        }
        if changed && !options.apply {
            super::report!(
                "Saved. Apply with `firecrab service resorce --apply` (restarts workloads)."
            );
        }
    }
    Ok(0)
}

pub fn run_apply(id: &str) -> Result<i32, Error> {
    run_apply_in(&home()?, id, Platform::native())
}

fn run_apply_in(home: &Path, id: &str, platform: Platform) -> Result<i32, Error> {
    let _lock = lock(home)?;
    let result = platform.apply_saved(home);
    let operation = ManagerResourceOperation {
        id: id.to_owned(),
        state: if result.is_ok() { "complete" } else { "failed" }.into(),
        error: result.as_ref().err().map(ToString::to_string),
    };
    write(&home.join("runtime/resource-operation.json"), &operation)?;
    result.map(|()| 0)
}

#[cfg(test)]
pub(super) mod tests;
