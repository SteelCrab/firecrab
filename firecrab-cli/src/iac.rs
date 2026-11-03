//! Exact, retryable ownership metadata using the normal authenticated API client.

use std::{io::Read, path::PathBuf};

use clap::Subcommand;
use firecrab_stack::{
    ChangeResourceOwner, ManagedResource, NativeOperation, OwnershipOperation, ResourceOwnership,
};
use uuid::Uuid;

use crate::api_client::{ApiClient, ApiError};

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Read the physical identity of this CLI's native installation without changing it.
    #[command(hide = true)]
    NativeIdentity,
    /// Internal native metadata broker; caller must own the selected native installation.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[command(hide = true)]
    NativeBroker,
    /// Serve native ownership over authenticated Firecrab mTLS.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    NativeServe(crate::native_host::server::Options),
    /// Read persistent endpoint identity and implemented ownership capabilities.
    Identity,
    /// List active Stack and Terraform ownership records as JSON.
    Owners,
    /// Observe an exact ownership generation; does not assert resource existence.
    Owner {
        #[arg(long)]
        kind: String,
        /// Exact resource identity, never an implicit display-name adoption.
        #[arg(long)]
        id: String,
    },
    /// Adopt, transfer or release ownership with an explicit versioned request.
    ChangeOwner {
        /// JSON request with endpoint, operation UUID, expected owner and generation.
        /// Retain the file unchanged when retrying a lost acknowledgement.
        #[arg(long)]
        file: PathBuf,
    },
    /// Recover an immutable ownership-change acknowledgement after disconnection.
    OwnerChange {
        #[arg(long)]
        id: Uuid,
    },
    /// Observe a durable native backend operation without replaying it.
    NativeOperation {
        #[arg(long)]
        id: Uuid,
    },
    /// Find operation IDs after losing a native resource request acknowledgement.
    NativeOperations {
        #[arg(long)]
        kind: String,
        #[arg(long)]
        id: String,
    },
    /// Explicitly recover the same typed native operation after an uncertain outcome.
    RecoverNativeOperation {
        #[arg(long)]
        id: Uuid,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not observe the native installation: {0}")]
    Native(#[from] std::io::Error),
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error("{0}")]
    Input(&'static str),
}

impl Command {
    pub fn is_local(&self) -> bool {
        match self {
            Self::NativeIdentity => true,
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Self::NativeBroker | Self::NativeServe(_) => true,
            _ => false,
        }
    }
}

/// Local native administration does not depend on a selected remote API or TLS profile.
pub fn run_local(command: Command) -> Result<(), Error> {
    match command {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        Command::NativeBroker => {
            crate::native_host::run_metadata()?;
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        Command::NativeServe(options) => crate::native_host::server::run(options)?,
        Command::NativeIdentity => {
            #[cfg(target_os = "linux")]
            let root = crate::service::env::ServiceEnv::from_process_env().confdir;
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            let root = crate::micromanager::native_iac_home().map_err(std::io::Error::other)?;
            print_json(&crate::native_host::inspect(&root)?)
        }
        _ => return Err(Error::Input("expected a local native command")),
    }
    Ok(())
}

pub fn run(client: &ApiClient, command: Command) -> Result<(), Error> {
    match command {
        Command::NativeIdentity => run_local(Command::NativeIdentity)?,
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        Command::NativeBroker => run_local(Command::NativeBroker)?,
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        Command::NativeServe(options) => run_local(Command::NativeServe(options))?,
        Command::NativeOperation { id } | Command::RecoverNativeOperation { id } if id.is_nil() => {
            return Err(Error::Input("operation UUID must be non-nil"));
        }
        Command::Identity => print_json(&client.get::<serde_json::Value>("/api/iac/identity")?),
        Command::Owners => print_json(&client.get::<Vec<ResourceOwnership>>("/api/iac/owners")?),
        Command::Owner { kind, id } => {
            let mut resource = ManagedResource { kind, id };
            resource.normalize().map_err(Error::Input)?;
            let mut url =
                reqwest::Url::parse("http://localhost/api/iac/owners/").expect("static URL");
            url.path_segments_mut()
                .expect("hierarchical URL")
                .pop_if_empty()
                .push(&resource.kind)
                .push(&resource.id);
            print_json(&client.get::<ResourceOwnership>(url.path())?)
        }
        Command::ChangeOwner { file } => {
            let mut bytes = Vec::new();
            std::fs::File::open(file)
                .map_err(|_| Error::Input("could not open ownership request file"))?
                .take(16 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| Error::Input("could not read ownership request file"))?;
            if bytes.len() > 16 * 1024 {
                return Err(Error::Input("ownership request exceeds 16 KiB"));
            }
            let mut request: ChangeResourceOwner =
                serde_json::from_slice(&bytes).map_err(|_| {
                    Error::Input("ownership request must be valid JSON without unknown fields")
                })?;
            request.validate().map_err(Error::Input)?;
            print_json(
                &client.post::<_, ResourceOwnership>("/api/iac/owners/transition", &request)?,
            )
        }
        Command::OwnerChange { id } => {
            if id.is_nil() {
                return Err(Error::Input("operation UUID must be non-nil"));
            }
            print_json(&client.get::<OwnershipOperation>(&format!("/api/iac/owner-changes/{id}"))?)
        }
        Command::NativeOperation { id } => {
            print_json(&client.get::<NativeOperation>(&format!("/api/iac/native-operations/{id}"))?)
        }
        Command::NativeOperations { kind, id } => {
            let mut resource = ManagedResource { kind, id };
            resource.normalize().map_err(Error::Input)?;
            let mut url =
                reqwest::Url::parse("http://localhost/api/iac/native-operations/resources/")
                    .expect("static URL");
            url.path_segments_mut()
                .expect("hierarchical URL")
                .pop_if_empty()
                .push(&resource.kind)
                .push(&resource.id);
            print_json(&client.get::<Vec<NativeOperation>>(url.path())?)
        }
        Command::RecoverNativeOperation { id } => {
            let observed =
                client.get::<NativeOperation>(&format!("/api/iac/native-operations/{id}"))?;
            if observed.operation_id != id {
                return Err(Error::Input("native operation identity changed"));
            }
            if matches!(
                observed.state,
                firecrab_stack::NativeOperationState::Succeeded
                    | firecrab_stack::NativeOperationState::Failed
            ) {
                print_json(&observed);
            } else {
                print_json(&client.recover_native_operation(&observed)?);
            }
        }
    };
    Ok(())
}

fn print_json(value: &impl serde::Serialize) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("ownership serializes")
    );
}
