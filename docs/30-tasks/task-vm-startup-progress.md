---
tags:
  - firecrab
  - vm
status: 완료
scope: 3주차
updated: 2026-07-23
---

# VM 시작(starting) 단계별 진행 상황 표시

지금 `starting` 상태는 배지 하나만 보여주고 내부적으로 무슨 일이 일어나는지 안 보인다(rootfs 준비/설치,
Firecracker 기동, 부팅 확인 등 여러 단계가 뭉뚱그려짐). rootfs 복사처럼 수 초 걸리는 단계도 "멈춘 것"과
구분이 안 됨. 이름 붙은 단계로 나눠 로그에 남기고, 기존 3초 폴링으로 대시보드에도 단계별로 보여준다.

## 작업

- 시작 파이프라인을 단계로 명시: rootfs 준비(설치) → Firecracker 설정 생성 → 프로세스 기동 → API 소켓
  대기 → VM 설정 적용 → 부팅 확인
- 각 단계 진입/완료를 `tracing`으로 구조화 로그 남김(request_id와 함께)
- 현재 단계를 VM 상태에 실어 노출(`VmResponse`에 `startupStep: string | null` 같은 필드 추가) — 새
  WS 없이 기존 폴링(`GET /api/vms`)으로 반영
- 프론트: `starting` 행에서 배지 대신 단계 리스트(완료/진행중/대기) 표시

## 완료 기준

- 생성→시작 전체 과정에서 각 단계가 순서대로, 폴링 주기 내에 UI에 반영됨
- 실패 시 로그만 보고 어느 단계에서 멈췄는지 바로 알 수 있음
- rootfs 복사처럼 오래 걸리는 단계도 진행 중임이 화면에 드러남(멈춘 것처럼 안 보임)

## 산출물

`firecrab-api-types/src/lib.rs`(단계 enum·`VmResponse` 확장), `firecrab-api/src/firecracker.rs`·
`rootfs.rs`(단계 전환 기록), `firecrab-api/src/handlers/vms.rs`, `firecrab-frontend/src/components/VmTable.tsx`
(또는 별도 진행 상황 컴포넌트)
