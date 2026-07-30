---
tags:
  - firecrab
  - vm
status: 보류
updated: 2026-07-23
---

# MicroVM 중지 API 구현

## 브랜치 개요

- 브랜치: `feat/microvm-stop-api`
- 커밋: `3d15b4f feat: add asynchronous VM stop API`
- 상태: 구현 브랜치 존재
- 변경 규모: 6개 파일, 520줄 추가, 9줄 삭제
- 목적: `POST /api/vms/{id}/stop`으로 `starting` 또는 `running` VM의 종료를 비동기로 접수한다.

## Handler

```rust
pub async fn stop_vm(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Extension(request_context): Extension<RequestContext>,
) -> Result<(StatusCode, Json<OperationResponse>), AppError> {
    let reservation = state
        .store
        .request_stop(
            id,
            request_context.idempotency(),
        )
        .await
        .map_err(AppError::from_transition)?;

    state.operations.notify();

    Ok((StatusCode::ACCEPTED, Json(reservation.operation().into())))
}
```

- `running`에서는 `stopping` 전이와 새 stop operation을 같은 transaction에 만듦.
- `starting`에서는 active start operation 때문에 두 번째 active operation을 만들지 않고, `starting -> stopping`, start row의 `cancel_requested_at/reason`, stop 요청의 별도 idempotency record와 event를 함께 commit한 뒤 기존 operation을 반환함.
- start worker가 cancellation을 phase 경계에서 확인해 아직 spawn 전이면 rollback하고, spawn 뒤면 exact process를 종료한 후 operation을 `cancelled`, VM을 `stopped`로 만듦.

- start 취소 응답은 `acceptedIntent=stop`과 기존 operation ID를 함께 반환함.
- 이 경우 operation의 terminal `cancelled`는 stop intent 실패가 아니라 start가 취소됐다는 뜻이며, UI/API client는 최종 VM `stopped` 상태와 `cancelled_by_stop` result code를 함께 확인함.

## 종료 정책

1. `starting`이면 readiness task를 취소하고 현재 child를 회수한다.
2. `running`이면 vsock guest agent shutdown을 우선 시도하고 지원되는 x86_64 Guest에서만 `SendCtrlAltDel`을 fallback으로 사용한다.
3. 설정된 timeout 동안 `child.wait()`를 기다린다.
4. timeout이면 PID identity를 재검증한 후 `SIGTERM`, 다시 timeout이면 `SIGKILL`을 보낸다.
5. 실제 종료를 확인한 후 VM network policy와 TAP, socket과 runtime artifact를 정리한다. IP/MAC/CID lease는 stop 동안 유지한다.
6. snapshot에서 복원된 process라면 jail의 state/memory read-only bind mount를 해제한 뒤 memory backend in-use lease를 반환한다. 둘 다 process exit 확인 전에는 수행하지 않는다.
7. 정리 결과와 `stopped` 전환을 기록한다.

- 중지 요청을 접수했다는 이유만으로 즉시 `stopped`로 기록하지 않음.
- process supervisor의 exit event와 stop job은 operation ID와 조건부 transition으로 경쟁을 해소함.
- 종료 실패는 `error` 상태와 event로 남김.

## 응답 계약

- 접수 성공: `202 Accepted`
- VM 없음: `404 Not Found`
- 이미 중지됐거나 삭제 중인 VM: `409 Conflict`

## 테스트 및 검증

- 정상 shutdown, timeout 뒤 강제 종료, start 중 stop, 이미 종료된 child를 각각 테스트한다.
- 다른 프로세스로 재사용된 PID에는 signal이 전송되지 않아야 한다.

## 완료 및 후속 범위

- 구현 브랜치와 커밋이 존재함.
- 위 테스트 및 검증 항목을 모두 통과한 뒤 완료로 판정함.
