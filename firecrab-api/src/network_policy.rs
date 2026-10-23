//! Resolve saved selections. Missing or corrupt policies never mean legacy access.

use crate::{error::AppError, model::VmRecord, persistence::Store, state::AppState};
pub use firecrab_api_types::EgressPolicy;
use firecrab_helper_protocol::security_group::{SecurityGroupPolicySpec, SecurityGroupRuleSpec};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use uuid::Uuid;

/// Preserve an empty group as `Some`, so it still denies new connections.
pub(crate) fn resolve(
    store: &Store,
    id: Option<Uuid>,
    network: Option<Uuid>,
) -> Result<Option<SecurityGroupPolicySpec>, String> {
    let Some(id) = id else {
        return Ok(None);
    };
    let group = store
        .security_group(id)
        .map_err(|e| e.to_string())?
        .ok_or("security group does not exist")?;
    if network.is_some_and(|network| network != group.micro_network_id) {
        return Err("security group belongs to another MicroNetwork".into());
    }
    let network = store
        .micro_network(group.micro_network_id)
        .map_err(|e| e.to_string())?
        .ok_or("security group network does not exist")?;
    if network.ipv6_cidr.is_some() {
        return Err("security groups require an IPv4-only MicroNetwork".into());
    }
    let policy = SecurityGroupPolicySpec {
        id,
        micro_network_id: group.micro_network_id,
        rules: group
            .rules
            .iter()
            .map(SecurityGroupRuleSpec::from_api)
            .collect::<Result<_, _>>()?,
    };
    policy.validate()?;
    Ok(Some(policy))
}

pub(crate) fn validate_selection(
    state: &AppState,
    id: Option<Uuid>,
    network: Option<Uuid>,
    request: Uuid,
) -> Result<(), AppError> {
    resolve(&state.store, id, network)
        .map(|_| ())
        .map_err(|message| {
            AppError::validation(
                BTreeMap::from([("securityGroupId".into(), message)]),
                request,
            )
        })
}

/// Probe the shim's held lock rather than relying on a DB state or stale socket.
pub(crate) fn vm_is_active(state: &AppState, vm: &VmRecord) -> Result<bool, std::io::Error> {
    if !vm.state.can_edit_resources()
        || state
            .processes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(&vm.id)
    {
        return Ok(true);
    }
    let path =
        crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id)
            .dir
            .join("vm.lock");
    let file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    // SAFETY: this file owns the descriptor; drop releases a successful lock.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(false);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Ok(true)
    } else {
        Err(error)
    }
}
