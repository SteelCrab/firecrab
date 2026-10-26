//! Typed adapters. Environment cleanup and Tunnel changes remain separate operations.
use super::*;

#[derive(Debug, Subcommand)]
/// Manage project-owned environments through the same typed REST lifecycle as Dashboard.
pub enum EnvironmentCommand {
    /// Discover immutable template pins, including boot-arguments SHA-256.
    Images {
        /// Project whose read permission scopes the installed template inventory.
        project: Uuid,
    },
    /// List saved environment generations, resources, readiness and retention state.
    List {
        /// Project whose environments the API may return.
        project: Uuid,
    },
    /// Save an immutable SandboxEnvironmentSpec from stdin; reuse id after uncertainty.
    Create {
        /// Project that owns the new environment and its resource ledger.
        project: Uuid,
        /// Caller-chosen environment identity; inspect this ID after an uncertain response.
        id: Uuid,
    },
    /// Read the saved specification and observed resources without provisioning again.
    Inspect {
        /// Project required to authorize this environment read.
        project: Uuid,
        /// Environment whose generation and reserved resource IDs are inspected.
        id: Uuid,
    },
    /// Resume reconciliation of the saved resource ledger without rerunning task commands.
    Reconcile {
        /// Project that owns the environment being reconciled.
        project: Uuid,
        /// Existing environment whose reserved VM and network IDs must be reused.
        id: Uuid,
    },
    /// Stop owned guests while retaining their disks, packages and configuration.
    Stop {
        /// Project that authorizes stopping these managed guests.
        project: Uuid,
        /// Environment to stop after active work and guest sync permit it.
        id: Uuid,
    },
    /// Start the retained resources again and recheck each readiness layer.
    Resume {
        /// Project that owns the retained guests.
        project: Uuid,
        /// Stopped environment to resume with its existing disks.
        id: Uuid,
    },
    /// Verify required task results and artifact bytes before granting owned cleanup.
    Export {
        /// Project that authorizes reading this environment's task exports.
        project: Uuid,
        /// Environment whose generation-bound outputs must be complete.
        id: Uuid,
    },
    /// Delete only owned resources after export; retained Workspace disks need authorization.
    Delete {
        /// Project whose ownership ledger constrains every resource removed.
        project: Uuid,
        /// Environment to clean up only after required outputs are exported.
        id: Uuid,
        /// Explicitly authorize destroying retained Workspace disks; false preserves them.
        #[arg(long)]
        authorize_workspace_deletion: bool,
    },
}
#[derive(Debug, Subcommand)]
/// Inspect or signal only the runtime and processes admitted for the selected task.
pub enum GuestCommand {
    /// Probe actual guest execution and container-engine capabilities.
    Health,
    /// List attempt-owned process identities including boot ID and start ticks.
    Processes,
    /// Read an exact process identity and termination signal from stdin.
    Signal,
}
/// Dispatch task-scoped guest operations; a signal receipt does not confirm termination.
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
/// Manage durable preview bindings while keeping helper and public evidence separate.
pub enum TunnelCommand {
    /// Operator inventory retains retirement warnings after project deletion.
    Inventory,
    /// Retarget the same URL to a new task/runtime using SandboxTunnelRetarget JSON.
    Retarget {
        /// Existing binding whose fixed address transfers to a new immutable binding.
        id: Uuid,
    },
    /// List saved Pipe addresses without reading their tokens.
    Addresses,
    /// List a project's saved task/attempt/runtime bindings and observed connection state.
    List {
        /// Project whose preview inventory is authorized.
        project: Uuid,
    },
    /// Save SandboxTunnelSpec JSON from stdin. Registers no credentials.
    Save {
        /// Project that owns the target task and the saved binding.
        project: Uuid,
        /// Caller-chosen binding identity; task and address become immutable after saving.
        id: Uuid,
    },
    /// Read helper, HTTPS, WebSocket and retirement evidence for one saved binding.
    Inspect {
        /// Binding to inspect, including retained retirement warnings.
        id: Uuid,
    },
    /// Publish or reconnect the ready application through its exclusive saved connector.
    Publish {
        /// Authorized binding whose saved address and admitted runtime are rechecked.
        id: Uuid,
    },
    /// Compare saved ownership with actual helper state after restart or uncertainty.
    Reconcile {
        /// Binding whose connector and applied target must still match.
        id: Uuid,
    },
    /// Stop the exclusive connector while preserving the saved token and fixed URL.
    Stop {
        /// Exclusive binding to stop without revoking the saved registration token.
        id: Uuid,
    },
    /// Remove local binding/connection; remote dashboard retirement remains explicit.
    Delete {
        /// Binding whose local connection is removed after ownership verification.
        id: Uuid,
    },
    /// Check actual public HTTPS content or WebSocket upgrade using JSON from stdin.
    Verify {
        /// Fixed-URL binding to probe using the path and expectations from stdin.
        id: Uuid,
    },
}

/// Decode versioned manifests and deletion consent, then return the API's typed result.
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
/// Send the selected binding operation without accepting or reading registration secrets.
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
