use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::model::VmRecord;
use crate::rootfs;

const CONFIG_FILE_NAME: &str = "firecracker.json";

#[derive(Debug, Error)]
pub enum FirecrackerError {
    #[error("failed to create VM directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to serialize Firecracker config for {path}: {source}")]
    Serialize {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to write Firecracker config {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Serialize)]
pub struct FirecrackerConfig {
    #[serde(rename = "boot-source")]
    boot_source: BootSource,
    drives: Vec<Drive>,
    #[serde(rename = "machine-config")]
    machine_config: MachineConfig,
}

#[derive(Debug, Serialize)]
struct BootSource {
    kernel_image_path: PathBuf,
    boot_args: String,
}

#[derive(Debug, Serialize)]
struct Drive {
    drive_id: String,
    path_on_host: PathBuf,
    is_root_device: bool,
    is_read_only: bool,
}

#[derive(Debug, Serialize)]
struct MachineConfig {
    vcpu_count: u8,
    mem_size_mib: u32,
}

impl FirecrackerConfig {
    pub fn for_vm(
        vm: &VmRecord,
        kernel_image_path: &Path,
        boot_args: &str,
        rootfs_path: &Path,
    ) -> Self {
        Self {
            boot_source: BootSource {
                kernel_image_path: kernel_image_path.to_owned(),
                boot_args: boot_args.to_owned(),
            },
            drives: vec![Drive {
                drive_id: "rootfs".to_owned(),
                path_on_host: rootfs_path.to_owned(),
                is_root_device: true,
                is_read_only: false,
            }],
            machine_config: MachineConfig {
                vcpu_count: vm.cpu,
                mem_size_mib: vm.ram,
            },
        }
    }
}

/// Renders the VM's Firecracker config to `{vms_dir}/{id}/firecracker.json`,
/// overwriting any previous config. The root drive points at the disk
/// `prepare_rootfs` publishes for the same VM.
pub fn write_config(
    vms_dir: &Path,
    vm: &VmRecord,
    kernel_image_path: &Path,
    boot_args: &str,
) -> Result<PathBuf, FirecrackerError> {
    let vm_dir = vms_dir.join(vm.id.to_string());
    fs::create_dir_all(&vm_dir).map_err(|source| FirecrackerError::CreateDirectory {
        path: vm_dir.clone(),
        source,
    })?;

    let path = vm_dir.join(CONFIG_FILE_NAME);
    let config = FirecrackerConfig::for_vm(
        vm,
        kernel_image_path,
        boot_args,
        &rootfs::rootfs_path(vms_dir, vm.id),
    );
    let json =
        serde_json::to_vec_pretty(&config).map_err(|source| FirecrackerError::Serialize {
            path: path.clone(),
            source,
        })?;
    fs::write(&path, json).map_err(|source| FirecrackerError::Write {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;
    use uuid::Uuid;

    use super::*;
    use crate::model::VmState;

    fn record(cpu: u8, ram: u32) -> VmRecord {
        VmRecord {
            id: Uuid::new_v4(),
            name: "test-vm".to_owned(),
            state: VmState::Created,
            template: "ubuntu-26.04".to_owned(),
            template_version: "ubuntu-26.04-v1".to_owned(),
            template_kernel_sha256: "kernel".to_owned(),
            template_rootfs_sha256: "rootfs".to_owned(),
            template_boot_args_sha256: "args".to_owned(),
            cpu,
            ram,
        }
    }

    #[test]
    fn config_reflects_requested_resources() {
        let directory = tempdir().unwrap();
        let vms_dir = directory.path().join("vms");
        let vm = record(3, 768);

        let path = write_config(&vms_dir, &vm, Path::new("/images/vmlinux"), "console=ttyS0")
            .unwrap();

        assert_eq!(path, vms_dir.join(vm.id.to_string()).join("firecracker.json"));
        let config: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(config["machine-config"]["vcpu_count"], 3);
        assert_eq!(config["machine-config"]["mem_size_mib"], 768);
    }

    #[test]
    fn config_wires_boot_source_and_root_drive() {
        let directory = tempdir().unwrap();
        let vms_dir = directory.path().join("vms");
        let vm = record(1, 512);

        let path = write_config(&vms_dir, &vm, Path::new("/images/vmlinux"), "console=ttyS0")
            .unwrap();

        let config: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(config["boot-source"]["kernel_image_path"], "/images/vmlinux");
        assert_eq!(config["boot-source"]["boot_args"], "console=ttyS0");

        let drive = &config["drives"][0];
        assert_eq!(drive["drive_id"], "rootfs");
        assert_eq!(drive["is_root_device"], true);
        assert_eq!(drive["is_read_only"], false);
        assert_eq!(
            drive["path_on_host"],
            rootfs::rootfs_path(&vms_dir, vm.id).to_str().unwrap()
        );
    }

    #[test]
    fn rewriting_config_overwrites_previous_content() {
        let directory = tempdir().unwrap();
        let vms_dir = directory.path().join("vms");
        let mut vm = record(1, 512);

        write_config(&vms_dir, &vm, Path::new("/images/vmlinux"), "console=ttyS0").unwrap();
        vm.cpu = 2;
        vm.ram = 1024;
        let path = write_config(&vms_dir, &vm, Path::new("/images/vmlinux"), "console=ttyS0")
            .unwrap();

        let config: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(config["machine-config"]["vcpu_count"], 2);
        assert_eq!(config["machine-config"]["mem_size_mib"], 1024);
    }
}
