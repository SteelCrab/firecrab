use std::path::PathBuf;

use clap::Subcommand;
use firecrab_api_types::{
    DeployMicroServiceRequest, DeployStrategy, MicroServiceDeployment, MicroServiceResponse,
    MicroServiceSpec, RetryServiceRequest, RollbackServiceRequest, ServicePhase,
    UpdateServiceRequest,
};
use uuid::Uuid;

use crate::api_client::{ApiClient, ApiError};

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Update a running domain service through Nginx without stopping it first.
    Rollout {
        id: Uuid,
        #[arg(long)]
        file: PathBuf,
        /// Reuse this UUID after a lost response. A new UUID is printed if omitted.
        #[arg(long)]
        operation_id: Option<Uuid>,
        /// Reject the request if another release became active.
        #[arg(long)]
        expected_release: Option<Uuid>,
        #[arg(long, default_value = "rolling", value_parser = ["rolling", "blue_green"])]
        strategy: String,
        #[arg(long)]
        no_wait: bool,
        #[arg(long, default_value_t = 1800)]
        timeout: u64,
    },
    /// List successful, digest-pinned releases retained for rollback.
    Releases { id: Uuid },
    /// Roll back to a successful release through the same Deploy executor.
    Rollback {
        id: Uuid,
        #[arg(long)]
        release: Uuid,
        #[arg(long)]
        operation_id: Option<Uuid>,
        #[arg(long)]
        expected_release: Option<Uuid>,
        #[arg(long, default_value = "rolling", value_parser = ["rolling", "blue_green"])]
        strategy: String,
        #[arg(long)]
        no_wait: bool,
        #[arg(long, default_value_t = 1800)]
        timeout: u64,
    },
    /// Resume an interrupted Deploy, including a drain timeout or failed cleanup.
    Retry {
        id: Uuid,
        #[arg(long)]
        no_wait: bool,
        #[arg(long, default_value_t = 1800)]
        timeout: u64,
    },
    /// Pull the image, start replicas, and wait for app health.
    Deploy {
        id: Uuid,
        /// First host TCP port; omit when the service has a domain.
        #[arg(long, default_value_t = 0)]
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
        Command::Rollout {
            id,
            file,
            operation_id,
            expected_release,
            strategy,
            no_wait,
            timeout,
        } => {
            let operation_id = operation_id.unwrap_or_else(Uuid::new_v4);
            let spec = read_spec(file)?;
            eprintln!("Deploy operation: {operation_id}");
            let deployment: MicroServiceDeployment = client.post(
                &format!("/api/micro-services/{id}/updates"),
                &UpdateServiceRequest {
                    expected_release_id: expected_release,
                    operation_id,
                    spec,
                    strategy: deploy_strategy(&strategy),
                },
            )?;
            let deployment = if no_wait {
                deployment
            } else {
                wait(client, deployment, ServicePhase::Running, timeout)?
            };
            print_json(&deployment);
        }
        Command::Releases { id } => {
            let releases: Vec<MicroServiceDeployment> =
                client.get(&format!("/api/micro-services/{id}/releases"))?;
            print_json(&releases);
        }
        Command::Rollback {
            id,
            release,
            operation_id,
            expected_release,
            strategy,
            no_wait,
            timeout,
        } => {
            let operation_id = operation_id.unwrap_or_else(Uuid::new_v4);
            eprintln!("Deploy operation: {operation_id}");
            let deployment: MicroServiceDeployment = client.post(
                &format!("/api/micro-services/{id}/rollback"),
                &RollbackServiceRequest {
                    expected_release_id: expected_release,
                    operation_id,
                    release_id: release,
                    strategy: deploy_strategy(&strategy),
                },
            )?;
            let deployment = if no_wait {
                deployment
            } else {
                wait(client, deployment, ServicePhase::Running, timeout)?
            };
            print_json(&deployment);
        }
        Command::Retry {
            id,
            no_wait,
            timeout,
        } => {
            let current: Option<MicroServiceDeployment> =
                client.get(&format!("/api/micro-services/{id}/deployment"))?;
            let current = current.ok_or_else(|| {
                Error::Deployment("Deploy has no interrupted operation to retry".into())
            })?;
            let deployment: MicroServiceDeployment = client.post(
                &format!("/api/micro-services/{id}/deployment/retry"),
                &RetryServiceRequest {
                    operation_id: current.id,
                },
            )?;
            let expected = if deployment
                .update
                .as_ref()
                .is_some_and(|update| update.cancelled)
            {
                ServicePhase::Stopped
            } else {
                ServicePhase::Running
            };
            let deployment = if no_wait {
                deployment
            } else {
                wait(client, deployment, expected, timeout)?
            };
            print_json(&deployment);
        }
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
    let path = if deployment.update.is_some() {
        format!(
            "/api/micro-services/{}/deployments/{}",
            deployment.service_id, deployment.id
        )
    } else {
        format!("/api/micro-services/{}/deployment", deployment.service_id)
    };
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

fn deploy_strategy(value: &str) -> DeployStrategy {
    if value == "blue_green" {
        DeployStrategy::BlueGreen
    } else {
        DeployStrategy::Rolling
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn wait_reports_the_failed_attempt_even_when_a_previous_release_is_running() {
        let service = Uuid::new_v4();
        let operation = Uuid::new_v4();
        let spec = serde_json::json!({"name":"web","image":"ghcr.io/team/web:v1","port":80,"ram":128,"cpu":1});
        let previous = serde_json::json!({"id":Uuid::new_v4(),"serviceId":service,"phase":"running","spec":spec,"request":{"hostPort":0},"digest":null,"instances":[],"log":[],"error":null});
        let mut pending: MicroServiceDeployment = serde_json::from_value(serde_json::json!({
            "id":operation,"serviceId":service,"phase":"queued","spec":spec,
            "request":{"hostPort":0},"digest":null,"instances":[],"log":[],"error":null,
            "update":{"previous":previous,"requestedSpec":spec,"strategy":"rolling","rollbackOf":null,
                "restoring":false,"routedNew":false,"transition":null,"retiring":[],"completed":false}
        })).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = ApiClient::new(format!("http://{}", listener.local_addr().unwrap()));
        let mut failed = pending.clone();
        failed.phase = ServicePhase::Failed;
        failed.error = Some("candidate health failed; previous release restored".into());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = [0; 4096];
            let count = stream.read(&mut bytes).unwrap();
            let request = String::from_utf8_lossy(&bytes[..count]);
            assert!(
                request.starts_with(&format!(
                    "GET /api/micro-services/{service}/deployments/{operation} "
                )),
                "{request}"
            );
            let body = serde_json::to_string(&failed).unwrap();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        pending.phase = ServicePhase::Draining;
        let error = wait(&client, pending, ServicePhase::Running, 5).unwrap_err();
        assert!(error.to_string().contains("candidate health failed"));
        server.join().unwrap();
    }
}
