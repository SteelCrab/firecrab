use super::*;
use core::assert_matches;

use crate::m2image_manifest;

fn ubuntu_artifact(architecture: Architecture) -> crate::m2image_manifest::ReleaseArtifact {
    m2image_manifest::image("ubuntu-26.04")
        .expect("ubuntu is a release image")
        .artifacts
        .remove(architecture.as_str())
        .expect("ubuntu publishes this architecture")
}

#[test]
fn kernel_pair_for_x86_64_is_the_ubuntu_vmlinux_without_an_initrd() {
    let pair = boot::kernel_pair_for(Architecture::X86_64).expect("ubuntu publishes x86_64");
    let ubuntu = ubuntu_artifact(Architecture::X86_64);

    assert_eq!(pair.architecture, Architecture::X86_64);
    assert_eq!(pair.source_alias, "ubuntu-26.04");
    assert_eq!(
        pair.kernel.as_os_str(),
        "kernel/vmlinux-ubuntu-26.04-x86_64"
    );
    assert_eq!(pair.initrd, None);
    assert_eq!(pair.boot_args, ubuntu.boot_args);
    assert!(!pair.boot_args.contains("keep_bootcon"));
    assert!(pair.boot_args.contains("console=ttyS0"));
    assert!(pair.boot_args.contains("root=/dev/vda"));
}

#[test]
fn kernel_pair_for_aarch64_is_the_ubuntu_image_without_an_initrd() {
    let pair = boot::kernel_pair_for(Architecture::Aarch64).expect("ubuntu publishes aarch64");
    let ubuntu = ubuntu_artifact(Architecture::Aarch64);

    assert_eq!(pair.architecture, Architecture::Aarch64);
    assert_eq!(pair.source_alias, "ubuntu-26.04");
    assert_eq!(pair.kernel.as_os_str(), "kernel/Image-ubuntu-26.04-aarch64");
    assert_eq!(pair.initrd, None);
    assert_eq!(pair.boot_args, ubuntu.boot_args);
    assert!(pair.boot_args.contains("keep_bootcon"));
    assert!(pair.boot_args.contains("console=ttyS0"));
    assert!(pair.boot_args.contains("root=/dev/vda"));
}

#[test]
fn host_kernel_pair_is_the_pair_for_this_build() {
    let host = boot::host_kernel_pair().expect("a no-initrd host kernel exists");
    let expected = boot::kernel_pair_for(Architecture::HOST).expect("host pair exists");
    assert_eq!(host, expected);
}

fn elf_kernel(machine: u16) -> Vec<u8> {
    let mut header = vec![0_u8; 64];
    header[0..4].copy_from_slice(b"\x7fELF");
    header[4] = 2;
    header[5] = 1;
    header[6] = 1;
    header[16..18].copy_from_slice(&2_u16.to_le_bytes());
    header[18..20].copy_from_slice(&machine.to_le_bytes());
    header
}

fn arm64_image_kernel() -> Vec<u8> {
    let mut header = vec![0_u8; 64];
    header[0..2].copy_from_slice(b"MZ");
    header[56..60].copy_from_slice(&0x644d_5241_u32.to_le_bytes());
    header
}

fn kernel_bytes_for(architecture: Architecture) -> Vec<u8> {
    match architecture {
        Architecture::Aarch64 => arm64_image_kernel(),
        Architecture::X86_64 => elf_kernel(0x3e),
    }
}

fn write_file(path: &std::path::Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create fixture parent");
    }
    std::fs::write(path, bytes).expect("write fixture file");
}

fn packed_ext4(path: std::path::PathBuf) -> OciExt4Image {
    write_file(&path, b"ext4-fixture");
    OciExt4Image {
        path,
        size_bytes: 8 * 1024 * 1024,
        payload_bytes: 64,
        free_bytes: 1024 * 1024,
        toolbox: Sha256Digest::of_bytes(b"toolbox-fixture"),
    }
}

fn install_kernel(image_root: &std::path::Path, bytes: &[u8]) -> std::path::PathBuf {
    let pair = boot::host_kernel_pair().expect("host kernel pair");
    let path = image_root.join(&pair.kernel);
    write_file(&path, bytes);
    pair.kernel
}

#[test]
fn pairing_records_the_host_kernel_and_its_boot_args() {
    let directory = tempfile::tempdir().expect("create fixture directory");
    let image_root = directory.path();
    let expected_kernel = install_kernel(image_root, &kernel_bytes_for(Architecture::HOST));
    let ext4_path = image_root.join("rootfs/oci.ext4");
    let image = packed_ext4(ext4_path.clone());
    let expected = boot::host_kernel_pair().expect("host kernel pair");

    let paired = pair_ext4_with_host_kernel(image, image_root).expect("pair kernel");

    assert_eq!(paired.architecture(), Architecture::HOST);
    assert_eq!(paired.kernel(), expected_kernel.as_path());
    assert_eq!(paired.initrd(), None);
    assert_eq!(paired.boot_args(), expected.boot_args);
    assert_eq!(paired.rootfs().path(), ext4_path.as_path());
    assert_eq!(
        std::fs::read(&ext4_path).expect("ext4 remains"),
        b"ext4-fixture"
    );
    assert!(!image_root.join(".templates.json").exists());
}

