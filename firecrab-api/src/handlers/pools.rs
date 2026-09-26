//! `/api/pools`: warm MicroVM pools and their leases (issue #291).

use std::collections::{BTreeMap, HashSet};

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use firecrab_api_types::{
    CreatePoolRequest, IDEMPOTENCY_KEY_HEADER, PoolLeaseResponse, PoolLeaseState, PoolResponse,
    UpdatePoolRequest,
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::handlers::vms::parse_id;
use crate::model::VmState;
use crate::persistence::{Acquired, PoolRow, PoolStoreError};
use crate::pool::{lease_response, now_ms, pool_response, valid_pool_name};
use crate::server::RequestId;
use crate::state::AppState;

/// Most members one pool may hold.
const MAX_POOL_SIZE: u16 = 32;
/// Shortest and longest lease a pool may hand out.
const LEASE_TTL_RANGE_SECONDS: std::ops::RangeInclusive<u32> = 60..=7 * 24 * 60 * 60;
/// Longest accepted idempotency key.
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 255;

pub async fn list_pools(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
) -> Result<Json<Vec<PoolResponse>>, AppError> {
    let store = state.store.clone();
    let pools = blocking(request_id, move || {
        store
            .list_pools()?
            .into_iter()
            .map(|pool| {
                let members = store.pool_members(pool.id)?;
                Ok((pool, members))
            })
            .collect::<Result<Vec<_>, PoolStoreError>>()
    })
    .await?;
    Ok(Json(
        pools
            .iter()
            .map(|(pool, members)| pool_response(&state, pool, members))
            .collect(),
    ))
}

pub async fn get_pool(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<PoolResponse>, AppError> {
    let id = parse_id(&id, request_id.0)?;
    Ok(Json(load_pool(&state, request_id, id).await?))
}

pub async fn create_pool(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    ValidatedJson(req): ValidatedJson<CreatePoolRequest>,
) -> Result<(StatusCode, Json<PoolResponse>), AppError> {
    let mut fields = validate_limits(req.min_ready, req.max_size, req.lease_ttl_seconds);
    if !valid_pool_name(&req.name) {
        fields.insert(
            "name".to_owned(),
            "must be 1-40 ASCII letters, numbers, '.', '_' or '-'".to_owned(),
        );
    }
    if req.template == crate::microboot::MICROBOOT_ALIAS {
        fields.insert("template".to_owned(), "is not supported".to_owned());
    }
    let storage_root = req
        .storage_root
        .clone()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| state.storage.default_id().to_owned());
    // Members are ordinary VMs, so the spec is checked exactly as a VM's.
    let member = firecrab_api_types::CreateVmRequest {
        name: "pool-member".to_owned(),
        template: req.template.clone(),
        ram: req.ram,
        cpu: req.cpu,
        disk_gb: req.disk_gb,
        egress_policy: req.egress_policy,
        micro_network_id: req.micro_network_id,
        storage_root: Some(storage_root.clone()),
        shell_ids: Vec::new(),
        port_forwards: Vec::new(),
        env: BTreeMap::new(),
    };
    for (field, message) in crate::handlers::vms::validate_create(&member, &state) {
        fields.entry(field).or_insert(message);
    }
    if !fields.is_empty() {
        return Err(AppError::validation(fields, request_id.0));
    }
    let template = state
        .templates
        .resolve_alias(&req.template)
        .ok_or_else(|| AppError::internal(request_id.0))?;

    let row = PoolRow {
        id: Uuid::new_v4(),
        name: req.name,
        template: template.name.clone(),
        template_version: template.version.clone(),
        cpu: req.cpu,
        ram: req.ram,
        disk_gb: req.disk_gb,
        egress_policy: req.egress_policy,
        micro_network_id: req.micro_network_id,
        storage_root,
        min_ready: req.min_ready,
        max_size: req.max_size,
        lease_ttl_seconds: req.lease_ttl_seconds,
        deleting: false,
        created_at_ms: now_ms(),
    };
    let store = state.store.clone();
    let inserted = row.clone();
    match tokio::task::spawn_blocking(move || store.insert_pool(&inserted))
        .await
        .map_err(|_| AppError::internal(request_id.0))?
    {
        Ok(()) => {}
        Err(PoolStoreError::DuplicateName(_)) => {
            return Err(AppError::validation(
                BTreeMap::from([(
                    "name".to_owned(),
                    "is already used by another pool".to_owned(),
                )]),
                request_id.0,
            ));
        }
        Err(error) => return Err(internal(request_id, error)),
    }
    tracing::info!(
        request_id = %request_id.0,
        pool_id = %row.id,
        name = row.name,
        template = row.template,
        template_version = row.template_version,
        min_ready = row.min_ready,
        max_size = row.max_size,
        "pool created"
    );
    state.pools.wake();
    Ok((StatusCode::CREATED, Json(pool_response(&state, &row, &[]))))
}

pub async fn update_pool(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(req): ValidatedJson<UpdatePoolRequest>,
) -> Result<Json<PoolResponse>, AppError> {
    let id = parse_id(&id, request_id.0)?;
    let store = state.store.clone();
    let current = blocking(request_id, move || store.pool(id))
        .await?
        .ok_or_else(|| AppError::not_found(request_id.0))?;
    let min_ready = req.min_ready.unwrap_or(current.min_ready);
    let max_size = req.max_size.unwrap_or(current.max_size);
    let lease_ttl_seconds = req.lease_ttl_seconds.unwrap_or(current.lease_ttl_seconds);
    let fields = validate_limits(min_ready, max_size, lease_ttl_seconds);
    if !fields.is_empty() {
        return Err(AppError::validation(fields, request_id.0));
    }
    let store = state.store.clone();
    let updated = blocking(request_id, move || {
        store.update_pool_limits(id, min_ready, max_size, lease_ttl_seconds)
    })
    .await?;
    if !updated {
        return Err(AppError::not_found(request_id.0));
    }
    state.pools.wake();
    Ok(Json(load_pool(&state, request_id, id).await?))
}

/// Flags the pool for deletion; the reconciler deletes its members and then
/// the pool. Refused while any lease is active.
pub async fn delete_pool(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<PoolResponse>), AppError> {
    let id = parse_id(&id, request_id.0)?;
    let store = state.store.clone();
    let marked = tokio::task::spawn_blocking(move || store.mark_pool_deleting(id))
        .await
        .map_err(|_| AppError::internal(request_id.0))?;
    match marked {
        Ok(_) => {}
        Err(PoolStoreError::MissingPool(_)) => return Err(AppError::not_found(request_id.0)),
        Err(PoolStoreError::InUse { .. }) => {
            return Err(AppError::conflict(
                "pool_in_use",
                "release every active lease before deleting the pool",
                request_id.0,
            ));
        }
        Err(error) => return Err(internal(request_id, error)),
    }
    tracing::info!(request_id = %request_id.0, pool_id = %id, "pool deleting");
    state.pools.wake();
    Ok((
        StatusCode::ACCEPTED,
        Json(load_pool(&state, request_id, id).await?),
    ))
}

/// Leases one ready member. With an `Idempotency-Key`, a retry returns the
/// first call's lease (`200`) instead of leasing another VM (`201`).
pub async fn acquire_pool_lease(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<PoolLeaseResponse>), AppError> {
    let pool_id = parse_id(&id, request_id.0)?;
    let key = idempotency_key(&headers, request_id)?;
    // Only a member whose VM is up right now may be handed out; one that died
    // since the last reconcile pass is left for the reconciler to replace.
    let running: HashSet<Uuid> = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .values()
        .filter(|vm| vm.state == VmState::Running)
        .map(|vm| vm.id)
        .collect();
    let store = state.store.clone();
    let now = now_ms();
    let acquired = tokio::task::spawn_blocking(move || {
        store.acquire_pool_lease(pool_id, key.as_deref(), now, |vm_id| {
            running.contains(&vm_id)
        })
    })
    .await
    .map_err(|_| AppError::internal(request_id.0))?;
    let Acquired { lease, replayed } = match acquired {
        Ok(acquired) => acquired,
        Err(PoolStoreError::MissingPool(_)) => return Err(AppError::not_found(request_id.0)),
        Err(PoolStoreError::Deleting(_)) => {
            return Err(AppError::conflict(
                "pool_deleting",
                "the pool is being deleted",
                request_id.0,
            ));
        }
        Err(PoolStoreError::Exhausted(_)) => {
            state.pools.wake();
            return Err(AppError::conflict(
                "pool_exhausted",
                "no ready member right now; retry with the same Idempotency-Key",
                request_id.0,
            ));
        }
        Err(error) => return Err(internal(request_id, error)),
    };
    if replayed {
        return Ok((StatusCode::OK, Json(lease_response(&state, lease).await)));
    }
    tracing::info!(
        request_id = %request_id.0,
        %pool_id,
        lease_id = %lease.id,
        vm_id = %lease.vm_id,
        "pool lease acquired"
    );
    // A leased member no longer counts as warm: boot its replacement now.
    state.pools.wake();
    Ok((
        StatusCode::CREATED,
        Json(lease_response(&state, lease).await),
    ))
}

pub async fn list_pool_leases(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<Vec<PoolLeaseResponse>>, AppError> {
    let pool_id = parse_id(&id, request_id.0)?;
    let store = state.store.clone();
    let (exists, leases) = blocking(request_id, move || {
        Ok((store.pool(pool_id)?.is_some(), store.pool_leases(pool_id)?))
    })
    .await?;
    if !exists {
        return Err(AppError::not_found(request_id.0));
    }
    let mut responses = Vec::with_capacity(leases.len());
    for lease in leases {
        responses.push(lease_response(&state, lease).await);
    }
    Ok(Json(responses))
}

pub async fn get_pool_lease(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path((id, lease_id)): Path<(String, String)>,
) -> Result<Json<PoolLeaseResponse>, AppError> {
    let pool_id = parse_id(&id, request_id.0)?;
    let lease_id = parse_id(&lease_id, request_id.0)?;
    let store = state.store.clone();
    let lease = blocking(request_id, move || store.pool_lease(lease_id))
        .await?
        .filter(|lease| lease.pool_id == pool_id)
        .ok_or_else(|| AppError::not_found(request_id.0))?;
    Ok(Json(lease_response(&state, lease).await))
}

/// Ends the lease. Its VM is deleted and replaced, never leased again. A
/// repeated release returns the ended lease unchanged.
pub async fn release_pool_lease(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path((id, lease_id)): Path<(String, String)>,
) -> Result<Json<PoolLeaseResponse>, AppError> {
    let pool_id = parse_id(&id, request_id.0)?;
    let lease_id = parse_id(&lease_id, request_id.0)?;
    let store = state.store.clone();
    let now = now_ms();
    let lease = blocking(request_id, move || match store.pool_lease(lease_id)? {
        Some(lease) if lease.pool_id == pool_id => {
            store.end_pool_lease(lease_id, PoolLeaseState::Released, now)
        }
        _ => Ok(None),
    })
    .await?
    .ok_or_else(|| AppError::not_found(request_id.0))?;
    tracing::info!(
        request_id = %request_id.0,
        %pool_id,
        %lease_id,
        vm_id = %lease.vm_id,
        "pool lease released"
    );
    state.pools.wake();
    Ok(Json(lease_response(&state, lease).await))
}

fn validate_limits(
    min_ready: u16,
    max_size: u16,
    lease_ttl_seconds: u32,
) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    if !(1..=MAX_POOL_SIZE).contains(&max_size) {
        fields.insert(
            "maxSize".to_owned(),
            format!("must be between 1 and {MAX_POOL_SIZE}"),
        );
    }
    if min_ready > max_size {
        fields.insert(
            "minReady".to_owned(),
            "must not be greater than maxSize".to_owned(),
        );
    }
    if !LEASE_TTL_RANGE_SECONDS.contains(&lease_ttl_seconds) {
        fields.insert(
            "leaseTtlSeconds".to_owned(),
            format!(
                "must be between {} and {} seconds",
                LEASE_TTL_RANGE_SECONDS.start(),
                LEASE_TTL_RANGE_SECONDS.end()
            ),
        );
    }
    fields
}

/// The optional `Idempotency-Key`: 1-255 visible ASCII characters.
fn idempotency_key(headers: &HeaderMap, request_id: RequestId) -> Result<Option<String>, AppError> {
    let Some(value) = headers.get(IDEMPOTENCY_KEY_HEADER) else {
        return Ok(None);
    };
    let key = value.as_bytes();
    if key.is_empty()
        || key.len() > MAX_IDEMPOTENCY_KEY_BYTES
        || !key.iter().all(|byte| (0x21..=0x7e).contains(byte))
    {
        return Err(AppError::validation(
            BTreeMap::from([(
                "Idempotency-Key".to_owned(),
                format!("must be 1-{MAX_IDEMPOTENCY_KEY_BYTES} visible ASCII characters"),
            )]),
            request_id.0,
        ));
    }
    Ok(Some(String::from_utf8_lossy(key).into_owned()))
}

async fn load_pool(
    state: &AppState,
    request_id: RequestId,
    id: Uuid,
) -> Result<PoolResponse, AppError> {
    let store = state.store.clone();
    let (pool, members) = blocking(request_id, move || {
        let Some(pool) = store.pool(id)? else {
            return Ok(None);
        };
        let members = store.pool_members(id)?;
        Ok(Some((pool, members)))
    })
    .await?
    .ok_or_else(|| AppError::not_found(request_id.0))?;
    Ok(pool_response(state, &pool, &members))
}

async fn blocking<T, F>(request_id: RequestId, work: F) -> Result<T, AppError>
where
    F: FnOnce() -> Result<T, PoolStoreError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| AppError::internal(request_id.0))?
        .map_err(|error| internal(request_id, error))
}

