---
tags:
  - firecrab
  - snapshot
status: 미완료
scope: 7주차
updated: 2026-07-23
---

# VM snapshot 관리 UI 구현

## 브랜치 개요

- 브랜치: `feat/vm-snapshot-ui` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: VM별 snapshot 목록, one-shot full checkpoint 생성, 소비 상태, operation 진행과 동일 VM 복원·폐기를 Rust/Wasm UI에서 제공함.

## 모델

```rust
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SnapshotSummary {
    pub id: Uuid,
    pub vm_id: Uuid,
    pub status: SnapshotStatus,
    pub created_at: OffsetDateTime,
    pub logical_size_bytes: Option<u64>,
    pub allocated_size_bytes: Option<u64>,
    pub restore_policy: RestorePolicy,
    pub consumed_at: Option<OffsetDateTime>,
    pub compatible: bool,
    pub incompatibility_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotStatus {
    Creating,
    Ready,
    Failed,
    Deleting,
}
```

- snapshot memory에는 secret이 포함될 수 있으므로 파일 download와 host path 표시는 제공하지 않음.
- `restorable`은 status만 보고 계산하지 않고 compatibility, one-shot 소비 여부와 VM checkpoint ownership을 포함한 서버 판정을 사용함.

- 표시 크기는 memory, state와 rootfs 사본을 모두 합한 logical/allocated 값을 구분함.
- reflink의 현재 allocated size가 향후 COW upper bound가 아님을 quota 화면에 반영함.
- snapshot이 disk를 포함하지 않은 것처럼 오해할 문구를 사용하지 않고, 생성 중 예상 pause 시간과 crash-consistent 특성을 명시함.

## Action 규칙

```rust
fn snapshot_actions(vm: VmState, snapshot: &SnapshotSummary) -> Vec<SnapshotAction> {
    match (vm, snapshot.status, snapshot.compatible) {
        (VmState::Checkpointed, SnapshotStatus::Ready, true)
            if snapshot.consumed_at.is_none() => vec![SnapshotAction::Restore, SnapshotAction::Discard],
        (VmState::Checkpointed, SnapshotStatus::Ready, false)
            if snapshot.consumed_at.is_none() => vec![SnapshotAction::Discard],
        (VmState::Stopped, SnapshotStatus::Ready, _)
            if snapshot.consumed_at.is_some() => vec![SnapshotAction::Delete],
        _ => Vec::new(),
    }
}
```

- 새 one-shot checkpoint 생성은 VM이 `running`이고 active lifecycle operation이 없을 때만 활성화함.
- 성공하면 VM이 자동 재개되지 않고 `checkpointed`가 되며, restore 또는 checkpoint 폐기 전에는 일반 start를 할 수 없음.
- UI의 disabled 상태는 편의 기능이며 API가 동일 조건을 다시 검증함.

## 비동기 operation

- `202 Accepted`의 operation ID를 저장하고 operation endpoint를 polling함.
- 중복 클릭을 차단하되 browser reload 뒤에도 서버의 active operation을 다시 조회해 진행 상태를 복원함.

- 생성 dialog에는 VM이 checkpoint 시점에서 멈추고 snapshot을 한 번 restore하거나 폐기하기 전까지 일반 start할 수 없음을 표시함.
- 복원 dialog에는 one-shot snapshot이 resume intent에서 소비됨, memory와 writable disk가 checkpoint 시점으로 이어짐, 기존 network connection은 보존되지 않음, 호환성 검사가 다시 수행됨을 표시함.
- discard/delete도 비동기 operation을 추적하고 in-use conflict를 표시함.
- 사용자가 VM 이름을 다시 입력해야 최종 위험 요청을 보낼 수 있게 함.

## 테스트 및 검증

- 생성 성공·실패, pause timeout, checkpoint 폐기, consumed/incompatible snapshot, restore 중 API restart와 `409 Conflict`를 test함.
- UI가 consumed snapshot을 다시 활성화하지 않고 상태와 서버 operation이 어긋나면 서버 상태를 우선해 다시 동기화해야 함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
