//! Historical records for legacy validation tests; never compiled into the CLI.
use super::*;

// Frozen pins validate legacy installations. Explicit installs use the TOML.
pub const DEBIAN_ROOTFS_VERSION: &str = "1.26.0.0";
pub const FIRECRACKER_VERSION: &str = "v1.17.0";
pub const FIRECRAB_VERSION: &str = "v0.2.2";

pub(super) const INSTALLER: ArtifactSpec<'static> = ArtifactSpec {
    label: "Firecrab guest installer",
    filename: "install-firecrab-v0.2.2.sh",
    url: "https://github.com/SteelCrab/firecrab/releases/download/v0.2.2/install.sh",
    algorithm: HashAlgorithm::Sha256,
    digest: "af2fb56b92dff1559cdaa449aa025808e0990437c16ccc0bb2b4d558ef50dacb",
};

/// Microsoft's WSL manifest points `wsl --install -d Debian` at these Debian
/// builds, so the managed distribution is the same image users already get.
pub(super) static X86_64: Host<'static> = Host {
    architecture: "x86_64",
    rootfs_version: DEBIAN_ROOTFS_VERSION,
    firecracker_version: FIRECRACKER_VERSION,
    firecrab_version: FIRECRAB_VERSION,
    kernel_version: None,
    distribution: None,
    manifest_id: None,
    debian_architecture: "amd64",
    artifacts: [
        ArtifactSpec {
            label: "Debian 13 WSL rootfs",
            filename: "Debian_WSL_AMD64_v1.26.0.0.wsl",
            url: concat!(
                "https://salsa.debian.org/debian/WSL/-/jobs/9606244/artifacts/raw/",
                "Debian_WSL_AMD64_v1.26.0.0.wsl"
            ),
            algorithm: HashAlgorithm::Sha256,
            digest: "5ec7dc68216e75d1d4d4761474e99d8461a98d316537110314b137122a879e0f",
        },
        ArtifactSpec {
            label: "Firecracker x86_64 release",
            filename: "firecracker-v1.17.0-x86_64.tgz",
            url: "https://github.com/firecracker-microvm/firecracker/releases/download/v1.17.0/firecracker-v1.17.0-x86_64.tgz",
            algorithm: HashAlgorithm::Sha256,
            digest: "06094a1108ae9e82aa4c23a775aa92758f53f1175d422270d9d6162cb9ade558",
        },
        ArtifactSpec {
            label: "Firecrab x86_64 GNU host bundle",
            filename: "firecrab-host-x86_64-gnu.tar.gz",
            url: "https://github.com/SteelCrab/firecrab/releases/download/v0.2.2/firecrab-host-x86_64-gnu.tar.gz",
            algorithm: HashAlgorithm::Sha256,
            digest: "ad59b359ccc6d4f28ca6927339b6157b7684b9d734ded79c639a546d709b5315",
        },
        INSTALLER,
    ],
    kernel_extractor: "extract-vmlinux",
    boot_args: "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw init=/sbin/init",
    workload_exit: "reboot -f",
};

pub(super) static ARM64: Host<'static> = Host {
    architecture: "aarch64",
    rootfs_version: DEBIAN_ROOTFS_VERSION,
    firecracker_version: FIRECRACKER_VERSION,
    firecrab_version: FIRECRAB_VERSION,
    kernel_version: None,
    distribution: None,
    manifest_id: None,
    debian_architecture: "arm64",
    artifacts: [
        ArtifactSpec {
            label: "Debian 13 WSL rootfs",
            filename: "Debian_WSL_ARM64_v1.26.0.0.wsl",
            url: concat!(
                "https://salsa.debian.org/debian/WSL/-/jobs/9606244/artifacts/raw/",
                "Debian_WSL_ARM64_v1.26.0.0.wsl"
            ),
            algorithm: HashAlgorithm::Sha256,
            digest: "09120df4fadc36fb2a0f7298e197785e6f599f12aaf95d43cba07a8ac7fb316b",
        },
        ArtifactSpec {
            label: "Firecracker ARM64 release",
            filename: "firecracker-v1.17.0-aarch64.tgz",
            url: "https://github.com/firecracker-microvm/firecracker/releases/download/v1.17.0/firecracker-v1.17.0-aarch64.tgz",
            algorithm: HashAlgorithm::Sha256,
            digest: "e351ebe4f7a16b5873bbd51005d2e6767103cff4d5ebc829df2d3f95a93e2256",
        },
        ArtifactSpec {
            label: "Firecrab ARM64 GNU host bundle",
            filename: "firecrab-host-aarch64-gnu.tar.gz",
            url: "https://github.com/SteelCrab/firecrab/releases/download/v0.2.2/firecrab-host-aarch64-gnu.tar.gz",
            algorithm: HashAlgorithm::Sha256,
            digest: "2e995ed27d8c19848baaf38c8bf8a2d5d126ab90776738d91790524a8564e982",
        },
        INSTALLER,
    ],
    kernel_extractor: "extract-arm64-image",
    boot_args: "keep_bootcon console=ttyS0 reboot=k panic=1 root=/dev/vda rw init=/sbin/init",
    workload_exit: "poweroff -f",
};

/// The pinned artifacts for the architecture this CLI was built for.
pub fn host() -> Result<&'static Host<'static>, Error> {
    host_for(std::env::consts::ARCH).ok_or(Error::UnsupportedArchitecture(std::env::consts::ARCH))
}

pub(super) fn host_for(architecture: &str) -> Option<&'static Host<'static>> {
    match architecture {
        "x86_64" => Some(&X86_64),
        "aarch64" => Some(&ARM64),
        _ => None,
    }
}
