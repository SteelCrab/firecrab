//! Controller registry and placement commands, separate from saved `host` profiles.
mod services;

use crate::api_client::{ApiClient, ApiError};
use firecrab_api_types::{ClusterHost, DrainHostRequest, PlacementSpec, VmPlacement};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, clap::Subcommand)]
/// Commands executed against the selected controller API.
pub enum Command {
    /// Deploy and reconcile services with one identity across workload hosts.
    Service {
        #[command(subcommand)]
        command: services::Command,
    },
    /// Inspect or drain registered workload hosts.
    Host {
        #[command(subcommand)]
        /// Registered host operation.
        command: HostCommand,
    },
    /// Place and manage VMs through their recorded owner.
    Vm {
        #[command(subcommand)]
        /// Controller placement operation.
        command: VmCommand,
    },
}

#[derive(Debug, clap::Subcommand)]
/// Inspect inventory or change admission state for an enrolled host.
pub enum HostCommand {
    /// List registered hosts, live heartbeat state and reserved capacity.
    List {
        #[arg(long)]
        /// Print the full typed response as JSON.
        json: bool,
    },
    /// Show a registered host and its host-local inventory.
    Show {
        /// Stable controller placement or registered host UUID.
        id: Uuid,
        #[arg(long)]
        /// Print the full typed response as JSON.
        json: bool,
    },
    /// Prevent new placement without stopping existing VMs.
    Drain {
        /// Stable controller placement or registered host UUID.
        id: Uuid,
        #[arg(long)]
        /// Print the full typed response as JSON.
        json: bool,
    },
    /// Allow placement on a previously drained host.
    Resume {
        /// Stable controller placement or registered host UUID.
        id: Uuid,
        #[arg(long)]
        /// Print the full typed response as JSON.
        json: bool,
    },
}

#[derive(Debug, clap::Subcommand)]
/// Submit and reconcile the lifecycle of a controller-owned VM.
pub enum VmCommand {
    /// Persist and dispatch an image-pinned placement from a JSON specification.
    Place {
        #[arg(long)]
        /// Path to a JSON placement specification with host-scoped candidates.
        file: PathBuf,
        /// Reuse this key and the exact specification after a timeout.
        #[arg(long)]
        idempotency_key: String,
        #[arg(long)]
        /// Print the full typed response as JSON.
        json: bool,
    },
    /// List placements, including unresolved operations and deletion tombstones.
    List {
        #[arg(long)]
        /// Print the full typed response as JSON.
        json: bool,
    },
    /// Show the immutable host/VM identity and latest observation.
    Show {
        /// Stable controller placement or registered host UUID.
        id: Uuid,
        #[arg(long)]
        /// Print the full typed response as JSON.
        json: bool,
    },
    /// Start the VM on its recorded host.
    Start {
        /// Stable controller placement or registered host UUID.
        id: Uuid,
        #[arg(long)]
        /// Print the full typed response as JSON.
        json: bool,
    },
    /// Stop the VM on its recorded host.
    Stop {
        /// Stable controller placement or registered host UUID.
        id: Uuid,
        #[arg(long)]
        /// Print the full typed response as JSON.
        json: bool,
    },
    /// Delete on the recorded host; retain ownership until cleanup is acknowledged.
    Delete {
        /// Stable controller placement or registered host UUID.
        id: Uuid,
        #[arg(long)]
        /// Print the full typed response as JSON.
        json: bool,
    },
    /// Query and retry the recorded owner's current operation.
    Reconcile {
        /// Stable controller placement or registered host UUID.
        id: Uuid,
        #[arg(long)]
        /// Print the full typed response as JSON.
        json: bool,
    },
}

