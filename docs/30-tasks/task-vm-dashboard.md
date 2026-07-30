---
tags:
  - firecrab
  - frontend
status: 완료
scope: 3주차
updated: 2026-07-23
---

# VM 대시보드

Yew(Wasm) UI — 목록/생성/시작/중지/삭제 + 상태 polling.

## 작업

- Yew CSR + trunk, `firecrab-api-types` 재사용 (VmResponse/VmState/ErrorResponse)
- 구조: `src/main.rs`, `api.rs`(gloo-net client), `model.rs`, `components/{create_vm,vm_table}.rs`
- 상태별 action: created/stopped/error → Start·Delete, running → Stop, starting/stopping → 없음
- 요청 중인 VM의 action 잠금(중복 클릭 방지), 주기 polling으로 목록 갱신(종료 감시 반영), 연속 실패 시 backoff
- `409 invalid_state`는 즉시 재조회 + 현재 상태 표시, 오류는 `error.message` 배너 노출
- template은 select 고정 옵션만 (자유 입력 금지)
- dev는 trunk proxy로 API 연결(base URL hardcode 금지), production은 same-origin 상대 경로

## 완료 기준

- 상태별 action만 활성화, 중복 클릭 차단
- 전이 상태(starting/stopping)·409·시작 실패(`error` + 메시지) 표시
- `trunk serve --port 8080`으로 실행, 생성→시작→중지→삭제 lifecycle 동작

## 산출물

`firecrab-frontend/Cargo.toml`, `firecrab-frontend/src`, `firecrab-frontend/index.html`, `docs/vm-dashboard-smoke.md`
