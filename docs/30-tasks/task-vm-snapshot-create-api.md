---
tags:
  - firecrab
  - snapshot
status: 미완료
scope: 7주차
updated: 2026-07-23
---

# VM snapshot 생성 API 구현

## 브랜치 개요

- 브랜치: `feat/vm-snapshot-create-api` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: 실행 중 VM을 pause해 crash-consistent full checkpoint를 비동기 생성함.
- 초기 production 범위에서는 source timeline을 재개하지 않는 one-shot full snapshot만 지원함.

## API

```text
POST /api/vms/{vm_id}/snapshots
202 Accepted
```

```rust
pub async fn create_snapshot(
    State(state): State<AppState>,
    Path(vm_id): Path<Uuid>,
) -> Result<(StatusCode, Json<OperationResponse>), AppError> {
    let operation = state.store.reserve_snapshot_operation(vm_id).await?;
    state.operations.notify();

    Ok((StatusCode::ACCEPTED, Json(operation.into())))
}
```

- DB의 active operation unique constraint로 start, stop, delete, snapshot 동시 실행을 차단함.

## 생성 순서

1. VM process identity, Guest boot 완료와 Firecracker API readiness를 재확인함.
2. memory와 rootfs 사본을 포함한 snapshot quota를 예약하고 비어 있는 임시 directory를 준비함.
3. Firecracker API로 VM을 pause함.
4. 새 경로에 full snapshot state와 memory 파일 생성을 요청함. 기존 파일을 재사용해 truncation하지 않음.
5. VM이 계속 pause된 상태에서 초기 scope의 writable rootfs backing file을 먼저 fsync하고 reflink 또는 full copy한 뒤 destination과 parent를 fsync함. 추가 writable drive가 발견되면 누락한 채 성공하지 않고 unsupported config로 실패함.
6. checksum과 인증 manifest를 staging에 기록하고 operation phase `checkpoint_materialized`와 expected process-exit intent를 commit함.
7. paused Firecracker process를 종료하고 정확한 process identity의 exit를 확인함. 성공 checkpoint에서는 Guest execution을 resume하지 않음.
8. snapshot directory publish, `running -> checkpointed`, ready snapshot, event와 operation 성공을 transaction으로 기록함.

- `checkpoint_materialized` commit 전 오류는 staging을 정리하고 원본 VM resume을 시도함.
- Firecracker snapshot create가 vsock transport를 reset할 수 있으므로 resume 뒤 fresh challenge로 Guest agent를 재연결하고 provisioning/network readiness를 다시 확인하기 전까지 기존 접속 정보를 노출하지 않음.
- commit 이후에는 source timeline을 다시 실행하지 않고 process 종료와 publish를 recovery가 이어받음.
- 이 commit point를 넘은 뒤 무조건 resume하는 cleanup은 snapshot과 원본 state를 둘 다 실행시키므로 금지함.
- Rust의 async cleanup을 `Drop`에만 맡기지 않고 phase별 명시적 compensation과 daemon recovery를 함께 둠.

- Guest filesystem freeze를 지원하지 않는 초기 snapshot은 crash-consistent임을 API와 UI에 명시함.
- pause만 하고 disk 사본을 생략하면 memory와 disk 시점이 달라지므로 성공으로 처리하지 않음.
- reflink가 불가능한 큰 disk는 pause 시간이 길어질 수 있어 예상 downtime과 timeout을 admission 단계에서 검사함.
- 일반 live rollback mode는 VMGenID만으로 arbitrary userspace token과 외부 side effect 중복을 해결할 수 없으므로 이 task에 포함하지 않음.

## 테스트 및 검증

- snapshot 생성, disk clone, timeout, disk full, API 연결 종료와 daemon crash를 commit point 전후에 주입함.
- commit 전 실패는 VM이 `running`으로 복구되고 성공 또는 commit 후 recovery는 source process가 종료된 `checkpointed` 상태로 수렴해야 함.
- snapshot rootfs를 수정해도 active VM rootfs가 바뀌지 않아야 하며 중복 timeline이 실행되면 안 됨.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
