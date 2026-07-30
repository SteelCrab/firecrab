---
tags:
  - firecrab
  - observability
status: 미완료
scope: 4주차
updated: 2026-07-23
---

# 구조화된 logging 및 tracing 구현

## 브랜치 개요

- 브랜치: `feat/structured-logging-and-tracing` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: HTTP 요청, lifecycle operation, helper 요청과 Firecracker 결과를 request ID와 VM ID로 연결함.

## 초기화

```rust
pub fn init_tracing() -> anyhow::Result<()> {
    let (writer, guard) = tracing_appender::non_blocking(std::io::stdout());
    tracing_subscriber::fmt()
        .json()
        .with_writer(writer)
        .with_current_span(true)
        .with_span_list(true)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "firecrab=info,tower_http=info".into()),
        )
        .try_init()?;
    retain_log_guard(guard);
    Ok(())
}
```

- production filter는 allowlist module/level만 허용하고 dependency trace를 임의 환경값으로 켜 request payload가 늘어나지 않게 검증함.
- non-blocking writer queue는 bounded하며 drop counter와 rate-limited 경고를 제공함.
- 보안 audit은 lossy tracing queue에만 의존하지 않고 DB transaction/outbox 계약을 사용함.

## 요청 span

```rust
#[tracing::instrument(
    skip(state, request),
    fields(request_id = %request_id, vm_id = %vm_id)
)]
async fn start_vm_operation(
    state: AppState,
    request_id: Uuid,
    vm_id: Uuid,
    request: StartVmRequest,
) -> Result<(), AppError> {
    state.vm_service.start(vm_id).await
}
```

- API가 매 요청 생성한 server request ID를 helper protocol과 lifecycle event에 전달함.
- HTTP response에도 `X-Request-ID`를 넣고 client가 보낸 값은 검증 후 별도 correlation hint로만 기록함.
- client 값으로 server ID를 대체하지 않음.

## Redaction

- Authorization header, public/private key 본문, 환경 변수 전체, rootfs 내용, raw request body를 log에 남기지 않음.
- host 내부 경로는 debug level에서도 필요한 최소 식별자만 기록함.

## 테스트 및 검증

- 하나의 start 요청이 API 접수, DB 전이, helper spawn, readiness, 완료 event까지 같은 request ID로 검색되어야 함.
- 실패 log에도 secret과 private key가 없어야 함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
