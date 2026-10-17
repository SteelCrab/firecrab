//! WSL2 allocations are global; the managed distro's VHD is separate.
use std::fs;
use std::path::{Path, PathBuf};

use firecrab_api_types::{ManagerHostCapacity, ManagerNetworkMode, ManagerResources};

use super::super::resource::{self, Error};
use super::{daemon, wsl};

fn wsl_config_path() -> Result<PathBuf, Error> {
    #[cfg(test)]
    if let Some(path) = wsl::fake::config_path() {
        return Ok(path);
    }
    std::env::var_os("USERPROFILE")
        .map(|home| PathBuf::from(home).join(".wslconfig"))
        .ok_or_else(|| Error::Invalid("USERPROFILE is not set".into()))
}

fn powershell(script: &str) -> Result<String, Error> {
    wsl::run_program(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", script],
    )
    .map_err(|error| Error::Invalid(error.to_string()))
}

pub fn host_capacity(home: &Path) -> Result<ManagerHostCapacity, Error> {
    let path = home.to_string_lossy().replace('\'', "''");
    let script = format!(
        "$ErrorActionPreference='Stop'; $c=Get-CimInstance Win32_ComputerSystem; $p='{path}'; while (!(Test-Path -LiteralPath $p)) {{ $p=Split-Path -Parent $p }}; $d=Get-CimInstance Win32_LogicalDisk | Where-Object {{ $_.DeviceID -eq [IO.Path]::GetPathRoot($p).TrimEnd('\\') }}; [ordered]@{{cpu=[int]$c.NumberOfLogicalProcessors;memoryMib=[uint64][math]::Floor($c.TotalPhysicalMemory/1MB);diskAvailableGib=[uint64][math]::Floor($d.FreeSpace/1GB);networkInterfaces=@(Get-NetAdapter | Where-Object Status -eq 'Up' | Select-Object -ExpandProperty Name)}} | ConvertTo-Json -Compress"
    );
    let mut value: serde_json::Value = serde_json::from_str(powershell(&script)?.trim())?;
    value["networkModes"] = serde_json::json!(["nat", "mirrored"]);
    Ok(serde_json::from_value(value)?)
}

fn read_config() -> Result<String, Error> {
    match fs::read_to_string(wsl_config_path()?) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

fn value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let mut in_wsl = false;
    let mut found = None;
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.starts_with('[') {
            in_wsl = line.eq_ignore_ascii_case("[wsl2]");
            continue;
        }
        if !in_wsl || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some((name, val)) = line.split_once('=')
            && name.trim().eq_ignore_ascii_case(key)
        {
            found = Some(val.split(['#', ';']).next().unwrap_or("").trim());
        }
    }
    found
}

fn size_mib(text: &str) -> Option<u64> {
    let text = text.trim().to_ascii_lowercase();
    if let Some(n) = text.strip_suffix("gb") {
        return n.trim().parse::<u64>().ok()?.checked_mul(1024);
    }
    if let Some(n) = text.strip_suffix("mb") {
        return n.trim().parse().ok();
    }
    text.parse::<u64>().ok().map(|bytes| bytes / (1 << 20))
}

pub fn defaults(_home: &Path, host: &ManagerHostCapacity) -> Result<ManagerResources, Error> {
    let text = read_config()?;
    let network_mode = match value(&text, "networkingMode")
        .unwrap_or("nat")
        .to_ascii_lowercase()
        .as_str()
    {
        "nat" => ManagerNetworkMode::Nat,
        "mirrored" => ManagerNetworkMode::Mirrored,
        mode => {
            return Err(Error::Invalid(format!(
                "WSL network mode {mode} is not supported by resource management"
            )));
        }
    };
    Ok(ManagerResources {
        cpu: value(&text, "processors")
            .and_then(|v| v.parse().ok())
            .unwrap_or(host.cpu),
        memory_mib: value(&text, "memory")
            .and_then(size_mib)
            .unwrap_or(host.memory_mib / 2),
        disk_gib: resource::read::<ManagerResources>(&_home.join("runtime/resources-active.json"))?
            .map(|current| current.disk_gib)
            .unwrap_or(
                value(&text, "defaultVhdSize")
                    .and_then(size_mib)
                    .map(|m| m.div_ceil(1024))
                    .unwrap_or(1024),
            ),
        network_mode,
        network_interface: None,
    })
}

/// Preserve other sections, comments, and every setting Firecrab does not own.
pub fn merge_config(text: &str, settings: &ManagerResources) -> String {
    let keys = ["processors", "memory", "networkingMode"];
    let mut lines = Vec::new();
    let mut in_wsl = false;
    let mut inserted = false;
    for line in text.lines() {
        let trimmed = line.trim().trim_start_matches('\u{feff}');
        if trimmed.starts_with('[') {
            in_wsl = trimmed.eq_ignore_ascii_case("[wsl2]");
            lines.push(line.trim_start_matches('\u{feff}').to_owned());
            if in_wsl && !inserted {
                lines.extend(config_lines(settings));
                inserted = true;
            }
            continue;
        }
        let owned = in_wsl
            && trimmed.split_once('=').is_some_and(|(key, _)| {
                keys.iter()
                    .any(|known| key.trim().eq_ignore_ascii_case(known))
            });
        if !owned {
            lines.push(line.to_owned());
        }
    }
    if !inserted {
        lines.push("[wsl2]".into());
        lines.extend(config_lines(settings));
    }
    format!("{}\n", lines.join("\r\n"))
}

