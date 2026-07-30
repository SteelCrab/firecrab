---
tags:
  - firecrab
  - snapshot
status: 미완료
scope: 4주차
updated: 2026-07-23
---

# 격리·관측·snapshot 통합 테스트 구현

## 브랜치 개요

- 브랜치: `test/isolation-observability-snapshot-tests` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: jailer, cgroup, filesystem 권한, metrics, tracing과 snapshot의 정상·실패·복구 경로를 Rust test harness로 검증함.

## Test fixture

```rust
pub struct IsolatedVmFixture {
    pub vm_id: Uuid,
    pub artifact_dir: TempDir,
    pub fake_runtime: FakeJailerRuntime,
    pub metrics: TestMetricsSink,
}

impl IsolatedVmFixture {
    pub async fn assert_clean(self) {
        assert!(!self.fake_runtime.process_alive(self.vm_id).await);
        assert!(!self.fake_runtime.cgroup_exists(self.vm_id).await);
    }
}
```

## 일반 CI matrix

- helper protocol에서 임의 path, UID, cgroup, command 거부
- cgroup limit 계산과 stale cgroup 복구
- metrics FIFO의 느린 reader, malformed JSON, bounded retention
- request ID 전파와 secret redaction
- snapshot operation 동시성, rootfs 포함 atomic publish, checksum/HMAC, quota와 in-use GC
- checkpoint commit 전 resume와 commit 후 source 종료, one-shot consume 및 resume 응답 유실 복구
- metrics polling 취소, stale 응답, snapshot action 상태와 API 재동기화

## 실제 KVM matrix

```sh
FIRECRAB_KVM_TEST=1 cargo test --test isolation_snapshot -- --nocapture
```

- 실제 test에서는 chroot 탈출 실패, CPU/memory 제한, full snapshot의 memory·state·rootfs 생성과 one-shot 복원, Firecracker crash와 API 재시작을 검증함.
- 원래 TAP/block/vsock path 재구성, load 전 metrics/log 설정, wall clock/agent generation, network reconnect, snapshot memory pin과 artifact mode도 확인함.
- 같은 snapshot의 두 번째 restore와 checkpoint 후 일반 start가 거부되어야 함.

## 테스트 및 검증

- test 실패와 강제 중단 뒤에도 process, mount, cgroup, socket과 임시 snapshot이 남지 않아야 함.
- 실패 artifact는 민감 정보 제거 후 명시적으로 요청한 경우만 보존함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
