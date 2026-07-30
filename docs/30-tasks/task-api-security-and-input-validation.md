---
tags:
  - firecrab
  - api
status: 완료
scope: 2주차
updated: 2026-07-23
---

# 관리 API 보안 및 입력 계약 강화

## 브랜치 개요

- 브랜치: `feat/api-security-input-validation`
- 커밋: `ca10d3a feat: harden API input and network defaults`
- 상태: 구현 브랜치 존재
- 변경 규모: 9개 파일, 482줄 추가, 29줄 삭제
- 목적: 인증과 TLS가 없는 관리 API를 기본적으로 loopback에만 노출하고, CORS와 요청 본문 및 VM 필드를 서버에서 검증한다.

## 서버 경계

```rust
use std::{sync::Arc, time::Duration};

use axum::{
    extract::{DefaultBodyLimit, Request, State},
    http::{header, HeaderName, HeaderValue, Method},
    middleware::{self, Next},
    response::Response,
};
use tokio::sync::Semaphore;
use tower_http::cors::CorsLayer;

#[derive(Clone)]
struct RequestLimits {
    permits: Arc<Semaphore>,
    timeout: Duration,
}

async fn enforce_origin(
    State(policy): State<HttpPolicy>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        if !policy.allowed_origins.contains(origin) {
            return Err(AppError::forbidden_origin());
        }
    }
    Ok(next.run(request).await)
}

async fn enforce_limits(
    State(limits): State<RequestLimits>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let _permit = limits
        .permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::too_many_requests())?;

    tokio::time::timeout(limits.timeout, next.run(request))
        .await
        .map_err(|_| AppError::gateway_timeout())
}

let cors = CorsLayer::new()
    .allow_origin(HeaderValue::from_static("http://localhost:8080"))
    .allow_methods([Method::GET, Method::POST, Method::DELETE])
    .allow_headers([
        header::CONTENT_TYPE,
        HeaderName::from_static("idempotency-key"),
    ]);

let app = routes(state)
    .layer(DefaultBodyLimit::max(64 * 1024))
    .layer(middleware::from_fn_with_state(policy, enforce_origin))
    .layer(middleware::from_fn_with_state(limits, enforce_limits))
    .layer(cors);

let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
```

- bind 주소와 origin은 환경 변수로 바꿀 수 있게 하되, 빈 값의 기본값은 각각 `127.0.0.1:3000`과 local development UI origin으로 둔다.
- production frontend는 same-origin이므로 cross-origin CORS를 비활성화함.
- CORS header만으로 인증이나 접근 제어를 대신할 수 없으므로 origin을 실제로 거부하려면 위와 같은 middleware가 필요하다.
- `Origin`이 없는 CLI 요청은 통과시키되 Week 5 인증/TLS가 아직 없으면 non-loopback bind 설정은 startup에서 거부함.
- 이후에도 non-loopback은 인증과 TLS가 둘 다 검증된 경우만 허용하고 proxy header를 신뢰하려면 명시적인 trusted proxy allowlist가 필요함.
- semaphore는 대기 queue를 무제한으로 만들지 않고 즉시 구조화된 `429`를 반환하며 timeout은 구조화된 `504`로 변환함.
- DB transaction과 snapshot 같은 장기 operation은 HTTP timeout 안에서 실행하지 않고 먼저 영속 operation으로 접수함.

## 입력 검증

```rust
fn validate_create(req: &CreateVmRequest, templates: &TemplateRegistry) -> FieldErrors {
    let mut fields = FieldErrors::new();

    if !valid_vm_name(&req.name) {
        fields.insert("name", "must be 1-64 ASCII letters, numbers, '.', '_' or '-'");
    }
    if !templates.contains(&req.template) {
        fields.insert("template", "is not supported");
    }
    if !(1..=32).contains(&req.cpu) {
        fields.insert("cpu", "must be between 1 and 32");
    }
    if !(128..=32_768).contains(&req.ram) {
        fields.insert("ram", "must be between 128 and 32768 MiB");
    }

    fields
}
```

- VM 이름은 첫 문자가 영문 또는 숫자이고 나머지는 영문, 숫자, `.`, `_`, `-`만 허용한다.
- `CreateVmRequest`에는 `#[serde(deny_unknown_fields)]`를 적용한다.

- `Json<T>`의 기본 rejection status를 그대로 노출하지 않음.
- custom extractor에서 content type 누락은 `415 unsupported_media_type`, syntax·unknown field·field type 오류는 `400 invalid_json`, body 초과는 `413 request_too_large`로 공통 `AppError`에 매핑함.
- `cpu: 1.5`처럼 `u8`로 역직렬화할 수 없는 값도 이 경로로 처리함.

## 구조화된 오류

```rust
#[derive(Serialize)]
struct ErrorResponse {
    error: ApiError,
}

#[derive(Serialize)]
struct ApiError {
    code: String,
    message: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    fields: BTreeMap<String, String>,
    request_id: Uuid,
}
```

- JSON 오류는 `400`, body 초과는 `413`, JSON이 아닌 content type은 `415`, 허용하지 않은 origin은 origin middleware에서 `403`으로 거부한다.
- timeout과 concurrency rejection도 JSON 오류 envelope로 변환함.
- DB와 host 내부 경로 같은 상세 오류는 응답에 포함하지 않는다.
- 공통 구현은 [API 오류·idempotency·비동기 operation 계약](task-api-operation-contracts.md)을 따름.

## 테스트 및 검증

- 정상 요청과 함께 잘못된 이름, 미등록 template, 소수 CPU, 범위 밖 RAM, unknown field, 64 KiB 초과 body, timeout과 concurrency 초과를 각각 테스트한다.
- 모든 오류가 content type과 request ID를 포함한 같은 envelope를 사용해야 한다.

## 완료 및 후속 범위

- 구현 브랜치와 커밋이 존재함.
- 위 테스트 및 검증 항목을 모두 통과한 뒤 완료로 판정함.
