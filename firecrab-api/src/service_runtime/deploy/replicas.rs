//! Pinned replica creation and surge capacity.

use super::*;

pub(super) fn target(
    deployment: &MicroServiceDeployment,
    restoring: bool,
) -> &MicroServiceDeployment {
    if restoring {
        &deployment.update.as_ref().expect("journal").previous
    } else {
        deployment
    }
}
pub(super) fn target_mut(
    deployment: &mut MicroServiceDeployment,
    restoring: bool,
) -> &mut MicroServiceDeployment {
    if restoring {
        &mut deployment.update.as_mut().expect("journal").previous
    } else {
        deployment
    }
}
pub(super) fn pinned_reference(deployment: &MicroServiceDeployment) -> Result<String, String> {
    let original =
        ImageReference::parse(&deployment.spec.image).map_err(|_| "Invalid release image")?;
    let digest = deployment
        .digest
        .as_ref()
        .ok_or("Release has no image digest")?;
    Sha256Digest::parse(digest).map_err(|_| "Invalid release image digest")?;
    Ok(format!(
        "{}/{}@{digest}",
        original.registry, original.repository
    ))
}
pub(super) fn template_alias(deployment: &MicroServiceDeployment) -> Result<String, String> {
    let reference = ImageReference::parse(&pinned_reference(deployment)?)
        .map_err(|_| "Invalid pinned release image")?;
    oci::template_name_from_reference(&reference)
        .map(|name| name.alias)
        .map_err(|_| "Could not select the release image".into())
}

