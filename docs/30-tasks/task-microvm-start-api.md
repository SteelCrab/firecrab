---
tags:
  - firecrab
  - vm
status: 보류
updated: 2026-07-23
---

# MicroVM 시작 API 구현

## 브랜치 개요

- 브랜치: `feat/microvm-start-api`
- 커밋: `4700b0a feat: add asynchronous VM start API`
- 상태: 구현 브랜치 존재
- 변경 규모: 8개 파일, 678줄 추가, 10줄 삭제
- 목적: `POST /api/vms/{id}/start` 요청을 비동기 lifecycle 작업으로 접수한다.
- `created`, `stopped`, `error` 상태만 시작할 수 있다.

## Handler

```rust
pub async fn start_vm(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<OperationResponse>), AppError> {
    let operation = state
        .store
        .reserve_vm_operation(
            id,
            OperationKind::Start,
            &[VmState::Created, VmState::Stopped, VmState::Error],
            VmState::Starting,
            NewVmEvent::info("start_requested"),
        )
        .await
        .map_err(AppError::from_transition)?;

    state.operations.notify();

    Ok((StatusCode::ACCEPTED, Json(operation.into())))
}
```

- DB의 조건부 상태 변경과 active operation unique index가 동시 요청의 승자를 결정함.
- process map을 먼저 확인한 뒤 상태를 쓰는 방식은 두 요청이 동시에 통과할 수 있으므로 사용하지 않음.
- lifecycle job은 bare `tokio::spawn`이 아니라 [공통 operation supervisor](task-api-operation-contracts.md)에 등록함.

## Service 순서

1. VM과 고정된 template registry version을 읽고 기존 runtime identity를 reconcile한다.
2. 종료가 확인된 이전 runtime directory만 정리하고 active rootfs는 보존한다.
3. `created`이며 active generation이 없을 때만 초기 rootfs를 원자적으로 준비한다. `stopped`에서는 기존 writable rootfs를 반드시 재사용한다.
4. Week 3 network가 설치된 경우 기존 lease를 확인하고 TAP, default-deny policy와 DHCP generation을 process spawn 전에 준비한다.
5. 새 `runtime_id` directory와 config를 준비한다.
6. Firecracker를 spawn하고 전체 process identity를 DB에 저장한다.
7. API socket readiness를 확인한다.
8. Week 3 이후에는 현재 runtime generation의 Guest agent, network, SSH provisioning readiness까지 확인한다.
9. `starting -> running`과 event를 한 transaction으로 기록한다.

- 중간 실패는 artifact rollback 후 `error`, 정제된 `last_error`와 failed operation을 같은 transaction으로 기록함.
- 단, stop cancellation으로 상태가 이미 `stopping`이면 일반 start 실패로 덮지 않고 process를 정리해 `stopped/cancelled_by_stop`으로 완료함.
- runtime monitor handoff와 `starting -> running` transaction도 `cancel_requested_at IS NULL`을 조건으로 해 stop과의 마지막 순간 경쟁에서 VM을 다시 running으로 되돌리지 않음.
- `error`에서 재시작할 때는 이전 process, socket, TAP과 artifact가 없거나 안전하게 reconcile됐는지 먼저 확인함.

## 응답 계약

- 접수 성공: `202 Accepted`
- VM 없음: `404 Not Found`
- 허용되지 않은 상태 또는 동시 start 패배: `409 Conflict`
- 형식이 틀린 UUID: `400 Bad Request`

## 테스트 및 검증

- 동일 VM에 start 요청 20개를 동시에 보내도 하나만 새 `202` operation을 만들고 Firecracker process가 하나만 생성되어야 함.
- 같은 idempotency key 재전송은 새 operation이 아니라 기존 operation을 반환해야 함.

## 완료 및 후속 범위

- 구현 브랜치와 커밋이 존재함.
- 위 테스트 및 검증 항목을 모두 통과한 뒤 완료로 판정함.
