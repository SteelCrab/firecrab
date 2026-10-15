//! Repair the resident service without reinstalling binaries or provisioning disks.

use std::fs;
use std::net::IpAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use super::super::{managed_home, report};
use super::{daemon, filesystem, lifecycle::Layout, provision};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    ManagedHome(#[from] managed_home::Error),
    #[error("cannot repair microManager: {0} is missing; run `firecrab service reinstall`")]
    MissingFile(PathBuf),
    #[error("cannot repair microManager: {0} is not executable; run `firecrab service reinstall`")]
    NotExecutable(PathBuf),
    #[error(transparent)]
    Provision(#[from] provision::Error),
    #[error(transparent)]
    Filesystem(#[from] filesystem::Error),
    #[error("microManager repair failed: {0}; inspect `firecrab service debug --logs`")]
    Daemon(#[from] daemon::Error),
    #[error("microManager repair failed: {0}; inspect `firecrab service debug --logs`")]
    Unhealthy(String),
}

pub fn run(layout: &Layout) -> Result<daemon::Status, Error> {
    report!("[REPAIR 1/6] validate installed files and provisioning");
    validate(layout)?;
    let helper = repair_helper(layout);
    filesystem::require_helper(&helper)?;
    let key = layout.managed_home.join("runtime/manager_ed25519");
    report!("[REPAIR 2/6] stop the resident service and management VM");
    daemon::stop(layout)?;
    report!("[REPAIR 3/6] back up, repair the OS filesystem, and restore missing OS files");
    filesystem::repair(layout, &helper)?;
    report!("[REPAIR 4/6] rebuild the launchd plist and daemon wrapper");
    daemon::install(layout, &layout.helper_path(), &key)?;
    report!("[REPAIR 5/6] start the management VM and wait for the API");
    let status = daemon::start(layout)?;
    running_manager(&status)?;
    report!("[REPAIR 6/6] check guest boot report and local API");
    let status = daemon::status(layout)?;
    let ip = running_manager(&status)?;
    // start removes the old report before boot. The guest publishes this
    // through virtiofs only after systemctl verifies its services.
    let path = layout.managed_home.join("runtime/manager-ready");
    managed_home::reject_symlink_components(&path)?;
    let marker = fs::read_to_string(&path)
        .map_err(|error| Error::Unhealthy(format!("cannot read guest boot report: {error}")))?;
    verify_guest_boot_report(&marker, ip)?;
    Ok(status)
}

fn verify_guest_boot_report(marker: &str, ip: IpAddr) -> Result<(), Error> {
    for (name, expected) in [
        ("schema", provision::PROVISION_SCHEMA.to_string()),
        ("ip", ip.to_string()),
        ("firecrab_api", "active".to_string()),
        ("firecrab_net_helper", "active".to_string()),
    ] {
        let mut values = marker.lines().filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key == name).then_some(value)
        });
        if values.next() != Some(expected.as_str()) || values.next().is_some() {
            return Err(Error::Unhealthy(format!(
                "guest boot report: {name} must be {expected}"
            )));
        }
    }
    Ok(())
}

fn repair_helper(layout: &Layout) -> PathBuf {
    let candidate = super::helper_path(
        std::env::var_os(super::HELPER_ENV),
        std::env::current_exe().ok().as_deref(),
    );
    if candidate.is_file() {
        candidate
    } else {
        layout.helper_path()
    }
}

/// Reject an incomplete installation before stopping a currently running service.
pub(super) fn validate(layout: &Layout) -> Result<(), Error> {
    managed_home::reject_symlink_components(&layout.managed_home)?;
    for path in [
        layout.cli_path(),
        layout.helper_path(),
        layout.managed_home.join("system/debian-system.raw"),
        layout.managed_home.join("data/firecrab-data.raw"),
        layout.managed_home.join("runtime/manager_ed25519"),
    ] {
        managed_home::reject_symlink_components(&path)?;
        if !path.is_file() {
            return Err(Error::MissingFile(path));
        }
    }
    for path in [layout.cli_path(), layout.helper_path()] {
        if !fs::metadata(&path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0) {
            return Err(Error::NotExecutable(path));
        }
    }
    provision::require_guest_provisioned(&layout.managed_home)?;
    Ok(())
}

