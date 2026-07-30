---
tags:
  - firecrab
  - vm
status: 미완료
scope: 4주차
updated: 2026-07-23
---

# VM 상태 동기화 및 서버 시작 복구 구현

## 브랜치 개요

- 브랜치: `feat/vm-state-recovery`
- 커밋: `b75d221 feat: recover and reconcile VM runtime state`
- 상태: 구현 브랜치 존재
- 변경 규모: 7개 파일, 609줄 추가, 5줄 삭제
- 목적: Firecracker의 예상·비예상 종료를 상태에 반영하고 API daemon 재시작 시 DB와 실제 host 자원을 조정한다.

## Runtime identity

```rust
#[derive(Debug, Clone)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_ticks: u64,
    pub executable_device: u64,
    pub executable_inode: u64,
    pub runtime_id: Uuid,
}

fn same_process(expected: &ProcessIdentity) -> anyhow::Result<bool> {
    let actual = read_process_identity(expected.pid)?;
    Ok(actual.start_ticks == expected.start_ticks
        && actual.executable_device == expected.executable_device
        && actual.executable_inode == expected.executable_inode
        && actual.runtime_id == expected.runtime_id)
}
```

- PID 하나만 비교하면 daemon이 내려간 동안 재사용된 PID를 Firecracker로 오인할 수 있음.
- 실행 중에는 pidfd를 우선 사용하고 재시작 뒤에는 start tick, executable device/inode와 검증된 command/runtime artifact identity를 함께 확인함.
- Week 4 이후에는 cgroup과 Jailer runtime ID도 필수로 검증함.
- Jailer PID namespace 사용 시 Jailer가 기록한 pid file도 검증된 jail directory handle에서 읽음.

## 시작 복구 순서

```text
DB에서 non-terminal VM 조회
        |
        v
PID identity 일치 여부 확인
   | 일치              | 불일치
   v                   v
socket/API 확인       stale socket 정리
   |                   상태를 error로 전환
   v
monitor 재등록 또는 상태 보정
```

- `running`인데 process가 없으면 `error`로 전환한다.
- `starting`이 timeout을 넘겼으면 process를 확인해 `running` 또는 `error`로 보정한다.
- `stopping`인데 process가 없으면 `stopped`로 마무리한다.
- `created`, `stopped`, `checkpointed`, `error`, `deleted` VM에는 process가 없어야 한다.
- `checkpointed`에는 restore 가능한 unconsumed snapshot이 정확히 하나 연결되어야 함.

- 복구 작업은 각 변경과 event를 transaction으로 기록하고, Firecrab 소유가 확인되지 않은 process, TAP, policy나 socket은 삭제하지 않는다.
- stopped/deleted VM의 Firecrab 소유 orphan runtime network는 정리하고 running VM의 누락 policy는 TAP을 down한 상태에서 복원함.
- snapshot memory in-use lease는 해당 runtime identity의 종료가 확인된 경우에만 반환함.

- `queued`와 `running` operation도 함께 조회함.
- phase가 process spawn 전이면 안전하게 재실행하고, spawn 이후면 process·socket·artifact를 reconcile한 뒤 이어서 완료하거나 실패 처리함.
- snapshot restore의 `resume_intent` 이후에는 snapshot을 다시 실행하지 않고 기존 process identity와 active rootfs generation을 복구함.
- active operation 없이 transition 상태인 VM도 복구 event를 만들고 명시적으로 보정함.

## 종료 감시

- `child.wait()` 결과와 영속 종료 의도를 함께 본다.
- stop 요청 뒤 exit는 `stopped`, one-shot checkpoint의 materialized phase 뒤 exit는 snapshot publish recovery로 넘기고, 의도하지 않은 non-zero exit나 signal 종료만 `error`로 기록한다.

## 테스트 및 검증

- API를 `starting`, `running`, `stopping` 각각의 시점에 강제 종료하고 재시작한다.
- DB 상태, process 수, socket과 artifact가 정의한 정책으로 수렴해야 한다.

## 완료 및 후속 범위

- 구현 브랜치와 커밋이 존재함.
- 위 테스트 및 검증 항목을 모두 통과한 뒤 완료로 판정함.
