//! Egress policy selection for VM network isolation.
//!
//! The API never hands the privileged helper a raw CIDR. It selects one of a
//! fixed set of named egress policy IDs; the helper resolves that ID against
//! its own root-owned configuration into concrete nftables rules.
//!
//! [`EgressPolicy`] itself lives in `firecrab-api-types` (the shared wire
//! type crate, same as `StartupStep`/`VmState`) since it's also a field on
//! `CreateVmRequest`/`UpdateVmResourcesRequest`/`VmResponse` — re-exported
//! here so existing `crate::network_policy::EgressPolicy` call sites don't
//! need to change.
pub use firecrab_api_types::EgressPolicy;
