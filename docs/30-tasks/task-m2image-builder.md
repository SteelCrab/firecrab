---
tags:
  - firecrab
  - m2image
  - template
status: 진행 중
scope: MVP-2주
updated: 2026-08-02
---

# M2Image-builder — Firecracker용 템플릿 굽기

> [!summary] 한 줄 요약
> Ubuntu/Alpine/Rocky Linux·Firecracker는 “완성된 게스트 이미지 파일”을 주지 않는다.
> 업스트림 소스를 받아 **kernel + (initrd) + ext4 rootfs** 로 굽는 **builder** 가 필요하다.

## 왜

- Firecracker는 VMM만 제공하고 게스트 이미지를 배포하지 않음
- Ubuntu/Alpine/Rocky Linux은 cloud/base·패키지 소스만 제공 — Firecracker용 vmlinux+ext4 한 세트는 없음
- `firecrab-api`는 비특권이라 호스트에서 docker 빌드를 돌릴 수 없음 → **빌드는 API 밖**
- 지금 빌드는 `scripts/firecracker-menual/install-*-rootfs.sh` 수동 경로뿐 — 서비스·CI 파이프라인 없음
- 레지스트리([task-m2image-registry](task-m2image-registry.md))에 올릴 아티팩트의 **생산 원천**

## AWS로 비유하면

| firecrab | AWS 대응 |
|---|---|
| M2Image-builder | AMI 빌드 파이프라인 (Packer / EC2 Image Builder) |
| `{alias}.tar.zst` 산출 | 등록 전 AMI 스냅샷 산출물 |

## 전제 (확정)

- 입력: Ubuntu Base / Alpine minirootfs / Rocky BaseOS·AppStream + 배포판 커널 패키지 (공식 업스트림 URL)
- 출력: Firecracker용 상대 경로 규약 (`kernel/…`, `rootfs/…`) 및 `package-m2images.sh` 호환 `.tar.zst`
- 실행 위치: **빌드 호스트 또는 CI** — `firecrab-api` 프로세스 안이 아님
- 재현성: 가능하면 [재현 가능한 template build](task-reproducible-template-builds.md) 와 맞춤 (전체 재현성은 5주 범위, MVP는 “같은 스크립트로 다시 구울 수 있음”)

## 작업

### MVP (제출 범위)

- [x] `install-alpine-rootfs.sh` · `install-ubuntu-roofs.sh` · `install-rocky-rootfs.sh` 를 **공식 빌드 진입점**으로 문서화 (입력 소스 URL · 산출 경로 · 소요 시간)
- [x] `package-m2images.sh` 와 한 줄로 이어지는 흐름 고정: `build → package → dist/m2images/{alias}.tar.zst`
- [x] 수동 runbook: ubuntu-26.04 · alpine-3.24 · rocky-9 패키지를 재생성할 수 있음
- [x] 산출물 SHA256(`SHA256SUMS`) 생성 및 `sha256sum -c` 검증

### 빌드 검증 기록 (2026-08-02)

- `./scripts/build-m2images.sh`로 Alpine 3.24와 Ubuntu 26.04를 실제 재생성했다.
- `dist/m2images/alpine-3.24.tar.zst`, `ubuntu-26.04.tar.zst`, `SHA256SUMS`가 생성됐고,
  두 항목 모두 `sha256sum -c dist/m2images/SHA256SUMS`를 통과했다.
- 패키지 내부 경로는 각 template spec이 요구하는 `kernel/`·`rootfs/` 항목과 일치한다.
- runbook: [M2Image-Packer 가이드](../20-guides/m2image-builder.md).

### Rocky Linux 9 빌드 검증 기록 (2026-08-02)

- `./scripts/build-m2images.sh --alias rocky-9`로 공식 Rocky BaseOS/AppStream 패키지와
  EL9 kernel·dracut initramfs를 실제 생성했다.
- `dist/m2images/rocky-9.tar.zst`가 생성됐고, 전체 `SHA256SUMS`의 Alpine · Ubuntu · Rocky
  세 항목이 `sha256sum -c`를 통과했다.
- 빈 이미지 root에서 `POST /api/images/rocky-9/package` → `POST /api/images/rocky-9/install`
  → 삭제 → 캐시 패키지 재가져오기까지 성공했다.

### 제출 후 (풀 서비스 — Icebox에 가깝지만 설계만 MVP에 명시)

- [ ] 장기 실행 **builder 서비스/job API** (요청 → 큐 → 로그 → 아티팩트 업로드)
- [ ] 격리 빌드 환경 (일회용 VM/namespace, 네트워크 allowlist)
- [ ] manifest 고정 + 바이트 단위 재현 ([task-reproducible-template-builds](task-reproducible-template-builds.md))

## 완료 기준 (MVP)

- [x] 빈 워크트리(또는 깨끗한 빌드 머신)에서 문서의 명령만으로 alpine · ubuntu · rocky 패키지를 다시 만들 수 있음
- 산출 `{alias}.tar.zst` 가 `POST /api/images/{alias}/package` (BASE_URL 경유)로 내려받아
  검증된 뒤, `POST /api/images/{alias}/install`로 설치·등록됨
- “업스트림 공식 링크를 그대로 install URL에 넣지 않는다”가 문서에 한 줄로 명시됨

## 참고

- 패키징: `scripts/package-m2images.sh`
- Alpine 빌드: `scripts/firecracker-menual/install-alpine-rootfs.sh`
- Ubuntu 빌드: `scripts/firecracker-menual/install-ubuntu-roofs.sh`
- Rocky 빌드: `scripts/firecracker-menual/install-rocky-rootfs.sh`
- 배포·카탈로그: [task-m2image-registry](task-m2image-registry.md)
- 설치 API: [task-m2image-catalog-api](task-m2image-catalog-api.md) · `image_install.rs`
- MVP 플랜: [weeks/mvp-3week-submit-2026-08-27](weeks/mvp-3week-submit-2026-08-27.md)
