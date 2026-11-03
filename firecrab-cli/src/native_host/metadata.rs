//! Physical native metadata stays outside the guest and shares all native settings locks.

use super::{
    inspect, invalid,
    journal::{Journal, Snapshot},
};
use firecrab_helper_protocol::native_iac::{Reply, Request, Target};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::Path,
};

/// Explicit unlock prevents forked children from extending the metadata transaction lifetime.
pub(super) struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

/// Lock paths are selected by the native installation, never by protocol fields.
fn lock(path: &Path) -> io::Result<Lock> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("native lock must be a regular file"));
    }
    fs2::FileExt::try_lock_exclusive(&file).map_err(|_| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "native installation has an unfinished operation",
        )
    })?;
    Ok(Lock(file))
}

/// Reject links at the authority boundary instead of following an alternate owner ledger.
fn regular(path: &Path, directory: bool) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || (if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file()
        })
    {
        return Err(invalid(
            "native ownership requires protected regular files and directories",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != nix::unistd::Uid::current().as_raw() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "run the native broker as the installation owner",
            ));
        }
        if metadata.mode() & if directory { 0o022 } else { 0o077 } != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native ownership state is not private",
            ));
        }
    }
    Ok(())
}

/// Read a bounded owner journal without consulting mutable settings or guest runtime.
fn read(home: &Path, target: &Target) -> io::Result<Journal> {
    let directory = home.join("iac-native");
    match fs::symlink_metadata(&directory) {
        Ok(_) => regular(&directory, true)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Journal::new(target.clone()));
        }
        Err(error) => return Err(error),
    }
    let path = directory.join("ownership.json");
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Journal::new(target.clone()));
        }
        Err(error) => return Err(error),
    };
    regular(home, true)?;
    regular(&directory, true)?;
    regular(&path, false)?;
    if metadata.len() > 64 * 1024 * 1024 {
        return Err(invalid("native ownership state exceeds its read limit"));
    }
    let mut bytes = Vec::new();
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
    }
    options
        .open(path)?
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err(invalid("native ownership state exceeds its read limit"));
    }
    let journal: Journal =
        serde_json::from_slice(&bytes).map_err(|_| invalid("native ownership state is corrupt"))?;
    journal.check_target(target).map_err(invalid)?;
    Ok(journal)
}

/// Flush the complete state before acknowledging either a CAS or no-effect rejection.
fn write(home: &Path, journal: &Journal) -> io::Result<()> {
    let directory = home.join("iac-native");
    regular(home, true)?;
    regular(&directory, true)?;
    let bytes = serde_json::to_vec(journal)?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err(invalid(
            "native ownership state exceeds its retention capacity",
        ));
    }
    let mut file = tempfile::NamedTempFile::new_in(&directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    commit(file, &directory.join("ownership.json"))?;
    #[cfg(unix)]
    File::open(directory)?.sync_all()?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) struct MaintenanceLease {
    home: std::path::PathBuf,
    target: Target,
    journal: Journal,
    request: super::maintenance::Request,
    _locks: Vec<Lock>,
}

#[cfg(target_os = "linux")]
impl MaintenanceLease {
    /// EOF/drop cannot clear busy state; only explicit successful original completion can.
    pub(super) fn complete(mut self) -> io::Result<()> {
        if !self.home.exists() && self.request.action == super::maintenance::Action::Uninstall {
            return Ok(());
        }
        if inspect(&self.home)? != self.target {
            return Err(invalid(
                "native maintenance installation changed before completion",
            ));
        }
        self.journal
            .finish_maintenance(&self.request)
            .map_err(invalid)?;
        write(&self.home, &self.journal)
    }
}

