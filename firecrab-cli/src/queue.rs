//! Host-local RabbitMQ engine and application queues.
use crate::api_client::{ApiClient, ApiError};
use clap::{Subcommand, ValueEnum};
use firecrab_api_types::*;
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Inspect or control the stateful internal RabbitMQ engine.
    Engine {
        #[command(subcommand)]
        command: EngineCommand,
    },
    /// List application queues and service references.
    List,
    /// Create a durable queue and a failure queue.
    Create {
        #[arg(long)]
        name: String,
        #[arg(long, default_value_t = 5)]
        delivery_limit: u16,
        #[arg(long, default_value_t = 64)]
        max_mib: u16,
    },
    /// Read waiting, unacknowledged, consumer and failure counts.
    Metrics { id: Uuid },
    /// Recover an interrupted or failed queue declaration.
    Retry { id: Uuid },
    /// Reveal credentials explicitly. Treat the printed URL as a secret.
    Connection {
        id: Uuid,
        #[arg(long, value_enum)]
        access: Access,
    },
    /// Delete an empty, unused queue. Referencing services must be unbound first.
    Delete { id: Uuid },
}
#[derive(Debug, Subcommand)]
pub enum EngineCommand {
    Status,
    /// Import the official image and create one stateful RabbitMQ MicroVM.
    Create {
        #[arg(long)]
        network: Uuid,
        #[arg(long, default_value_t = 4)]
        cpu: u8,
        #[arg(long, default_value_t = 4096)]
        ram: u32,
        #[arg(long, default_value_t = 8)]
        disk_gb: u16,
        #[arg(long)]
        storage_root: Option<String>,
    },
    /// Start or recover the existing VM without replacing its disk.
    Start,
    /// Stop messaging, keeping the VM and all queue data.
    Stop,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Access {
    Publish,
    Consume,
}

pub fn run(client: &ApiClient, command: Command) -> Result<(), ApiError> {
    match command {
        Command::Engine { command } => match command {
            EngineCommand::Status => {
                print(&client.get::<Option<QueueEngineResponse>>("/api/queue/engine")?)
            }
            EngineCommand::Create {
                network,
                cpu,
                ram,
                disk_gb,
                storage_root,
            } => print(&client.post::<_, QueueEngineResponse>(
                "/api/queue/engine",
                &CreateQueueEngineRequest {
                    micro_network_id: network,
                    cpu,
                    ram,
                    disk_gb,
                    storage_root,
                },
            )?),
            EngineCommand::Start => {
                print(&client.post_empty::<QueueEngineResponse>("/api/queue/engine/start")?)
            }
            EngineCommand::Stop => {
                print(&client.post_empty::<QueueEngineResponse>("/api/queue/engine/stop")?)
            }
        },
        Command::List => print(&client.get::<Vec<QueueResponse>>("/api/queues")?),
        Command::Create {
            name,
            delivery_limit,
            max_mib,
        } => print(&client.post::<_, QueueResponse>(
            "/api/queues",
            &CreateQueueRequest {
                name,
                delivery_limit,
                max_bytes: u64::from(max_mib) * 1024 * 1024,
            },
        )?),
        Command::Metrics { id } => {
            print(&client.get::<QueueMetrics>(&format!("/api/queues/{id}/metrics"))?)
        }
        Command::Retry { id } => {
            print(&client.post_empty::<QueueResponse>(&format!("/api/queues/{id}/retry"))?)
        }
        Command::Connection { id, access } => {
            let access = match access {
                Access::Publish => "publish",
                Access::Consume => "consume",
            };
            print(
                &client.post_empty::<QueueConnection>(&format!(
                    "/api/queues/{id}/connection/{access}"
                ))?,
            );
        }
        Command::Delete { id } => {
            client.delete(&format!("/api/queues/{id}"))?;
            println!("Queue deleted");
        }
    }
    Ok(())
}
fn print(value: &impl Serialize) {
    match serde_json::to_string_pretty(value) {
        Ok(text) => println!("{text}"),
        Err(_) => eprintln!("Could not format Queue response"),
    }
}
