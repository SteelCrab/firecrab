//! Durable local native-maintenance identity. No HTTP request can supply this authority.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub(super) enum Action {
    Install,
    Reinstall,
    Uninstall,
    Start,
    Stop,
    Restart,
    Enable,
    Disable,
    Update,
    Repair,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Request {
    pub(super) operation_id: Uuid,
    pub(super) action: Action,
    pub(super) input_sha256: String,
}

impl Request {
    pub(super) fn validate(&self) -> Result<(), &'static str> {
        if self.operation_id.is_nil()
            || self.input_sha256.len() != 64
            || self
                .input_sha256
                .bytes()
                .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
        {
            return Err("provide the original native maintenance UUID and input hash");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Entry {
    pub(super) request: Request,
    pub(super) completed: bool,
}

/// This local handshake retains native admission locks while a privileged installer runs.
#[cfg(target_os = "linux")]
#[derive(Debug, clap::Args)]
pub(crate) struct Options {
    #[arg(long)]
    operation_id: Uuid,
    #[arg(long, value_enum)]
    action: Action,
    #[arg(long)]
    input_sha256: String,
}

#[cfg(target_os = "linux")]
pub(crate) fn run(options: Options) -> std::io::Result<()> {
    use std::io::{Read, Write};
    if !nix::unistd::Uid::effective().is_root() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "native service maintenance requires the installation owner root account",
        ));
    }
    let request = Request {
        operation_id: options.operation_id,
        action: options.action,
        input_sha256: options.input_sha256,
    };
    request.validate().map_err(super::invalid)?;
    let home = super::selected_home()?;
    let lease = super::metadata::maintenance_lease(&home, request.clone())?;
    let Some(lease) = lease else {
        println!(
            "{}",
            serde_json::json!({"operationId":request.operation_id,"state":"completed"})
        );
        return Ok(());
    };
    println!(
        "{}",
        serde_json::json!({"operationId":request.operation_id,"state":"ready"})
    );
    std::io::stdout().flush()?;
    let mut input = Vec::new();
    std::io::stdin().take(257).read_to_end(&mut input)?;
    if input != b"complete\n" {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "native maintenance needs explicit completion of its original request",
        ));
    }
    lease.complete()
}

#[cfg(target_os = "linux")]
pub(crate) fn check(options: Options) -> std::io::Result<()> {
    if !nix::unistd::Uid::effective().is_root() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "native maintenance checks require root installation authority",
        ));
    }
    let request = Request {
        operation_id: options.operation_id,
        action: options.action,
        input_sha256: options.input_sha256,
    };
    super::metadata::check_maintenance(&super::selected_home()?, &request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_host::journal::{Journal, Snapshot};
    use firecrab_helper_protocol::native_iac::{Platform, Reply, Target};

    #[test]
    fn interrupted_native_maintenance_stays_busy_after_restart_and_exact_resume() {
        let target = Target {
            platform: Platform::Macos,
            installation_id: Uuid::new_v4(),
            machine_sha256: "a".repeat(64),
            configuration_root: "/fixture/native".into(),
            directory_id: "1:42:1234".into(),
        };
        let mut journal = Journal::new(target.clone());
        let request = Request {
            operation_id: Uuid::new_v4(),
            action: Action::Install,
            input_sha256: "b".repeat(64),
        };
        assert!(journal.begin_maintenance(&request).unwrap());
        let bytes = serde_json::to_vec(&journal).unwrap();
        drop(journal);
        let mut journal: Journal = serde_json::from_slice(&bytes).unwrap();
        journal.check_target(&target).unwrap();
        let resource = firecrab_stack::ManagedResource {
            kind: "management_settings".into(),
            id: target.installation_id.to_string(),
        };
        let snapshot = Snapshot {
            exists: true,
            busy: false,
            configuration_sha256: "c".repeat(64),
        };
        assert!(
            journal
                .observe(Uuid::from_u128(548), &resource, &snapshot)
                .unwrap()
                .busy
        );
        let mut different = request.clone();
        different.input_sha256 = "d".repeat(64);
        assert!(journal.begin_maintenance(&different).is_err());
        different.operation_id = Uuid::new_v4();
        assert!(journal.begin_maintenance(&different).is_err());
        assert!(journal.finish_maintenance(&different).is_err());
        assert!(journal.begin_maintenance(&request).unwrap());
        journal.finish_maintenance(&request).unwrap();
        assert!(!journal.begin_maintenance(&request).unwrap());
        assert!(
            !journal
                .observe(Uuid::from_u128(548), &resource, &snapshot)
                .unwrap()
                .busy
        );
        let owner = firecrab_helper_protocol::native_iac::OwnerChange {
            target,
            configuration_sha256: snapshot.configuration_sha256.clone(),
            change: firecrab_stack::ChangeResourceOwner {
                operation_id: Uuid::new_v4(),
                expected_endpoint_id: Uuid::from_u128(548),
                resource,
                expected_generation: 0,
                expected_owner: None,
                owner: Some(firecrab_stack::ResourceOwner {
                    manager: firecrab_stack::Manager::Terraform,
                    id: Uuid::new_v4(),
                    key: "firecrab_management_settings.host".into(),
                }),
            },
        };
        assert!(matches!(
            journal.change(owner, Some(&snapshot)).unwrap(),
            Reply::OwnerReceipt { receipt: Some(_) }
        ));
        assert!(journal.begin_maintenance(&different).is_err());
    }
}