fn internal(request_id: RequestId, error: PoolStoreError) -> AppError {
    tracing::error!(request_id = %request_id.0, %error, "pool store operation failed");
    AppError::internal(request_id.0)
}

#[cfg(test)]
mod tests {
    use std::path::Path as FsPath;
    use std::time::Duration;

    use axum::http::HeaderValue;
    use axum::response::IntoResponse;
    use firecrab_api_types::{EgressPolicy, PoolMemberState};

    use super::*;
    use crate::firecracker::test_support::{SERVE_LOOP, fake_firecracker, short_tempdir};
    use crate::handlers::micro_networks::test_support::seed_internet_micro_network;
    use crate::handlers::vms::test_support::test_state_with_binary;
    use crate::pool::reconcile;

    const BOOTS: &str = "signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))";

    /// A Firecracker whose guest never reports network readiness, so every
    /// start fails.
    const NEVER_READY: &str = r#"
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
srv = socket.socket(socket.AF_UNIX)
srv.bind(sock_path)
srv.listen(1)
while True:
    conn, _ = srv.accept()
    conn.recv(1024)
    conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
    conn.close()
"#;

    async fn pool_state(root: &FsPath, firecracker: &str) -> (AppState, Uuid) {
        let binary = fake_firecracker(root, firecracker);
        let state = test_state_with_binary(root, binary).await;
        let network = seed_internet_micro_network(&state);
        (state, network)
    }

