use super::managed_home;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub schema: u32,
    pub cpu: u16,
    pub memory_mib: u32,
    pub autostart: bool,
    pub sleepy: bool,
    pub idle_minutes: u16,
    pub api_port: u16,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            schema: 1,
            cpu: 2,
            memory_mib: 4096,
            autostart: true,
            sleepy: false,
            idle_minutes: 10,
            api_port: 5523,
        }
    }
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Path(#[from] managed_home::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("Settings changed in another process; reopen the editor before saving")]
    Conflict,
}
impl Settings {
    pub fn validate(&self) -> Result<(), Error> {
        let error = if self.schema != 1 {
            Some("Unsupported settings schema")
        } else if !(1..=256).contains(&self.cpu) {
            Some("CPU must be 1..256")
        } else if !(512..=1_048_576).contains(&self.memory_mib) {
            Some("Memory must be 512..1048576 MiB")
        } else if !(1..=1440).contains(&self.idle_minutes) {
            Some("Idle timeout must be 1..1440 minutes")
        } else if self.api_port < 1024 || self.api_port == super::sleepy::GUEST_PORT {
            Some("API port must be 1024..65535; 5524 is reserved")
        } else {
            None
        };
        error.map_or(Ok(()), |message| Err(Error::Invalid(message.into())))
    }
    pub fn set(&mut self, assignment: &str) -> Result<(), Error> {
        let (key, value) = assignment
            .split_once('=')
            .ok_or_else(|| Error::Invalid("Use KEY=VALUE".into()))?;
        let invalid = || Error::Invalid(format!("Invalid {key}: {value}"));
        match key {
            "cpu" => self.cpu = value.parse().map_err(|_| invalid())?,
            "memory_mib" => self.memory_mib = value.parse().map_err(|_| invalid())?,
            "autostart" => self.autostart = value.parse().map_err(|_| invalid())?,
            "sleepy" => self.sleepy = value.parse().map_err(|_| invalid())?,
            "idle_minutes" => self.idle_minutes = value.parse().map_err(|_| invalid())?,
            "api_port" => self.api_port = value.parse().map_err(|_| invalid())?,
            _ => return Err(Error::Invalid(format!("Unknown setting: {key}"))),
        }
        Ok(())
    }
}

pub struct Store {
    pub home: PathBuf,
    pub path: PathBuf,
}
pub struct Snapshot {
    pub settings: Settings,
    bytes: Option<Vec<u8>>,
}
impl Store {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.into(),
            path: home.join("settings.json"),
        }
    }
    fn bytes(&self) -> Result<Option<Vec<u8>>, Error> {
        managed_home::reject_symlink_components(&self.path)?;
        match fs::read(&self.path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    pub fn load(&self) -> Result<Snapshot, Error> {
        let bytes = self.bytes()?;
        let settings: Settings = bytes
            .as_deref()
            .map(serde_json::from_slice)
            .transpose()?
            .unwrap_or_default();
        settings.validate()?;
        Ok(Snapshot { settings, bytes })
    }
    pub fn save(&self, previous: &Snapshot, settings: Settings) -> Result<Snapshot, Error> {
        settings.validate()?;
        managed_home::reject_symlink_components(&self.home)?;
        fs::create_dir_all(&self.home)?;
        let lock_path = self.home.join("settings.lock");
        managed_home::reject_symlink_components(&lock_path)?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| Error::Conflict)?;
        if self.bytes()? != previous.bytes {
            return Err(Error::Conflict);
        }
        let mut bytes = serde_json::to_vec_pretty(&settings)?;
        bytes.push(b'\n');
        let mut staged = tempfile::NamedTempFile::new_in(&self.home)?;
        staged.write_all(&bytes)?;
        staged.as_file().sync_all()?;
        staged.persist(&self.path).map_err(|e| e.error)?;
        Ok(Snapshot {
            settings,
            bytes: Some(bytes),
        })
    }
}

pub fn run(home: &Path, json: bool, assignments: &[String]) -> Result<i32, Error> {
    let store = Store::new(home);
    let snapshot = store.load()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&snapshot.settings)?);
    } else if assignments.is_empty() {
        super::settings_tui::run(&store, snapshot)?;
    } else {
        let mut settings = snapshot.settings.clone();
        for assignment in assignments {
            settings.set(assignment)?;
        }
        store.save(&snapshot, settings)?;
        println!(
            "Saved {}. Run `firecrab service start` to apply resource/port changes and activate the controller.",
            store.path.display()
        );
    }
    Ok(0)
}

