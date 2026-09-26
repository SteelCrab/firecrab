//! `firecrab pool`: warm MicroVM pools and their leases (issue #291).

use std::fmt::Write;

use firecrab_api_types::{
    CreatePoolRequest, PoolLeaseResponse, PoolLeaseState, PoolMemberState, PoolResponse,
    UpdatePoolRequest,
};
use serde::Serialize;
use uuid::Uuid;

use crate::api_client::{ApiClient, ApiError};
use crate::vm::EgressArg;

/// Pool operations backed by firecrab-api.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// List every pool with its member counts.
    List {
        /// Emit the API response as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Create a pool that keeps booted MicroVMs ready to lease.
    Create {
        /// Pool name; member VMs are named after it.
        #[arg(long)]
        name: String,
        /// Installed image alias; the pool pins its current version.
        #[arg(long)]
        template: String,
        /// MicroNetwork UUID every member joins.
        #[arg(long)]
        network: Uuid,
        /// Members kept booted and unleased.
        #[arg(long)]
        min_ready: u16,
        /// Most members at once, leased and draining included.
        #[arg(long)]
        max_size: u16,
        /// Seconds a lease lasts before its VM is deleted and replaced.
        #[arg(long, default_value_t = 600)]
        lease_ttl: u32,
        /// Number of virtual CPUs per member.
        #[arg(long, default_value_t = 1)]
        cpu: u8,
        /// Memory in MiB per member.
        #[arg(long, default_value_t = 512)]
        ram: u32,
        /// Disk capacity in GiB per member.
        #[arg(long, default_value_t = 2)]
        disk_gb: u16,
        /// Outbound network posture of every member.
        #[arg(long, value_enum, default_value = "internet")]
        egress: EgressArg,
        /// Storage root id; omitted uses the API's default root.
        #[arg(long)]
        storage_root: Option<String>,
        /// Emit the created pool as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show one pool and its members.
    Show {
        /// Pool UUID or name.
        pool: String,
        /// Emit the pool as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Change a pool's sizing or lease TTL.
    Update {
        /// Pool UUID or name.
        pool: String,
        #[arg(long)]
        min_ready: Option<u16>,
        #[arg(long)]
        max_size: Option<u16>,
        /// Applies to leases acquired after the change.
        #[arg(long)]
        lease_ttl: Option<u32>,
        /// Emit the pool as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Delete a pool and its members once no lease is active.
    Delete {
        /// Pool UUID or name.
        pool: String,
        /// Emit the pool as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Lease one ready VM from a pool.
    Acquire {
        /// Pool UUID or name.
        pool: String,
        /// Retrying with the same key returns the same lease instead of a
        /// second VM.
        #[arg(long)]
        idempotency_key: Option<String>,
        /// Emit the lease as JSON.
        #[arg(long)]
        json: bool,
    },
    /// List a pool's leases, newest first.
    Leases {
        /// Pool UUID or name.
        pool: String,
        /// Emit the API response as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Give a leased VM back; it is deleted and replaced, never reused.
    Release {
        /// Pool UUID or name.
        pool: String,
        /// Lease UUID.
        lease: Uuid,
        /// Emit the ended lease as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// Errors produced while executing a pool command.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// REST API operation failure.
    #[error(transparent)]
    Api(#[from] ApiError),
    /// Name lookup found no matching pool.
    #[error("no pool named {0:?}")]
    PoolNotFound(String),
}

/// Executes one `firecrab pool` command through the REST API.
pub fn run(client: &ApiClient, command: Command) -> Result<(), Error> {
    match command {
        Command::List { json } => {
            let pools: Vec<PoolResponse> = client.get("/api/pools")?;
            print_output(json, &pools, format_list_human(&pools));
        }
        Command::Create {
            name,
            template,
            network,
            min_ready,
            max_size,
            lease_ttl,
            cpu,
            ram,
            disk_gb,
            egress,
            storage_root,
            json,
        } => {
            let request = CreatePoolRequest {
                name,
                template,
                cpu,
                ram,
                disk_gb,
                egress_policy: egress.into(),
                micro_network_id: network,
                storage_root,
                min_ready,
                max_size,
                lease_ttl_seconds: lease_ttl,
            };
            let pool: PoolResponse = client.post("/api/pools", &request)?;
            print_output(json, &pool, format_pool_human(&pool));
        }
        Command::Show { pool, json } => {
            let id = resolve_pool(client, &pool)?;
            let pool: PoolResponse = client.get(&format!("/api/pools/{id}"))?;
            print_output(json, &pool, format_pool_human(&pool));
        }
        Command::Update {
            pool,
            min_ready,
            max_size,
            lease_ttl,
            json,
        } => {
            let id = resolve_pool(client, &pool)?;
            let request = UpdatePoolRequest {
                min_ready,
                max_size,
                lease_ttl_seconds: lease_ttl,
            };
            let pool: PoolResponse = client.patch(&format!("/api/pools/{id}"), &request)?;
            print_output(json, &pool, format_pool_human(&pool));
        }
        Command::Delete { pool, json } => {
            let id = resolve_pool(client, &pool)?;
            let pool: PoolResponse = client.delete_json(&format!("/api/pools/{id}"))?;
            print_output(
                json,
                &pool,
                format!("deleting pool {}; its members are being removed\n", pool.id),
            );
        }
        Command::Acquire {
            pool,
            idempotency_key,
            json,
        } => {
            let id = resolve_pool(client, &pool)?;
            let lease: PoolLeaseResponse = client.post_empty_idempotent(
                &format!("/api/pools/{id}/acquire"),
                idempotency_key.as_deref(),
            )?;
            print_output(json, &lease, format_lease_human(&lease));
        }
        Command::Leases { pool, json } => {
            let id = resolve_pool(client, &pool)?;
            let leases: Vec<PoolLeaseResponse> = client.get(&format!("/api/pools/{id}/leases"))?;
            print_output(json, &leases, format_leases_human(&leases));
        }
        Command::Release { pool, lease, json } => {
            let id = resolve_pool(client, &pool)?;
            let lease: PoolLeaseResponse =
                client.delete_json(&format!("/api/pools/{id}/leases/{lease}"))?;
            print_output(json, &lease, format_lease_human(&lease));
        }
    }
    Ok(())
}

/// A UUID is used as-is; otherwise the pool list is searched by name, which
/// the API keeps unique.
fn resolve_pool(client: &ApiClient, target: &str) -> Result<Uuid, Error> {
    if let Ok(id) = Uuid::parse_str(target) {
        return Ok(id);
    }
    let pools: Vec<PoolResponse> = client.get("/api/pools")?;
    pools
        .into_iter()
        .find(|pool| pool.name == target)
        .map(|pool| pool.id)
        .ok_or_else(|| Error::PoolNotFound(target.to_owned()))
}

fn print_output<T: Serialize + ?Sized>(json: bool, value: &T, human: String) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(value).expect("CLI output serializes")
        );
    } else {
        print!("{human}");
    }
}

fn count(pool: &PoolResponse, state: PoolMemberState) -> usize {
    pool.members.iter().filter(|m| m.state == state).count()
}

fn format_list_human(pools: &[PoolResponse]) -> String {
    let mut out = String::new();
    writeln!(
        out,
        "ID\tNAME\tIMAGE\tREADY/MIN\tLEASED\tMEMBERS/MAX\tLEASE_TTL_S"
    )
    .unwrap();
    for pool in pools {
        writeln!(
            out,
            "{}\t{}{}\t{}\t{}/{}\t{}\t{}/{}\t{}",
            pool.id,
            pool.name,
            if pool.deleting { " (deleting)" } else { "" },
            pool.template_version,
            count(pool, PoolMemberState::Ready),
            pool.min_ready,
            count(pool, PoolMemberState::Leased),
            pool.members.len(),
            pool.max_size,
            pool.lease_ttl_seconds,
        )
        .unwrap();
    }
    out
}

fn format_pool_human(pool: &PoolResponse) -> String {
    let mut out = String::new();
    writeln!(out, "pool {}", pool.id).unwrap();
    writeln!(out, "  name:      {}", pool.name).unwrap();
    writeln!(
        out,
        "  image:     {}@{}",
        pool.template, pool.template_version
    )
    .unwrap();
    writeln!(
        out,
        "  resources: {} vCPU, {} MiB RAM, {} GiB disk",
        pool.cpu, pool.ram, pool.disk_gb
    )
    .unwrap();
    writeln!(out, "  network:   {}", pool.micro_network_id).unwrap();
    writeln!(
        out,
        "  sizing:    {} ready minimum, {} members maximum",
        pool.min_ready, pool.max_size
    )
    .unwrap();
    writeln!(out, "  lease ttl: {}s", pool.lease_ttl_seconds).unwrap();
    if pool.deleting {
        writeln!(out, "  deleting:  yes").unwrap();
    }
    if let Some(error) = &pool.last_error {
        writeln!(out, "  error:     {error}").unwrap();
    }
    writeln!(out, "  members:").unwrap();
    for member in &pool.members {
        writeln!(
            out,
            "    {}\t{}",
            member.vm_id,
            member_state_name(member.state)
        )
        .unwrap();
    }
    out
}

fn format_lease_human(lease: &PoolLeaseResponse) -> String {
    let mut out = String::new();
    writeln!(out, "lease {}", lease.id).unwrap();
    writeln!(out, "  state:   {}", lease_state_name(lease.state)).unwrap();
    writeln!(out, "  vm:      {}", lease.vm_id).unwrap();
    let ipv4 = lease.vm.as_ref().and_then(|vm| vm.ipv4.as_deref());
    writeln!(out, "  ipv4:    {}", ipv4.unwrap_or("-")).unwrap();
    writeln!(out, "  expires: {} (unix ms)", lease.expires_at_ms).unwrap();
    if lease.state == PoolLeaseState::Active {
        writeln!(out, "  console: firecrab vm console {}", lease.vm_id).unwrap();
    }
    out
}

fn format_leases_human(leases: &[PoolLeaseResponse]) -> String {
    let mut out = String::new();
    writeln!(out, "ID\tSTATE\tVM\tIPV4\tEXPIRES_AT_MS").unwrap();
    for lease in leases {
        writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}",
            lease.id,
            lease_state_name(lease.state),
            lease.vm_id,
            lease
                .vm
                .as_ref()
                .and_then(|vm| vm.ipv4.as_deref())
                .unwrap_or("-"),
            lease.expires_at_ms,
        )
        .unwrap();
    }
    out
}

fn member_state_name(state: PoolMemberState) -> &'static str {
    match state {
        PoolMemberState::Provisioning => "provisioning",
        PoolMemberState::Ready => "ready",
        PoolMemberState::Leased => "leased",
        PoolMemberState::Draining => "draining",
    }
}

fn lease_state_name(state: PoolLeaseState) -> &'static str {
    match state {
        PoolLeaseState::Active => "active",
        PoolLeaseState::Released => "released",
        PoolLeaseState::Expired => "expired",
    }
}

