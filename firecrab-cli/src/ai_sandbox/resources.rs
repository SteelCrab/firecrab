//! Typed adapters. Environment cleanup and Tunnel changes remain separate operations.
use super::*;

#[derive(Debug, Subcommand)]
pub enum EnvironmentCommand {
    /// Discover immutable template pins, including boot-arguments SHA-256.
    Images {
        project: Uuid,
    },
    List {
        project: Uuid,
    },
    /// Save an immutable SandboxEnvironmentSpec from stdin; reuse id after uncertainty.
    Create {
        project: Uuid,
        id: Uuid,
    },
    Inspect {
        project: Uuid,
        id: Uuid,
    },
    Reconcile {
        project: Uuid,
        id: Uuid,
    },
    Stop {
        project: Uuid,
        id: Uuid,
    },
    Resume {
        project: Uuid,
        id: Uuid,
    },
    Export {
        project: Uuid,
        id: Uuid,
    },
    /// Delete only owned resources after export; retained Workspace disks need authorization.
    Delete {
        project: Uuid,
        id: Uuid,
        #[arg(long)]
        authorize_workspace_deletion: bool,
    },
}
#[derive(Debug, Subcommand)]
pub enum GuestCommand {
    Health,
    Processes,
    Signal,
}
pub(super) fn guest(
    client: &ApiClient,
    project: Uuid,
    id: Uuid,
    command: GuestCommand,
    input: impl Read,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let base = format!("/api/ai-sandbox/projects/{project}/tasks/{id}/guest");
    Ok(match command {
        GuestCommand::Health => serde_json::to_value(
            client.get::<SandboxGuestCapabilities>(&format!("{base}/health"))?,
        )?,
        GuestCommand::Processes => serde_json::to_value(
            client.get::<Vec<SandboxGuestProcess>>(&format!("{base}/processes"))?,
        )?,
        GuestCommand::Signal => {
            client.post_no_content(
                &format!("{base}/signal"),
                &read_input::<SandboxGuestSignalRequest>(input)?,
            )?;
            serde_json::json!({"sent":true,"terminationConfirmed":false})
        }
    })
}
#[derive(Debug, Subcommand)]
pub enum TunnelCommand {
    /// Operator inventory retains retirement warnings after project deletion.
    Inventory,
    /// Retarget the same URL to a new task/runtime using SandboxTunnelRetarget JSON.
    Retarget {
        id: Uuid,
    },
    /// List saved Pipe addresses without reading their tokens.
    Addresses,
    List {
        project: Uuid,
    },
    /// Save SandboxTunnelSpec JSON from stdin. Registers no credentials.
    Save {
        project: Uuid,
        id: Uuid,
    },
    Inspect {
        id: Uuid,
    },
    Publish {
        id: Uuid,
    },
    Reconcile {
        id: Uuid,
    },
    /// Stop the exclusive connector while preserving the saved token and fixed URL.
    Stop {
        id: Uuid,
    },
    /// Remove local binding/connection; remote dashboard retirement remains explicit.
    Delete {
        id: Uuid,
    },
    /// Check actual public HTTPS content or WebSocket upgrade using JSON from stdin.
    Verify {
        id: Uuid,
    },
}

