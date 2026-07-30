---
tags:
  - firecrab
  - vm
status: 완료
scope: 3주차
updated: 2026-07-23
---

# 구축된 VM CPU/MEM/DISK 수정 기능

지금은 생성 시점에만 cpu/ram/disk를 정할 수 있고 이후 바꿀 방법이 없다. `created`/`stopped`/`error`
상태(=프로세스가 안 떠 있는 상태)에서 값을 바꿔 다음 시작부터 반영되게 한다.

## 작업

- `PUT /api/vms/{id}`: cpu/ram/diskGb를 받아 검증 후 레코드 갱신. `running`/`starting`/`stopping`
  중에는 거부(`409`) — 뜬 프로세스의 실시간 리소스를 바꾸는 게 아니라 다음 시작에 반영되는 값이라서
- 검증: cpu 1–32, ram 128–32768 MiB는 생성 때와 동일. disk는 축소 불가(ext4 shrink 미지원) — 현재
  값 이상, 상한(500GiB) 이하만 허용
- `rootfs::prepare_rootfs`가 기존 디스크를 재사용하는 경로에서도 목표 크기로 확장하도록 수정(현재는
  최초 생성 시에만 확장) — 그래야 수정 후 다음 시작에서 실제로 커짐
- 프론트: 상세 모달에서 편집 가능한 상태일 때 cpu/ram/disk를 입력 필드로 바꾸고 저장 버튼 추가

## 완료 기준

- `stopped` VM의 cpu/ram/disk를 바꾸고 다시 시작하면 Firecracker config와 실제 디스크 크기에
  반영됨(guest가 새 크기를 인식)
- `running` 상태에서는 수정 API가 거부됨
- disk를 현재 값보다 작게 주면 검증 오류

## 산출물

`firecrab-api-types/src/lib.rs`, `firecrab-api/src/handlers/vms.rs`, `firecrab-api/src/rootfs.rs`,
`firecrab-frontend/src/components/VmDetailModal.tsx`
