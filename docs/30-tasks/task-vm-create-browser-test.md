---
tags:
  - firecrab
  - vm
status: 완료
scope: 2주차
updated: 2026-07-23
---

# VM 생성 브라우저 테스트 페이지 구현

## 브랜치 개요

- 브랜치: `feat/vm-create-browser-test`
- 커밋: `f0f13ad feat: add browser test page for vm create`
- 상태: 구현 브랜치 존재
- 변경 규모: 2개 파일, 627줄 추가
- 목적: 정적 브라우저 페이지에서 Rust API의 `POST /api/vms`를 호출하고 요청 상태와 응답을 표시한다.

## Rust API 설정

- 개발 UI origin만 허용한다.
- 전체 origin을 뜻하는 `Any`는 로컬 개발 초기 단계에서만 사용한다.

- 현재 `main`의 browser page는 동작하지만 API는 origin, method와 header에 모두 `Any`를 사용함.
- 따라서 이 task는 UI 기준 기초 완료이며 아래 CORS 제한은 [관리 API 보안 및 입력 계약 강화](task-api-security-and-input-validation.md)에서 구현해야 함.

```rust
use axum::http::{HeaderValue, Method};
use tower_http::cors::CorsLayer;

let cors = CorsLayer::new()
    .allow_origin(HeaderValue::from_static("http://localhost:8080"))
    .allow_methods([Method::POST])
    .allow_headers([axum::http::header::CONTENT_TYPE]);

let app = Router::new()
    .route("/api/vms", post(create_vm))
    .layer(cors)
    .with_state(state);
```

## 브라우저 요청

```javascript
const response = await fetch("http://127.0.0.1:3000/api/vms", {
  method: "POST",
  headers: { "Content-Type": "application/json" },
  body: JSON.stringify({ name, template, cpu, ram }),
});

const body = await response.json();
if (!response.ok) throw new Error(body.error?.message ?? "request failed");
```

- 버튼은 요청 중 disabled 상태로 두고 성공·실패 모두에서 다시 활성화한다.
- 클라이언트 검증은 편의 기능이며 Rust API 검증을 대체하지 않는다.

- production frontend는 API와 same-origin으로 제공하고 상대 URL `/api/vms`를 사용함.
- 별도 origin CORS는 로컬 개발용으로만 유지함.

## 테스트 및 검증

```sh
cd firecrab-frontend
python3 -m http.server 8080
```

- 브라우저에서 한 번의 클릭으로 요청이 한 번만 전송되고 성공 시 UUID, 실패 시 API 오류 메시지가 표시되어야 한다.

## 완료 및 후속 범위

- 구현 브랜치와 커밋이 존재함.
- 위 테스트 및 검증 항목을 모두 통과한 뒤 완료로 판정함.