#[test]
fn pairing_rejects_a_missing_host_kernel() {
    let directory = tempfile::tempdir().expect("create fixture directory");
    let image_root = directory.path();
    let ext4_path = image_root.join("rootfs/oci.ext4");
    let image = packed_ext4(ext4_path.clone());
    let expected = boot::host_kernel_pair().expect("host kernel pair");

    let error = pair_ext4_with_host_kernel(image, image_root).expect_err("missing kernel");

    match error {
        ResolveError::KernelMissing {
            path,
            hint,
            architecture,
        } => {
            assert_eq!(path, expected.kernel);
            assert_eq!(hint, expected.source_alias);
            assert_eq!(architecture, Architecture::HOST);
        }
        other => panic!("expected KernelMissing, got {other}"),
    }
    assert_eq!(
        std::fs::read(&ext4_path).expect("ext4 remains"),
        b"ext4-fixture"
    );
}

#[test]
fn pairing_rejects_a_kernel_built_for_another_architecture() {
    let directory = tempfile::tempdir().expect("create fixture directory");
    let image_root = directory.path();
    install_kernel(image_root, &kernel_bytes_for(Architecture::HOST.other()));
    let ext4_path = image_root.join("rootfs/oci.ext4");
    let image = packed_ext4(ext4_path);

    let error = pair_ext4_with_host_kernel(image, image_root).expect_err("foreign arch");

    match error {
        ResolveError::KernelArchitectureMismatch { found, host, .. } => {
            assert_eq!(found, Architecture::HOST.other());
            assert_eq!(host, Architecture::HOST);
        }
        other => panic!("expected KernelArchitectureMismatch, got {other}"),
    }
}

#[test]
fn pairing_rejects_an_elf_firecracker_cannot_boot() {
    let directory = tempfile::tempdir().expect("create fixture directory");
    let image_root = directory.path();
    install_kernel(image_root, &elf_kernel(0xf3));
    let image = packed_ext4(image_root.join("rootfs/oci.ext4"));

    let error = pair_ext4_with_host_kernel(image, image_root).expect_err("riscv");

    match error {
        ResolveError::UnsupportedKernelArchitecture { machine, .. } => {
            assert_eq!(machine, 0xf3);
        }
        other => panic!("expected UnsupportedKernelArchitecture, got {other}"),
    }
}

#[test]
fn pairing_does_not_follow_a_kernel_symlink() {
    let directory = tempfile::tempdir().expect("create fixture directory");
    let image_root = directory.path();
    let pair = boot::host_kernel_pair().expect("host kernel pair");
    let target = directory.path().join("elsewhere");
    write_file(&target, &kernel_bytes_for(Architecture::HOST));
    let kernel_path = image_root.join(&pair.kernel);
    std::fs::create_dir_all(kernel_path.parent().unwrap()).expect("create kernel parent");
    std::os::unix::fs::symlink(&target, &kernel_path).expect("symlink kernel");
    let image = packed_ext4(image_root.join("rootfs/oci.ext4"));

    let error = pair_ext4_with_host_kernel(image, image_root).expect_err("symlink");

    assert_matches!(
        error,
        ResolveError::KernelIo { .. },
        "expected KernelIo for a kernel path that is a symlink, got {error}"
    );
}

#[test]
fn pairing_rejects_an_unclassifiable_kernel() {
    let directory = tempfile::tempdir().expect("create fixture directory");
    let image_root = directory.path();
    install_kernel(image_root, b"not-a-kernel");
    let image = packed_ext4(image_root.join("rootfs/oci.ext4"));

    let error = pair_ext4_with_host_kernel(image, image_root).expect_err("garbage");

    match error {
        ResolveError::KernelUnrecognized { path } => {
            assert_eq!(path, boot::host_kernel_pair().expect("host pair").kernel);
        }
        other => panic!("expected KernelUnrecognized, got {other}"),
    }
}

#[test]
fn kernel_pair_skips_kernels_that_need_an_initrd() {
    for architecture in [Architecture::X86_64, Architecture::Aarch64] {
        let pair = boot::kernel_pair_for(architecture).expect("a no-initrd kernel exists");
        for alias in ["alpine-3.24.1", "rocky-9.8"] {
            let artifact = m2image_manifest::image(alias)
                .expect("release image")
                .artifacts
                .remove(architecture.as_str())
                .expect("release image publishes this architecture");
            assert!(
                artifact.initrd.is_some(),
                "{alias}/{architecture} must remain an initrd kernel so this test stays meaningful"
            );
            assert_ne!(pair.kernel, std::path::PathBuf::from(artifact.kernel));
            assert_ne!(pair.source_alias, alias);
        }
    }
}