#[cfg(target_os = "linux")]
pub(super) fn maintenance_lease(
    home: &Path,
    request: super::maintenance::Request,
) -> io::Result<Option<MaintenanceLease>> {
    crate::micromanager::managed_home_for_iac(home)?;
    fs::create_dir_all(home)?;
    regular(home, true)?;
    let target = inspect(home)?;
    let directory = home.join("iac-native");
    match fs::create_dir(&directory) {
        Ok(()) => {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => regular(&directory, true)?,
        Err(error) => return Err(error),
    }
    let mut locks = vec![lock(&directory.join("ownership.lock"))?];
    locks.extend(native_locks(home)?);
    let mut journal = read(home, &target)?;
    if !journal.begin_maintenance(&request).map_err(invalid)? {
        return Ok(None);
    }
    write(home, &journal)?;
    Ok(Some(MaintenanceLease {
        home: home.to_owned(),
        target,
        journal,
        request,
        _locks: locks,
    }))
}

#[cfg(target_os = "linux")]
pub(super) fn check_maintenance(
    home: &Path,
    request: &super::maintenance::Request,
) -> io::Result<()> {
    let target = inspect(home)?;
    read(home, &target)?
        .check_maintenance(request)
        .map_err(invalid)
}

/// Native Unix rename preserves the prior complete file if committing fails.
#[cfg(unix)]
fn commit(file: tempfile::NamedTempFile, destination: &Path) -> io::Result<()> {
    file.persist(destination).map_err(|error| error.error)?;
    Ok(())
}

/// Windows write-through replacement flushes the rename rather than relying on directory fsync.
#[cfg(target_os = "windows")]
fn commit(file: tempfile::NamedTempFile, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let (handle, path) = file.into_parts();
    drop(handle);
    let source = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SAFETY: both paths are terminated UTF-16 and live until the call returns.
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The local OS account is authoritative for this selected native installation.
pub(super) fn require_unmanaged(home: &Path) -> io::Result<()> {
    match fs::symlink_metadata(home.join("iac-native")) {
        Ok(_) => regular(&home.join("iac-native"), true)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    let target = inspect(home)?;
    writable(home, &target)
}

/// The production caller supplies only the independently inspected native target.
fn writable(home: &Path, target: &Target) -> io::Result<()> {
    let journal = read(home, target)?;
    if journal.protected("management_settings")
        || journal.protected("host_configuration")
        || journal.maintenance_busy()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "native installation is managed or awaits API ownership confirmation",
        ));
    }
    Ok(())
}

/// Validate resource identity before opening any settings-selected metadata slot.
fn resource(request: &Request) -> Option<&firecrab_stack::ManagedResource> {
    match request {
        Request::Observe { resource, .. } => Some(resource),
        Request::ChangeOwner { request } => Some(&request.change.resource),
        _ => None,
    }
}

/// Root metadata and all legacy native configuration writers serialize on the same files.
fn native_locks(home: &Path) -> io::Result<Vec<Lock>> {
    let runtime = home.join("runtime");
    fs::create_dir_all(&runtime)?;
    regular(&runtime, true)?;
    [
        runtime.join("native-install.lock"),
        runtime.join("resource.lock"),
        runtime.join("controller-install.lock"),
        home.join("settings.lock"),
    ]
    .iter()
    .map(|path| lock(path))
    .collect()
}

/// Native install/uninstall keep this lock across every service and filesystem side effect.
#[cfg(any(target_os = "macos", target_os = "windows", test))]
pub(super) fn installation_lock(home: &Path) -> io::Result<Lock> {
    crate::micromanager::managed_home_for_iac(home)?;
    let runtime = home.join("runtime");
    fs::create_dir_all(&runtime)?;
    regular(home, true)?;
    regular(&runtime, true)?;
    let guard = lock(&runtime.join("native-install.lock"))?;
    require_unmanaged(home)?;
    Ok(guard)
}

/// Persistent native settings are read with no helper request, VM boot or service mutation.
fn snapshot(home: &Path, kind: &str) -> io::Result<Snapshot> {
    #[cfg(target_os = "linux")]
    if kind == "host_configuration" {
        let env = crate::service::env::ServiceEnv::from_process_env();
        let mut snapshot = super::configuration::snapshot(home, &env.unitdir)?;
        let jobs = std::process::Command::new("systemctl")
            .args(["list-jobs", "--plain", "--no-legend", "--no-pager"])
            .output()?;
        snapshot.busy = !jobs.status.success()
            || String::from_utf8_lossy(&jobs.stdout)
                .split_ascii_whitespace()
                .any(|part| {
                    crate::service::env::UNITS.contains(&part)
                        || part == crate::service::env::LEGACY_HELPER_UNIT
                });
        return Ok(snapshot);
    }
    #[cfg(any(target_os = "macos", target_os = "windows", test))]
    {
        if kind != "management_settings" {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this native facet is not available",
            ));
        }
        let (exists, busy, configuration_sha256) =
            crate::micromanager::native_iac_snapshot(home).map_err(io::Error::other)?;
        Ok(Snapshot {
            exists,
            busy,
            configuration_sha256,
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", test)))]
    {
        let _ = (home, kind);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native host configuration is not yet available",
        ))
    }
}

