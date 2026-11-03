//! Root-only metadata broker. Requests never choose filesystem or executable paths.

use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use firecrab_helper_protocol::host_iac::{HostIdentity, Reply, Request};
use firecrab_helper_protocol::pipe::Config;

use super::owner_ledger::Ledger;
use crate::service::env::ServiceEnv;

/// IO and protocol rejection without secret configuration contents.
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Bound input and the existing Pipe lock cover lookup, CAS, atomic save and acknowledgement.
pub(super) fn run() -> Result<()> {
    super::host::require_root()?;
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(16 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 {
        return Err("host ownership request exceeds 16 KiB".into());
    }
    let request: Request = serde_json::from_slice(&bytes)?;
    // Address mutations retain this lock through remote work and local commit.
    let _address_lock = super::firecrab::address_lock()?;
    let _lock = super::host::lock()?;
    let root = fs::canonicalize(ServiceEnv::from_process_env().confdir)?;
    let metadata = fs::metadata(&root)?;
    if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err("host configuration root must be a protected root-owned directory".into());
    }
    if root.join("pipe").exists() {
        protected_directory(&root.join("pipe"))?;
    }
    let host = HostIdentity {
        machine_id: fs::read_to_string("/etc/machine-id")?.trim().to_owned(),
        configuration_root: root
            .to_str()
            .ok_or("configuration root must be UTF-8")?
            .into(),
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    let mut ledger = read(&root)?;
    let reply = match request {
        Request::OwnerReceipt {
            endpoint_id,
            operation_id,
        } => Reply::OwnerReceipt {
            receipt: ledger.receipt(&host, endpoint_id, operation_id)?,
        },
        Request::ConfirmOwner {
            endpoint_id,
            operation_id,
        } => {
            let receipt = ledger.confirm(&host, endpoint_id, operation_id)?;
            write(&root, &ledger)?;
            Reply::OwnerReceipt {
                receipt: Some(receipt),
            }
        }
        Request::Observe {
            endpoint_id,
            resource,
        } => {
            if resource.kind == "firecrab_tunnel" {
                ledger.set_addresses(super::firecrab::ownership_projections(&root)?);
            }
            let config = config(&root)?;
            let present = ledger
                .observe(&host, endpoint_id, &resource, config.as_ref())?
                .exists;
            prepare_activity(&mut ledger, &resource, present, config.as_ref())?;
            Reply::Observation {
                observation: ledger.observe(&host, endpoint_id, &resource, config.as_ref())?,
            }
        }
        Request::ChangeOwner { request } => {
            // A retained terminal receipt is readable even if the saved manifest was later removed.
            let terminal = ledger.has_outcome(request.change.operation_id);
            let config = if terminal {
                None
            } else {
                if request.change.resource.kind == "firecrab_tunnel" {
                    ledger.set_addresses(super::firecrab::ownership_projections(&root)?);
                }
                config(&root)?
            };
            if !terminal {
                let present = ledger
                    .observe(
                        &host,
                        request.change.expected_endpoint_id,
                        &request.change.resource,
                        config.as_ref(),
                    )?
                    .exists;
                prepare_activity(
                    &mut ledger,
                    &request.change.resource,
                    present,
                    config.as_ref(),
                )?;
            }
            let reply = ledger.transition(&host, config.as_ref(), *request)?;
            write(&root, &ledger)?;
            reply
        }
    };
    println!("{}", serde_json::to_string(&reply)?);
    Ok(())
}

/// Native controls queue asynchronous systemd jobs; releasing their CLI lock does not finish them.
fn native_busy(unit: &str) -> Result<bool> {
    use crate::shell::{CommandRunner, RealCommandRunner};
    let state = RealCommandRunner.run("systemctl", &["show", unit, "--property=ActiveState"])?;
    let jobs = RealCommandRunner.run(
        "systemctl",
        &["list-jobs", "--no-legend", "--plain", "--no-pager"],
    )?;
    if !state.status.success() || !jobs.status.success() {
        return Err("native ingress activity is unavailable".into());
    }
    let state = String::from_utf8(state.stdout)?;
    let state = state
        .lines()
        .find_map(|line| line.strip_prefix("ActiveState="))
        .ok_or("native ingress activity is invalid")?;
    Ok(!matches!(state, "active" | "inactive" | "failed")
        || String::from_utf8(jobs.stdout)?
            .split_whitespace()
            .any(|field| field == unit))
}

/// Only validated stable IDs select native units; controller ownership never comes from display names.
fn prepare_activity(
    ledger: &mut Ledger,
    resource: &firecrab_stack::ManagedResource,
    present: bool,
    config: Option<&Config>,
) -> Result<()> {
    firecrab_helper_protocol::host_iac::validate_resource(resource)?;
    let unit = if resource.kind == "quick_tunnel" {
        let id = uuid::Uuid::parse_str(&resource.id)?;
        ledger.set_controller_owned(super::firecrab::connector_is_owned(id)?);
        firecrab_helper_protocol::pipe::quick_tunnel_unit(id)
    } else if resource.kind == "firecrab_tunnel" {
        "firecrab-address-updater.service".into()
    } else if resource.kind == "named_tunnel" {
        use sha2::{Digest, Sha256};
        let credentials = config.map(Config::credentials).transpose()?.flatten();
        ledger.set_named_credentials_sha256(
            credentials
                .map(|value| {
                    serde_json::to_vec(&value).map(|bytes| format!("{:x}", Sha256::digest(bytes)))
                })
                .transpose()?,
        );
        super::TUNNEL_UNIT.into()
    } else {
        super::INGRESS_UNIT.into()
    };
    ledger
        .set_runtime_busy(super::firecrab::pipe_has_pending()? || (present && native_busy(&unit)?));
    Ok(())
}

/// Read the original managed manifest; malformed configuration cannot masquerade as absence.
pub(super) fn config(root: &Path) -> Result<Option<Config>> {
    let path = root.join("pipe/manifest.json");
    match fs::metadata(&path) {
        Ok(_) => Config::read(&path).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Absence is unmanaged; invalid or unsafe retained state fails closed for native mutations.
pub(super) fn read(root: &Path) -> Result<Ledger> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(root.join("pipe/ownership.json"));
    let mut file = match file {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Ledger::default()),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    protected_directory(root)?;
    protected_directory(&root.join("pipe"))?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o077 != 0 {
        return Err("host ownership state must be a private root-owned regular file".into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err("host ownership state exceeds its read limit".into());
    }
    let mut ledger: Ledger =
        serde_json::from_slice(&bytes).map_err(|_| "host ownership state is invalid")?;
    ledger.normalize_keys()?;
    Ok(ledger)
}

/// Preserve prior receipts through a synced atomic rename under the Pipe transaction lock.
fn write(root: &Path, ledger: &Ledger) -> Result<()> {
    let directory = root.join("pipe");
    fs::create_dir_all(&directory)?;
    protected_directory(&directory)?;
    let bytes = serde_json::to_vec(ledger)?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err("host ownership state exceeds its retention capacity".into());
    }
    super::host::write_atomic(&directory.join("ownership.json"), Some(&bytes), 0o600)
}

/// A private file cannot retain authority inside a directory writable by an unprivileged process.
fn protected_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err("host ownership directories must be protected and root-owned".into());
    }
    Ok(())
}

