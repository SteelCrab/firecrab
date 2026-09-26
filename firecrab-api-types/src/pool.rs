//! Warm MicroVM pool wire types (`/api/pools`, issue #291).
//!
//! A pool keeps booted, never-used VMs ready to lease. A released or expired
//! member is stopped, deleted, and replaced by a fresh VM; its disk is never
//! handed to another caller.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{EgressPolicy, VmResponse};

/// Request header that makes `POST /api/pools/{id}/acquire` idempotent: a
/// retry with the same key returns the lease the first call created.
pub const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";

/// Body for `POST /api/pools`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreatePoolRequest {
    /// 1–40 chars, alphanumeric plus `.`/`_`/`-`. Member VMs are named after it.
    pub name: String,
    /// Installed image alias. The pool pins the version it resolves to now,
    /// so every member boots the same image.
    pub template: String,
    /// vCPU count of every member.
    pub cpu: u8,
    /// RAM in MiB of every member.
    pub ram: u32,
    /// Disk capacity in GiB of every member.
    pub disk_gb: u16,
    /// Outbound network posture of every member.
    #[serde(default)]
    pub egress_policy: EgressPolicy,
    /// MicroNetwork every member joins.
    pub micro_network_id: Uuid,
    /// Storage root id; omitted uses the API's default root.
    #[serde(default)]
    pub storage_root: Option<String>,
    /// Members kept booted and unleased.
    pub min_ready: u16,
    /// Ceiling on members in any state, leased and draining included.
    pub max_size: u16,
    /// How long a lease lasts before its VM is deleted and replaced.
    pub lease_ttl_seconds: u32,
}

/// Body for `PATCH /api/pools/{id}`. Omitted fields keep their value; the
/// image and VM spec are fixed for the life of the pool.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdatePoolRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_ready: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_size: Option<u16>,
    /// Applies to leases acquired after the change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_ttl_seconds: Option<u32>,
}

/// Where a pool member is in its single-use life.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PoolMemberState {
    /// Created and booting; not leasable yet.
    Provisioning,
    /// Booted, never leased, and waiting for a caller.
    Ready,
    /// Handed to a caller under an active lease.
    Leased,
    /// Being stopped and deleted. Never leased again.
    Draining,
}

/// One VM owned by a pool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PoolMemberResponse {
    pub vm_id: Uuid,
    pub state: PoolMemberState,
    pub created_at_ms: u64,
}

/// A pool as returned by the list/detail/create/update endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PoolResponse {
    pub id: Uuid,
    pub name: String,
    /// Image alias the pool was created from.
    pub template: String,
    /// Image version every member boots.
    pub template_version: String,
    pub cpu: u8,
    pub ram: u32,
    pub disk_gb: u16,
    pub egress_policy: EgressPolicy,
    pub micro_network_id: Uuid,
    pub storage_root: String,
    pub min_ready: u16,
    pub max_size: u16,
    pub lease_ttl_seconds: u32,
    /// Set by `DELETE`; the pool disappears once its members are gone.
    pub deleting: bool,
    pub members: Vec<PoolMemberResponse>,
    /// Most recent failure creating or booting a member; cleared once a
    /// member becomes ready again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub created_at_ms: u64,
}

/// Where a lease is in its life.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PoolLeaseState {
    /// The caller holds the VM.
    Active,
    /// The caller gave the VM back.
    Released,
    /// The TTL ran out, or the VM stopped while leased.
    Expired,
}

/// A lease on one pool member.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PoolLeaseResponse {
    pub id: Uuid,
    pub pool_id: Uuid,
    pub vm_id: Uuid,
    pub state: PoolLeaseState,
    pub acquired_at_ms: u64,
    pub expires_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<u64>,
    /// The leased VM while it exists: address, hostname, and state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vm: Option<VmResponse>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const NETWORK: &str = "00000000-0000-0000-0000-000000000001";

    #[test]
    fn create_pool_request_defaults_egress_and_storage_when_absent() {
        let request: CreatePoolRequest = serde_json::from_str(&format!(
            r#"{{"name":"ci","template":"alpine-3.24.1","cpu":1,"ram":512,"diskGb":2,
                "microNetworkId":"{NETWORK}","minReady":2,"maxSize":4,"leaseTtlSeconds":600}}"#
        ))
        .unwrap();

        assert_eq!(request.egress_policy, EgressPolicy::Internet);
        assert_eq!(request.storage_root, None);
        assert_eq!((request.min_ready, request.max_size), (2, 4));
        assert_eq!(request.lease_ttl_seconds, 600);
    }

    #[test]
    fn create_pool_request_rejects_unknown_fields() {
        let error = serde_json::from_str::<CreatePoolRequest>(&format!(
            r#"{{"name":"ci","template":"alpine-3.24.1","cpu":1,"ram":512,"diskGb":2,
                "microNetworkId":"{NETWORK}","minReady":2,"maxSize":4,"leaseTtlSeconds":600,
                "templateVersion":"v1"}}"#
        ))
        .unwrap_err();

        assert!(error.to_string().contains("templateVersion"), "{error}");
    }

    #[test]
    fn update_pool_request_accepts_a_partial_body() {
        let request: UpdatePoolRequest = serde_json::from_str(r#"{"minReady":3}"#).unwrap();

        assert_eq!(
            request,
            UpdatePoolRequest {
                min_ready: Some(3),
                ..UpdatePoolRequest::default()
            }
        );
    }

    #[test]
    fn lease_response_uses_camel_case_and_omits_absent_fields() {
        let lease = PoolLeaseResponse {
            id: Uuid::from_u128(1),
            pool_id: Uuid::from_u128(2),
            vm_id: Uuid::from_u128(3),
            state: PoolLeaseState::Active,
            acquired_at_ms: 10,
            expires_at_ms: 20,
            ended_at_ms: None,
            vm: None,
        };

        let json = serde_json::to_value(&lease).unwrap();

        assert_eq!(json["state"], "active");
        assert_eq!(json["expiresAtMs"], 20);
        assert!(json.get("endedAtMs").is_none());
        assert!(json.get("vm").is_none());
    }

    #[test]
    fn member_states_serialize_as_camel_case_words() {
        let states = [
            PoolMemberState::Provisioning,
            PoolMemberState::Ready,
            PoolMemberState::Leased,
            PoolMemberState::Draining,
        ];

        let json = serde_json::to_string(&states).unwrap();

        assert_eq!(json, r#"["provisioning","ready","leased","draining"]"#);
    }
}
