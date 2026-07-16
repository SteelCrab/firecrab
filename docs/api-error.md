# API 에러

모든 에러는 동일한 JSON envelope로 반환된다. `error.requestId`는 응답의 `X-Request-Id` 헤더와 동일한 값이다.

## 응답 형식

```json
{
  "error": {
    "code": "validation_failed",
    "message": "request validation failed",
    "fields": { "cpu": "must be between 1 and 32" },
    "requestId": "<uuid>"
  }
}
```

- `fields`는 필드 검증 실패 시에만 포함됨
- DB 경로, 내부 오류 상세 등은 응답에 노출하지 않음

## 에러 코드

| code | status | 설명 |
| --- | --- | --- |
| `validation_failed` | 400 | 요청 필드 검증 실패 (`fields`에 상세 사유 포함) |
| `invalid_json` | 400 | JSON body가 아니거나 파싱 실패 |
| `unsupported_media_type` | 415 | `Content-Type`이 `application/json`이 아님 |
| `request_too_large` | 413 | 요청 바디가 64 KiB 초과 |
| `forbidden_origin` | 403 | 허용되지 않은 Origin |
| `too_many_requests` | 429 | 동시 요청 한도(128) 초과 |
| `request_timeout` | 504 | 처리 시간 10초 초과 |
| `not_found` | 404 | 정의되지 않은 라우트 |
| `internal_error` | 500 | 서버 내부 오류 |
