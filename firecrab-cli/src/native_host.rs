//! Read the actual native machine and directory object; never substitute a guest descriptor.

use firecrab_helper_protocol::native_iac::{Platform, Target};
use sha2::{Digest, Sha256};
use std::{fs, io, path::Path};
use uuid::Uuid;

#[cfg(target_os = "linux")]
mod configuration;

mod journal;
mod maintenance;
mod metadata;
pub(crate) mod server;

/// Only the native launcher/process environment selects this installation.
pub(crate) fn selected_home() -> io::Result<std::path::PathBuf> {
    #[cfg(target_os = "linux")]
    {
        Ok(crate::service::env::ServiceEnv::from_process_env().confdir)
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        crate::micromanager::native_iac_home().map_err(io::Error::other)
    }
}

/// Local native broker uses the operating system's installation-file authority.
pub(crate) fn run_metadata() -> io::Result<()> {
    use std::io::Read;
    let home = selected_home()?;
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(16 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 {
        return Err(invalid("native ownership request exceeds 16 KiB"));
    }
    let envelope: firecrab_helper_protocol::native_iac::Envelope =
        serde_json::from_slice(&bytes).map_err(|_| invalid("invalid native ownership request"))?;
    envelope.validate().map_err(invalid)?;
    let reply = metadata::handle(&home, envelope.request)?;
    println!("{}", serde_json::to_string(&reply)?);
    Ok(())
}

/// Existing native configuration writers cannot bypass the retained owner or target reservation.
pub(crate) fn require_unmanaged(home: &Path) -> io::Result<()> {
    metadata::require_unmanaged(home)
}

/// A retained descriptor serializes native installation maintenance with ownership admission.
#[cfg(any(target_os = "macos", target_os = "windows", test))]
pub(crate) struct InstallationGuard {
    _lock: metadata::Lock,
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
pub(crate) fn installation_guard(home: &Path) -> io::Result<Option<InstallationGuard>> {
    match fs::symlink_metadata(home) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(_) => (),
    }
    Ok(Some(InstallationGuard {
        _lock: metadata::installation_lock(home)?,
    }))
}

#[cfg(target_os = "linux")]
pub(crate) use maintenance::{
    Options as MaintenanceOptions, check as check_maintenance, run as run_maintenance,
};

/// Identity acquisition cannot create or reconfigure an installation.
pub(crate) fn inspect(home: &Path) -> io::Result<Target> {
    #[cfg(target_os = "linux")]
    let platform = Platform::Linux;
    #[cfg(target_os = "macos")]
    let platform = Platform::Macos;
    #[cfg(target_os = "windows")]
    let platform = Platform::Windows;
    build(home, platform, machine_identity()?)
}

/// Keep machine identity values out of diagnostics.
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// The custom UUID is stable for this machine/directory object without creating an identity file.
fn build(home: &Path, platform: Platform, machine: String) -> io::Result<Target> {
    let root = fs::canonicalize(home)?;
    if !fs::metadata(&root)?.is_dir() {
        return Err(invalid("native installation root is not a directory"));
    }
    let directory_id = directory_identity(&root)?;
    let machine_sha256 = format!("{:x}", Sha256::digest(machine.as_bytes()));
    let fingerprint = Sha256::digest(format!(
        "native-installation-v1:{}:{machine_sha256}:{directory_id}",
        platform.as_str()
    ));
    let mut id: [u8; 16] = fingerprint[..16].try_into().expect("fixed SHA256 prefix");
    id[6] = (id[6] & 0x0f) | 0x80;
    id[8] = (id[8] & 0x3f) | 0x80;
    let target = Target {
        platform,
        installation_id: Uuid::from_bytes(id),
        machine_sha256,
        configuration_root: root
            .to_str()
            .ok_or_else(|| invalid("native installation path is not UTF-8"))?
            .into(),
        directory_id,
    };
    target.validate().map_err(invalid)?;
    Ok(target)
}

#[cfg(unix)]
/// Creation time prevents a later filesystem object from reusing a retired inode identity.
fn directory_identity(path: &Path) -> io::Result<String> {
    let metadata = firecrab_helper_protocol::physical_directory::observe(path)?;
    let created = u128::from(metadata.created_seconds) * 1_000_000_000
        + u128::from(metadata.created_nanoseconds);
    Ok(format!(
        "{:x}:{:x}:{created:x}",
        metadata.device, metadata.inode
    ))
}

#[cfg(target_os = "windows")]
/// ReFS requires the full 128-bit FileId together with its containing volume.
/// See https://learn.microsoft.com/en-us/windows/desktop/api/winbase/ns-winbase-file_id_info.
fn directory_identity(path: &Path) -> io::Result<String> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
        FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx,
    };
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let mut info = std::mem::MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: the live file handle and correctly sized output buffer remain valid throughout this call.
    let result = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            info.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful FileIdInfo call initialized the complete FILE_ID_INFO structure.
    let info = unsafe { info.assume_init() };
    let mut basic = std::mem::MaybeUninit::<FILE_BASIC_INFO>::uninit();
    // SAFETY: the same live directory handle and correctly sized output remain valid.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            basic.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful FileBasicInfo call initialized this complete structure.
    let created = unsafe { basic.assume_init() }.CreationTime;
    if created <= 0 {
        return Err(invalid("native directory creation time is unavailable"));
    }
    if info.FileId.Identifier.iter().all(|byte| *byte == 0) {
        return Err(invalid(
            "native filesystem has no stable directory identity",
        ));
    }
    let id = info
        .FileId
        .Identifier
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("{:x}:{id}:{created:x}", info.VolumeSerialNumber))
}

