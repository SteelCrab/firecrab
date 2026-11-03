//! Every VM start verifies its original content before network or runtime effects.

use crate::{
    model::VmRecord,
    templates::{TemplateRegistry, TemplateVersion},
};

/// Exact operator review; none of these values grants a managed writer's authority.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Review {
    pub operation_id: uuid::Uuid,
    pub expected_endpoint_id: uuid::Uuid,
    pub expected_generation: u64,
    pub pin: firecrab_stack::TemplatePin,
}

impl Review {
    pub(crate) fn valid(&self) -> bool {
        !self.operation_id.is_nil()
            && !self.expected_endpoint_id.is_nil()
            && self.expected_generation <= i64::MAX as u64
            && self.pin.valid()
    }
}

/// A mutable registry alias cannot approve new boot content for an existing VM.
pub(crate) fn validate(vm: &VmRecord, template: &TemplateVersion) -> Result<(), String> {
    let observed = template.stack_pin();
    if vm.template != template.name
        || vm.template_version != observed.version
        || vm.template_kernel_sha256 != observed.kernel_sha256
        || vm.template_rootfs_sha256 != observed.rootfs_sha256
        || vm.template_boot_args_sha256 != observed.boot_args_sha256
    {
        return Err(
            "Original VM boot content changed; restore its exact template before starting".into(),
        );
    }
    match &vm.boot_pin {
        Some(approved) if approved == &observed => Ok(()),
        // Old records contain the three hashes above but no initrd evidence.
        // Never derive approval for a present initrd from today's registry.
        None if observed.initrd_sha256.is_none() => Ok(()),
        None => Err(
            "Legacy VM has no original initrd proof; explicit boot-input review is required".into(),
        ),
        Some(_) => Err(
            "Original VM initrd content changed; restore its exact template before starting".into(),
        ),
    }
}

/// Verify every original file even when this VM has no Stack owner or prepared disk.
pub(crate) fn verify(
    vm: &VmRecord,
    registry: &TemplateRegistry,
    template: &TemplateVersion,
) -> Result<(), String> {
    validate(vm, template)?;
    for artifact in std::iter::once(&template.kernel)
        .chain(std::iter::once(&template.rootfs))
        .chain(template.initrd.iter())
    {
        registry
            .open_verified(artifact)
            .map_err(|_| "Original VM boot artifact is unavailable or changed".to_owned())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{handlers::vms::test_support::record, persistence::Store, templates::TemplateSpec};
    use std::{fs, path::Path};
    use tempfile::tempdir;
    use uuid::Uuid;

    fn registry(root: &Path, initrd: bool) -> TemplateRegistry {
        for name in ["kernel", "rootfs", "initrd"] {
            fs::write(root.join(name), name.as_bytes()).unwrap();
        }
        TemplateRegistry::from_specs(
            root,
            [TemplateSpec {
                alias: "ubuntu-rootfs-26.04".into(),
                version: "v1".into(),
                kernel: "kernel".into(),
                rootfs: "rootfs".into(),
                initrd: initrd.then(|| "initrd".into()),
                boot_args: "console=ttyS0".into(),
            }],
        )
        .unwrap()
    }

    fn pinned(template: &TemplateVersion) -> VmRecord {
        let mut vm = record("boot-proof", Uuid::new_v4());
        vm.template_kernel_sha256 = template.kernel.sha256().to_owned();
        vm.template_rootfs_sha256 = template.rootfs.sha256().to_owned();
        vm.template_boot_args_sha256 = template.boot_args_sha256();
        vm.boot_pin = Some(template.stack_pin());
        vm
    }

    #[test]
    fn universal_vm_boot_pin_survives_restart_and_refuses_replacement() {
        let root = tempdir().unwrap();
        let registry = registry(root.path(), true);
        let template = registry.resolve_alias("ubuntu-rootfs-26.04").unwrap();
        let vm = pinned(&template);
        let path = root.path().join("db.sqlite");
        let store = Store::open(&path).unwrap();
        store.insert(&vm).unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        let mut loaded = store.load_all().unwrap().remove(&vm.id).unwrap();
        assert_eq!(loaded.boot_pin, vm.boot_pin);
        verify(&loaded, &registry, &template).unwrap();
        loaded.boot_pin.as_mut().unwrap().initrd_sha256 = None;
        assert!(store.update(&loaded).is_err());
        assert_eq!(store.load_all().unwrap()[&vm.id].boot_pin, vm.boot_pin);
    }

    #[test]
    fn universal_vm_boot_rejects_unrecorded_legacy_initrd() {
        let root = tempdir().unwrap();
        let registry = registry(root.path(), true);
        let template = registry.resolve_alias("ubuntu-rootfs-26.04").unwrap();
        let mut vm = pinned(&template);
        vm.boot_pin = None;
        assert!(validate(&vm, &template).is_err());
    }

    #[test]
    fn universal_vm_boot_legacy_without_initrd_checks_all_original_hashes() {
        let root = tempdir().unwrap();
        let registry = registry(root.path(), false);
        let template = registry.resolve_alias("ubuntu-rootfs-26.04").unwrap();
        let mut vm = pinned(&template);
        vm.boot_pin = None;
        verify(&vm, &registry, &template).unwrap();
        vm.template_kernel_sha256 = "0".repeat(64);
        assert!(verify(&vm, &registry, &template).is_err());
    }

    #[test]
    fn universal_vm_boot_rejects_added_removed_and_retargeted_initrd() {
        let root = tempdir().unwrap();
        let registry = registry(root.path(), true);
        let template = registry.resolve_alias("ubuntu-rootfs-26.04").unwrap();
        let vm = pinned(&template);
        let mut removed = template.as_ref().clone();
        removed.initrd = None;
        assert!(validate(&vm, &removed).is_err());
        let mut absent = pinned(&removed);
        assert!(validate(&absent, &template).is_err());
        absent.boot_pin.as_mut().unwrap().initrd_sha256 = Some("0".repeat(64));
        assert!(validate(&absent, &template).is_err());
    }

    #[test]
    fn universal_vm_boot_verifies_bytes_for_unowned_vms() {
        for changed in ["kernel", "rootfs", "initrd"] {
            let root = tempdir().unwrap();
            let registry = registry(root.path(), true);
            let template = registry.resolve_alias("ubuntu-rootfs-26.04").unwrap();
            let vm = pinned(&template);
            verify(&vm, &registry, &template).unwrap();
            fs::write(root.path().join(changed), b"changed original content").unwrap();
            assert!(verify(&vm, &registry, &template).is_err(), "{changed}");
        }
    }
}
