---
tags:
  - firecrab
  - storage
  - test
updated: 2026-07-31
---

# VM 물리 디스크 선택 테스트

> [!summary] 한 줄 요약
> `FIRECRAB_STORAGE_ROOTS`로 등록한 저장 위치에 VM 디스크를 나누고,
> 여유 공간 부족·알 수 없는 id는 생성 시점에 거부하는지 확인한다.

## 자동 테스트 (root 불필요)

```sh
cargo test -p firecrab-api storage
cargo test -p firecrab-api micro_storage
cargo test -p firecrab-api assign_vm_storage
cargo test -p firecrab-api-types
cd firecrab-frontend && npx tsc --noEmit
```

## 확인 항목

- env 미설정 → 단일 root `default` → `data/vms/{id}/` (기존과 동일)
- `FIRECRAB_STORAGE_ROOTS=disk-a=/mnt/a:disk-b=/mnt/b` 파싱, 중복 id 거부
- `GET /api/storage`가 default/env + MicroStorage 통합 목록
- `GET /api/storage/devices`가 마운트 파티션 목록 (proc/sysfs 제외)
- MicroStorage CRUD, VM 있으면 DELETE 409
- `CreateVmRequest.storageRoot` 미지정 → default root
- `PUT /api/vms/{id}/storage` 수동 재할당 (rootfs 없으면 성공)
- 알 수 없는 id → `storageRoot` 필드 검증 오류
- 여유 공간 < `diskGb` → 생성 거부(복사 전)
- 프론트: MicroStorage 모달, 생성 폼 select, 상세 편집 시 재할당

## 수동 확인 (물리 디스크 2개)

```sh
# 예: 두 마운트 포인트
export FIRECRAB_STORAGE_ROOTS="nvme0=/var/lib/firecrab:nvme1=/mnt/disk2"
# API 재시작 후
curl -s localhost:3000/api/storage
# 각각 다른 storageRoot로 VM 생성·동시 start → iostat으로 장치별 util 분산 확인
```

## 완료 기준 대조

- 다중 디스크 동시 start I/O 분산 — 수동(호스트 의존)
- 여유 공간 부족 시 생성 거부 — **자동 테스트**
- 미지정 시 기존 단일 디스크 흐름 — **자동 테스트**