/// Check the exact proposed configuration at the privileged commit boundary.
pub(super) fn check_configuration(root: &Path, next: &Config) -> Result<()> {
    let mut ledger = read(root)?;
    ledger.set_addresses(super::firecrab::ownership_projections(root)?);
    let current = config(root)?;
    ledger.check_configuration(current.as_ref(), next)?;
    if ledger.check_named_control().is_err()
        && let Some(current) = current.as_ref()
        && current.credentials()? != next.credentials()?
    {
        return Err(
            "named tunnel credentials are managed; release ownership before replacing them".into(),
        );
    }
    Ok(())
}

/// Shared ingress unit control affects every owned route on that listener.
pub(super) fn check_ingress_control(root: &Path) -> Result<()> {
    read(root)?.check_ingress_control()
}

/// Exact saved connector ownership also gates the legacy shared tunnel service controls.
pub(super) fn check_quick_control(root: &Path, id: uuid::Uuid) -> Result<()> {
    read(root)?.check_resource_control(&firecrab_helper_protocol::host_iac::quick_tunnel_resource(
        id,
    ))
}

/// Check the shared named slot before credentials, remote provisioning or unit mutations.
pub(super) fn check_named_control(root: &Path) -> Result<()> {
    read(root)?.check_named_control()
}

/// The original label stays reserved even after an external private-binding deletion.
pub(super) fn check_address_control(root: &Path, subdomain: &str) -> Result<()> {
    read(root)?.check_resource_control(
        &firecrab_helper_protocol::host_iac::firecrab_tunnel_resource(subdomain),
    )
}
