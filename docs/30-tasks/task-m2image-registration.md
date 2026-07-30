---
tags:
  - firecrab
  - m2image
  - template
status: 미완료
scope: 4주차
updated: 2026-07-29
---

# M2Image 등록·삭제 — 사용자가 이미지를 추가

> [!summary] 한 줄 요약
> 지금 이미지를 추가하려면 **코드를 고쳐야 한다**.
> 실행 중인 firecrab에 이미지를 등록·삭제할 수 있게 한다.

## 왜

- 템플릿 목록이 `TemplateRegistry::load_default()`의 하드코딩된 spec 목록에서 나옴
- 새 배포판을 추가하려면 소스 수정 + 재빌드 + 재시작 — AMI를 등록하듯 다룰 수 없음
- 이미지가 늘어날수록 [카탈로그 API](task-m2image-catalog-api.md)만으로는 부족

## 작업

- `images` 테이블 — alias, 버전, 커널/rootfs 경로, digest, 최소 크기, 등록 시각
- `POST /api/images` — host 경로 등록(업로드는 범위 밖). 등록 시 digest 계산 + 부팅 인자 검증
- `DELETE /api/images/{id}` — 그 이미지로 만들어진 VM이 있으면 `409`
- 시작 시 기본 spec을 DB에 seed(기존 동작 유지, 중복 등록 금지)

## 완료 기준

- 재시작·재빌드 없이 이미지를 추가하고 그 이미지로 VM을 만들 수 있음
- 잘못된 경로·읽을 수 없는 파일·digest 불일치는 등록 시점에 거부
- 사용 중인 이미지는 삭제되지 않음

> [!warning] 경로는 신뢰 경계
> 등록 경로는 사용자 입력이다. symlink·상위 경로 탈출을 [템플릿 레지스트리](task-template-registry.md)의
> 기존 검증과 같은 수준으로 막을 것.
