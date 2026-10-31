//! Git source configuration, observation and explicit synchronization commands.

use firecrab_stack::{
    ConfigureGitSource, DeleteGitSource, GitCheck, GitSource, GitSourceGeneration, GitSourceStatus,
    SyncGitSource,
};
use std::path::PathBuf;

use super::{Error, name_valid, print_json, read_manifest};
use crate::api_client::ApiClient;

/// Source metadata is configured separately from the resource manifest.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Configure a GitHub source. Automatic mode permits later polls to apply resources.
    Configure {
        /// Existing registered stack name.
        name: String,
        /// JSON source configuration, without token values.
        #[arg(long)]
        file: PathBuf,
        /// Current generation; use zero for the first configuration.
        #[arg(long)]
        expected_generation: u64,
    },
    /// Read source configuration, next poll time and latest redacted observation.
    Status { name: String },
    /// Read current or removed-source generation before configuring it again.
    Generation { name: String },
    /// Disconnect exact Git configuration without deleting bound infrastructure.
    Remove {
        name: String,
        #[arg(long)]
        file: PathBuf,
    },
    /// Accept a read-only check. Poll source status for its result.
    Check {
        name: String,
        /// Current source generation.
        #[arg(long)]
        generation: u64,
    },
    /// Resolve Git and apply supported changes, also while polling is paused.
    Sync {
        name: String,
        #[arg(long)]
        generation: u64,
        /// Acknowledge an inspected failed/interrupted operation before fresh recovery.
        #[arg(long)]
        recover_operation_id: Option<uuid::Uuid>,
    },
    /// Read the latest fifty observations, newest first.
    History { name: String },
    /// Stop periodic checks. Wait for any active check before changing configuration.
    Pause {
        name: String,
        #[arg(long)]
        generation: u64,
    },
    /// Resume periodic checks and automatic synchronization if configured.
    Resume {
        name: String,
        #[arg(long)]
        generation: u64,
    },
}

pub(super) fn run(client: &ApiClient, command: Command) -> Result<(), Error> {
    match command {
        Command::Configure {
            name,
            file,
            expected_generation,
        } => {
            name_valid(&name)?;
            let raw = read_manifest(&file).map_err(super::diagnostics)?;
            let source: GitSource = serde_json::from_str(&raw).map_err(|_| Error::Validation("source file must contain one valid JSON source configuration without unknown fields".into()))?;
            source.validate().map_err(|e| Error::Validation(e.into()))?;
            let status: GitSourceStatus = client.put(
                &format!("/api/stacks/{name}/source"),
                &ConfigureGitSource {
                    expected_generation,
                    source,
                },
            )?;
            print_json(&status);
        }
        Command::Status { name } => {
            name_valid(&name)?;
            let status: GitSourceStatus = client.get(&format!("/api/stacks/{name}/source"))?;
            print_json(&status);
        }
        Command::Generation { name } => {
            name_valid(&name)?;
            print_json(
                &client
                    .get::<GitSourceGeneration>(&format!("/api/stacks/{name}/source/generation"))?,
            );
        }
        Command::Remove { name, file } => {
            name_valid(&name)?;
            let raw = read_manifest(&file).map_err(super::diagnostics)?;
            let request:DeleteGitSource=serde_json::from_str(&raw).map_err(|_|Error::Validation("source removal file must contain one versioned JSON request without unknown fields".into()))?;
            let result: firecrab_stack::NativeOperation =
                client.delete_with_body(&format!("/api/stacks/{name}/source"), &request)?;
            print_json(&result);
        }
        Command::Check { name, generation } => {
            name_valid(&name)?;
            let status: GitSourceStatus = client.post(
                &format!("/api/stacks/{name}/source/check"),
                &GitSourceGeneration { generation },
            )?;
            print_json(&status);
        }
        Command::Sync {
            name,
            generation,
            recover_operation_id,
        } => {
            name_valid(&name)?;
            let status: GitSourceStatus = client.post(
                &format!("/api/stacks/{name}/source/sync"),
                &SyncGitSource {
                    generation,
                    recover_operation_id,
                },
            )?;
            print_json(&status);
        }
        Command::History { name } => {
            name_valid(&name)?;
            let history: Vec<GitCheck> =
                client.get(&format!("/api/stacks/{name}/source/history"))?;
            print_json(&history);
        }
        Command::Pause { name, generation } => pause(client, name, generation, true)?,
        Command::Resume { name, generation } => pause(client, name, generation, false)?,
    }
    Ok(())
}

fn pause(client: &ApiClient, name: String, generation: u64, paused: bool) -> Result<(), Error> {
    name_valid(&name)?;
    let mut status: GitSourceStatus = client.get(&format!("/api/stacks/{name}/source"))?;
    if generation != status.generation {
        return Err(Error::Validation(
            "source generation changed; read source status again".into(),
        ));
    }
    status.source.paused = paused;
    let status: GitSourceStatus = client.put(
        &format!("/api/stacks/{name}/source"),
        &ConfigureGitSource {
            expected_generation: generation,
            source: status.source,
        },
    )?;
    print_json(&status);
    Ok(())
}
