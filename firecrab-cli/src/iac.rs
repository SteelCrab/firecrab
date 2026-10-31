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
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error("{0}")]
    Input(&'static str),
}

pub fn run(client: &ApiClient, command: Command) -> Result<(), Error> {
    match command {
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
        Command::RecoverNativeOperation { id } => print_json(&client.post::<_, NativeOperation>(
            &format!("/api/iac/native-operations/{id}/recover"),
            &serde_json::json!({}),
        )?),
    };
    Ok(())
}

fn print_json(value: &impl serde::Serialize) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("ownership serializes")
    );
}
