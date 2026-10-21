use std::path::PathBuf;

use clap::Subcommand;
use firecrab_api_types::{
    DeployMicroServiceRequest, MicroServiceDeployment, MicroServiceResponse, MicroServiceSpec,
    ServicePhase,
};
use uuid::Uuid;

use crate::api_client::{ApiClient, ApiError};

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Pull the image, start replicas, and wait for app health.
    Deploy {
        id: Uuid,
        /// First host TCP port; replicas use consecutive ports.
        #[arg(long)]
        host_port: u16,
        /// Existing network UUID. Omit to create/use the services network.
        #[arg(long)]
        network: Option<Uuid>,
        /// Return after the job is accepted.
        #[arg(long)]
        no_wait: bool,
        #[arg(long, default_value_t = 1800)]
        timeout: u64,
    },
    /// Read actual VM states, HTTP health, endpoints and deployment logs.
    Status { id: Uuid },
    /// Stop and remove the service's VM instances, keeping its configuration.
    Stop {
        id: Uuid,
        #[arg(long)]
        no_wait: bool,
    },
    /// List saved service configurations.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Read one service configuration.
    Get { id: Uuid },
    /// Save a service configuration from a JSON file (does not deploy it).
    Create {
        #[arg(long)]
        file: PathBuf,
    },
    /// Replace every configuration field from a JSON file.
    Update {
        id: Uuid,
        #[arg(long)]
        file: PathBuf,
    },
    /// Delete a saved service configuration.
    Delete { id: Uuid },
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Deployment(String),
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error("could not read service configuration {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid service configuration {path}: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
}

pub fn run(client: &ApiClient, command: Command) -> Result<(), Error> {
    match command {
        Command::Deploy {
            id,
            host_port,
            network,
            no_wait,
            timeout,
        } => {
            let deployment: MicroServiceDeployment = client.post(
                &format!("/api/micro-services/{id}/deployment"),
                &DeployMicroServiceRequest {
                    host_port,
                    micro_network_id: network,
                },
            )?;
            let deployment = if no_wait {
                deployment
            } else {
                wait(client, deployment, ServicePhase::Running, timeout)?
            };
            print_json(&deployment);
        }
        Command::Status { id } => {
            let deployment: Option<MicroServiceDeployment> =
                client.get(&format!("/api/micro-services/{id}/deployment"))?;
            print_json(&deployment);
        }
        Command::Stop { id, no_wait } => {
            let deployment: MicroServiceDeployment =
                client.post_empty(&format!("/api/micro-services/{id}/stop"))?;
            let deployment = if no_wait {
                deployment
            } else {
                let timeout = u64::from(deployment.spec.drain_timeout) + 300;
                wait(client, deployment, ServicePhase::Stopped, timeout)?
            };
            print_json(&deployment);
        }
        Command::List { json } => {
            let services: Vec<MicroServiceResponse> = client.get("/api/micro-services")?;
            if json {
                print_json(&services);
            } else {
                println!("ID\tNAME\tIMAGE\tPORT\tREPLICAS\tRAM MiB\tCPU\tDOMAIN");
                for service in services {
                    let spec = service.spec;
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                        service.id,
                        spec.name,
                        spec.image,
                        spec.port,
                        spec.replicas,
                        spec.ram,
                        spec.cpu,
                        spec.domain.as_deref().unwrap_or("-")
                    );
                }
            }
        }
        Command::Get { id } => {
            let service: MicroServiceResponse = client.get(&format!("/api/micro-services/{id}"))?;
            print_json(&service);
        }
        Command::Create { file } => {
            let service: MicroServiceResponse =
                client.post("/api/micro-services", &read_spec(file)?)?;
            print_json(&service);
        }
        Command::Update { id, file } => {
            let service: MicroServiceResponse =
                client.put(&format!("/api/micro-services/{id}"), &read_spec(file)?)?;
            print_json(&service);
        }
        Command::Delete { id } => {
            client.delete(&format!("/api/micro-services/{id}"))?;
            print_json(&serde_json::json!({ "deleted": id }));
        }
    }
    Ok(())
}

fn wait(
    client: &ApiClient,
    mut deployment: MicroServiceDeployment,
    expected: ServicePhase,
    timeout: u64,
) -> Result<MicroServiceDeployment, Error> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
    let id = deployment.id;
    let path = format!("/api/micro-services/{}/deployment", deployment.service_id);
    let mut previous = None;
    loop {
        if previous != Some(deployment.phase) {
            eprintln!("service {}: {:?}", deployment.service_id, deployment.phase);
            previous = Some(deployment.phase);
        }
        if deployment.phase == expected {
            return Ok(deployment);
        }
        if !deployment.phase.is_busy() {
            return Err(Error::Deployment(deployment.error.unwrap_or_else(|| {
                format!(
                    "service ended in {:?}; inspect microservice status",
                    deployment.phase
                )
            })));
        }
        if std::time::Instant::now() >= deadline {
            return Err(Error::Deployment("Timed out waiting; the job continues on the host. Use microservice status to inspect it".into()));
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
        deployment = client
            .get::<Option<MicroServiceDeployment>>(&path)?
            .ok_or_else(|| Error::Deployment("Deployment no longer exists".into()))?;
        if deployment.id != id {
            return Err(Error::Deployment(
                "A newer deployment replaced this job".into(),
            ));
        }
    }
}

fn read_spec(path: PathBuf) -> Result<MicroServiceSpec, Error> {
    let bytes = std::fs::read(&path).map_err(|source| Error::Read {
        path: path.clone(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| Error::Json { path, source })
}

fn print_json(value: &impl serde::Serialize) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("API response serializes")
    );
}
