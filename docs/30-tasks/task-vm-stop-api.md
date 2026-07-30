---
tags:
  - firecrab
  - vm
status: 완료
scope: 2주차
updated: 2026-07-23
---

# VM 중지 API

`POST /api/vms/{id}/stop` — 동기 처리.

## 작업

- 상태 검사: `running`만 허용, 그 외 409
- `stopping` 저장 → 프로세스 종료(SIGTERM→SIGKILL) → `stopped` 저장
- 성공 200 + `VmResponse`
- `docs/api.md` 갱신

## 완료 기준

- `running` → `stopped` 200, 프로세스 실제 종료
- 허용 외 상태 409, 없는 VM 404

## 산출물

`firecrab-api/src/handlers/vms.rs`, `docs/api.md`
