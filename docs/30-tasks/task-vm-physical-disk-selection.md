---
tags:
  - firecrab
  - storage
status: 완료
scope: 4주차
updated: 2026-07-31
---

# VM 생성 시 물리 디스크(저장 위치) 선택 · MicroStorage

VM 여러 대를 동시에 시작하면 전부 같은 물리 디스크에 I/O가 몰린다. 저장 위치를 나누고
MicroStorage 서비스로 등록·수동 할당한다. 개념 설명: [micro-storage.md](../20-guides/micro-storage.md).

## 구현

| 항목 | 내용 |
|---|---|
| env | `FIRECRAB_STORAGE_ROOTS=id=path:…` (미설정 시 `default` → `data/`) |
| MicroStorage | `POST/GET/DELETE /api/micro-storages` — 이름+절대경로 풀 |
| 통합 목록 | `GET /api/storage` (default/env + MicroStorage) |
| 파티션 탐색 | `GET /api/storage/devices` — 마운트만 발견 (생성·포맷 없음) |
| create | `storageRoot` 선택, 여유 공간 검증 |
| 수동 할당 | `PUT /api/vms/{id}/storage` — 비활성·rootfs 없을 때만 |
| 경로 | `{path}/vms/{id}/` |
| UI | MicroStorage 모달, 생성 폼, VM 상세 재할당 |

검증: [tests/vm-physical-disk-selection](../40-tests/vm-physical-disk-selection.md)

## 완료 기준

- 물리 디스크 2개 이상 등록된 상태에서 VM 여러 대를 각각 다른 저장 위치로 동시에 시작하면 한쪽
  디스크에만 I/O가 몰리지 않고(`iostat`로 확인), 전체 완료 시간이 단일 디스크 대비 단축됨
- 여유 공간이 부족한 저장 위치를 선택하면 생성 시점에 검증 오류로 거부(디스크 복사 중간에 실패하지
  않음)
- 저장 위치를 지정하지 않은 기존 흐름(단일 디스크)은 그대로 동작

## 산출물

`firecrab-api/src/storage.rs`, `firecrab-api/src/state.rs`, `firecrab-api/src/rootfs.rs`(경로 호출부),
`firecrab-api-types/src/lib.rs`, `firecrab-api/src/handlers/vms.rs`,
`firecrab-api/src/handlers/storage.rs`, `firecrab-frontend/src/components/CreateVm.tsx`
