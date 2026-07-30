---
tags:
  - firecrab
  - vm
status: 미완료
scope: 4주차
updated: 2026-07-23
---

# Lifecycle event 저장 및 조회 구현

## 브랜치 개요

- 브랜치: `feat/lifecycle-event-api`
- 커밋: `e335084 feat: add lifecycle event API`
- 상태: 구현 브랜치 존재
- 변경 규모: 6개 파일, 218줄 추가, 4줄 삭제
- 목적: VM의 생성, 상태 전이, 시작·중지 성공과 실패를 구조화된 event로 저장하고 `GET /api/vms/{id}/events`에서 조회한다.
- console log와 혼동하지 않도록 API 명칭을 event로 통일함.

## Event 모델

```rust
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct VmEvent {
    pub id: i64,
    pub vm_id: String,
    pub request_id: String,
    pub operation_id: Option<String>,
    pub kind: VmEventKind,
    pub code: String,
    pub message: String,
    pub details: BTreeMap<String, String>,
    pub created_at: String,
}
```

- 상태 변경과 event 삽입은 하나의 transaction에서 처리한다.

```rust
pub async fn transition_with_event(
    tx: &mut Transaction<'_, Sqlite>,
    vm_id: Uuid,
    from: &[VmState],
    to: VmState,
    event: NewVmEvent,
) -> Result<(), StoreError> {
    conditional_transition(tx, vm_id, from, to).await?;
    insert_event(tx, vm_id, event).await?;
    Ok(())
}
```

## 조회 Handler

```rust
pub async fn list_vm_events(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<EventQuery>,
) -> Result<Json<EventPage>, AppError> {
    Ok(Json(state.store.list_events(id, query.after_id, query.limit).await?))
}
```

- SQL은 VM 존재/삭제 정책을 먼저 확인한 뒤 `WHERE vm_id = ? AND id > ? ORDER BY id ASC LIMIT ?`로 안정적인 cursor pagination을 제공하고 기본 100, 최대 500 limit을 둠.
- `kind`와 `code`는 allowlist enum으로 검증하고 `details`에는 phase, 이전·다음 상태처럼 정해진 key만 넣음.
- 내부 오류 문자열, console stdout/stderr, PID와 host path를 DB message나 API에 넣지 않고 정제된 event만 저장함.
- request ID와 operation ID로 tracing 및 비동기 결과를 연결함.

- `after_id`는 0 이상, limit은 `1..=500`만 허용함.
- lifecycle event는 audit log가 아니므로 VM별 row/age retention과 전체 storage budget을 두고 terminal operation이 참조하지 않는 오래된 row만 bounded batch로 정리함.
- 삭제 cursor보다 오래된 요청에는 빈 page로 history 전체가 존재하는 것처럼 오해시키지 않고 retention 시작 시점을 metadata로 반환함.

## 테스트 및 검증

- 상태가 바뀌었는데 event가 없거나 event만 있고 상태가 그대로인 경우가 없어야 한다.
- 존재하지 않는 VM은 `404`, 잘못된 UUID는 `400`을 반환해야 한다.
- 여러 event의 순서가 API 재시작 후에도 유지되어야 한다.
- event page 경계에 누락·중복이 없고 console 내용과 host path가 응답에 없어야 한다.

## 완료 및 후속 범위

- 구현 브랜치와 커밋이 존재함.
- 위 테스트 및 검증 항목을 모두 통과한 뒤 완료로 판정함.
