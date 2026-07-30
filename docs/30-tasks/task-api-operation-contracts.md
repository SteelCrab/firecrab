---
tags:
  - firecrab
  - api
status: 미완료
scope: 7주차
updated: 2026-07-23
---

# API 오류·idempotency·비동기 operation 계약 구현

## 브랜치 개요

- 브랜치: `feat/api-operation-contracts`
- 커밋: `d23ada7 feat: add idempotent operation contracts`
- 상태: 구현 브랜치 존재
- 변경 규모: 15개 파일, 1250줄 추가, 55줄 삭제
- 목적: 모든 handler가 같은 오류 envelope를 사용하고, 변경 요청을 영속 operation으로 접수해 중복 실행과 daemon 재시작을 안전하게 처리함.

## 공통 오류

```rust
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: ApiError,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, String>,
    pub request_id: Uuid,
}
```

- `AppError`가 `IntoResponse`를 구현해 extractor, validation, DB, conflict와 내부 오류를 이 구조로 변환함.
- 내부 SQL, token, PID와 host path는 응답에 넣지 않음.

- router 최외곽 request ID middleware가 요청마다 새 server UUID를 생성해 extension, error/event/operation과 `X-Request-ID` response header에 넣음.
- client header는 형식과 길이를 검증해 별도 `client_request_id` correlation hint로만 기록하고 primary ID, authorization/idempotency 식별자로 사용하지 않음.
- extractor나 CORS/origin 거부처럼 handler 전 오류도 같은 server request ID와 envelope를 가져야 함.

## Operation 모델

```sql
CREATE TABLE api_operations (
    id                   TEXT PRIMARY KEY,
    vm_id                TEXT REFERENCES vms(id),
    principal_id         TEXT,
    kind                 TEXT NOT NULL,
    status               TEXT NOT NULL,
    phase                TEXT NOT NULL,
    request_id           TEXT NOT NULL,
    resource_id          TEXT,
    error_code           TEXT,
    cancel_requested_at  TEXT,
    cancel_reason        TEXT,
    worker_instance_id   TEXT,
    lease_expires_at     TEXT,
    attempt_count        INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL,
    CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    CHECK ((cancel_requested_at IS NULL) = (cancel_reason IS NULL)),
    CHECK ((status = 'running') = (worker_instance_id IS NOT NULL)),
    CHECK ((status = 'running') = (lease_expires_at IS NOT NULL))
);

CREATE UNIQUE INDEX one_active_operation_per_vm
ON api_operations(vm_id)
WHERE status IN ('queued', 'running');

CREATE TABLE api_idempotency_records (
    id                      TEXT PRIMARY KEY,
    scope                   TEXT NOT NULL,
    key_hash                TEXT NOT NULL,
    key_version             INTEGER NOT NULL CHECK (key_version > 0),
    request_hash            TEXT NOT NULL,
    operation_id            TEXT REFERENCES api_operations(id),
    resource_id             TEXT,
    response_body_json      TEXT,
    response_status         INTEGER NOT NULL CHECK (response_status BETWEEN 100 AND 599),
    created_at              TEXT NOT NULL,
    expires_at              TEXT NOT NULL,
    CHECK (operation_id IS NOT NULL OR response_body_json IS NOT NULL),
    UNIQUE (scope, key_hash)
);
```

- `scope`는 인증 principal, HTTP method와 route template을 포함함.
- `request_hash`는 canonical path parameter, 의미 있는 query와 request DTO를 함께 인코딩한 hash이며 raw body나 secret을 저장하지 않음.
- path의 VM ID를 빼면 같은 key로 다른 VM의 빈-body start 요청이 기존 operation을 잘못 반환할 수 있으므로 반드시 포함함.
- 같은 key에 다른 request hash가 오면 `409 idempotency_conflict`를 반환하고, 같은 hash면 기존 status와 resource/operation을 반환함.
- idempotency row를 분리해 create처럼 operation이 없는 응답과 start 취소처럼 여러 HTTP intent가 같은 operation을 가리키는 경우를 지원함.
- operation 요청 재전송은 같은 operation ID의 최신 공개 상태를 반환하고, 즉시 create 응답은 host path/secret이 없는 bounded canonical API response snapshot을 저장해 최초 `201` body를 재현함.

- `kind`와 `phase`는 Rust enum 및 migration의 allowlist 값만 허용함.
- supervisor는 DB 문자열을 shell command, helper path나 function name으로 동적 해석하지 않고 exhaustive match로 고정 job에 연결함.
- 지원하지 않는 값은 side effect 전에 recovery 오류로 격리함.

