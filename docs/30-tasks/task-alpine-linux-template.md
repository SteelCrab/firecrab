---
tags:
  - firecrab
  - template
status: 완료
scope: 3주차
updated: 2026-07-23
---

# 이미지 템플릿 Alpine Linux 추가

지금은 `TemplateRegistry::load_default`에 `ubuntu-26.04` 하나만 등록돼 있다. Alpine Linux를 두
번째 템플릿으로 추가해 생성 폼에서 고를 수 있게 한다.

## 작업

- Alpine용 커널(`vmlinux`)과 rootfs(ext4) 이미지를 빌드해 `images/kernel/`, `images/rootfs/`에 추가
  (Ubuntu 이미지와 동일하게 Firecracker 부팅 가능한 형태 — `console=ttyS0 ... root=/dev/vda rw` 계열
  boot_args, 최소 설치)
- `TemplateRegistry::load_default`의 `TemplateSpec` 목록에 `alias: "alpine-3.x"` 항목 추가(기존
  `ubuntu-26.04`와 같은 패턴 — `verify_artifact`가 SHA256을 자동 계산하므로 해시 직접 관리 불필요)
- 프론트: `CreateVm.tsx`의 `TEMPLATES` 배열에 추가(현재 하드코딩된 단일 옵션 목록)

## 완료 기준

- 생성 폼 template 드롭다운에 Alpine이 보이고 선택해 생성·시작하면 실제로 Alpine 커널이 부팅됨
- 기존 Ubuntu 템플릿 동작(생성/시작/디스크 확장/리소스 수정)에 회귀 없음

## 산출물

`images/kernel/`, `images/rootfs/`(신규 Alpine 이미지 파일), `firecrab-api/src/templates.rs`,
`firecrab-frontend/src/components/CreateVm.tsx`
