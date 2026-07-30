---
tags:
  - firecrab
  - vm
status: 완료
scope: 2주차
updated: 2026-07-23
---

# VM 삭제 API

`DELETE /api/vms/{id}` — hard delete.

## 작업

- 상태 검사: `starting/running/stopping`이면 409
- `data/vms/{id}` 디렉터리 삭제 → 레코드 삭제 → 204
- `docs/api.md` 갱신

## 완료 기준

- 삭제 후 재조회 404, 목록에서 제외
- 실행 중 삭제 409
- VM 디렉터리 잔여물 없음

## 산출물

`firecrab-api/src/handlers/vms.rs`, `docs/api.md`
