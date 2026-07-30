---
tags:
  - firecrab
  - storage
status: 미완료
scope: 4주차
updated: 2026-07-23
---

# SQLite migration 및 상태 모델 구현

## 브랜치 개요

- 브랜치: `feat/sqlite-state-model`
- 커밋: `0ed6ee3 feat: migrate VM state to SQLite`
- 상태: 구현 브랜치 존재
- 변경 규모: 11개 파일, 1813줄 추가, 47줄 삭제
- 목적: JSON 전체 저장을 SQLite transaction으로 교체하고 VM 상태 전이를 DB 조건으로 보호한다.
- SQLite는 WAL mode와 busy timeout을 사용한다.

## 의존성과 migration

```toml
sqlx = { version = "0.8", features = ["runtime-tokio", "sqlite", "migrate"] }
time = { version = "0.3", features = ["serde"] }
```

```sql
-- migrations/0001_vms.sql
CREATE TABLE vms (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    state         TEXT NOT NULL CHECK (
        state IN ('created', 'starting', 'running', 'stopping',
                  'stopped', 'error', 'deleting', 'deleted')
    ),
    template_name TEXT NOT NULL,
    template_version TEXT NOT NULL,
    template_kernel_sha256 TEXT NOT NULL,
    template_rootfs_sha256 TEXT NOT NULL,
    cpu           INTEGER NOT NULL CHECK (cpu BETWEEN 1 AND 32),
    ram           INTEGER NOT NULL CHECK (ram BETWEEN 128 AND 32768),
    last_error_code TEXT,
    last_error_message TEXT,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);

CREATE UNIQUE INDEX unique_active_vm_name
ON vms(name)
WHERE state <> 'deleted';

CREATE TABLE vm_disk_generations (
    id                 TEXT PRIMARY KEY,
    vm_id              TEXT NOT NULL REFERENCES vms(id),
    status             TEXT NOT NULL CHECK (
        status IN ('preparing', 'active', 'retained', 'deleting')
    ),
    source_kind        TEXT NOT NULL CHECK (source_kind IN ('template', 'snapshot')),
    source_ref         TEXT NOT NULL,
    logical_size_bytes INTEGER CHECK (logical_size_bytes >= 0),
    allocated_size_bytes INTEGER CHECK (allocated_size_bytes >= 0),
    created_at         TEXT NOT NULL
);

CREATE UNIQUE INDEX one_active_disk_generation_per_vm
ON vm_disk_generations(vm_id) WHERE status = 'active';

CREATE TABLE vm_events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    vm_id        TEXT NOT NULL REFERENCES vms(id),
    request_id   TEXT NOT NULL,
    operation_id TEXT,
    kind         TEXT NOT NULL CHECK (kind IN ('info', 'warning', 'error', 'state_transition')),
    code         TEXT NOT NULL,
    message      TEXT NOT NULL,
    details_json TEXT NOT NULL DEFAULT '{}',
    created_at   TEXT NOT NULL
);

CREATE TABLE vm_runtime_instances (
    runtime_id            TEXT PRIMARY KEY,
    vm_id                 TEXT NOT NULL UNIQUE REFERENCES vms(id),
    pid                   INTEGER NOT NULL CHECK (pid > 0),
    process_start_ticks   INTEGER NOT NULL CHECK (process_start_ticks > 0),
    executable_device     TEXT NOT NULL,
    executable_inode      TEXT NOT NULL,
    cgroup_id             TEXT,
    jail_id               TEXT,
    started_at            TEXT NOT NULL,
    CHECK ((cgroup_id IS NULL) = (jail_id IS NULL))
);
```

```rust
use std::{str::FromStr, time::Duration};
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
};

let options = SqliteConnectOptions::from_str("sqlite://data/firecrab.db")?
    .create_if_missing(true)
    .journal_mode(SqliteJournalMode::Wal)
    .synchronous(SqliteSynchronous::Full)
    .busy_timeout(Duration::from_secs(5))
    .foreign_keys(true);

let pool = SqlitePoolOptions::new()
    .max_connections(8)
    .connect_with(options)
    .await?;

sqlx::migrate!().run(&pool).await?;
```