/// Preserve unrelated INI sections, comments, BOM and line endings.
#[cfg(any(target_os = "windows", test))]
pub fn merge_wsl(text: &str, settings: &Settings) -> Result<String, Error> {
    let bom = text.starts_with('\u{feff}');
    let text = text.trim_start_matches('\u{feff}');
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut output = Vec::new();
    let mut in_section = false;
    let mut found = false;
    let mut cpu = false;
    let mut memory = false;
    let finish = |output: &mut Vec<String>, cpu: bool, memory: bool| {
        if !cpu {
            output.push(format!("processors={}", settings.cpu));
        }
        if !memory {
            output.push(format!("memory={}MB", settings.memory_mib));
        }
    };
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if in_section {
                finish(&mut output, cpu, memory);
            }
            in_section = trimmed.eq_ignore_ascii_case("[wsl2]");
            if in_section {
                if found {
                    return Err(Error::Invalid(
                        "Duplicate [wsl2] sections; existing .wslconfig was preserved".into(),
                    ));
                }
                found = true;
            }
        }
        let key = trimmed.split_once('=').map(|(key, _)| key.trim());
        let replacement =
            if in_section && key.is_some_and(|key| key.eq_ignore_ascii_case("processors")) {
                if cpu {
                    return Err(Error::Invalid("Duplicate WSL processors setting".into()));
                }
                cpu = true;
                Some(format!("processors={}", settings.cpu))
            } else if in_section && key.is_some_and(|key| key.eq_ignore_ascii_case("memory")) {
                if memory {
                    return Err(Error::Invalid("Duplicate WSL memory setting".into()));
                }
                memory = true;
                Some(format!("memory={}MB", settings.memory_mib))
            } else {
                None
            };
        output.push(replacement.unwrap_or_else(|| line.to_string()));
    }
    if in_section {
        finish(&mut output, cpu, memory);
    }
    if !found {
        output.push("[wsl2]".into());
        finish(&mut output, false, false);
    }
    Ok(format!(
        "{}{}{newline}",
        if bom { "\u{feff}" } else { "" },
        output.join(newline)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wsl_resource_merge_preserves_other_distributions_settings() {
        let input = "\u{feff}# comment\r\n[wsl2]\r\nnetworkingMode=mirrored\r\nmemory=2GB\r\n[experimental]\r\nautoMemoryReclaim=gradual\r\n";
        let merged = merge_wsl(input, &Settings::default()).unwrap();
        assert!(merged.starts_with("\u{feff}# comment\r\n"));
        assert!(merged.contains(
            "networkingMode=mirrored\r\nmemory=4096MB\r\nprocessors=2\r\n[experimental]"
        ));
        assert!(merged.contains("autoMemoryReclaim=gradual"));
        assert_eq!(merge_wsl(&merged, &Settings::default()).unwrap(), merged);
        assert!(merge_wsl("[wsl2]\n[wsl2]\n", &Settings::default()).is_err());
    }
    #[test]
    fn rejects_invalid_and_concurrent_writes_without_losing_previous_settings() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let first = store.load().unwrap();
        let other = store.load().unwrap();
        let mut desired = first.settings.clone();
        desired.sleepy = true;
        let saved = store.save(&first, desired.clone()).unwrap();
        assert!(matches!(
            store.save(&other, Settings::default()),
            Err(Error::Conflict)
        ));
        desired.cpu = 0;
        assert!(store.save(&saved, desired).is_err());
        assert!(store.load().unwrap().settings.sleepy);
    }
    #[test]
    fn rejects_unknown_schema_and_corrupt_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        fs::write(&store.path, b"{\"schema\":2}").unwrap();
        assert!(store.load().is_err());
        fs::write(&store.path, b"{\"new_setting\":true}").unwrap();
        assert!(store.load().is_err());
        fs::write(&store.path, b"invalid").unwrap();
        assert!(store.load().is_err());
    }
    #[cfg(unix)]
    #[test]
    fn does_not_follow_settings_or_lock_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("manager");
        fs::create_dir(&home).unwrap();
        let victim = root.path().join("victim");
        fs::write(&victim, b"{}").unwrap();
        let store = Store::new(&home);
        let snapshot = store.load().unwrap();
        std::os::unix::fs::symlink(&victim, home.join("settings.lock")).unwrap();
        assert!(store.save(&snapshot, Settings::default()).is_err());
        std::os::unix::fs::symlink(&victim, &store.path).unwrap();
        assert!(store.load().is_err());
        assert_eq!(fs::read(&victim).unwrap(), b"{}");
    }
}
