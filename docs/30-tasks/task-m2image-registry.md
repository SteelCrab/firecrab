---
tags:
  - firecrab
  - m2image
  - template
status: 미완료
scope: MVP-2주
updated: 2026-08-02
---

# M2Image 레지스트리 — 구운 이미지 게시·다운로드

> [!summary] 한 줄 요약
> builder가 만든 Firecracker용 패키지를 **게시**하고, 호스트 `firecrab-api`가
> **카탈로그 + URL + digest** 로 받아 설치하게 한다. (호스트에서 다시 굽지 않음)

## 왜

- API는 docker/루트로 이미지를 구울 수 없음 → 런타임은 **다운로드만**
- 지금 배포는 `FIRECRAB_IMAGE_BASE_URL` + flat `{alias}.tar.zst` 수준 — 카탈로그·digest 게시 규약이 약함
- 심사/빈 호스트 재현: “어디서 공식 패키지를 받는가”가 데모 병목
- builder([task-m2image-builder](task-m2image-builder.md)) 산출물을 올릴 **저장소 역할**이 필요

## AWS로 비유하면

| firecrab | AWS 대응 |
|---|---|
| M2Image 레지스트리 | AMI 레지스트리 / ECR(이미지 메타 + pull) |
| `packageUrl` · digest | AMI ID · 스냅샷 체크섬 |
| `POST …/package` | 패키지 pull · 호스트 로컬 캐시 준비 |
| `POST …/install` | 준비된 아티팩트를 템플릿으로 등록 |

## 모델 (MVP)

```text
{base}/                    ← FIRECRAB_IMAGE_BASE_URL
  catalog.json             ← (권장) alias · version · package · sha256 · minDiskGb
  alpine-3.24.tar.zst
  ubuntu-26.04.tar.zst
  rocky-9.tar.zst
  SHA256SUMS
```

- 패키지 URL: `{base}/{alias}.tar.zst` (기존 `image_install.rs` 규약 유지)
- 패키지 설치: 다운로드 → 멤버 구조 검증 → 호스트 로컬 캐시 준비
- 이미지 설치: 준비된 로컬 패키지 extract → 아티팩트 검증 → `register_spec`
  (이미지를 삭제해도 로컬 패키지는 남아 재설치에 사용)
- **하드코딩된 기본 BASE_URL은 두지 않음** — 운영자가 `FIRECRAB_IMAGE_BASE_URL` 설정
- 게시 매체: GitHub Releases, 객체 스토리지, 또는 로컬 `python -m http.server` (데모)

## 작업

### MVP (제출 범위)

- [ ] 게시 레이아웃·`catalog.json`(또는 `SHA256SUMS`만) 스키마를 문서에 고정
- [ ] builder 산출물을 레지스트리 레이아웃으로 올리는 절차 (`gh release` 또는 업로드 스크립트)
- [ ] `GET /api/images` 가 설정 시 `packageUrl` 노출 (설치 대상 링크) — 코드 일부 있음
- [ ] (선택) 설치 전 `SHA256SUMS` / catalog digest 와 다운로드 파일 대조
- [ ] 빈 호스트: `FIRECRAB_IMAGE_BASE_URL=<게시 base>` → 대시보드 설치 → VM start 1회
- [ ] install.md · api.md 에 “레지스트리 = 패키지 호스트” 한 절

### 제출 후

- [ ] 인증 pull · 서명(cosign 등) · 다중 아키텍처 인덱스
- [ ] 호스트 내장 프록시 캐시 / 미러 동기화
- [ ] UI에서 원격 카탈로그 새로고침·버전 pin

## 완료 기준 (MVP)

- 공개(또는 데모용 고정) base URL 아래에서 alpine · ubuntu · rocky 패키지를 HTTP(S)로 받을 수 있음
- `FIRECRAB_IMAGE_BASE_URL` 만 맞추면 `install.sh --no-images` 호스트에서도 대시보드 설치 가능
- 잘못된/누락 패키지는 패키지 설치 로그에 실패 사유가 남고 레지스트리에 부분 등록되지 않음

## 참고

- 설치 구현: `firecrab-api/src/image_install.rs`
- 패키징: `scripts/package-m2images.sh`
- 빌드: [task-m2image-builder](task-m2image-builder.md)
- 카탈로그 API: [task-m2image-catalog-api](task-m2image-catalog-api.md)
- 가이드: [install.md](../20-guides/install.md) · [api.md](../20-guides/api.md)
- MVP 플랜: [weeks/mvp-3week-submit-2026-08-27](weeks/mvp-3week-submit-2026-08-27.md)
