---
tags:
  - firecrab
  - m2image
  - template
status: 미완료
scope: 4주차
updated: 2026-07-29
---

# M2Image 캡처 — VM에서 이미지 만들기

> [!summary] 한 줄 요약
> 설정을 끝낸 VM의 디스크를 새 이미지로 굳힌다.
> AWS로 치면 인스턴스에서 AMI를 만드는 것.

## 왜

- 지금은 VM을 만들 때마다 같은 설치·설정을 반복해야 함
- 골든 이미지를 만들려면 host에서 스크립트를 직접 돌려야 함(`scripts/firecracker-menual/`)
- 대회 데모에서 "환경을 갖춘 뒤 그대로 복제"가 가장 자주 나오는 흐름

## 작업

- `POST /api/vms/{id}/capture` — 중지된 VM의 rootfs를 복사해 새 이미지로 등록
- 캡처 전 정리(specialize의 역연산): hostname, machine-id, SSH host key, `/var/log`, DHCP lease
- 원본 VM은 건드리지 않음 — 복사본에만 정리를 적용
- 진행 상태는 기존 startup step과 같은 방식으로 노출

## 완료 기준

- 캡처한 이미지로 VM을 만들면 hostname·SSH host key가 원본과 **다름**(정리가 실제로 됨)
- `running` VM은 `409`로 거부(파일시스템 일관성)
- 캡처 실패 시 반쯤 만들어진 이미지가 등록되지 않음

## 참고

- 등록 경로는 [M2Image 등록](task-m2image-registration.md)과 공유
- 실행 중 VM의 시점 캡처는 [7주차 snapshot](weeks/week7-tasks.md) — 목적이 다름
  (snapshot = 그 VM의 복원, 이미지 = 새 VM의 원본)
