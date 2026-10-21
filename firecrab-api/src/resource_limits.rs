//! Host-side ceilings for a VM's systemd unit.
//!
//! A guest cannot use more than its configured RAM and vCPUs, but the
//! processes that run it can: Firecracker's device emulation, the shim, the
//! kernel's page tables for the guest and the host's page cache for its disk
//! all count against the unit. The ceilings sit above what a healthy VM uses,
//! so they only bite on a runaway VMM, and keep it from taking the host (and
//! the API and the other VMs with it) down.

/// The least memory allowance on top of the guest's RAM, in MiB.
const MEMORY_ALLOWANCE_FLOOR_MIB: u32 = 256;
/// Beyond the floor, the allowance is this fraction of the guest's RAM (1/8).
const MEMORY_ALLOWANCE_DIVISOR: u32 = 8;

/// What the helper applies to one VM's unit (`MemoryMax`, `CPUQuota`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResourceLimits {
    /// Memory ceiling in MiB.
    pub(crate) memory_max_mib: u32,
    /// CPU ceiling as a percentage of one core.
    pub(crate) cpu_quota_percent: u32,
}

impl ResourceLimits {
    /// The ceilings for a VM with `ram_mib` of guest RAM and `vcpus` vCPUs.
    ///
    /// Memory is the guest's RAM plus an eighth of it, at least 256 MiB, for
    /// Firecracker, the shim, page tables and cache. CPU is one core per vCPU
    /// plus one for Firecracker's own device-emulation thread.
    pub(crate) fn for_vm(ram_mib: u32, vcpus: u8) -> Self {
        let allowance = (ram_mib / MEMORY_ALLOWANCE_DIVISOR).max(MEMORY_ALLOWANCE_FLOOR_MIB);
        Self {
            memory_max_mib: ram_mib.saturating_add(allowance),
            cpu_quota_percent: (u32::from(vcpus) + 1) * 100,
        }
    }
}

#[cfg(test)]
mod tests {
    use firecrab_helper_protocol::network::{UNIT_CPU_QUOTA_PERCENT, UNIT_MEMORY_MAX_MIB};

    use super::*;

    #[test]
    fn a_typical_guest_gets_the_floor_allowance_and_one_extra_core() {
        // `scripts/ci-qa-lifetime.sh` checks these same numbers for its
        // 512 MiB, one-vCPU VM through `systemctl show`.
        assert_eq!(
            ResourceLimits::for_vm(512, 1),
            ResourceLimits {
                memory_max_mib: 768,
                cpu_quota_percent: 200,
            }
        );
    }

    #[test]
    fn the_smallest_guest_still_gets_the_floor_allowance() {
        assert_eq!(ResourceLimits::for_vm(128, 1).memory_max_mib, 384);
    }

    #[test]
    fn a_large_guest_gets_an_eighth_of_its_ram_on_top() {
        assert_eq!(
            ResourceLimits::for_vm(4096, 4),
            ResourceLimits {
                memory_max_mib: 4608,
                cpu_quota_percent: 500,
            }
        );
    }

    #[test]
    fn every_guest_the_api_accepts_stays_inside_what_the_helper_accepts() {
        // The API accepts 128 MiB to 32 GiB in powers of two and 1 to 32 vCPUs
        // (`handlers::vms`).
        let mut ram = 128;
        while ram <= 32_768 {
            for vcpus in 1..=32 {
                let limits = ResourceLimits::for_vm(ram, vcpus);
                assert!(
                    UNIT_MEMORY_MAX_MIB.contains(&limits.memory_max_mib),
                    "{ram} MiB: {limits:?}"
                );
                assert!(
                    UNIT_CPU_QUOTA_PERCENT.contains(&limits.cpu_quota_percent),
                    "{vcpus} vCPUs: {limits:?}"
                );
                // The ceiling sits above what the guest itself may use.
                assert!(limits.memory_max_mib > ram, "{ram} MiB: {limits:?}");
                assert!(
                    limits.cpu_quota_percent > u32::from(vcpus) * 100,
                    "{vcpus} vCPUs: {limits:?}"
                );
            }
            ram *= 2;
        }
    }
}