/// Internal handlers accept only a broker-selected native home; transports must authenticate callers.
pub(super) fn handle(home: &Path, request: Request) -> io::Result<Reply> {
    let target = inspect(home)?;
    regular(home, true)?;
    let kinds: &[&str] = match target.platform {
        firecrab_helper_protocol::native_iac::Platform::Linux => &["host_configuration"],
        _ => &["management_settings"],
    };
    if let Some(resource) = resource(&request) {
        firecrab_helper_protocol::native_iac::validate_resource(resource, &target)
            .map_err(invalid)?;
        if !kinds.contains(&resource.kind.as_str()) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this native installation does not implement the requested facet",
            ));
        }
    }
    if matches!(request, Request::Identity) {
        return Ok(Reply::Identity {
            target,
            kinds: kinds.iter().map(|kind| (*kind).to_owned()).collect(),
        });
    }
    let directory = home.join("iac-native");
    match fs::create_dir(&directory) {
        Ok(()) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
            }
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => regular(&directory, true)?,
        Err(error) => return Err(error),
    }
    let _metadata = lock(&directory.join("ownership.lock"))?;
    let mut journal = read(home, &target)?;
    let reply = match request {
        Request::Identity => unreachable!("identity returns before metadata admission"),
        Request::OwnerReceipt {
            endpoint_id,
            operation_id,
        } => journal
            .outcome(endpoint_id, operation_id)
            .map_err(invalid)?
            .unwrap_or(Reply::OwnerReceipt { receipt: None }),
        Request::ConfirmOwner {
            endpoint_id,
            operation_id,
        } => {
            let receipt = journal
                .confirm(endpoint_id, operation_id)
                .map_err(invalid)?;
            write(home, &journal)?;
            Reply::OwnerReceipt {
                receipt: Some(receipt),
            }
        }
        Request::Observe {
            endpoint_id,
            resource,
        } => {
            let _locks = native_locks(home)?;
            Reply::Observation {
                observation: journal
                    .observe(endpoint_id, &resource, &snapshot(home, &resource.kind)?)
                    .map_err(invalid)?,
            }
        }
        Request::ChangeOwner { request } => {
            let terminal = journal
                .outcome(
                    request.change.expected_endpoint_id,
                    request.change.operation_id,
                )
                .map_err(invalid)?
                .is_some();
            let _locks = if terminal {
                vec![]
            } else {
                native_locks(home)?
            };
            let snapshot = if terminal {
                None
            } else {
                Some(snapshot(home, &request.change.resource.kind)?)
            };
            let reply = journal
                .change(*request, snapshot.as_ref())
                .map_err(invalid)?;
            write(home, &journal)?;
            reply
        }
    };
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use firecrab_helper_protocol::native_iac::{OwnerChange, Platform};
    use firecrab_stack::{ChangeResourceOwner, ManagedResource, Manager, ResourceOwner};
    use uuid::Uuid;

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn actual_native_settings_ownership_fences_legacy_writers_until_confirmed_release() {
        let home = tempfile::tempdir().unwrap();
        let settings = crate::micromanager::native_iac_fixture_settings(home.path()).unwrap();
        let target = inspect(home.path()).unwrap();
        let resource = firecrab_stack::ManagedResource {
            kind: "management_settings".into(),
            id: target.installation_id.to_string(),
        };
        let endpoint = Uuid::new_v4();
        let Reply::Observation { observation } = handle(
            home.path(),
            Request::Observe {
                endpoint_id: endpoint,
                resource: resource.clone(),
            },
        )
        .unwrap() else {
            panic!("expected observation")
        };
        assert!(observation.exists);
        assert!(!observation.busy);
        let mut request = OwnerChange {
            target: target.clone(),
            configuration_sha256: observation.configuration_sha256,
            change: ChangeResourceOwner {
                operation_id: Uuid::new_v4(),
                expected_endpoint_id: endpoint,
                resource,
                expected_generation: 0,
                expected_owner: None,
                owner: Some(ResourceOwner {
                    manager: Manager::Terraform,
                    id: Uuid::new_v4(),
                    key: "firecrab_management_settings.host".into(),
                }),
            },
        };
        let original = handle(
            home.path(),
            Request::ChangeOwner {
                request: Box::new(request.clone()),
            },
        )
        .unwrap();
        assert!(require_unmanaged(home.path()).is_err());
        assert!(crate::micromanager::native_iac_fixture_settings(home.path()).is_err());
        assert!(super::super::installation_guard(home.path()).is_err());
        // Even a malformed later settings file cannot replace the original committed result.
        fs::write(
            home.path().join("settings.json"),
            b"corrupt later configuration",
        )
        .unwrap();
        assert_eq!(
            handle(
                home.path(),
                Request::ChangeOwner {
                    request: Box::new(request.clone())
                }
            )
            .unwrap(),
            original
        );
        fs::write(home.path().join("settings.json"), &settings).unwrap();
        handle(
            home.path(),
            Request::ConfirmOwner {
                endpoint_id: endpoint,
                operation_id: request.change.operation_id,
            },
        )
        .unwrap();
        let Reply::Observation { observation } = handle(
            home.path(),
            Request::Observe {
                endpoint_id: endpoint,
                resource: request.change.resource.clone(),
            },
        )
        .unwrap() else {
            panic!("expected observation")
        };
        request.configuration_sha256 = observation.configuration_sha256;
        request.change.operation_id = Uuid::new_v4();
        request.change.expected_generation = 1;
        request.change.expected_owner = request.change.owner.take();
        handle(
            home.path(),
            Request::ChangeOwner {
                request: Box::new(request.clone()),
            },
        )
        .unwrap();
        assert!(require_unmanaged(home.path()).is_err());
        handle(
            home.path(),
            Request::ConfirmOwner {
                endpoint_id: endpoint,
                operation_id: request.change.operation_id,
            },
        )
        .unwrap();
        require_unmanaged(home.path()).unwrap();
        crate::micromanager::native_iac_fixture_settings(home.path()).unwrap();
        assert_eq!(inspect(home.path()).unwrap(), target);
    }

    #[cfg(unix)]
    #[test]
    fn missing_ledger_cannot_hide_a_redirected_metadata_directory() {
        let home = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), home.path().join("iac-native")).unwrap();
        assert!(require_unmanaged(home.path()).is_err());
        assert!(!elsewhere.path().join("ownership.json").exists());
    }

    #[test]
    fn native_file_receipt_and_release_reservation_survive_reader_restart() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path();
        let private = home.join("iac-native");
        fs::create_dir(&private).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let platform = if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::Macos
        } else {
            Platform::Linux
        };
        let target =
            crate::native_host::build(home, platform, "fixture-native-machine".into()).unwrap();
        let mut journal = Journal::new(target.clone());
        let snapshot = Snapshot {
            exists: true,
            busy: false,
            configuration_sha256: "b".repeat(64),
        };
        let mut request = OwnerChange {
            target: target.clone(),
            configuration_sha256: snapshot.configuration_sha256.clone(),
            change: ChangeResourceOwner {
                operation_id: Uuid::new_v4(),
                expected_endpoint_id: Uuid::from_u128(548),
                resource: ManagedResource {
                    kind: "management_settings".into(),
                    id: target.installation_id.to_string(),
                },
                expected_generation: 0,
                expected_owner: None,
                owner: Some(ResourceOwner {
                    manager: Manager::Terraform,
                    id: Uuid::from_u128(1),
                    key: "firecrab_management_settings.host".into(),
                }),
            },
        };
        let original = journal.change(request.clone(), Some(&snapshot)).unwrap();
        write(home, &journal).unwrap();
        drop(journal);
        let mut journal = read(home, &target).unwrap();
        assert_eq!(
            journal
                .outcome(
                    request.change.expected_endpoint_id,
                    request.change.operation_id
                )
                .unwrap(),
            Some(original)
        );
        assert_eq!(
            writable(home, &target).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        journal
            .confirm(
                request.change.expected_endpoint_id,
                request.change.operation_id,
            )
            .unwrap();
        request.change.operation_id = Uuid::new_v4();
        request.change.expected_generation = 1;
        request.change.expected_owner = request.change.owner.take();
        journal.change(request.clone(), Some(&snapshot)).unwrap();
        write(home, &journal).unwrap();
        assert!(writable(home, &target).is_err());
        journal
            .confirm(
                request.change.expected_endpoint_id,
                request.change.operation_id,
            )
            .unwrap();
        write(home, &journal).unwrap();
        writable(home, &target).unwrap();
        let other = tempfile::tempdir().unwrap();
        let result = handle(
            other.path(),
            Request::Observe {
                endpoint_id: Uuid::from_u128(548),
                resource: ManagedResource {
                    kind: "vm".into(),
                    id: Uuid::from_u128(42).to_string(),
                },
            },
        );
        assert!(result.is_err());
        assert!(!other.path().join("iac-native").exists());
        assert!(require_unmanaged(other.path()).is_ok());
    }
}