fn config_lines(settings: &ManagerResources) -> [String; 3] {
    [
        format!("processors={}", settings.cpu),
        format!("memory={}MB", settings.memory_mib),
        format!(
            "networkingMode={}",
            if settings.network_mode == ManagerNetworkMode::Mirrored {
                "mirrored"
            } else {
                "nat"
            }
        ),
    ]
}

pub fn validate_configuration(settings: &ManagerResources) -> Result<(), Error> {
    if settings.network_mode == ManagerNetworkMode::Mirrored {
        let build: u32 = powershell("[Environment]::OSVersion.Version.Build")?
            .trim()
            .parse()
            .map_err(|_| Error::Invalid("could not detect the Windows build".into()))?;
        if build < 22621 {
            return Err(Error::Invalid(
                "mirrored networking requires Windows 11 22H2 or later".into(),
            ));
        }
    }
    Ok(())
}

fn configure_wsl(home: &Path, settings: &ManagerResources) -> Result<(), Error> {
    validate_configuration(settings)?;
    let running = wsl::run(&["--list", "--running", "--quiet"])
        .map_err(|error| Error::Invalid(error.to_string()))?;
    let others: Vec<_> = running
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty() && !name.eq_ignore_ascii_case(wsl::DISTRO_NAME))
        .collect();
    if !others.is_empty() {
        return Err(Error::Invalid(format!(
            "stop these WSL distributions before applying global resources: {}",
            others.join(", ")
        )));
    }
    if !wsl::contains(&wsl::distributions(), wsl::DISTRO_NAME) {
        return Err(Error::Invalid(
            "microManager is not installed; run firecrab service install".into(),
        ));
    }
    // The virtual disk's actual filesystem is the shrink floor, even if a
    // previous resource apply did not finish writing its active snapshot.
    let actual = wsl::root_shell("df -BG --output=size / | tail -n 1")
        .map_err(|error| Error::Invalid(error.to_string()))?;
    let actual_gib: u64 = actual
        .trim()
        .trim_end_matches('G')
        .parse()
        .map_err(|_| Error::Invalid("could not determine the WSL disk capacity".into()))?;
    if settings.disk_gib < actual_gib {
        return Err(Error::Invalid(
            "shrinking the WSL disk is not supported".into(),
        ));
    }
    let old = read_config()?;
    let config_path = wsl_config_path()?;
    let updated = merge_config(&old, settings);
    let configured = (|| {
        // Keep the native resource controller alive. Terminating its scheduled
        // task would also kill the apply worker before it can restart WSL.
        wsl::root_shell("helper=firecrab-helper; if [ \"$(systemctl show --property=LoadState --value firecrab-helper.service)\" = not-found ]; then helper=firecrab-net-helper; fi; systemctl stop firecrab-api \"$helper\"")
            .map_err(|error| Error::Invalid(error.to_string()))?;
        wsl::run(&["--terminate", wsl::DISTRO_NAME])
            .map_err(|error| Error::Invalid(error.to_string()))?;
        wsl::run(&["--shutdown"]).map_err(|error| Error::Invalid(error.to_string()))?;
        resource::write_bytes(
            &home.join("runtime/wslconfig.before-resources"),
            old.as_bytes(),
        )?;
        resource::write_bytes(&config_path, updated.as_bytes())?;
        if settings.disk_gib > actual_gib
            && let Err(error) = wsl::run(&[
                "--manage",
                wsl::DISTRO_NAME,
                "--resize",
                &format!("{}GB", settings.disk_gib),
            ])
        {
            if let Err(restore) = resource::write_bytes(&config_path, old.as_bytes()) {
                eprintln!("[WARNING] could not restore WSL configuration: {restore}");
            }
            return Err(Error::Invalid(format!("WSL disk resize failed: {error}")));
        }
        Ok(())
    })();
    if let Err(error) = configured {
        let _ = daemon::start();
        return Err(error);
    }
    Ok(())
}

pub fn apply(home: &Path, settings: &ManagerResources) -> Result<(), Error> {
    configure_wsl(home, settings)?;
    let layout = super::lifecycle::Layout {
        managed_home: home.to_owned(),
    };
    if let Err(error) = daemon::install(&layout) {
        let _ = daemon::start();
        return Err(Error::Invalid(error.to_string()));
    }
    let status = daemon::start().map_err(|error| Error::Invalid(error.to_string()))?;
    if !status.success() {
        return Err(Error::Invalid(
            "management VM did not become healthy; inspect service debug --logs".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
