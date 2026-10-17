//! Executes service configurations through the existing OCI and VM pipelines.
//! A saved deployment owns its VM ids; stop and failed-deploy cleanup delete only those ids.

use std::collections::BTreeMap;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use firecrab_api_types::{
    CreateMicroNetworkRequest, CreateVmRequest, DeployMicroServiceRequest, EgressPolicy,
    ImageInstallStatus, MicroServiceDeployment, MicroServiceSpec, PortForward, PortProtocol,
    ServiceInstance, ServicePhase, VmState,
};
use futures_util::future::join_all;
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::handlers::micro_services::database;
use crate::handlers::{micro_networks, oci as oci_handler, vms};
use crate::image_install::Architecture;
use crate::oci::{self, ImageReference, ImageVersion, Sha256Digest};
use crate::server::RequestId;
use crate::state::AppState;

/// Validate deployment settings, persist a queued snapshot and start background work.
pub async fn deploy(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(request): ValidatedJson<DeployMicroServiceRequest>,
) -> Result<(StatusCode, Json<MicroServiceDeployment>), AppError> {
    let id = vms::parse_id(&id, request_id.0)?;
    let _claim = state.service_mutations.lock().await;
    ensure_editable(&state, id, request_id.0).await?;
    let store = state.store.clone();
    let service = database(request_id.0, move || store.micro_service(id))
        .await?
        .ok_or_else(|| AppError::not_found(request_id.0))?;
    let fields = deployment_fields(&service.spec, &request);
    if !fields.is_empty() {
        return Err(AppError::validation(fields, request_id.0));
    }
    if let Some(network_id) = request.micro_network_id {
        let store = state.store.clone();
        if database(request_id.0, move || store.micro_network(network_id))
            .await?
            .is_none()
        {
            return Err(AppError::validation(
                BTreeMap::from([("microNetworkId".into(), "network does not exist".into())]),
                request_id.0,
            ));
        }
    }
    let deployment = MicroServiceDeployment {
        id: Uuid::new_v4(),
        service_id: id,
        spec: service.spec,
        request,
        phase: ServicePhase::Queued,
        digest: None,
        instances: vec![],
        log: vec!["Deployment queued".into()],
        error: None,
    };
    save(&state, &deployment)
        .await
        .map_err(|_| AppError::internal(request_id.0))?;
    let task_state = state.clone();
    let task_deployment = deployment.clone();
    tokio::spawn(async move {
        run_deploy(task_state, task_deployment).await;
    });
    Ok((StatusCode::ACCEPTED, Json(deployment)))
}

