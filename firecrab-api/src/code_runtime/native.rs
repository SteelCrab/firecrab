use super::{Driver, directory};
use crate::{
    ai_sandbox::guest,
    handlers::vms,
    model::{VmPurpose, VmRecord},
    persistence::code_runtime::Record,
    server::RequestId,
    state::AppState,
};
use CodeRuntimeFailure as Failure;
use axum::{
    Extension,
    extract::{Path, State},
};
use firecrab_api_types::*;
use std::{collections::BTreeMap, time::Duration};
use uuid::Uuid;

pub(crate) struct Native {
    pub state: AppState,
}

pub(crate) fn request(record: &Record) -> CreateVmRequest {
    CreateVmRequest {
        name: format!("code-{}", record.runtime.workspace_id.simple()),
        template: record.runtime.placement.template.name.clone(),
        cpu: record.spec.resources.cpu,
        ram: record.spec.resources.ram,
        disk_gb: record.spec.resources.disk_gb,
        micro_network_id: record.runtime.placement.micro_network_id,
        storage_root: Some(record.runtime.placement.storage_root.clone()),
        egress_policy: EgressPolicy::Internet,
        security_group_id: None,
        block_storages: vec![],
        shell_ids: vec![],
        port_forwards: vec![],
        env: BTreeMap::new(),
    }
}

impl Native {
    fn vm(&self, record: &Record) -> Result<Option<VmRecord>, Failure> {
        let vm = self
            .state
            .vms
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&record.runtime.vm_id)
            .cloned();
        if let Some(vm) = &vm {
            let req = request(record);
            let pin = &record.runtime.placement.template;
            if vm.purpose != VmPurpose::Instance
                || vm.name != req.name
                || vm.cpu != req.cpu
                || vm.ram != req.ram
                || vm.disk_gb != req.disk_gb
                || vm.template != pin.name
                || vm.template_version != pin.version
                || vm.template_kernel_sha256 != pin.kernel_sha256
                || vm.template_rootfs_sha256 != pin.rootfs_sha256
                || vm.template_boot_args_sha256 != pin.boot_args_sha256
                || vm.micro_network_id != req.micro_network_id
                || vm.storage_root != record.runtime.placement.storage_root
                || vm.egress_policy != EgressPolicy::Internet
                || vm.security_group_id.is_some()
                || !vm.env.is_empty()
                || self
                    .state
                    .store
                    .vm_has_service_owner(vm.id)
                    .map_err(|_| Failure::OwnershipChanged)?
                || self
                    .state
                    .store
                    .cluster_worker(vm.id)
                    .map_err(|_| Failure::OwnershipChanged)?
                    .is_some()
            {
                return Err(Failure::OwnershipChanged);
            }
        }
        Ok(vm)
    }
}

impl Driver for Native {
    async fn create(&self, record: &Record) -> Result<(), Failure> {
        if self.vm(record)?.is_some() {
            if !record.vm_created {
                vms::resume_cluster_create(
                    &self.state,
                    record.runtime.vm_id,
                    RequestId(Uuid::new_v4()),
                )
                .await
                .map_err(|_| Failure::CreationFailed)?;
            }
            return Ok(());
        }
        if record.vm_created {
            return Err(Failure::OwnershipChanged);
        }
        super::guest::repository(&self.state, record).await?;
        let pin = &record.runtime.placement.template;
        let exact = self
            .state
            .templates
            .resolve_version(&pin.name, &pin.version)
            .ok_or(Failure::CreationFailed)?;
        if exact.kernel.sha256() != pin.kernel_sha256
            || exact.rootfs.sha256() != pin.rootfs_sha256
            || exact.boot_args_sha256() != pin.boot_args_sha256
            || exact.initrd.as_ref().map(|a| a.sha256().to_owned()) != pin.initrd_sha256
        {
            return Err(Failure::OwnershipChanged);
        }
        let _ = vms::create_vm_from_template(
            self.state.clone(),
            RequestId(Uuid::new_v4()),
            request(record),
            record.runtime.vm_id,
            Some(exact),
        )
        .await
        .map_err(|_| Failure::CreationFailed)?;
        Ok(())
    }

    async fn start(&self, record: &Record) -> Result<(), Failure> {
        let vm = self.vm(record)?.ok_or(Failure::OwnershipChanged)?;
        match vm.state {
            VmState::Running => {}
            VmState::Created | VmState::Stopped | VmState::Error => {
                let pin = &record.runtime.placement.template;
                let exact = self
                    .state
                    .templates
                    .resolve_version(&pin.name, &pin.version)
                    .ok_or(Failure::StartupFailed)?;
                if exact.initrd.as_ref().map(|a| a.sha256().to_owned()) != pin.initrd_sha256 {
                    return Err(Failure::OwnershipChanged);
                }
                let _ = vms::start_vm(
                    State(self.state.clone()),
                    Extension(RequestId(Uuid::new_v4())),
                    Path(vm.id.to_string()),
                )
                .await
                .map_err(|_| Failure::StartupFailed)?;
            }
            _ => return Err(Failure::StartupFailed),
        }
        if self
            .vm(record)?
            .is_none_or(|vm| vm.state != VmState::Running)
        {
            return Err(Failure::StartupFailed);
        }
        Ok(())
    }

    async fn prepare(&self, record: &Record) -> Result<(Uuid, String), Failure> {
        self.vm(record)?.ok_or(Failure::OwnershipChanged)?;
        let binding = guest::binding(&self.state, record.runtime.vm_id)
            .map_err(|_| Failure::GuestUnavailable)?;
        let dir = directory(&self.state, record.runtime.workspace_id);
        // Boot completion precedes SSH readiness. Retry only the read-only capability probe.
        let ready = tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                if guest::capabilities(&self.state, &binding, &dir)
                    .await
                    .is_ok()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        })
        .await;
        if ready.is_err() {
            return Err(Failure::GuestUnavailable);
        }
        super::guest::prepare(&self.state, record, &binding, &dir).await
    }

    async fn stop(&self, record: &Record) -> Result<(), Failure> {
        let Some(vm) = self.vm(record)? else {
            return Err(Failure::OwnershipChanged);
        };
        if matches!(
            vm.state,
            VmState::Created | VmState::Stopped | VmState::Error
        ) {
            return Ok(());
        }
        if vm.state != VmState::Running {
            return Err(Failure::StopFailed);
        }
        let binding = guest::binding(&self.state, vm.id).map_err(|_| Failure::SyncFailed)?;
        guest::sync_filesystems(
            &self.state,
            &binding,
            &directory(&self.state, record.runtime.workspace_id),
        )
        .await
        .map_err(|_| Failure::SyncFailed)?;
        let _ = vms::stop_vm(
            State(self.state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(vm.id.to_string()),
        )
        .await
        .map_err(|_| Failure::StopFailed)?;
        if self
            .vm(record)?
            .is_none_or(|vm| vm.state != VmState::Stopped)
        {
            return Err(Failure::StopFailed);
        }
        Ok(())
    }
}
