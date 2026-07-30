---
tags:
  - firecrab
  - observability
status: 미완료
scope: 4주차
updated: 2026-07-23
---

# VM 관측 대시보드 구현

## 브랜치 개요

- 브랜치: `feat/observability-dashboard` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: Rust/Wasm UI에서 VM metrics, lifecycle event, request correlation과 API dependency 상태를 한 화면에서 조회함.

## 화면 구조

```text
VM detail
├── Overview: state, uptime, resources, connection
├── Metrics: CPU, memory, block, network
├── Events: lifecycle event와 request ID
└── Health: Firecracker, network, storage 상태
```

- 운영 화면은 좁은 card 모음보다 scan 가능한 table, compact chart와 상태 badge를 사용함.
- 내부 PID, host path, raw metrics payload는 노출하지 않음.

## Rust 모델

```rust
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct VmMetricsWindow {
    pub vm_id: Uuid,
    pub from: OffsetDateTime,
    pub to: OffsetDateTime,
    pub resolution_seconds: u32,
    pub samples: Vec<VmMetricSample>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct VmMetricSample {
    pub timestamp: OffsetDateTime,
    pub cpu_percent: f32,
    pub host_process_memory_bytes: u64,
    pub block_bytes: u64,
    pub network_bytes: u64,
    pub error_count: u64,
}
```

- Prometheus text를 browser에서 직접 해석하지 않고 UI용 bounded JSON endpoint를 제공함.
- API가 time range, 최대 sample 수와 downsampling을 강제함.

- Firecracker 기본 metrics만으로 정확한 Guest 내부 사용 memory를 알 수 있다고 표시하지 않음.
- `host_process_memory_bytes`는 cgroup/process 관측값이며 Guest memory usage가 필요하면 지원되는 balloon stats 또는 Guest agent 지표를 별도 source label과 함께 사용함.

## Polling

```rust
async fn poll_metrics(api: MetricsApi, vm_id: Uuid, cancelled: Rc<Cell<bool>>) {
    let mut interval = gloo_timers::future::IntervalStream::new(5_000);
    while interval.next().await.is_some() && !cancelled.get() {
        if document_is_visible() {
            api.refresh_window(vm_id).await;
        }
    }
}
```

- 탭이 숨겨지거나 component가 unmount되면 polling을 중단함.
- 같은 VM 요청을 합치고 느린 응답이 최신 응답을 덮어쓰지 않게 sequence를 비교함.

## 오류 처리

- `404`: VM 목록으로 이동하고 삭제됨을 표시함.
- `401/403`: session 갱신 또는 권한 안내로 전환함.
- `503`: readiness component 상태와 마지막 정상 sample을 함께 표시함.
- parse/timeout: chart를 지우지 않고 stale 표시와 재시도를 제공함.

## 테스트 및 검증

- sample 0개, 최대 window, VM 종료, API restart, 느린 network와 background tab을 test함.
- DOM node와 timer 수가 계속 증가하지 않고 keyboard와 screen reader로 탭·상태를 탐색할 수 있어야 함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