/// Read the latest deployment with live VM states and application health checks.
pub async fn get(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<Option<MicroServiceDeployment>>, AppError> {
    let id = vms::parse_id(&id, request_id.0)?;
    let store = state.store.clone();
    let (service, deployment) = database(request_id.0, move || {
        Ok((store.micro_service(id)?, store.service_deployment(id)?))
    })
    .await?;
    if service.is_none() {
        return Err(AppError::not_found(request_id.0));
    }
    let Some(mut deployment) = deployment else {
        return Ok(Json(None));
    };
    refresh_instances(&state, &mut deployment).await;
    Ok(Json(Some(deployment)))
}

/// Queue cleanup of the deployment's owned VMs, rejecting concurrent operations.
pub async fn stop(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<MicroServiceDeployment>), AppError> {
    let id = vms::parse_id(&id, request_id.0)?;
    let _claim = state.service_mutations.lock().await;
    let store = state.store.clone();
    let mut deployment = database(request_id.0, move || store.service_deployment(id))
        .await?
        .ok_or_else(|| AppError::not_found(request_id.0))?;
    if deployment.phase.is_busy() {
        return Err(AppError::conflict(
            "service_busy",
            "wait for the current service operation to finish",
            request_id.0,
        ));
    }
    deployment.phase = ServicePhase::Stopping;
    deployment.error = None;
    deployment.log.push("Stopping service instances".into());
    save(&state, &deployment)
        .await
        .map_err(|_| AppError::internal(request_id.0))?;
    let task_state = state.clone();
    let mut task_deployment = deployment.clone();
    tokio::spawn(async move {
        let Ok(_permit) = task_state.service_deploy_permits.acquire().await else {
            return;
        };
        match cleanup(&task_state, &mut task_deployment).await {
            Ok(()) => {
                task_deployment.phase = ServicePhase::Stopped;
            }
            Err(error) => {
                task_deployment.phase = ServicePhase::Failed;
                task_deployment.error = Some(error);
            }
        }
        let _ = save(&task_state, &task_deployment).await;
    });
    Ok((StatusCode::ACCEPTED, Json(deployment)))
}

/// Reject mutations while a deployment is busy or still owns any VM identifiers.
pub(crate) async fn ensure_editable(
    state: &AppState,
    id: Uuid,
    request_id: Uuid,
) -> Result<(), AppError> {
    let store = state.store.clone();
    if let Some(deployment) = database(request_id, move || store.service_deployment(id)).await?
        && (deployment.phase.is_busy() || !deployment.instances.is_empty())
    {
        return Err(AppError::conflict(
            "service_in_use",
            "stop the service before editing, deleting or deploying it again",
            request_id,
        ));
    }
    Ok(())
}

/// Reject unsupported routing and secret settings or invalid host-port ranges.
fn deployment_fields(
    spec: &MicroServiceSpec,
    request: &DeployMicroServiceRequest,
) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    let end = u32::from(request.host_port) + u32::from(spec.replicas) - 1;
    let api_port = crate::server::HttpConfig::load().map_or(5523, |config| config.bind_addr.port());
    if request.host_port < 1024
        || end > 65535
        || (u32::from(request.host_port)..=end).contains(&5523)
        || (u32::from(request.host_port)..=end).contains(&u32::from(api_port))
    {
        fields.insert(
            "hostPort".into(),
            "replicas need consecutive host ports from 1024 to 65535; port 5523 is reserved".into(),
        );
    }
    if spec.domain.is_some() {
        fields.insert(
            "domain".into(),
            "automatic domain routing needs PipeIngress; clear domain to deploy using host ports"
                .into(),
        );
    }
    if !spec.secrets.is_empty() {
        fields.insert(
            "secrets".into(),
            "CrabSecrets injection is not available yet; secret references cannot be deployed"
                .into(),
        );
    }
    fields
}

/// Persist a snapshot without blocking the async executor on SQLite work.
async fn save(state: &AppState, deployment: &MicroServiceDeployment) -> Result<(), String> {
    let store = state.store.clone();
    let record = deployment.clone();
    database(deployment.id, move || {
        store.save_service_deployment(&record)
    })
    .await
    .map_err(|_| "Could not save deployment state".to_owned())
}

/// Record a phase transition and progress message, retaining the latest 128 entries.
async fn phase(
    state: &AppState,
    deployment: &mut MicroServiceDeployment,
    phase: ServicePhase,
    message: impl Into<String>,
) -> Result<(), String> {
    deployment.phase = phase;
    if deployment.log.len() >= 128 {
        deployment.log.remove(0);
    }
    deployment.log.push(message.into());
    save(state, deployment).await
}

/// Run under the deployment permit and clean up owned VMs if deployment fails.
async fn run_deploy(state: AppState, mut deployment: MicroServiceDeployment) {
    let Ok(_permit) = state.service_deploy_permits.acquire().await else {
        return;
    };
    if let Err(mut error) = execute(&state, &mut deployment).await {
        if let Err(cleanup_error) = cleanup(&state, &mut deployment).await {
            error.push_str(&format!("; cleanup requires retry: {cleanup_error}"));
        }
        deployment.phase = ServicePhase::Failed;
        deployment.error = Some(error.clone());
        deployment.log.push(error);
        if let Err(error) = save(&state, &deployment).await {
            tracing::error!(service_id = %deployment.service_id, %error, "could not record failed deployment");
        }
    }
}