#[derive(Debug, thiserror::Error)]
/// CLI failures preserve the normal API, file, and JSON error categories.
pub enum Error {
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub fn run(client: &ApiClient, command: Command) -> Result<(), Error> {
    match command {
        Command::Service { command } => services::run(client, command)?,
        Command::Host { command } => {
            let multiple = matches!(&command, HostCommand::List { .. });
            let (hosts, json) = match command {
                HostCommand::List { json } => {
                    (client.get::<Vec<ClusterHost>>("/api/cluster/hosts")?, json)
                }
                HostCommand::Show { id, json } => {
                    (vec![client.get(&format!("/api/cluster/hosts/{id}"))?], json)
                }
                HostCommand::Drain { id, json } => (
                    vec![client.patch(
                        &format!("/api/cluster/hosts/{id}"),
                        &DrainHostRequest { draining: true },
                    )?],
                    json,
                ),
                HostCommand::Resume { id, json } => (
                    vec![client.patch(
                        &format!("/api/cluster/hosts/{id}"),
                        &DrainHostRequest { draining: false },
                    )?],
                    json,
                ),
            };
            if json {
                if multiple {
                    println!("{}", serde_json::to_string_pretty(&hosts)?);
                } else {
                    println!("{}", serde_json::to_string_pretty(&hosts[0])?);
                }
            } else {
                println!("HOST ID\tNAME\tSTATE\tCPU RESERVED/TOTAL\tRAM MiB RESERVED/TOTAL");
                for h in hosts {
                    println!(
                        "{}\t{}\t{}\t{}/{}\t{}/{}",
                        h.report.id,
                        h.report.name,
                        if !h.online {
                            "offline"
                        } else if h.draining {
                            "draining"
                        } else {
                            "ready"
                        },
                        h.reserved.cpu,
                        h.report.capacity.cpu,
                        h.reserved.ram_mib,
                        h.report.capacity.ram_mib
                    );
                }
            }
        }
        Command::Vm { command } => {
            let multiple = matches!(&command, VmCommand::List { .. });
            let (placements, json) = match command {
                VmCommand::List { json } => (
                    client.get::<Vec<VmPlacement>>("/api/cluster/placements")?,
                    json,
                ),
                VmCommand::Place {
                    file,
                    idempotency_key,
                    json,
                } => {
                    let spec: PlacementSpec = serde_json::from_slice(&std::fs::read(file)?)?;
                    (
                        vec![client.post_idempotent(
                            "/api/cluster/placements",
                            &spec,
                            &idempotency_key,
                        )?],
                        json,
                    )
                }
                VmCommand::Show { id, json } => (
                    vec![client.get(&format!("/api/cluster/placements/{id}"))?],
                    json,
                ),
                VmCommand::Start { id, json } => (
                    vec![client.post_empty(&format!("/api/cluster/placements/{id}/start"))?],
                    json,
                ),
                VmCommand::Stop { id, json } => (
                    vec![client.post_empty(&format!("/api/cluster/placements/{id}/stop"))?],
                    json,
                ),
                VmCommand::Delete { id, json } => (
                    vec![client.post_empty(&format!("/api/cluster/placements/{id}/delete"))?],
                    json,
                ),
                VmCommand::Reconcile { id, json } => (
                    vec![client.post_empty(&format!("/api/cluster/placements/{id}/reconcile"))?],
                    json,
                ),
            };
            if json {
                if multiple {
                    println!("{}", serde_json::to_string_pretty(&placements)?);
                } else {
                    println!("{}", serde_json::to_string_pretty(&placements[0])?);
                }
            } else {
                println!("PLACEMENT ID\tNAME\tHOST ID\tVM ID\tPHASE\tGENERATION\tERROR");
                for p in placements {
                    println!(
                        "{}\t{}\t{}\t{}\t{:?}\t{}\t{}",
                        p.id,
                        p.spec.name,
                        p.vm.host_id,
                        p.vm.resource_id,
                        p.phase,
                        p.generation,
                        p.error.as_deref().unwrap_or("-")
                    );
                }
            }
        }
    }
    Ok(())
}
