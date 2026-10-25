//! Cross-host service lifecycle. Explicit retry keys survive uncertain HTTP outcomes.
use super::{ApiClient, Error};
use firecrab_api_types::*;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Create and deploy a digest-pinned service from a JSON specification.
    Create {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        idempotency_key: String,
        #[arg(long)]
        json: bool,
    },
    /// Replace the release after checking its current operation generation.
    Update {
        id: Uuid,
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        expected_generation: u64,
        #[arg(long)]
        idempotency_key: String,
        #[arg(long)]
        json: bool,
    },
    /// Stop every host deployment; unresolved ownership remains recorded.
    Stop {
        id: Uuid,
        #[arg(long)]
        expected_generation: u64,
        #[arg(long)]
        idempotency_key: String,
        #[arg(long)]
        json: bool,
    },
    /// List service identity, desired/observed status and release generation.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Inspect a service and all host deployment/resource IDs.
    Show {
        id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Retry the durable intent on each recorded host.
    Reconcile {
        id: Uuid,
        #[arg(long)]
        json: bool,
    },
    /// Inspect accepted operations, retry keys and historical outcomes.
    Operations { id: Uuid },
}

pub(super) fn run(client: &ApiClient, command: Command) -> Result<(), Error> {
    let base = "/api/cluster/services";
    let multiple = matches!(command, Command::List { .. });
    let (services, json) = match command {
        Command::Create {
            file,
            idempotency_key,
            json,
        } => {
            let spec: ClusterServiceSpec = serde_json::from_slice(&std::fs::read(file)?)?;
            (
                vec![client.post_idempotent(base, &spec, &idempotency_key)?],
                json,
            )
        }
        Command::Update {
            id,
            file,
            expected_generation,
            idempotency_key,
            json,
        } => {
            let spec: ClusterServiceSpec = serde_json::from_slice(&std::fs::read(file)?)?;
            (
                vec![client.post_idempotent(
                    &format!("{base}/{id}/update"),
                    &UpdateClusterServiceRequest {
                        expected_generation,
                        spec,
                    },
                    &idempotency_key,
                )?],
                json,
            )
        }
        Command::Stop {
            id,
            expected_generation,
            idempotency_key,
            json,
        } => (
            vec![client.post_idempotent(
                &format!("{base}/{id}/stop"),
                &StopClusterServiceRequest {
                    expected_generation,
                },
                &idempotency_key,
            )?],
            json,
        ),
        Command::List { json } => (client.get::<Vec<ClusterService>>(base)?, json),
        Command::Show { id, json } => (vec![client.get(&format!("{base}/{id}"))?], json),
        Command::Reconcile { id, json } => (
            vec![client.post_empty(&format!("{base}/{id}/reconcile"))?],
            json,
        ),
        Command::Operations { id } => {
            let ops: Vec<ClusterServiceOperation> =
                client.get(&format!("{base}/{id}/operations"))?;
            println!("{}", serde_json::to_string_pretty(&ops)?);
            return Ok(());
        }
    };
    if json {
        if multiple {
            println!("{}", serde_json::to_string_pretty(&services)?);
        } else {
            println!("{}", serde_json::to_string_pretty(&services[0])?);
        }
    } else {
        println!("SERVICE ID\tNAME\tDESIRED\tOBSERVED\tGENERATION\tRELEASE\tPENDING\tDEGRADED");
        for s in services {
            println!(
                "{}\t{}\t{:?}\t{:?}\t{}\t{}\t{}\t{}",
                s.id,
                s.spec.service.name,
                s.desired,
                s.observed,
                s.generation,
                s.release_generation,
                s.pending,
                s.degraded
            );
            for h in s.hosts {
                println!(
                    "  host={} deployment={} replicas={} error={}",
                    h.host_id,
                    h.intent.deployment_id,
                    h.intent.spec.replicas,
                    h.error.as_deref().unwrap_or("-")
                );
            }
        }
    }
    Ok(())
}
