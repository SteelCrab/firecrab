---
tags:
  - firecrab
  - firecracker
status: 완료
scope: 2주차
updated: 2026-07-23
---

# Firecracker config 생성

VM 레코드 → Firecracker config 파일.

## 작업

- `data/vms/{id}/firecracker.json` 생성
- `boot-source`: template kernel 경로 + boot_args
- `drives`: rootfs 경로, `is_root_device: true`
- `machine-config`: `vcpu_count` = cpu, `mem_size_mib` = ram

## 완료 기준

- 생성된 JSON의 `vcpu_count`/`mem_size_mib`가 요청값과 일치하는 단위 테스트

## 산출물

`firecrab-api/src/firecracker.rs`