/// Check capacity, import a pinned image and boot each replica until HTTP health passes.
async fn execute(state: &AppState, deployment: &mut MicroServiceDeployment) -> Result<(), String> {
    check_capacity(state, &deployment.spec).await?;
    let store = state.store.clone();
    let ports = database(deployment.id, move || store.list_all_port_forwards())
        .await
        .map_err(|_| "Could not check host ports".to_owned())?;
    let first = u32::from(deployment.request.host_port);
    let end = first + u32::from(deployment.spec.replicas);
    if ports.iter().any(|(_, port)| {
        port.protocol == PortProtocol::Tcp && (first..end).contains(&u32::from(port.host_port))
    }) {
        return Err("A requested host port is already assigned to a VM".into());
    }
    for port in first..end {
        std::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port as u16))
            .map_err(|_| format!("Host port {port} is already used by another process"))?;
    }
    phase(
        state,
        deployment,
        ServicePhase::Importing,
        "Resolving image tag to an immutable digest",
    )
    .await?;
    let original =
        ImageReference::parse(&deployment.spec.image).map_err(|error| error.to_string())?;
    let credential = oci_handler::stored_credential(state, RequestId(deployment.id)).await;
    let resolved = oci::resolve(
        &original,
        Architecture::HOST,
        oci::is_loopback_registry(&original.registry),
        credential.clone(),
    )
    .await
    .map_err(|error| format!("Image resolution failed: {error}"))?;
    deployment.digest = Some(resolved.digest.clone());
    save(state, deployment).await?;
    let pinned = ImageReference {
        version: ImageVersion::Digest(
            Sha256Digest::parse(&resolved.digest).map_err(|error| error.to_string())?,
        ),
        ..original
    };
    let alias = oci::template_name_from_reference(&pinned)
        .map_err(|error| error.to_string())?
        .alias;
    if let Some(template) = state.templates.resolve_alias(&alias) {
        if template.version != resolved.digest {
            return Err("Pinned image alias belongs to a different digest".into());
        }
    } else {
        match state
            .oci_imports
            .begin_with(&alias, "service image import started")
        {
            Ok(_) => {
                oci::run_oci_import(
                    state.oci_imports.clone(),
                    (*state.templates).clone(),
                    pinned,
                    alias.clone(),
                    credential,
                )
                .await
            }
            Err("running") => {
                tokio::time::timeout(Duration::from_secs(1800), async {
                    while state.oci_imports.is_running(&alias) {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                })
                .await
                .map_err(|_| "Timed out waiting for the image import".to_owned())?;
            }
            Err(_) => return Err("Could not start image import".into()),
        }
        let imported = state.oci_imports.snapshot(&alias);
        if imported.status != ImageInstallStatus::Succeeded {
            return Err(imported
                .log
                .lines()
                .last()
                .map(str::to_owned)
                .unwrap_or_else(|| "Image import failed".into()));
        }
    }
    let network_id = match deployment.request.micro_network_id {
        Some(id) => id,
        None => default_network(state, deployment.id).await?,
    };
    deployment.request.micro_network_id = Some(network_id);
    let template = state
        .templates
        .resolve_alias(&alias)
        .ok_or("Imported image is not registered")?;
    let disk_gb = u16::try_from(template.rootfs.length().div_ceil(1024 * 1024 * 1024))
        .map_err(|_| "Image is too large")?
        .max(1);
    let required_disk =
        u64::from(disk_gb) * 1024 * 1024 * 1024 * u64::from(deployment.spec.replicas);
    state
        .ensure_storage_capacity(state.storage.default_id(), required_disk)
        .map_err(|error| format!("Not enough disk space for every replica: {error}"))?;
    for index in 0..deployment.spec.replicas {
        phase(
            state,
            deployment,
            ServicePhase::Starting,
            format!(
                "Creating replica {} of {}",
                index + 1,
                deployment.spec.replicas
            ),
        )
        .await?;
        let host_port = deployment.request.host_port + index;
        let request = CreateVmRequest {
            name: format!(
                "ms-{}-{}-{index}",
                deployment.service_id.simple(),
                &deployment.id.simple().to_string()[..8]
            ),
            template: alias.clone(),
            ram: deployment.spec.ram,
            cpu: deployment.spec.cpu,
            disk_gb,
            egress_policy: EgressPolicy::Internet,
            micro_network_id: network_id,
            storage_root: None,
            shell_ids: vec![],
            env: deployment.spec.env.clone(),
            port_forwards: vec![PortForward {
                host_port,
                guest_port: deployment.spec.port,
                protocol: PortProtocol::Tcp,
            }],
        };
        let (_, Json(vm)) = vms::create_vm(
            State(state.clone()),
            Extension(RequestId(deployment.id)),
            ValidatedJson(request),
        )
        .await
        .map_err(|_| {
            "Could not create replica; check capacity, network and host port availability"
                .to_owned()
        })?;
        deployment.instances.push(ServiceInstance {
            vm_id: vm.id,
            ipv4: vm.ipv4.unwrap_or_default(),
            host_port,
            state: vm.state,
            healthy: false,
        });
        // Ownership is durable before booting anything.
        save(state, deployment).await?;
        let vm_id = vm.id;
        let Json(vm) = vms::start_vm(
            State(state.clone()),
            Extension(RequestId(deployment.id)),
            Path(vm.id.to_string()),
        )
        .await
        .map_err(|_| {
            state
                .vms
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .get(&vm_id)
                .and_then(|vm| {
                    vm.startup_timeline
                        .iter()
                        .rev()
                        .find_map(|step| step.detail.clone())
                })
                .unwrap_or_else(|| "MicroVM boot failed".into())
        })?;
        if vm.state != VmState::Running {
            return Err("MicroVM exited during startup".into());
        }
        let instance = deployment
            .instances
            .last_mut()
            .expect("replica was recorded");
        instance.state = vm.state;
        instance.ipv4 = vm.ipv4.ok_or("Running MicroVM has no IPv4 address")?;
        phase(
            state,
            deployment,
            ServicePhase::CheckingHealth,
            format!("Waiting for replica {} app health", index + 1),
        )
        .await?;
        let instance = deployment
            .instances
            .last_mut()
            .expect("replica was recorded");
        wait_healthy(&deployment.spec, instance).await?;
        instance.healthy = true;
        save(state, deployment).await?;
    }
    phase(
        state,
        deployment,
        ServicePhase::Running,
        "All replicas answered the app health check",
    )
    .await
}