#[cfg(target_os = "linux")]
/// Read the Linux installation's stable identity without consulting a guest-provided descriptor.
fn machine_identity() -> io::Result<String> {
    let value = fs::read_to_string("/etc/machine-id")?;
    let value = value.trim();
    if value.len() != 32
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
        || value.bytes().all(|byte| byte == b'0')
    {
        return Err(invalid("native Linux machine identity is unavailable"));
    }
    Ok(value.to_ascii_lowercase())
}

#[cfg(target_os = "macos")]
/// IORegistry describes the native Mac running the CLI, outside its management VM.
fn machine_identity() -> io::Result<String> {
    let result = std::process::Command::new("/usr/sbin/ioreg")
        .args(["-r", "-d", "1", "-c", "IOPlatformExpertDevice"])
        .output()?;
    if !result.status.success() {
        return Err(invalid("native macOS machine identity is unavailable"));
    }
    let value = String::from_utf8(result.stdout)
        .map_err(|_| invalid("native machine identity is not UTF-8"))?;
    let value = value
        .lines()
        .filter_map(|line| line.split_once('='))
        .find(|(key, _)| key.trim() == "\"IOPlatformUUID\"")
        .map(|(_, value)| value.trim().trim_matches('"'))
        .ok_or_else(|| invalid("native macOS machine identity is unavailable"))?;
    Uuid::parse_str(value)
        .ok()
        .filter(|id| !id.is_nil())
        .map(|id| id.to_string())
        .ok_or_else(|| invalid("native macOS machine identity is invalid"))
}

#[cfg(target_os = "windows")]
/// Read the native 64-bit registry view rather than a WSL filesystem substitute.
fn machine_identity() -> io::Result<String> {
    use windows_sys::Win32::{
        Foundation::ERROR_SUCCESS,
        System::Registry::{
            HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY, RegGetValueW,
        },
    };
    let key = "SOFTWARE\\Microsoft\\Cryptography\0"
        .encode_utf16()
        .collect::<Vec<_>>();
    let name = "MachineGuid\0".encode_utf16().collect::<Vec<_>>();
    let mut buffer = [0_u16; 128];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    // SAFETY: both inputs are terminated UTF-16; size describes the writable output buffer.
    let result = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    let length = buffer
        .iter()
        .position(|value| *value == 0)
        .ok_or_else(|| invalid("native Windows identity is invalid"))?;
    let value = String::from_utf16(&buffer[..length])
        .map_err(|_| invalid("native Windows identity is invalid"))?;
    Uuid::parse_str(&value)
        .ok()
        .filter(|id| !id.is_nil())
        .map(|id| id.to_string())
        .ok_or_else(|| invalid("native Windows machine identity is invalid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_native_installation_preflight_does_not_create_a_profile() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("not-installed");
        assert!(installation_guard(&home).unwrap().is_none());
        assert!(!home.exists());
    }

    #[test]
    fn replacing_a_native_directory_cannot_reuse_its_original_installation_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("installation");
        fs::create_dir(&root).unwrap();
        let platform = if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::Macos
        } else {
            Platform::Linux
        };
        let first = build(&root, platform, "fixture-native-machine".into()).unwrap();
        assert_eq!(
            build(&root, platform, "fixture-native-machine".into()).unwrap(),
            first
        );
        fs::rename(&root, temporary.path().join("former-fixture")).unwrap();
        fs::create_dir(&root).unwrap();
        let replaced = build(&root, platform, "fixture-native-machine".into()).unwrap();
        assert_ne!(first.directory_id, replaced.directory_id);
        assert_ne!(first.installation_id, replaced.installation_id);
        assert_ne!(
            first.installation_id,
            build(&root, platform, "another-fixture-machine".into())
                .unwrap()
                .installation_id
        );
    }
}
