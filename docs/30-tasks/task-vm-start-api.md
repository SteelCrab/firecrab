---
tags:
  - firecrab
  - vm
status: 완료
scope: 2주차
updated: 2026-07-23
---

# VM 시작 API

`POST /api/vms/{id}/start` — 동기 처리.

## 작업

- 상태 검사: `created/stopped/error`만 허용, 그 외 409
- 순서: rootfs 준비 → config 생성 → spawn → readiness → `running` 저장
- 성공 200 + `VmResponse`, 실패 시 `error` 상태 저장 + 500
- `docs/api.md` 갱신

## 완료 기준

- `created/stopped/error` → `running` 200
- 허용 외 상태 409, 없는 VM 404
- 실패 시 상태 `error` + 프로세스 잔여물 없음

## 산출물

`firecrab-api/src/handlers/vms.rs`, `docs/api.md`