- `busy_timeout`, synchronous mode와 foreign key 설정은 pool의 한 connection에서만 `PRAGMA`를 실행하는 방식이 아니라 connect option으로 모든 connection에 적용한다.
- data directory는 `0700`, DB와 WAL 관련 file은 service account만 읽고 쓸 수 있게 만들고 migration 전후 integrity check와 schema version을 기록함.

- 초기 architecture는 active-active API daemon을 지원하지 않음.
- process map, local supervisor와 helper ownership이 하나라는 전제이므로 startup에서 data root의 root-owned lock file을 symlink 없이 열고 non-blocking exclusive `flock`을 process lifetime 동안 유지함.
- lock을 얻지 못하면 다른 port로 두 번째 daemon을 시작하지 않고 명확히 실패함.
- backup/restore와 migration admin command도 같은 lock 또는 명시적인 read-only online protocol로 조정함.

## 상태 모델

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VmState {
    Created,
    Starting,
    Running,
    Stopping,
    Stopped,
    Error,
    Deleting,
    Deleted,
}
```

- DB에는 소문자 ASCII 값만 저장하고 API에는 같은 값으로 직렬화함.
- 시간은 UTC `Z`, 고정 fractional precision의 RFC 3339 형식으로 정규화하며 parse 불가능한 값을 내부 오류로 처리함.
- SQL text 비교가 필요한 expiry에서도 같은 canonical encoder만 사용함.
- soft delete 이후 같은 이름을 다시 사용할 수 있도록 column `UNIQUE`가 아니라 partial unique index를 사용함.

- 상태 변경은 먼저 읽고 나중에 쓰는 방식 대신 조건부 `UPDATE`로 경쟁을 차단한다.

```rust
let transitioned = sqlx::query_scalar::<_, String>(
    "UPDATE vms SET state = ?, updated_at = ?
     WHERE id = ? AND state IN ('created', 'stopped', 'error')
     RETURNING id",
)
.bind("starting")
.bind(now)
.bind(vm_id.to_string())
.fetch_optional(&mut *transaction)
.await?;

if transitioned.is_none() {
    return Err(classify_missing_or_conflict(&mut transaction, vm_id).await?);
}
```

- 상태 변경과 `vm_events` 및 `api_operations` 삽입은 같은 transaction에서 commit한다.
- process identity는 `vms`의 일부 필드로 흩어 놓지 않고 `vm_runtime_instances` 한 row로 기록해 PID, start tick과 executable identity가 부분 저장되지 않게 함.
- Week 2 direct runtime에서는 cgroup/jail pair가 `NULL`이고 Week 4 Jailer 전환 뒤에는 둘을 함께 기록함.
- runtime 종료를 확인하면 row를 제거하되 종료 event에는 `runtime_id`를 남김.
- `UPDATE` 실패 뒤 존재 여부를 같은 transaction에서 확인해 `404`와 `409`를 구분함.
- 기존 `Created` JSON을 가져오는 일회성 importer가 필요하면 migration 실행 전에 별도 명령으로 제공하고, 성공 후 원본을 백업한다.

## 테스트 및 검증

- 동일 VM에 start 요청을 동시에 보내면 하나의 `created -> starting`만 성공해야 한다.
- migration을 두 번 실행해도 schema가 중복 생성되지 않아야 한다.
- API를 재시작해도 상태와 event가 유지되어야 한다.
- invalid state, 범위 밖 CPU/RAM, 중복 active name과 VM별 중복 runtime identity가 DB constraint에서 거부되어야 한다.
- 같은 data root로 두 번째 API process를 시작하면 DB나 helper를 변경하기 전에 실패해야 한다.

## 완료 및 후속 범위

- 구현 브랜치와 커밋이 존재함.
- 위 테스트 및 검증 항목을 모두 통과한 뒤 완료로 판정함.
