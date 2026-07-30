---
tags:
  - firecrab
status: 미완료
scope: 4주차
updated: 2026-07-23
---

# 서비스 health 및 readiness API 구현

## 브랜치 개요

- 브랜치: `feat/service-health-readiness` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: process 생존 여부와 새 VM 작업을 받을 준비 상태를 분리함.
- liveness는 외부 dependency 장애로 실패시키지 않음.

## 응답 모델

```rust
#[derive(Serialize)]
pub struct HealthResponse {
    pub status: HealthStatus,
    pub components: BTreeMap<&'static str, ComponentHealth>,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Ok,
    Degraded,
    Unavailable,
}
```

## Endpoint

```rust
pub async fn live() -> StatusCode {
    StatusCode::OK
}

pub async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let report = tokio::time::timeout(
        Duration::from_secs(2),
        state.health.check_readiness(),
    ).await;

    match report {
        Ok(report) if report.is_ready() => (StatusCode::OK, Json(report)),
        Ok(report) => (StatusCode::SERVICE_UNAVAILABLE, Json(report)),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, Json(HealthResponse::timeout())),
    }
}
```

- readiness는 singleton data-root lock, SQLite query, operation supervisor heartbeat, runtime/network helper handshake, helper가 수행한 KVM preflight, Firecracker binary, template registry와 disk/audit capacity threshold를 확인함.
- unprivileged API가 `/dev/kvm`이나 jail artifact를 직접 열지 않음.
- 확인은 read-only이며 bridge 생성 같은 host 변경을 수행하지 않음.
- 비싼 check는 bounded worker에서 짧게 cache하고 동시 readiness 요청이 helper와 disk를 증폭 호출하지 않게 single-flight 처리함.

## 노출 정책

- `/health/live`는 상세 정보를 주지 않고 process watchdog 용도로 사용함.
- `/health/ready`의 내부 오류와 path는 외부 응답에서 정제하고 상세 원인은 구조화 log에 남김.

- health route는 lifecycle handler의 saturated semaphore 뒤에 두지 않고 작고 독립된 concurrency/time budget을 사용함.
- 그렇다고 무제한 bypass하지 않으며 listener/request body가 없는 GET만 허용함.
- lifecycle overload 중에도 liveness/readiness가 bounded 시간에 응답해야 함.

## 테스트 및 검증

- DB lock, helper 중단, KVM 권한 제거, template 누락을 각각 주입함.
- liveness는 `200`을 유지하고 readiness만 제한 시간 안에 `503`과 component 상태를 반환해야 함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