    fn request(network: Uuid, min_ready: u16, max_size: u16) -> CreatePoolRequest {
        CreatePoolRequest {
            name: "ci".to_owned(),
            template: "ubuntu-rootfs-26.04".to_owned(),
            cpu: 1,
            ram: 128,
            disk_gb: 1,
            egress_policy: EgressPolicy::Internet,
            micro_network_id: network,
            storage_root: None,
            min_ready,
            max_size,
            lease_ttl_seconds: 600,
        }
    }

    fn rid() -> Extension<RequestId> {
        Extension(RequestId(Uuid::new_v4()))
    }

    async fn create(state: &AppState, req: CreatePoolRequest) -> PoolResponse {
        let (status, Json(pool)) = create_pool(State(state.clone()), rid(), ValidatedJson(req))
            .await
            .unwrap();
        assert_eq!(status, StatusCode::CREATED);
        pool
    }

    async fn pool(state: &AppState, id: Uuid) -> PoolResponse {
        get_pool(State(state.clone()), rid(), Path(id.to_string()))
            .await
            .unwrap()
            .0
    }

    fn count(pool: &PoolResponse, state: PoolMemberState) -> usize {
        pool.members.iter().filter(|m| m.state == state).count()
    }

    /// Reconciles until `done` holds, letting detached starts and stops run
    /// in between. Returns the pool as it was when `done` first held.
    async fn reconcile_until(
        state: &AppState,
        id: Uuid,
        done: impl Fn(&PoolResponse) -> bool,
    ) -> PoolResponse {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            reconcile(state, now_ms()).await;
            let current = pool(state, id).await;
            assert!(
                current.members.len() <= usize::from(current.max_size),
                "never more than maxSize members: {current:?}"
            );
            if done(&current) {
                return current;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "pool never converged: {current:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn ready(state: &AppState, id: Uuid, n: usize) -> PoolResponse {
        reconcile_until(state, id, |p| {
            count(p, PoolMemberState::Ready) == n && p.members.len() == n
        })
        .await
    }

    async fn acquire(
        state: &AppState,
        id: Uuid,
        key: Option<&str>,
    ) -> Result<(StatusCode, PoolLeaseResponse), AppError> {
        let mut headers = HeaderMap::new();
        if let Some(key) = key {
            headers.insert(IDEMPOTENCY_KEY_HEADER, HeaderValue::from_str(key).unwrap());
        }
        acquire_pool_lease(State(state.clone()), rid(), Path(id.to_string()), headers)
            .await
            .map(|(status, Json(lease))| (status, lease))
    }

    fn vm_exists(state: &AppState, vm_id: Uuid) -> bool {
        state.vms.lock().unwrap().contains_key(&vm_id)
    }

    fn status_of(error: AppError) -> StatusCode {
        error.into_response().status()
    }

    #[tokio::test]
    async fn create_pool_rejects_bad_limits_names_and_duplicates() {
        let directory = short_tempdir();
        let (state, network) = pool_state(directory.path(), &format!("{BOOTS}{SERVE_LOOP}")).await;

        for bad in [
            CreatePoolRequest {
                min_ready: 3,
                max_size: 2,
                ..request(network, 0, 0)
            },
            CreatePoolRequest {
                max_size: 0,
                ..request(network, 0, 0)
            },
            CreatePoolRequest {
                lease_ttl_seconds: 59,
                ..request(network, 1, 1)
            },
            CreatePoolRequest {
                name: "has space".to_owned(),
                ..request(network, 1, 1)
            },
            CreatePoolRequest {
                template: "missing".to_owned(),
                ..request(network, 1, 1)
            },
        ] {
            let error = create_pool(State(state.clone()), rid(), ValidatedJson(bad.clone()))
                .await
                .unwrap_err();
            assert_eq!(status_of(error), StatusCode::BAD_REQUEST, "{bad:?}");
        }

        let created = create(&state, request(network, 0, 1)).await;
        assert_eq!(
            created.template_version, "v1",
            "the image version is pinned"
        );
        let duplicate = create_pool(
            State(state.clone()),
            rid(),
            ValidatedJson(request(network, 0, 1)),
        )
        .await
        .unwrap_err();
        assert_eq!(status_of(duplicate), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn min_ready_two_converges_to_two_ready_members() {
        let directory = short_tempdir();
        let (state, network) = pool_state(directory.path(), &format!("{BOOTS}{SERVE_LOOP}")).await;
        let created = create(&state, request(network, 2, 3)).await;

        let converged = ready(&state, created.id, 2).await;

        for member in &converged.members {
            let vm = state.vms.lock().unwrap()[&member.vm_id].clone();
            assert_eq!(vm.state, VmState::Running);
            assert_eq!(vm.purpose, crate::model::VmPurpose::Pool);
        }
        let Json(listed) = crate::handlers::vms::list_vms(State(state.clone())).await;
        assert!(listed.is_empty(), "pool members stay out of the VM list");
    }

    #[tokio::test]
    async fn concurrent_acquires_get_distinct_vms_and_a_retry_gets_the_original_lease() {
        let directory = short_tempdir();
        let (state, network) = pool_state(directory.path(), &format!("{BOOTS}{SERVE_LOOP}")).await;
        let created = create(&state, request(network, 2, 2)).await;
        ready(&state, created.id, 2).await;

        let (first, second) = tokio::join!(
            acquire(&state, created.id, Some("job-1")),
            acquire(&state, created.id, Some("job-2")),
        );
        let (first_status, first) = first.unwrap();
        let (_, second) = second.unwrap();
        assert_eq!(first_status, StatusCode::CREATED);
        assert_ne!(first.vm_id, second.vm_id);
        assert!(first.vm.is_some(), "the lease carries the VM's address");

        let (retry_status, retry) = acquire(&state, created.id, Some("job-1")).await.unwrap();
        assert_eq!(retry_status, StatusCode::OK);
        assert_eq!(retry.id, first.id);
        assert_eq!(retry.vm_id, first.vm_id);

        let exhausted = acquire(&state, created.id, Some("job-3"))
            .await
            .unwrap_err();
        assert_eq!(status_of(exhausted), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn a_released_member_is_deleted_and_replaced_by_a_fresh_vm() {
        let directory = short_tempdir();
        let (state, network) = pool_state(directory.path(), &format!("{BOOTS}{SERVE_LOOP}")).await;
        let created = create(&state, request(network, 1, 2)).await;
        ready(&state, created.id, 1).await;
        let (_, lease) = acquire(&state, created.id, None).await.unwrap();
        let used_dir =
            crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for("default"), lease.vm_id)
                .dir;
        std::fs::write(used_dir.join("caller-data"), b"secret").unwrap();

        let Json(released) = release_pool_lease(
            State(state.clone()),
            rid(),
            Path((created.id.to_string(), lease.id.to_string())),
        )
        .await
        .unwrap();
        assert_eq!(released.state, PoolLeaseState::Released);

        let replaced = reconcile_until(&state, created.id, |p| {
            p.members.len() == 1 && count(p, PoolMemberState::Ready) == 1
        })
        .await;
        assert!(!vm_exists(&state, lease.vm_id), "the used VM is deleted");
        assert!(!used_dir.exists(), "its disk and files are gone");
        let fresh = replaced.members[0].vm_id;
        assert_ne!(fresh, lease.vm_id);
        let fresh_dir =
            crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for("default"), fresh).dir;
        assert!(!fresh_dir.join("caller-data").exists());

        let Json(again) = release_pool_lease(
            State(state.clone()),
            rid(),
            Path((created.id.to_string(), lease.id.to_string())),
        )
        .await
        .unwrap();
        assert_eq!(
            again.state,
            PoolLeaseState::Released,
            "a repeated release is harmless"
        );
    }

    #[tokio::test]
    async fn an_expired_lease_is_ended_and_its_vm_replaced() {
        let directory = short_tempdir();
        let (state, network) = pool_state(directory.path(), &format!("{BOOTS}{SERVE_LOOP}")).await;
        let created = create(&state, request(network, 1, 2)).await;
        ready(&state, created.id, 1).await;
        let (_, lease) = acquire(&state, created.id, None).await.unwrap();

        reconcile(&state, lease.expires_at_ms).await;

        let Json(expired) = get_pool_lease(
            State(state.clone()),
            rid(),
            Path((created.id.to_string(), lease.id.to_string())),
        )
        .await
        .unwrap();
        assert_eq!(expired.state, PoolLeaseState::Expired);
        let replaced = ready(&state, created.id, 1).await;
        assert_ne!(replaced.members[0].vm_id, lease.vm_id);
        assert!(!vm_exists(&state, lease.vm_id));
    }

    #[tokio::test]
    async fn a_member_that_fails_to_boot_is_replaced_without_exceeding_max_size() {
        let directory = short_tempdir();
        let (state, network) = pool_state(directory.path(), NEVER_READY).await;
        let created = create(&state, request(network, 2, 2)).await;

        let failed = reconcile_until(&state, created.id, |p| p.last_error.is_some()).await;

        assert!(failed.last_error.unwrap().contains("did not boot"));
        // reconcile_until already asserted the ceiling on every pass; now the
        // broken members must be deleted rather than left behind.
        let cleaned = reconcile_until(&state, created.id, |p| p.members.is_empty()).await;
        assert!(cleaned.members.is_empty(), "backing off after the failure");
        let leftover = state
            .vms
            .lock()
            .unwrap()
            .values()
            .filter(|vm| vm.purpose == crate::model::VmPurpose::Pool)
            .count();
        assert_eq!(leftover, 0, "no failed member VM is leaked");
    }

    #[tokio::test]
    async fn members_whose_vms_stopped_are_replaced_and_their_leases_ended() {
        let directory = short_tempdir();
        let (state, network) = pool_state(directory.path(), &format!("{BOOTS}{SERVE_LOOP}")).await;
        let created = create(&state, request(network, 2, 3)).await;
        let converged = ready(&state, created.id, 2).await;
        let (_, lease) = acquire(&state, created.id, None).await.unwrap();

        // What an API restart does to every guest (see `reset_active_states`).
        for member in &converged.members {
            let _stopped = crate::handlers::vms::stop_vm(
                State(state.clone()),
                rid(),
                Path(member.vm_id.to_string()),
            )
            .await
            .unwrap();
        }

        let recovered = reconcile_until(&state, created.id, |p| {
            count(p, PoolMemberState::Ready) == 2
                && p.members
                    .iter()
                    .all(|m| !converged.members.iter().any(|old| old.vm_id == m.vm_id))
        })
        .await;
        assert_eq!(recovered.members.len(), 2);
        let Json(ended) = get_pool_lease(
            State(state.clone()),
            rid(),
            Path((created.id.to_string(), lease.id.to_string())),
        )
        .await
        .unwrap();
        assert_eq!(ended.state, PoolLeaseState::Expired);
    }

    #[tokio::test]
    async fn a_pool_is_deleted_only_once_its_leases_end_and_its_members_are_gone() {
        let directory = short_tempdir();
        let (state, network) = pool_state(directory.path(), &format!("{BOOTS}{SERVE_LOOP}")).await;
        let created = create(&state, request(network, 1, 1)).await;
        ready(&state, created.id, 1).await;
        let (_, lease) = acquire(&state, created.id, None).await.unwrap();

        let refused = delete_pool(State(state.clone()), rid(), Path(created.id.to_string()))
            .await
            .unwrap_err();
        assert_eq!(status_of(refused), StatusCode::CONFLICT);

        let _released = release_pool_lease(
            State(state.clone()),
            rid(),
            Path((created.id.to_string(), lease.id.to_string())),
        )
        .await
        .unwrap();
        let (status, Json(deleting)) =
            delete_pool(State(state.clone()), rid(), Path(created.id.to_string()))
                .await
                .unwrap();
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(deleting.deleting);

        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            reconcile(&state, now_ms()).await;
            if get_pool(State(state.clone()), rid(), Path(created.id.to_string()))
                .await
                .is_err()
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the pool was never deleted"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!vm_exists(&state, lease.vm_id));
        assert!(
            state
                .vms
                .lock()
                .unwrap()
                .values()
                .all(|vm| vm.purpose != crate::model::VmPurpose::Pool)
        );
    }

    #[tokio::test]
    async fn a_pool_vm_no_member_row_accounts_for_is_deleted() {
        let directory = short_tempdir();
        let (state, _) = pool_state(directory.path(), &format!("{BOOTS}{SERVE_LOOP}")).await;
        let mut orphan = crate::handlers::vms::test_support::record("pool-gone-1", Uuid::new_v4());
        orphan.purpose = crate::model::VmPurpose::Pool;
        crate::handlers::vms::test_support::seed_vm(&state, &orphan);

        reconcile(&state, now_ms()).await;

        assert!(!vm_exists(&state, orphan.id));
    }

    #[tokio::test]
    async fn acquire_rejects_a_malformed_idempotency_key() {
        let directory = short_tempdir();
        let (state, network) = pool_state(directory.path(), &format!("{BOOTS}{SERVE_LOOP}")).await;
        let created = create(&state, request(network, 0, 1)).await;

        let error = acquire(&state, created.id, Some(&"k".repeat(256)))
            .await
            .unwrap_err();

        assert_eq!(status_of(error), StatusCode::BAD_REQUEST);
    }
}
