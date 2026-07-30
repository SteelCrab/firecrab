---
tags:
  - firecrab
  - operations
status: 미완료
scope: 5주차
updated: 2026-07-23
---

# Graceful shutdown 및 maintenance mode 구현

## 브랜치 개요

- 브랜치: `feat/graceful-shutdown-and-maintenance` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: SIGTERM이나 upgrade 시 신규 변경 작업을 차단하고 진행 중 task를 제한 시간 동안 drain한 뒤 복구 가능한 상태로 종료함.

## Runtime 제어

```rust
#[derive(Clone)]
pub struct ShutdownController {
    pub cancellation: tokio_util::sync::CancellationToken,
    pub operation_jobs: tokio_util::task::TaskTracker,
    pub runtime_monitors: RuntimeMonitorRegistry,
    pub draining: DrainGate,
    pub maintenance: MaintenanceStore,
}
```

```rust
let shutdown_controller = controller.clone();
let shutdown = async move {
    wait_for_shutdown_signal().await;
    shutdown_controller.draining.close_locally();
    match tokio::time::timeout(
        config.control_timeout,
        shutdown_controller.maintenance.record_shutdown_lease(process_instance_id),
    ).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(%error, "shutdown lease write failed"),
        Err(_) => tracing::warn!("shutdown lease write timed out"),
    }
    shutdown_controller.cancellation.cancel();
};

axum::serve(listener, app)
    .with_graceful_shutdown(shutdown)
    .await?;

controller.runtime_monitors.begin_handoff();
controller.operation_jobs.close();
if tokio::time::timeout(config.drain_timeout, controller.operation_jobs.wait()).await.is_err() {
    tracing::warn!("operation drain timeout; recovery will resume persisted phases");
}
controller.runtime_monitors.finish_handoff_to_runtime_helper().await?;
```

- signal을 받은 즉시 in-memory drain gate를 닫아 DB가 느리거나 고장 나도 신규 변경 요청을 거부하고 readiness를 `503`으로 바꿈.
- Axum graceful shutdown이 새 connection을 받지 않는 것과 handler의 drain gate를 함께 사용해 signal 직전 들어온 요청도 차단함.

- runtime monitor registry는 handoff 시작 뒤 새 monitor 등록을 거부함.
- 이미 process를 spawn한 launch job은 identity를 영속화하고 helper 소유를 확인하며, process 전 단계 job은 취소됨.
- 따라서 operation drain과 monitor handoff 사이에 추적되지 않는 새 Child가 생기지 않음.

- 운영자가 설정한 maintenance mode와 reason은 DB singleton state에 compare-and-set으로 저장하고 재시작 후에도 명시적으로 해제할 때까지 유지함.
- 반면 SIGTERM의 shutdown lease는 process instance와 expiry를 가진 일시 상태이며 다음 startup recovery가 소유 process 부재를 확인한 뒤 정리함.
- 모든 service restart가 영구 maintenance로 남아 수동 해제를 요구하면 안 됨.
- read-only 조회 정책은 유지할 수 있음.

## Operation 정책

- DB transaction 중인 future를 강제 abort하지 않고 commit 또는 rollback까지 기다림.
- 아직 irreversible phase에 들어가지 않은 operation만 cooperative cancellation하고 reservation을 반환함.
- 이미 Firecracker를 실행한 operation은 상태와 process identity를 DB에 기록함.
- timeout 후 daemon이 종료돼도 다음 시작의 recovery가 이어받을 lease와 phase를 남김.
- runtime helper의 wait/reap ownership을 확인하지 못하면 보존 정책으로 orphan하지 않고 exact identity의 VM을 stop하거나 shutdown 자체를 실패시키는 configured fail-safe를 적용함.

- 기본 종료 정책은 실행 중 VM을 강제 중지하지 않음.
- 단, 이는 Firecracker가 API/helper service unit의 control group이 아니라 별도 `firecrab-vm.slice`/transient scope에 배치되어 systemd service stop의 kill 대상이 아니고 runtime recovery가 검증된 배포에서만 허용함.
- 이 격리가 없으면 shutdown은 모든 VM을 먼저 drain해야 하며 `KillMode` 우연에 의존해 orphan process를 남기지 않음.
- 운영자가 모든 VM 중지를 요구하는 별도 drain 명령도 제공함.

## 테스트 및 검증

- 각 lifecycle phase와 DB 장애 중 SIGTERM을 주입함.
- signal 직후 신규 변경 요청이 거부되고 SQLite integrity, 중복 process, 부분 artifact가 없어야 함.
- 정상 service restart는 stale shutdown lease를 자동 정리하고 operator maintenance만 유지하며 operation은 terminal 상태로 수렴해야 함.
- systemd `TimeoutStopSec`는 application drain timeout보다 길어야 함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
