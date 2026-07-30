---
tags:
  - firecrab
  - m2image
  - template
status: 미완료
scope: 4주차
updated: 2026-07-29
---

# M2Image 카탈로그 API — 이미지 목록을 서버가 알려주기

> [!summary] 한 줄 요약
> 지금 대시보드의 템플릿 목록은 **프론트엔드 코드 상수**다.
> 서버가 가진 레지스트리를 그대로 노출해서 둘이 어긋나지 않게 한다.

## 왜

- `firecrab-frontend/src/components/CreateVm.tsx`에 `TEMPLATES = ["ubuntu-26.04", "alpine-3.24"]`가 박혀 있음
- 서버에는 이미 [템플릿 레지스트리](task-template-registry.md)가 있는데(`templates.rs`: alias → 버전/digest),
  HTTP로 나가는 경로가 없어서 프론트가 따로 들고 있는 것
- 이미지를 추가해도 프론트를 고쳐 배포해야 보임 — 등록 기능([M2Image 등록](task-m2image-registration.md))의 선행

## 작업

- `GET /api/images` — alias, 버전, 커널/rootfs digest, rootfs 최소 크기, 설명
- `CreateVm.tsx`의 상수 제거 → 응답으로 드롭다운 구성(로딩·실패 상태 처리)
- 디스크 최소 크기를 응답에 실어 생성 폼이 미리 검증(지금은 서버 검증에서만 걸림)

## 완료 기준

- 레지스트리에 템플릿을 추가하면 **프론트 코드 수정 없이** 생성 폼에 나타남
- 이미지가 하나도 없으면 폼이 그 사실을 표시하고 생성 버튼을 막음
- 응답에 host 경로가 노출되지 않음(digest·alias만)

## 참고

- 재현 가능한 빌드·승격은 [5주차](weeks/week5-tasks.md)의
  [reproducible builds](task-reproducible-template-builds.md) /
  [무결성·승격](task-template-integrity-and-promotion.md)
