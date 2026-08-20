use std::time::Duration;

use firecrab_api_types::{VmResponse, VmState};
use reqwest::blocking::{Client, Response};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

/// VM attributes shared by every benchmark operation.
#[derive(Debug, Clone)]
pub struct VmSpec {
    /// Registered template alias.
    pub template: String,
    /// MicroNetwork receiving the benchmark VM lease.
    pub micro_network_id: Uuid,
    /// Guest RAM in MiB.
    pub ram: u32,
    /// Guest vCPU count.
    pub cpu: u8,
    /// Guest disk capacity in GiB.
    pub disk_gb: u16,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateVmBody<'a> {
    name: &'a str,
    template: &'a str,
    ram: u32,
    cpu: u8,
    disk_gb: u16,
    egress_policy: &'static str,
    micro_network_id: Uuid,
}

/// Failure returned by the VM API abstraction.
#[derive(Debug, Error)]
pub enum ApiError {
    /// Connection, timeout, or response decoding failure.
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
    /// Non-success HTTP response.
    #[error("API returned HTTP {status}: {body}")]
    Http {
        /// HTTP status code.
        status: u16,
        /// Response body retained for CI diagnostics.
        body: String,
    },
    /// API record disappeared during a benchmark.
    #[error("VM {0} was not found")]
    NotFound(Uuid),
}

/// Minimal VM lifecycle operations needed by benchmark algorithms.
pub trait VmApi: Sync {
    /// Creates an inactive VM record and returns its identifier.
    fn create(&self, spec: &VmSpec, name: &str) -> Result<Uuid, ApiError>;
    /// Starts an inactive VM.
    fn start(&self, id: Uuid) -> Result<(), ApiError>;
    /// Reads the current VM state.
    fn state(&self, id: Uuid) -> Result<VmState, ApiError>;
    /// Stops a running VM.
    fn stop(&self, id: Uuid) -> Result<(), ApiError>;
    /// Deletes an inactive VM record and its artifacts.
    fn delete(&self, id: Uuid) -> Result<(), ApiError>;
}

/// Blocking HTTP implementation of [`VmApi`] for a live Firecrab host.
pub struct HttpVmApi {
    base: String,
    client: Client,
}

impl HttpVmApi {
    /// Creates a client for an API base without a trailing slash.
    pub fn new(base: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("HTTP client construction");
        Self {
            base: base.trim_end_matches('/').to_owned(),
            client,
        }
    }

    fn checked(response: Response) -> Result<Response, ApiError> {
        if response.status().is_success() {
            Ok(response)
        } else {
            let status = response.status().as_u16();
            let body = response.text().unwrap_or_default();
            Err(ApiError::Http { status, body })
        }
    }
}

impl VmApi for HttpVmApi {
    fn create(&self, spec: &VmSpec, name: &str) -> Result<Uuid, ApiError> {
        let body = CreateVmBody {
            name,
            template: &spec.template,
            ram: spec.ram,
            cpu: spec.cpu,
            disk_gb: spec.disk_gb,
            egress_policy: "internet",
            micro_network_id: spec.micro_network_id,
        };
        let vm = Self::checked(
            self.client
                .post(format!("{}/api/vms", self.base))
                .json(&body)
                .send()?,
        )?
        .json::<VmResponse>()?;
        Ok(vm.id)
    }

    fn start(&self, id: Uuid) -> Result<(), ApiError> {
        Self::checked(
            self.client
                .post(format!("{}/api/vms/{id}/start", self.base))
                .send()?,
        )?;
        Ok(())
    }

    fn state(&self, id: Uuid) -> Result<VmState, ApiError> {
        let vm = Self::checked(
            self.client
                .get(format!("{}/api/vms/{id}", self.base))
                .send()?,
        )?
        .json::<VmResponse>()?;
        Ok(vm.state)
    }

    fn stop(&self, id: Uuid) -> Result<(), ApiError> {
        Self::checked(
            self.client
                .post(format!("{}/api/vms/{id}/stop", self.base))
                .send()?,
        )?;
        Ok(())
    }

    fn delete(&self, id: Uuid) -> Result<(), ApiError> {
        Self::checked(
            self.client
                .delete(format!("{}/api/vms/{id}", self.base))
                .send()?,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use mockito::Server;

    use super::*;

    #[test]
    fn http_client_executes_the_complete_vm_contract() {
        let id = Uuid::new_v4();
        let mut server = Server::new();
        let create = server
            .mock("POST", "/api/vms")
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(vm_json(id, "created"))
            .create();
        let start = server
            .mock("POST", format!("/api/vms/{id}/start").as_str())
            .with_status(200)
            .create();
        let state = server
            .mock("GET", format!("/api/vms/{id}").as_str())
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(vm_json(id, "running"))
            .create();
        let stop = server
            .mock("POST", format!("/api/vms/{id}/stop").as_str())
            .with_status(200)
            .create();
        let delete = server
            .mock("DELETE", format!("/api/vms/{id}").as_str())
            .with_status(204)
            .create();

        let api = HttpVmApi::new(format!("{}/", server.url()));
        let spec = VmSpec {
            template: "ubuntu".to_owned(),
            micro_network_id: Uuid::new_v4(),
            ram: 512,
            cpu: 1,
            disk_gb: 8,
        };
        assert_eq!(api.create(&spec, "benchmark").unwrap(), id);
        api.start(id).unwrap();
        assert_eq!(api.state(id).unwrap(), VmState::Running);
        api.stop(id).unwrap();
        api.delete(id).unwrap();

        for mock in [create, start, state, stop, delete] {
            mock.assert();
        }
    }

    #[test]
    fn http_client_retains_error_status_and_body() {
        let mut server = Server::new();
        let failure = server
            .mock("DELETE", "/api/vms/00000000-0000-0000-0000-000000000000")
            .with_status(409)
            .with_body("still running")
            .create();
        let api = HttpVmApi::new(server.url());
        let error = api.delete(Uuid::nil()).unwrap_err();
        assert!(matches!(
            error,
            ApiError::Http {
                status: 409,
                ref body
            } if body == "still running"
        ));
        failure.assert();
    }

    fn vm_json(id: Uuid, state: &str) -> String {
        serde_json::json!({
            "id": id,
            "name": "benchmark",
            "state": state,
            "template": "ubuntu",
            "templateVersion": "1",
            "cpu": 1,
            "ram": 512,
            "diskGb": 8,
            "startupStep": null,
            "egressPolicy": "internet",
            "ipv4": null,
            "mac": null,
            "hostname": "benchmark",
            "startupTimeline": [],
            "microNetworkId": id,
            "storageRoot": "default",
            "cpuUsagePercent": null,
            "memoryUsedMib": null,
            "memoryTotalMib": null,
            "memoryUsedPercent": null,
            "usageHistory": [],
            "shellRefs": [],
            "portForwards": [],
            "env": {}
        })
        .to_string()
    }
}
