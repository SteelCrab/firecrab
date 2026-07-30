---
tags:
  - firecrab
  - vm
status: 완료
scope: 2주차
updated: 2026-07-23
---

# VM 상세 API

`GET /api/vms/{id}` — 단건 조회.

## 작업

- `get_vm` handler 추가, route `/api/vms/{id}` 등록
- `VmResponse` 재사용, PID·host 경로 등 내부 정보 미노출
- `docs/api.md` 갱신

## 완료 기준

- 존재 200, 없음 404, UUID 형식 오류 400
- 상태가 소문자 JSON으로 직렬화

## 산출물

`firecrab-api/src/handlers/vms.rs`, `docs/api.md`
