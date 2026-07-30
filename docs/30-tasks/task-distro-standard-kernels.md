---
tags:
  - firecrab
status: 완료
scope: 3주차
updated: 2026-07-23
---

# 배포판 표준 커널 사용

지금은 Ubuntu/Alpine 두 템플릿이 우리가 직접 빌드한 vanilla 커널(`vmlinux-7.1.2-x86_64`) 하나를
공유한다. 각 템플릿이 실제 배포판이 배포하는 공식 커널(Ubuntu `linux-image-generic`, Alpine
`linux-virt`)을 쓰도록 바꾼다.

## AWS로 비유하면

| firecrab | AWS 대응 |
|---|---|
| 지금: 직접 빌드한 vanilla 커널 하나를 모든 템플릿이 공유 | 커스텀 vanilla 커널로 만든 자체 AMI(누구도 보안 패치·드라이버를 대신 관리해주지 않음) |
| 변경 후: Ubuntu `linux-image-generic`, Alpine `linux-virt` | AWS 공식 Ubuntu AMI가 쓰는 `linux-aws` 커널 패키지처럼, 배포판이 클라우드용으로 직접 유지보수하는 공식 커널 |

배포판마다 자기가 공식으로 관리하는 커널 패키지를 그대로 쓰면, 보안 패치·드라이버 지원을 배포판이 대신 챙겨주는 표준 경로를 타게 된다 — AWS가 자체 vanilla 커널 대신 각 배포판의 클라우드 전용 커널 패키지로 공식 AMI를 만드는 것과 같은 이유.

## 작업

- Ubuntu: rootfs 빌드에 `linux-image-generic` 설치 → `/boot/vmlinuz-*` 추출, `extract-vmlinux`로
  ELF 변환(Firecracker는 압축 bzImage를 직접 못 읽음 — "Invalid Elf magic number" 확인됨). 커널
  config에 virtio_blk/ext4가 builtin(`=y`)이라 initrd 불필요
- Alpine: rootfs 빌드에 `linux-virt` 설치 → `/boot/vmlinuz-virt` 추출 + ELF 변환. virtio_blk/ext4가
  **모듈**(`=m`)이라 `/boot/initramfs-virt`(Alpine이 이미 빌드해 배포하는 것)를 그대로 initrd로 써야
  부팅 가능
- `TemplateSpec`/`TemplateVersion`에 `initrd: Option<PathBuf>` 추가, 기존 `verify_artifact`로 동일하게
  무결성 검증
- `firecrab-api/src/firecracker.rs`의 boot-source JSON에 `initrd_path` 필드 조건부 추가
- virtio_mmio는 두 커널 모두 builtin이라 Firecracker `--enable-pci`는 불필요(기존 MMIO 기본값 유지)
- `images/kernel/`에 템플릿별 커널(+Alpine은 initrd)을 분리 저장(예: `vmlinux-ubuntu-26.04-*`,
  `vmlinux-alpine-virt-*`, `initramfs-alpine-virt-*`)

## 완료 기준

- 두 템플릿 모두 실제 배포판 공식 커널로 부팅되고 기존 동작(생성/시작/디스크 확장/리소스 수정)에
  회귀가 없다
- Ubuntu는 initrd 없이, Alpine은 initrd 포함해서 부팅 성공

## 산출물

`firecrab-api/src/templates.rs`, `firecrab-api/src/firecracker.rs`, `images/kernel/`,
`scripts/firecracker-menual/install-ubuntu-roofs.sh`,
`scripts/firecracker-menual/install-alpine-rootfs.sh`
