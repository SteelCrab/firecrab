//! AI Sandbox is a client of the shared REST lifecycle, with no local provider credentials.
use crate::api_client::ApiClient;
use clap::Subcommand;
use firecrab_api_types::*;
use std::io::{self, Read};
use std::time::{Duration, Instant};
use uuid::Uuid;
mod resources;
pub use resources::{EnvironmentCommand, TunnelCommand};

/// Thin commands over the shared Sandbox REST lifecycle.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Inspect a host's Sandbox resource grants (controller operator only).
    GetAccess { principal: Uuid },
    /// Replace grants from SandboxAccessPolicy JSON on stdin (operator only).
    SetAccess { principal: Uuid },
    /// Revoke all Sandbox grants for a host (operator only).
    RevokeAccess { principal: Uuid },
    /// List saved Sandbox projects as JSON.
    Projects,
    /// Inspect one project's context, environment and defaults.
    Project { id: Uuid },
    /// Delete project metadata only after active work and previews are settled.
    DeleteProject { id: Uuid },
    /// Export one retained artifact to a new local file, checking its size and SHA-256.
    Artifact {
        id: Uuid,
        artifact: Uuid,
        output: std::path::PathBuf,
    },
    /// Inspect and signal only the admitted task's owned guest processes.
    Guest {
        project: Uuid,
        id: Uuid,
        #[command(subcommand)]
        command: resources::GuestCommand,
    },
    /// Manage owned environments through the same REST contract.
    Environment {
        #[command(subcommand)]
        command: EnvironmentCommand,
    },
    /// Bind a saved Pipe address to a ready task application.
    Tunnel {
        #[command(subcommand)]
        command: TunnelCommand,
    },
    /// Save explicit settings from JSON on stdin. Source paths refer to the API host.
    SaveProject { id: Uuid },
    /// Inspect a project's durable conversations and code attempts.
    Tasks { project: Uuid },
    /// Start from SandboxTaskInput JSON on stdin. Reuse the same id after an uncertain response.
    Run { project: Uuid, id: Uuid },
    /// Inspect state, actual tool output and reviewable changes.
    Inspect { id: Uuid },
    /// Wait 1–60 seconds for a final result; interruption requires explicit reconciliation.
    Wait {
        id: Uuid,
        #[arg(long, default_value_t = 30)]
        timeout_seconds: u64,
    },
    /// Request cancellation; inspect until termination is confirmed.
    Cancel { id: Uuid },
    /// Recover an interrupted attempt without rerunning its commands.
    Reconcile { id: Uuid },
    /// Apply only named, reviewed files. Source conflicts stop the operation.
    Apply {
        id: Uuid,
        #[arg(required = true)]
        paths: Vec<String>,
    },
    /// Read model permissions or discover a provider catalogue.
    Models {
        connection: Uuid,
        #[arg(long)]
        discover: bool,
    },
    /// Verify actual inference access, optionally including a harmless native tool round trip.
    Verify {
        connection: Uuid,
        model: String,
        #[arg(long)]
        tools: bool,
    },
}
/// Read bounded typed JSON without placing prompts in process arguments.
fn read_input<T: serde::de::DeserializeOwned>(
    input: impl Read,
) -> Result<T, Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    input.take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err("Sandbox input exceeds 64 KiB".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Invalid Sandbox request JSON".into())
}
/// Execute one selected host operation and print its secret-free JSON result.
pub fn run(client: &ApiClient, command: Command) -> Result<(), Box<dyn std::error::Error>> {
    let result = request(client, command, io::stdin().lock())?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn request(
    client: &ApiClient,
    command: Command,
    input: impl Read,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let base = "/api/ai-sandbox";
    let result = match command {
        Command::GetAccess { principal } => serde_json::to_value(
            client.get::<SandboxAccessPolicy>(&format!("{base}/access/{principal}"))?,
        )?,
        Command::SetAccess { principal } => {
            serde_json::to_value(client.put::<_, SandboxAccessPolicy>(
                &format!("{base}/access/{principal}"),
                &read_input::<SandboxAccessPolicy>(input)?,
            )?)?
        }
        Command::RevokeAccess { principal } => serde_json::to_value(
            client.delete_json::<SandboxAccessPolicy>(&format!("{base}/access/{principal}"))?,
        )?,
        Command::Projects => {
            serde_json::to_value(client.get::<Vec<SandboxProject>>(&format!("{base}/projects"))?)?
        }
        Command::Project { id } => {
            serde_json::to_value(client.get::<SandboxProject>(&format!("{base}/projects/{id}"))?)?
        }
        Command::DeleteProject { id } => {
            client.delete(&format!("{base}/projects/{id}"))?;
            serde_json::json!({"deleted":id})
        }
        Command::Artifact {
            id,
            artifact,
            output,
        } => {
            use std::io::Write;
            let (metadata, bytes) = artifact_bytes(client, id, artifact, 16 * 1024 * 1024)?;
            let parent = output
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(std::path::Path::new("."));
            let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
            temporary.write_all(&bytes)?;
            temporary.as_file().sync_all()?;
            temporary.persist_noclobber(&output)?;
            serde_json::to_value(metadata)?
        }
        Command::Guest {
            project,
            id,
            command,
        } => resources::guest(client, project, id, command, input)?,
        Command::Environment { command } => resources::environment(client, command, input)?,
        Command::Tunnel { command } => resources::tunnel(client, command, input)?,
        Command::Wait {
            id,
            timeout_seconds,
        } => serde_json::to_value(wait_for_task(client, id, timeout_seconds)?)?,
        Command::SaveProject { id } => serde_json::to_value(client.put::<_, SandboxProject>(
            &format!("{base}/projects/{id}"),
            &read_input::<SandboxProjectSpec>(input)?,
        )?)?,
        Command::Tasks { project } => serde_json::to_value(
            client.get::<Vec<SandboxTaskSummary>>(&format!("{base}/projects/{project}/tasks"))?,
        )?,
        Command::Run { project, id } => serde_json::to_value(client.put::<_, SandboxTask>(
            &format!("{base}/projects/{project}/tasks/{id}"),
            &read_input::<SandboxTaskInput>(input)?,
        )?)?,
        Command::Inspect { id } => {
            serde_json::to_value(client.get::<SandboxTask>(&format!("{base}/tasks/{id}"))?)?
        }
        Command::Cancel { id } => serde_json::to_value(client.post::<_, SandboxTask>(
            &format!("{base}/tasks/{id}/cancel"),
            &serde_json::json!({}),
        )?)?,
        Command::Reconcile { id } => serde_json::to_value(client.post::<_, SandboxTask>(
            &format!("{base}/tasks/{id}/reconcile"),
            &serde_json::json!({}),
        )?)?,
        Command::Apply { id, paths } => serde_json::to_value(client.post::<_, SandboxTask>(
            &format!("{base}/tasks/{id}/apply"),
            &SandboxApply { paths },
        )?)?,
        Command::Models {
            connection,
            discover,
        } => serde_json::to_value(client.get::<SandboxModelCatalog>(&format!(
            "{base}/connections/{connection}/models?discover={discover}"
        ))?)?,
        Command::Verify {
            connection,
            model,
            tools,
        } => serde_json::to_value(client.post::<_, SandboxModel>(
            &format!("{base}/connections/{connection}/verify"),
            &SandboxVerifyModel { model, tools },
        )?)?,
    };
    Ok(result)
}

pub(crate) fn artifact_bytes(
    client: &ApiClient,
    task: Uuid,
    artifact: Uuid,
    limit: usize,
) -> Result<(SandboxArtifact, Vec<u8>), Box<dyn std::error::Error>> {
    use sha2::{Digest, Sha256};
    let task: SandboxTask = client.get(&format!("/api/ai-sandbox/tasks/{task}"))?;
    let metadata = task
        .artifacts
        .iter()
        .find(|a| a.id == artifact && a.task_id == task.id && a.attempt_id == task.attempt_id)
        .ok_or("Artifact does not belong to this task attempt")?
        .clone();
    if metadata.size > limit {
        return Err("Artifact exceeds this client's download bound".into());
    }
    let bytes = client.get_bytes_bounded(
        &format!("/api/ai-sandbox/tasks/{}/artifacts/{artifact}", task.id),
        limit,
    )?;
    if bytes.len() != metadata.size || format!("{:x}", Sha256::digest(&bytes)) != metadata.sha256 {
        return Err("Artifact size or SHA-256 did not match the retained result".into());
    }
    Ok((metadata, bytes))
}

pub(crate) fn wait_for_task(
    client: &ApiClient,
    id: Uuid,
    timeout_seconds: u64,
) -> Result<SandboxTaskWaitResult, Box<dyn std::error::Error>> {
    if !(1..=60).contains(&timeout_seconds) {
        return Err("timeoutSeconds must be between 1 and 60".into());
    }
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    let mut last = None;
    loop {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1));
        let task: SandboxTask =
            match client.get_with_timeout(&format!("/api/ai-sandbox/tasks/{id}"), remaining) {
                Ok(task) => task,
                Err(_) if Instant::now() >= deadline && last.is_some() => {
                    return Ok(SandboxTaskWaitResult {
                        reached: false,
                        task: last.unwrap(),
                    });
                }
                Err(error) => return Err(error.into()),
            };
        let reached = matches!(
            task.state,
            SandboxTaskState::Result
                | SandboxTaskState::Merged
                | SandboxTaskState::Failed
                | SandboxTaskState::Cancelled
        );
        if reached || task.state == SandboxTaskState::Interrupted || Instant::now() >= deadline {
            return Ok(SandboxTaskWaitResult { reached, task });
        }
        last = Some(task);
        std::thread::sleep(
            Duration::from_millis(250).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

#[cfg(test)]
#[path = "ai_sandbox_tests.rs"]
mod tests;