## Supervisor

```rust
#[derive(Clone)]
pub struct OperationSupervisor {
    jobs: tokio_util::task::TaskTracker,
    cancellation: tokio_util::sync::CancellationToken,
    store: Store,
    wakeup: Arc<tokio::sync::Notify>,
}

impl OperationSupervisor {
    pub fn notify(&self) {
        self.wakeup.notify_one();
    }
}
```

- handler transaction에서 상태 전이, event와 `queued` operation row를 함께 commit한 뒤 supervisor를 깨움.
- supervisor는 DB에서 queued row 또는 lease가 만료된 running row를 조건부 claim하고 worker instance, bounded lease, attempt를 기록한 뒤 `kind`에 맞는 유한 job을 `TaskTracker`에 등록함.
- VM lifetime 동안 지속되는 process monitor는 이 tracker에 넣지 않고 별도 runtime monitor registry가 소유함.
- job은 phase 경계에서 heartbeat/lease를 갱신하며 TaskTracker의 panic/JoinError도 관측해 lease expiry 전에 recovery를 깨움.
- notify가 유실돼도 주기 scan과 startup recovery가 처리함.
- lifecycle 코드에서 결과를 잃는 bare `tokio::spawn`을 사용하지 않음.
- process 시작 전 crash는 재실행할 수 있고, process 시작 후 crash는 phase와 process identity를 이용해 reconcile함.
- lease 만료만 보고 irreversible phase를 처음부터 재실행하지 않음.
- pre-commit retry는 exponential backoff와 attempt 상한을 두고, 상한 뒤에는 정제된 failed 상태와 수동 재시도 가능 event를 남겨 hot loop를 막음.

## API

- `GET /api/operations/{id}`: status, phase, resource ID와 정제된 오류 조회
- start, stop, delete, snapshot: `202 Accepted`와 operation 반환
- create: 즉시 저장이면 `201 Created`를 유지하되 `Idempotency-Key` 중복을 처리

- 성공·오류·page·operation DTO와 enum의 JSON 이름은 `firecrab-api-types` 같은 dependency가 가벼운 공통 crate에 둠.
- DB row나 runtime 내부 type을 이 crate에 넣지 않고 API wire type만 공유하며, Rust/Wasm frontend도 같은 type을 사용함.
- OpenAPI 문서는 이 type과 route에서 생성해 CI에서 checked-in `docs/api.md`와 diff를 검사함.
- 서버와 UI가 서로 다른 `VmState`, `OperationStatus`, cursor field를 수동으로 유지하면 안 됨.

- 모든 비동기 접수 응답에는 operation 조회 URL을 `Location` header와 JSON `operationId`로 함께 반환함.
- operation 조회도 principal과 resource authorization을 다시 검사함.
- `Idempotency-Key`는 길이와 문자 집합을 제한하고 원문 대신 전용 HMAC key의 keyed hash와 key version을 저장함.
- 이 key는 Week 2부터 mode `0600` credential로 공급하고 token/snapshot key와 재사용하지 않으며, 누락되면 기존 idempotency row를 무시한 채 시작하지 않음.
- rotation 동안 old key는 가장 긴 idempotency retention보다 오래 유지하고 lookup은 보존된 모든 version의 HMAC을 비교함.
- 인증 principal이 다르면 같은 key를 재사용할 수 있지만 같은 principal·method·route scope 안에서는 path/query/body를 포함한 request hash가 일치해야 함.

- terminal operation과 idempotency 결과에는 문서화된 retry window보다 긴 retention/expiry를 둠.
- active operation이나 다른 resource가 참조하는 row는 GC하지 않고, expired idempotency row를 먼저 제거한 뒤 참조 없는 terminal operation만 작은 batch로 삭제함.
- expiry 전 같은 key는 반드시 기존 결과를 반환하며 expiry 뒤 key 재사용이 새 요청으로 취급됨을 API 계약에 명시함.
- table을 무제한 history log로 사용하지 않음.

## 테스트 및 검증

- 같은 key 재전송, 같은 key의 다른 body, 지나치게 긴 key, principal 간 같은 key, retention 경계, commit 직후 crash, process spawn 직후 crash, shutdown 중 취소를 test함.
- operation이 영원히 `running`에 남거나 동일 VM process가 중복 생성되면 안 됨.
- OpenAPI와 Rust/Wasm wire type도 CI에서 호환성을 검사함.

## 완료 및 후속 범위

- 구현 브랜치와 커밋이 존재함.
- 위 테스트 및 검증 항목을 모두 통과한 뒤 완료로 판정함.
