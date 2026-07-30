---
tags:
  - firecrab
  - storage
status: 완료
scope: 2주차
updated: 2026-07-23
---

# rootfs 준비 모듈

VM별 writable rootfs를 template에서 복사.

## 작업

- `prepare_rootfs(id, template)` → `data/vms/{id}/rootfs.ext4`
- `.tmp`에 복사 후 rename (원자적 publish)
- 파일이 이미 있으면 재사용 (stopped 재시작 시 디스크 보존)

## 완료 기준

- 복사 성공 후 파일 존재
- 실패 시 `.tmp` 잔여물 없음
- 재호출 시 기존 파일 유지 (재복사 안 함)

## 산출물

`firecrab-api/src/rootfs.rs`
