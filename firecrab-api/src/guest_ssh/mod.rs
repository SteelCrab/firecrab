//! Per-VM SSH operator key and guest host key.
//!
//! Layout:
//! - [`keys`] — host paths, `ssh-keygen`, fingerprint
//! - [`service`] — `services.d/sshd` script and sshd_config drop-in
//! - [`install`] — write keys into the guest disk after specialize
//!
//! OCI guests run busybox as PID 1, so systemd never starts `sshd`. The
//! operator key lives on the host under `{vms}/{id}/ssh/` (not SQLite).
//! The public half is injected into `/root/.ssh/authorized_keys` on every
//! start. Host keys are generated once on the host and re-injected after
//! [`crate::rootfs::specialize_guest`] strips template-shared ones.

mod install;
pub mod keys;
pub mod service;
pub mod verify;

pub use install::install_on_guest;
pub use keys::{
    VmSshPaths, ensure_operator_key, host_fingerprint, pem_filename, relocate_ssh_artifacts,
};
pub use service::{GUEST_SSHD_SERVICE, sshd_service_script};
