---
tags:
  - firecrab
  - api
status: 미완료
scope: 5주차
updated: 2026-07-23
---

# 관리 API 인증 구현

## 브랜치 개요

- 브랜치: `feat/api-authentication` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: 고엔트로피 opaque bearer token으로 관리 API 호출자를 식별함.
- JWT와 자체 암호 protocol을 추가하지 않고 server-side 폐기와 rotation이 가능한 단순 token을 사용함.

## Token 형식과 저장

```rust
pub struct IssuedToken {
    pub id: Uuid,
    pub secret: secrecy::SecretString,
}

#[derive(sqlx::FromRow)]
pub struct TokenRow {
    pub id: String,
    pub secret_mac: Vec<u8>,
    pub key_version: i64,
    pub principal_id: String,
    pub expires_at: String,
    pub revoked_at: Option<String>,
}
```

- wire token은 version, lookup용 random token ID와 최소 256 bit CSPRNG secret을 구분할 수 있는 고정 형식으로 인코딩함.
- parser는 전체 길이와 각 field의 canonical encoding을 제한하고 중복·복수 `Authorization` header를 거부함.
- DB에는 token ID와 versioned server pepper를 사용해 `version || token_id || secret`에 적용한 HMAC-SHA-256만 저장하며 원문은 발급 응답에서 한 번만 보여줌.
- HMAC library의 constant-time verify API를 사용하고 key version으로 무중단 pepper rotation을 지원함.

## Axum middleware

```rust
pub async fn authenticate(
    State(auth): State<AuthService>,
    mut request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let token = bearer_token(request.headers())?;
    let principal = auth.verify(token).await?;
    request.extensions_mut().insert(principal);
    Ok(next.run(request).await)
}
```

- health liveness와 정제된 readiness를 제외한 관리 endpoint에 middleware를 적용함.
- token은 query string, URL과 cookie에 직접 허용하지 않고 non-browser client의 Authorization header만 사용함.
- 실패 인증은 token 원문 없이 rate limit하고 audit함.

- browser UI용 `/api/session`은 token을 HTTPS same-origin 요청에서 한 번 교환하고 짧은 server-side session cookie만 발급함.
- cookie는 `__Host-` prefix, `Path=/`, `HttpOnly`, `Secure`, `SameSite=Strict`를 사용하고 `Domain`을 설정하지 않음.
- 교환 endpoint도 Origin, CSRF/login attempt rate limit과 audit을 적용함.
- API token과 browser session의 저장소·만료·폐기 경계를 분리함.

## Rotation

- 신규 token 발급, 제한된 overlap, 기존 token revoke 순서로 rotation함.
- 만료와 revoke는 cache보다 DB가 최종 권한이며 cache TTL은 짧게 둠.

## 테스트 및 검증

- 누락·형식 오류·만료·폐기·MAC 불일치 token은 handler 전에 `401`로 거부되어야 함.
- 실패 rate limit, pepper rotation과 DB 유출 scenario를 test함.
- access log, tracing span, error와 DB dump에 token 원문이 없어야 함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