pub(super) async fn prepare_template(
    state: &AppState,
    deployment: &mut MicroServiceDeployment,
    restoring: bool,
) -> Result<(), String> {
    let current = target(deployment, restoring).clone();
    if current.digest.is_none() {
        phase(
            state,
            deployment,
            ServicePhase::Importing,
            "Resolving Deploy image to an immutable digest",
        )
        .await?;
        let reference =
            ImageReference::parse(&current.spec.image).map_err(|_| "Invalid Deploy image")?;
        let digest = if let ImageVersion::Digest(digest) = &reference.version {
            // A verified local pin can be reused while the registry is offline.
            // Admission still requires Auth; a cache miss uses authenticated import.
            digest.to_string()
        } else {
            let auth = require_image_auth(state, &current.spec.image, deployment.id)
                .await
                .map_err(|_| "Image account is no longer connected in Auth")?;
            let credential = auth.registry_credential().await.map_err(str::to_owned)?;
            oci::resolve(
                &reference,
                Architecture::HOST,
                oci::is_loopback_registry(&reference.registry),
                Some(credential),
            )
            .await
            .map_err(|_| "Could not resolve Deploy image; check Auth and registry access")?
            .digest
        };
        target_mut(deployment, restoring).digest = Some(digest);
        save(state, deployment).await?;
    }
    let current = target(deployment, restoring);
    let alias = template_alias(current)?;
    if let Some(template) = state.templates.resolve_alias(&alias) {
        if Some(&template.version) != current.digest.as_ref() {
            return Err("Release image cache has a different digest".into());
        }
        return Ok(());
    }
    if cache::restore_cached(
        state,
        &alias,
        current.digest.as_deref().ok_or("Release digest missing")?,
    )
    .await?
    {
        return Ok(());
    }
    let reference =
        ImageReference::parse(&pinned_reference(current)?).map_err(|_| "Invalid release image")?;
    let auth = require_image_auth(state, &current.spec.image, deployment.id)
        .await
        .map_err(|_| "Release image is unavailable; restore the matching Auth connection")?;
    let credential = auth.registry_credential().await.map_err(str::to_owned)?;
    match state
        .oci_imports
        .begin_with(&alias, "Deploy image import started")
    {
        Ok(_) => {
            oci::run_deploy_import(
                state.oci_imports.clone(),
                (*state.templates).clone(),
                reference,
                alias.clone(),
                Some(credential),
                state.store.clone(),
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
            .map_err(|_| "Timed out waiting for Deploy image")?;
        }
        Err(_) => return Err("Could not start Deploy image import".into()),
    }
    if state.oci_imports.snapshot(&alias).status != ImageInstallStatus::Succeeded {
        return Err("Deploy image import failed; inspect Images".into());
    }
    Ok(())
}

pub(super) async fn check_surge(
    state: &AppState,
    spec: &MicroServiceSpec,
    replicas: u16,
) -> Result<(), String> {
    let mut surge = spec.clone();
    surge.replicas = replicas;
    check_capacity(state, &surge).await?;
    let cpus = std::thread::available_parallelism()
        .map_err(|_| "Could not inspect host CPU capacity")?
        .get() as u64;
    let running = state
        .vms
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .values()
        .filter(|vm| {
            matches!(
                vm.state,
                VmState::Running | VmState::Starting | VmState::Paused
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let used = running.iter().map(|vm| u64::from(vm.cpu)).sum::<u64>();
    if used + u64::from(spec.cpu) * u64::from(replicas) > cpus {
        return Err(
            "Not enough CPU capacity for Deploy surge; the current release is retained".into(),
        );
    }
    Ok(())
}

pub(super) async fn start_replica(
    state: &AppState,
    deployment: &mut MicroServiceDeployment,
    restoring: bool,
    index: usize,
) -> Result<(), String> {
    let current = target(deployment, restoring).clone();
    let alias = template_alias(&current)?;
    let template = state
        .templates
        .resolve_alias(&alias)
        .ok_or("Pinned release image is unavailable")?;
    let disk_gb = u16::try_from(template.rootfs.length().div_ceil(1024 * 1024 * 1024))
        .map_err(|_| "Release image is too large")?
        .max(1);
    let network = current
        .request
        .micro_network_id
        .ok_or("Running release has no network identity")?;
    if index == current.instances.len() {
        target_mut(deployment, restoring)
            .instances
            .push(ServiceInstance {
                vm_id: Uuid::new_v4(),
                ipv4: String::new(),
                host_port: 0,
                state: VmState::Created,
                healthy: false,
            });
        save(state, deployment).await?;
    }
    let current = target(deployment, restoring).clone();
    let id = current.instances[index].vm_id;
    phase(
        state,
        deployment,
        if restoring {
            ServicePhase::RollingBack
        } else {
            ServicePhase::Starting
        },
        format!("Starting Deploy replica {}", index + 1),
    )
    .await?;
    let vm = state
        .vms
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(&id)
        .cloned();
    if vm.is_none() {
        state
            .ensure_storage_capacity(
                state.storage.default_id(),
                u64::from(disk_gb) * 1024 * 1024 * 1024,
            )
            .map_err(|_| "Not enough disk capacity for Deploy surge")?;
        check_surge(state, &current.spec, 1).await?;
        let request = CreateVmRequest {
            block_storages: vec![],
            name: format!(
                "ms-{}-{}-{}",
                deployment.service_id.simple(),
                &current.id.simple().to_string()[..8],
                &id.simple().to_string()[..8]
            ),
            template: alias,
            ram: current.spec.ram,
            cpu: current.spec.cpu,
            disk_gb,
            egress_policy: EgressPolicy::Internet,
            security_group_id: current.spec.security_group_id,
            micro_network_id: network,
            storage_root: None,
            shell_ids: vec![],
            env: current.spec.env.clone(),
            port_forwards: vec![],
        };
        let _ = vms::create_vm_with_id(state.clone(), RequestId(deployment.id), request, id)
            .await
            .map_err(|_| "Could not create Deploy replica; check capacity and network")?;
    }
    let vm = state
        .vms
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(&id)
        .cloned()
        .ok_or("Reserved Deploy VM is missing")?;
    if vm.template_version != current.digest.clone().ok_or("Release digest missing")?
        || vm.ram != current.spec.ram
        || vm.cpu != current.spec.cpu
        || vm.micro_network_id != network
        || vm.env != current.spec.env
        || vm.security_group_id != current.spec.security_group_id
    {
        return Err("Reserved Deploy VM does not match its recorded release".into());
    }
    let vm = if vm.state == VmState::Running {
        vm
    } else {
        let _ = vms::start_vm(
            State(state.clone()),
            Extension(RequestId(deployment.id)),
            Path(id.to_string()),
        )
        .await
        .map_err(|_| "Deploy MicroVM startup failed")?;
        state
            .vms
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(&id)
            .cloned()
            .ok_or("Deploy VM disappeared during startup")?
    };
    if vm.state != VmState::Running {
        return Err("Deploy VM exited during startup".into());
    }
    let Json(response) = vms::get_vm(
        State(state.clone()),
        Extension(RequestId(deployment.id)),
        Path(id.to_string()),
    )
    .await
    .map_err(|_| "Could not read the Deploy VM address")?;
    let instance = &mut target_mut(deployment, restoring).instances[index];
    instance.state = vm.state;
    instance.ipv4 = response.ipv4.ok_or("Running Deploy VM has no address")?;
    save(state, deployment).await?;
    let current = target(deployment, restoring);
    wait_healthy(&current.spec, &current.instances[index]).await?;
    target_mut(deployment, restoring).instances[index].healthy = true;
    save(state, deployment).await
}

/// Keep headroom for both the forward rollout and a one-at-a-time recovery.
pub(super) fn peak_resources(
    old: u64,
    old_count: u16,
    new: u64,
    new_count: u16,
    strategy: DeployStrategy,
) -> u64 {
    let old_total = old * u64::from(old_count);
    let new_total = new * u64::from(new_count);
    if strategy == DeployStrategy::BlueGreen {
        old_total + new_total
    } else {
        (old_total + new).max(new_total + old)
    }
}

pub(super) async fn check_update_capacity(
    state: &AppState,
    deployment: &MicroServiceDeployment,
) -> Result<(), String> {
    use crate::resource_limits::ResourceLimits;
    let update = deployment.update.as_ref().expect("journal");
    let old = &update.previous.spec;
    let new = &deployment.spec;
    let cpus = std::thread::available_parallelism()
        .map_err(|_| "Could not inspect host CPU capacity")?
        .get() as u64;
    let running = state
        .vms
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .values()
        .filter(|vm| {
            matches!(
                vm.state,
                VmState::Running | VmState::Starting | VmState::Paused
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let used = running.iter().map(|vm| u64::from(vm.cpu)).sum::<u64>();
    let owned = running
        .iter()
        .filter(|vm| {
            update
                .previous
                .instances
                .iter()
                .any(|instance| instance.vm_id == vm.id)
        })
        .collect::<Vec<_>>();
    let owned_cpu = owned.iter().map(|vm| u64::from(vm.cpu)).sum::<u64>();
    let owned_memory = owned
        .iter()
        .map(|vm| u64::from(ResourceLimits::for_vm(vm.ram, vm.cpu).memory_max_mib))
        .sum::<u64>();
    let peak = peak_resources(
        u64::from(old.cpu),
        old.replicas,
        u64::from(new.cpu),
        new.replicas,
        update.strategy,
    );
    if used.saturating_sub(owned_cpu) + peak > cpus {
        return Err(
            "Not enough CPU for Deploy and rollback surge; the active release is retained".into(),
        );
    }
    let old_memory = u64::from(ResourceLimits::for_vm(old.ram, old.cpu).memory_max_mib);
    let new_memory = u64::from(ResourceLimits::for_vm(new.ram, new.cpu).memory_max_mib);
    let extra = peak_resources(
        old_memory,
        old.replicas,
        new_memory,
        new.replicas,
        update.strategy,
    )
    .saturating_sub(owned_memory);
    let Json(host) = crate::handlers::network::get_host_status(State(state.clone())).await;
    if extra + 256 > host.memory_available_mib {
        return Err(format!(
            "Not enough memory for Deploy/rollback: need {extra} MiB extra plus a 256 MiB host reserve"
        ));
    }
    let template = state
        .templates
        .resolve_alias(&template_alias(deployment)?)
        .ok_or("Deploy image is unavailable")?;
    let new_disk = template.rootfs.length().div_ceil(1024 * 1024 * 1024).max(1);
    let (old_disk, owned_disk) = {
        let vms = state
            .vms
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let disks = update
            .previous
            .instances
            .iter()
            .filter_map(|instance| vms.get(&instance.vm_id))
            .map(|vm| u64::from(vm.disk_gb))
            .collect::<Vec<_>>();
        (
            disks.iter().copied().max().unwrap_or(new_disk),
            disks.iter().sum::<u64>(),
        )
    };
    let extra_disk = peak_resources(
        old_disk,
        old.replicas,
        new_disk,
        new.replicas,
        update.strategy,
    )
    .saturating_sub(owned_disk)
        * 1024
        * 1024
        * 1024;
    state
        .ensure_storage_capacity(state.storage.default_id(), extra_disk)
        .map_err(|_| "Not enough disk capacity for Deploy and rollback surge".into())
}
