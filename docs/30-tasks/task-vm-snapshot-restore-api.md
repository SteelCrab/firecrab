---
tags:
  - firecrab
  - snapshot
status: 미완료
scope: 7주차
updated: 2026-07-23
---

# VM snapshot 복원 API 구현

## 브랜치 개요

- 브랜치: `feat/vm-snapshot-restore-api` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: `checkpointed` 원본 VM을 자신이 소유한 호환 one-shot snapshot으로 한 번만 복원함.
- 다른 VM clone과 이미 source execution이 이어진 rollback snapshot은 지원하지 않음.

## API

```text
POST /api/vms/{vm_id}/snapshots/{snapshot_id}/restore
202 Accepted
```

```rust
pub async fn restore_snapshot(
    State(state): State<AppState>,
    Path((vm_id, snapshot_id)): Path<(Uuid, Uuid)>,
) -> Result<(StatusCode, Json<OperationResponse>), AppError> {
    let operation = state
        .store
        .reserve_restore_operation(vm_id, snapshot_id)
        .await?;

    state.operations.notify();

    Ok((StatusCode::ACCEPTED, Json(operation.into())))
}
```

- reservation transaction은 exact unconsumed snapshot을 claim하고 VM을 `checkpointed -> starting`으로 전환하며 event, quota와 queued operation을 함께 commit함.
- 조건이 바뀌었거나 다른 restore/discard가 선점했으면 `409`를 반환함.

## 호환성 검사

- snapshot `vm_id`와 요청 VM이 일치함.
- snapshot status가 `ready`, `restore_policy=one_shot`, `consumed_at IS NULL`이고 VM이 이 snapshot의 `checkpointed` 상태임.
- manifest HMAC, state, memory와 rootfs checksum이 일치함.
- architecture, Firecracker snapshot data version과 binary 지원 범위가 일치함.
- host kernel release, CPU vendor/model/features와 CPU template digest가 정책상 호환됨.
- block/TAP/vsock runtime path, kernel, machine config와 network identity가 manifest와 일치함.

## 복원 순서

- 기존 process가 없음을 확인하고 snapshot rootfs에서 새 writable rootfs generation을 clone함.
- snapshot 원본 disk와 이전 active generation은 수정하거나 즉시 삭제하지 않음.
- 동일 VM ID의 runtime/chroot, 원래 TAP 이름과 IP/MAC lease, block 상대 경로와 vsock backend를 snapshot state가 기대하는 상대 경로에 먼저 준비함.
- Firecracker는 `SnapshotRestore` mode로 config file 없이 시작해 snapshot load 전에 machine/device가 구성되지 않은 상태를 유지함.

- runtime helper는 published state/memory file을 jail의 root-owned `resources`에 read-only bind mount하고 VM GID에는 read만 허용함.
- logger와 metrics는 Firecracker가 허용하는 pre-load 단계에서 먼저 구성한 뒤 `resume_vm=false`로 snapshot load API를 호출함.
- memory backend는 snapshot memory file을 immutable하게 사용하고 VM 종료까지 in-use lease로 pin함.
- load 성공 직후 VM은 paused 상태임.
- previous disk를 `retained`, pending disk를 `active`로 바꾸고 operation phase, snapshot `consumed_at`과 `resume_intent`를 하나의 transaction으로 기록한 뒤 resume 요청을 보냄.
- resume 요청의 응답을 잃어 성공 여부가 불명확해도 같은 snapshot을 다시 실행하지 않음.

- resume 뒤 host가 발급한 새 runtime generation으로 Guest agent를 재연결하고 Guest wall clock 동기화, VMGenID를 지원하는 kernel의 entropy reseed, network와 SSH readiness를 확인한 뒤 `running`으로 전환함.
- VMGenID는 kernel PRNG에는 도움이 되지만 arbitrary userspace cached token의 replay를 막지 못하므로 one-shot 정책을 대체하지 않음.
- 기존 TCP/vsock 연결이 살아 있다고 가정하지 않음.

- `resume_intent` 전 실패는 새 process와 pending rootfs/runtime artifact를 정리하고 이전 active generation 및 `checkpointed` 상태로 rollback하며 memory lease를 반환함.
- `resume_intent` 이후 실패는 snapshot을 소비된 상태로 유지하고 새 rootfs generation과 process identity를 recovery 대상으로 남겨 `running` 또는 명확한 `error`로 수렴시킴.
- 이 시점 이후 이전 generation으로 자동 rollback하면 Guest 외부 side effect를 재실행할 수 있으므로 금지함.

## 테스트 및 검증

- 정상 복원 뒤 memory 상태, disk 시점, wall clock과 새 agent/network/SSH session이 정상인지 확인함.
- 다른 VM snapshot, consumed snapshot, 손상·변조 파일, block path, host kernel, CPU와 Firecracker snapshot format mismatch는 load 전에 거부되어야 함.
- resume 응답 유실 뒤 재시도해도 두 process나 두 timeline이 생기지 않고 실행 중 memory file 변경과 GC도 거부되어야 함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