/// Reuse the `services` network or create it on a free subnet in `172.30.0.0/16`.
async fn default_network(state: &AppState, request_id: Uuid) -> Result<Uuid, String> {
    let store = state.store.clone();
    let networks = database(request_id, move || store.list_micro_networks())
        .await
        .map_err(|_| "Could not read networks")?;
    if let Some(network) = networks.iter().find(|network| network.name == "services") {
        return Ok(network.id);
    }
    let cidr = (0..256)
        .map(|i| format!("172.30.{i}.0/24"))
        .find(|cidr| {
            let candidate =
                crate::ipam::SubnetSpec::parse(Uuid::nil(), cidr).expect("valid candidate CIDR");
            !networks
                .iter()
                .filter_map(|network| {
                    crate::ipam::SubnetSpec::parse(network.id, &network.subnet_cidr)
                })
                .any(|other| candidate.overlaps(&other))
        })
        .ok_or("No free default network CIDR; choose a MicroNetwork explicitly")?;
    let (_, Json(network)) = micro_networks::create_micro_network(
        State(state.clone()),
        Extension(RequestId(request_id)),
        ValidatedJson(CreateMicroNetworkRequest {
            name: "services".into(),
            subnet_cidr: cidr,
            internet_enabled: true,
            uplink: None,
            ipv6_cidr: None,
            ipv6_address_mode: None,
        }),
    )
    .await
    .map_err(
        |_| "Could not create the services network; check the helper or choose a MicroNetwork",
    )?;
    Ok(network.id)
}

/// Require memory for every replica's VM overhead and a 256 MiB host reserve.
async fn check_capacity(state: &AppState, spec: &MicroServiceSpec) -> Result<(), String> {
    let Json(host) = crate::handlers::network::get_host_status(State(state.clone())).await;
    let required = u64::from(
        crate::resource_limits::ResourceLimits::for_vm(spec.ram, spec.cpu).memory_max_mib,
    ) * u64::from(spec.replicas);
    if required + 256 > host.memory_available_mib {
        return Err(format!(
            "Not enough host memory: replicas need {required} MiB plus a 256 MiB host reserve"
        ));
    }
    Ok(())
}

/// Build a direct HTTP probe client with no proxy, no redirects and a two-second timeout.
fn health_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .expect("health HTTP client")
}

