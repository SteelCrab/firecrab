---
tags:
  - firecrab
  - security
status: 미완료
scope: 5주차
updated: 2026-07-23
---

# 웹 인증 및 역할 기반 UI 구현

## 브랜치 개요

- 브랜치: `feat/authentication-authorization-ui` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: Rust/Wasm frontend에 안전한 browser session과 viewer, operator, admin 역할별 route·action 제어를 적용함.

## Session 경계

- 장기 opaque API token을 `localStorage`, `sessionStorage`, URL에 저장하지 않음.
- production UI는 HTTPS same-origin session endpoint에서 token을 짧은 server-side session으로 교환하고 `__Host-` prefix, `Path=/`, `HttpOnly`, `Secure`, `SameSite=Strict`, no `Domain` cookie를 사용함.
- frontend asset도 API와 같은 origin에서 제공함.

```rust
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SessionPrincipal {
    pub id: Uuid,
    pub display_name: String,
    pub role: Role,
    pub expires_at: OffsetDateTime,
}

#[derive(Clone, PartialEq)]
pub enum AuthState {
    Loading,
    Anonymous,
    Authenticated(SessionPrincipal),
}
```

## Route와 action

```rust
fn can(role: Role, action: UiAction) -> bool {
    match role {
        Role::Viewer => matches!(action, UiAction::View),
        Role::Operator => matches!(
            action,
            UiAction::View | UiAction::CreateVm | UiAction::OperateVm | UiAction::Snapshot
        ),
        Role::Admin => true,
    }
}
```

- UI가 숨긴 action도 개발자 도구로 요청할 수 있으므로 server authorization이 항상 최종 판단함.
- `403` 응답은 UI state를 서버의 현재 principal로 다시 동기화함.

## Session 처리

- 시작 시 `/api/session/me`로 principal을 조회함.
- `401`이면 login 화면으로 이동하고 기존 민감 state를 지움.
- 만료 직전에는 server가 허용한 경우에만 rotation함.
- logout은 server session을 revoke한 뒤 frontend memory를 비움.
- cookie 기반 변경 요청에는 CSRF token 검증을 적용함.
- CSP는 `default-src 'self'`, same-origin `connect-src`, `object-src 'none'`, `base-uri 'none'`, `frame-ancestors 'none'`를 기본으로 하고 inline script와 임의 origin을 허용하지 않음.
- Trunk/Wasm build가 이 policy에서 실행되는지 CI browser test로 확인하고 session endpoint가 `Origin`과 `Sec-Fetch-Site`를 검증함.

## 테스트 및 검증

- anonymous, 각 역할, 만료, revoke, 다른 탭 logout, `401`, `403`, CSRF 실패를 test함.
- browser storage와 error telemetry에 token·cookie 원문이 없어야 함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
