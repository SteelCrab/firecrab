//! AI Sandbox is a client of the shared REST lifecycle, with no local provider credentials.
use crate::api_client::ApiClient;
use clap::Subcommand;
use firecrab_api_types::*;
use std::io::{self, Read};
use uuid::Uuid;

/// Thin commands over the shared Sandbox REST lifecycle.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// List saved Sandbox projects as JSON.
    Projects,
    /// Save explicit settings from JSON on stdin. Source paths refer to the API host.
    SaveProject { id: Uuid },
    /// Inspect a project's durable conversations and code attempts.
    Tasks { project: Uuid },
    /// Start from SandboxTaskInput JSON on stdin. Reuse the same id after an uncertain response.
    Run { project: Uuid, id: Uuid },
    /// Inspect state, actual tool output and reviewable changes.
    Inspect { id: Uuid },
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
        Command::Projects => {
            serde_json::to_value(client.get::<Vec<SandboxProject>>(&format!("{base}/projects"))?)?
        }
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

#[cfg(test)]
#[path = "ai_sandbox_tests.rs"]
mod tests;