/// Check for an HTTP success response at the guest's application health endpoint.
async fn healthy(
    client: &reqwest::Client,
    spec: &MicroServiceSpec,
    instance: &ServiceInstance,
) -> bool {
    let Ok(ip) = instance.ipv4.parse::<std::net::Ipv4Addr>() else {
        return false;
    };
    client
        .get(format!("http://{ip}:{}{}", spec.port, spec.health_path))
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

/// Retry health probes with one-second pauses until the post-boot startup timeout.
async fn wait_healthy(spec: &MicroServiceSpec, instance: &ServiceInstance) -> Result<(), String> {
    let client = health_client();
    tokio::time::timeout(
        Duration::from_secs(u64::from(spec.startup_timeout)),
        async {
            loop {
                if healthy(&client, spec, instance).await {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        },
    )
    .await
    .map_err(|_| {
        format!(
            "App health check {} failed within {} seconds",
            spec.health_path, spec.startup_timeout
        )
    })
}

/// Refresh the response snapshot and mark missing or unhealthy running replicas degraded.
async fn refresh_instances(state: &AppState, deployment: &mut MicroServiceDeployment) {
    {
        let vms = state
            .vms
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        for instance in &mut deployment.instances {
            instance.state = vms
                .get(&instance.vm_id)
                .map_or(VmState::Stopped, |vm| vm.state);
            instance.healthy = false;
        }
    }
    if matches!(
        deployment.phase,
        ServicePhase::Running | ServicePhase::Degraded
    ) {
        let client = health_client();
        let checks = join_all(deployment.instances.iter().map(|instance| async {
            instance.state == VmState::Running && healthy(&client, &deployment.spec, instance).await
        }))
        .await;
        for (instance, healthy) in deployment.instances.iter_mut().zip(checks) {
            instance.healthy = healthy;
        }
        if deployment.instances.len() != usize::from(deployment.spec.replicas)
            || deployment
                .instances
                .iter()
                .any(|instance| !instance.healthy)
        {
            deployment.phase = ServicePhase::Degraded;
        }
    }
}

/// Drain on Stop, then delete only owned VMs, persisting each successful removal.
/// Retain unremoved identifiers if cleanup fails so a later Stop can retry.
async fn cleanup(state: &AppState, deployment: &mut MicroServiceDeployment) -> Result<(), String> {
    if deployment.phase == ServicePhase::Stopping && deployment.spec.drain_timeout > 0 {
        let mut draining = false;
        for instance in &deployment.instances {
            let running = state
                .vms
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .get(&instance.vm_id)
                .is_some_and(|vm| vm.state == VmState::Running);
            if !running {
                continue;
            }
            let _ = vms::update_vm_port_forwards(
                State(state.clone()),
                Extension(RequestId(deployment.id)),
                Path(instance.vm_id.to_string()),
                ValidatedJson(firecrab_api_types::UpdateVmPortForwardsRequest {
                    port_forwards: vec![],
                }),
            )
            .await
            .map_err(|_| format!("Could not withdraw host ports for {}", instance.vm_id))?;
            draining = true;
        }
        if draining {
            deployment.log.push(format!(
                "Withdrew host ports; draining existing requests for {} seconds",
                deployment.spec.drain_timeout
            ));
            save(state, deployment).await?;
            tokio::time::sleep(Duration::from_secs(u64::from(
                deployment.spec.drain_timeout,
            )))
            .await;
        }
    }
    while let Some(instance) = deployment.instances.last() {
        let vm_id = instance.vm_id;
        let current = state
            .vms
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(&vm_id)
            .map(|vm| vm.state);
        if current == Some(VmState::Running) {
            let _ = vms::stop_vm(
                State(state.clone()),
                Extension(RequestId(deployment.id)),
                Path(vm_id.to_string()),
            )
            .await
            .map_err(|_| format!("Could not stop MicroVM {vm_id}"))?;
        }
        if current.is_some() {
            vms::delete_vm(
                State(state.clone()),
                Extension(RequestId(deployment.id)),
                Path(vm_id.to_string()),
            )
            .await
            .map_err(|_| format!("Could not delete MicroVM {vm_id}; retry Stop"))?;
        }
        deployment.instances.pop();
        save(state, deployment).await?;
    }
    Ok(())
}

/// VM startup reconciliation has already re-adopted live processes. Never invent
/// success for an operation the previous API process did not finish.
pub(crate) async fn recover(state: &AppState) {
    let store = state.store.clone();
    let Ok(services) = database(Uuid::new_v4(), move || store.list_micro_services()).await else {
        return;
    };
    for service in services {
        let store = state.store.clone();
        let Ok(Some(mut deployment)) =
            database(Uuid::new_v4(), move || store.service_deployment(service.id)).await
        else {
            continue;
        };
        if deployment.phase.is_busy() {
            deployment.phase = ServicePhase::Failed;
            deployment.error = Some(
                "API restart interrupted the operation; use Stop to clean up remaining instances"
                    .into(),
            );
            let _ = save(state, &deployment).await;
        }
    }
}

#[cfg(test)]
mod tests;