#[cfg(test)]
mod tests {
    use firecrab_api_types::{EgressPolicy, PoolMemberResponse};

    use super::*;

    fn sample_pool() -> PoolResponse {
        PoolResponse {
            id: Uuid::from_u128(1),
            name: "ci".to_owned(),
            template: "alpine-3.24.1".to_owned(),
            template_version: "alpine-3.24.1-v5".to_owned(),
            cpu: 1,
            ram: 512,
            disk_gb: 2,
            egress_policy: EgressPolicy::Internet,
            micro_network_id: Uuid::from_u128(2),
            storage_root: "default".to_owned(),
            min_ready: 2,
            max_size: 4,
            lease_ttl_seconds: 600,
            deleting: false,
            members: vec![
                PoolMemberResponse {
                    vm_id: Uuid::from_u128(3),
                    state: PoolMemberState::Ready,
                    created_at_ms: 1,
                },
                PoolMemberResponse {
                    vm_id: Uuid::from_u128(4),
                    state: PoolMemberState::Leased,
                    created_at_ms: 2,
                },
            ],
            last_error: None,
            created_at_ms: 1,
        }
    }

    #[test]
    fn the_list_shows_ready_against_min_and_members_against_max() {
        let out = format_list_human(&[sample_pool()]);

        let row = out.lines().nth(1).unwrap();
        assert!(row.contains("\tci\t"), "{row}");
        assert!(row.contains("\t1/2\t1\t2/4\t600"), "{row}");
    }

    #[test]
    fn an_active_lease_points_at_the_console_command() {
        let lease = PoolLeaseResponse {
            id: Uuid::from_u128(5),
            pool_id: Uuid::from_u128(1),
            vm_id: Uuid::from_u128(4),
            state: PoolLeaseState::Active,
            acquired_at_ms: 1,
            expires_at_ms: 2,
            ended_at_ms: None,
            vm: None,
        };

        let out = format_lease_human(&lease);

        assert!(
            out.contains(&format!("firecrab vm console {}", lease.vm_id)),
            "{out}"
        );
    }
}
