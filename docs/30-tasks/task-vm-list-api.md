---
tags:
  - firecrab
  - vm
status: 완료
scope: 2주차
updated: 2026-07-23
---

# VM 목록 API

`GET /api/vms` — 저장된 VM을 정렬된 배열로 반환. pagination 없음.

## 작업

- `list_vms` handler 추가, route `get(list_vms)` 등록
- 이름 오름차순 정렬 (같은 이름은 id 순), `VmResponse` 배열 반환
- `docs/api.md` 갱신

## 완료 기준

- 빈 상태 `[]` 200
- 생성한 VM이 목록에 포함
- 정렬 단위 테스트

## 산출물

`firecrab-api/src/handlers/vms.rs`, `firecrab-api/src/server.rs`, `docs/api.md`
