---
tags:
  - firecrab
  - vm
status: 미완료
scope: 5주차
updated: 2026-07-23
---

# MicroVM 삭제 API 구현

## 브랜치 개요

- 브랜치: `feat/microvm-delete-api`
- 커밋: `4e64a21 feat: add asynchronous VM delete API`
- 상태: 구현 브랜치 존재
- 변경 규모: 5개 파일, 285줄 추가, 4줄 삭제
- 목적: `DELETE /api/vms/{id}`로 실행 중이 아닌 VM의 artifact와 네트워크 자원을 제거하고 soft delete한다.

## Handler

```rust
pub async fn delete_vm(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<OperationResponse>), AppError> {
    let operation = state
        .store
        .reserve_vm_operation(
            id,
            OperationKind::Delete,
            &[VmState::Created, VmState::Stopped, VmState::Error],
            VmState::Deleting,
            NewVmEvent::info("delete_requested"),
        )
        .await
        .map_err(AppError::from_transition)?;

    state.operations.notify();
    Ok((StatusCode::ACCEPTED, Json(operation.into())))
}
```

## 정리 순서

1. process identity가 존재하지 않는지 다시 확인한다.
2. snapshot 기능이 설치된 schema에서는 연결된 snapshot과 in-use/backup lease가 없는지 확인하고 있으면 `409 snapshot_exists`로 거부한다.
3. anti-spoofing/firewall rule과 VM 전용 TAP을 idempotent하게 정리한다.
4. API socket, config, console log, rootfs를 정리한다.
5. network·artifact 정리가 모두 성공한 뒤 IP/MAC/CID/UID lease를 반환한다.
6. DB의 runtime/generation 정보를 비우고 `deleting -> deleted`, 완료 event와 operation을 commit한다.

- 정리 실패 시 무조건 `deleted`로 숨기지 않는다.
- 재시도 가능한 `error` 또는 `deleting` 상태와 실패 event를 남겨 복구 작업이 이어서 처리하게 한다.

## 응답 계약

- 접수 성공: `202 Accepted`와 operation
- VM 없음 또는 이미 soft delete됨: `404 Not Found`
- `starting`, `running`, `stopping`, `checkpointed` 또는 snapshot 보유: `409 Conflict`

## 테스트 및 검증

- 삭제 operation 성공 후 기본 목록과 상세 조회에서 VM이 보이지 않고 해당 VM의 rootfs, socket, TAP, policy, lease가 없어야 함.
- 정리 단계마다 crash를 주입해 재시도 가능하고 다른 VM의 artifact와 network가 변경되지 않아야 함.

## 완료 및 후속 범위

- 구현 브랜치와 커밋이 존재함.
- 위 테스트 및 검증 항목을 모두 통과한 뒤 완료로 판정함.