fn running_manager(status: &daemon::Status) -> Result<IpAddr, Error> {
    if !status.success() {
        return Err(Error::Unhealthy(format!(
            "launchd loaded={}, ready={}, API reachable={}",
            status.loaded, status.ready, status.api_reachable
        )));
    }
    status
        .detail
        .as_deref()
        .and_then(super::manager_ip)
        .ok_or_else(|| Error::Unhealthy("management VM address is missing".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn installed(root: &std::path::Path) -> Layout {
        let layout = Layout {
            install_dir: root.join("bin"),
            managed_home: root.join("micromanager"),
        };
        for path in [
            layout.cli_path(),
            layout.helper_path(),
            layout.managed_home.join("system/debian-system.raw"),
            layout.managed_home.join("data/firecrab-data.raw"),
            layout.managed_home.join("runtime/manager_ed25519"),
        ] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"preserved").unwrap();
        }
        for path in [layout.cli_path(), layout.helper_path()] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let runtime = layout.managed_home.join("runtime");
        fs::write(
            runtime.join("provisioned"),
            "schema=5\nkvm=usable\nfirecrab_api=active\nfirecrab_net_helper=active\nnested_firecracker=passed\n",
        ).unwrap();
        let mut kernel = [0_u8; 64];
        kernel[56..60].copy_from_slice(b"ARM\x64");
        fs::write(layout.managed_home.join("system/Image"), kernel).unwrap();
        fs::write(layout.managed_home.join("system/initrd.img"), b"initrd").unwrap();
        layout
    }

    #[test]
    fn repair_accepts_lost_registration_and_known_hosts_without_recreating_disks() {
        let root = tempfile::tempdir().unwrap();
        let layout = installed(root.path());
        validate(&layout).unwrap();
        for file in [
            "system/debian-system.raw",
            "data/firecrab-data.raw",
            "runtime/manager_ed25519",
        ] {
            assert_eq!(
                fs::read(layout.managed_home.join(file)).unwrap(),
                b"preserved"
            );
        }
        assert!(!layout.managed_home.join("runtime/known_hosts").exists());
        assert!(!layout.managed_home.join("downloads").exists());
    }

    #[test]
    fn incomplete_or_symlinked_installation_is_rejected_before_repair() {
        let root = tempfile::tempdir().unwrap();
        let layout = installed(root.path());
        let data = layout.managed_home.join("data/firecrab-data.raw");
        fs::remove_file(&data).unwrap();
        assert!(matches!(run(&layout), Err(Error::MissingFile(path)) if path == data));
        let external = root.path().join("external");
        fs::write(&external, b"keep me").unwrap();
        symlink(&external, &data).unwrap();
        assert!(matches!(run(&layout), Err(Error::ManagedHome(_))));
        assert_eq!(fs::read(external).unwrap(), b"keep me");
    }

    #[test]
    fn non_executable_helper_and_incomplete_provisioning_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let layout = installed(root.path());
        fs::set_permissions(layout.helper_path(), fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(run(&layout), Err(Error::NotExecutable(_))));
        fs::set_permissions(layout.helper_path(), fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_file(layout.managed_home.join("system/Image")).unwrap();
        assert!(matches!(run(&layout), Err(Error::Provision(_))));
    }

    #[test]
    fn guest_boot_report_requires_matching_live_address_and_active_services() {
        let ip = "192.0.2.7".parse().unwrap();
        let valid = "schema=5\nip=192.0.2.7\nfirecrab_api=active\nfirecrab_net_helper=active\n";
        verify_guest_boot_report(valid, ip).unwrap();
        for invalid in [
            valid.replace("schema=5", "schema=0"),
            valid.replace("192.0.2.7", "192.0.2.8"),
            valid.replace("firecrab_api=active", "firecrab_api=failed"),
            valid.replace("firecrab_net_helper=active\n", ""),
            format!("{valid}firecrab_api=active\n"),
            String::new(),
        ] {
            assert!(verify_guest_boot_report(&invalid, ip).is_err(), "{invalid}");
        }
    }

    #[test]
    fn a_cached_ready_marker_cannot_hide_an_unhealthy_service() {
        let mut status = daemon::Status {
            loaded: true,
            ready: true,
            api_reachable: false,
            detail: Some("ip=192.0.2.7\n".to_string()),
        };
        assert!(running_manager(&status).is_err());
        status.api_reachable = true;
        status.loaded = false;
        assert!(running_manager(&status).is_err());
        status.loaded = true;
        status.ready = false;
        assert!(running_manager(&status).is_err());
        status.ready = true;
        assert_eq!(
            running_manager(&status).unwrap(),
            "192.0.2.7".parse::<IpAddr>().unwrap()
        );
        status.detail = Some("ip=invalid\n".to_string());
        assert!(running_manager(&status).is_err());
    }
}