pub(super) fn environment(
    client: &ApiClient,
    command: EnvironmentCommand,
    input: impl Read,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let base = "/api/ai-sandbox/projects";
    let value =
        match command {
            EnvironmentCommand::Images { project } => {
                serde_json::to_value(client.get::<Vec<SandboxTemplatePin>>(&format!(
                    "{base}/{project}/environment-images"
                ))?)?
            }
            EnvironmentCommand::List { project } => serde_json::to_value(client.get::<Vec<
                SandboxEnvironment,
            >>(
                &format!("{base}/{project}/environments"),
            )?)?,
            EnvironmentCommand::Create { project, id } => {
                serde_json::to_value(client.put::<_, SandboxEnvironment>(
                    &format!("{base}/{project}/environments/{id}"),
                    &read_input::<SandboxEnvironmentSpec>(input)?,
                )?)?
            }
            EnvironmentCommand::Inspect { project, id } => serde_json::to_value(
                client.get::<SandboxEnvironment>(&format!("{base}/{project}/environments/{id}"))?,
            )?,
            EnvironmentCommand::Export { project, id } => {
                serde_json::to_value(client.post::<_, SandboxEnvironmentExport>(
                    &format!("{base}/{project}/environments/{id}/export"),
                    &serde_json::json!({}),
                )?)?
            }
            EnvironmentCommand::Delete {
                project,
                id,
                authorize_workspace_deletion,
            } => serde_json::to_value(client.delete_with_body::<_, SandboxEnvironment>(
                &format!("{base}/{project}/environments/{id}"),
                &SandboxEnvironmentDelete {
                    authorize_workspace_deletion,
                },
            )?)?,
            command => {
                let (project, id, action) = match command {
                    EnvironmentCommand::Reconcile { project, id } => (project, id, "reconcile"),
                    EnvironmentCommand::Stop { project, id } => (project, id, "stop"),
                    EnvironmentCommand::Resume { project, id } => (project, id, "resume"),
                    _ => unreachable!(),
                };
                serde_json::to_value(client.post::<_, SandboxEnvironment>(
                    &format!("{base}/{project}/environments/{id}/{action}"),
                    &serde_json::json!({}),
                )?)?
            }
        };
    Ok(value)
}
pub(super) fn tunnel(
    client: &ApiClient,
    command: TunnelCommand,
    input: impl Read,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let base = "/api/ai-sandbox";
    let value = match command {
        TunnelCommand::Inventory => serde_json::to_value(
            client.get::<Vec<SandboxTunnelBinding>>(&format!("{base}/tunnels"))?,
        )?,
        TunnelCommand::Retarget { id } => {
            serde_json::to_value(client.post::<_, SandboxTunnelBinding>(
                &format!("{base}/tunnels/{id}/retarget"),
                &read_input::<SandboxTunnelRetarget>(input)?,
            )?)?
        }
        TunnelCommand::Addresses => serde_json::to_value(
            client.get::<Vec<SandboxTunnelAddress>>(&format!("{base}/tunnel-addresses"))?,
        )?,
        TunnelCommand::List { project } => serde_json::to_value(client.get::<Vec<
            SandboxTunnelBinding,
        >>(&format!(
            "{base}/projects/{project}/tunnels"
        ))?)?,
        TunnelCommand::Save { project, id } => {
            serde_json::to_value(client.put::<_, SandboxTunnelBinding>(
                &format!("{base}/projects/{project}/tunnels/{id}"),
                &read_input::<SandboxTunnelSpec>(input)?,
            )?)?
        }
        TunnelCommand::Inspect { id } => serde_json::to_value(
            client.get::<SandboxTunnelBinding>(&format!("{base}/tunnels/{id}"))?,
        )?,
        TunnelCommand::Delete { id } => serde_json::to_value(
            client.delete_json::<SandboxTunnelBinding>(&format!("{base}/tunnels/{id}"))?,
        )?,
        TunnelCommand::Verify { id } => {
            serde_json::to_value(client.post::<_, SandboxTunnelBinding>(
                &format!("{base}/tunnels/{id}/verify"),
                &read_input::<SandboxTunnelVerify>(input)?,
            )?)?
        }
        command => {
            let (id, action) = match command {
                TunnelCommand::Publish { id } => (id, "publish"),
                TunnelCommand::Reconcile { id } => (id, "reconcile"),
                TunnelCommand::Stop { id } => (id, "stop"),
                _ => unreachable!(),
            };
            serde_json::to_value(client.post::<_, SandboxTunnelBinding>(
                &format!("{base}/tunnels/{id}/{action}"),
                &serde_json::json!({}),
            )?)?
        }
    };
    Ok(value)
}
