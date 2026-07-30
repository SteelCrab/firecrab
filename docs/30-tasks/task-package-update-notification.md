---
tags:
  - firecrab
  - release
status: 미완료
scope: 4주차
updated: 2026-07-23
---

# VM 패키지 최신 상태 확인 및 업데이트 알림

`POST /api/vms/{id}/packages/update`(콘솔 주입 방식, 이미 구현됨)는 "실행"만 한다 — 지금은
사용자가 먼저 실행해봐야 성공/실패를 안다. 이 task는 그 반대: **VM이 구버전인지 먼저 알려주는
것**.

## AWS로 비유하면

| firecrab | AWS 대응 |
|---|---|
| guest agent가 설치된 패키지 버전을 주기적으로 host에 보고 | **SSM Patch Manager Patch Compliance** — Patch Baseline 기준으로 스캔해 Compliant/Non-Compliant, 누락 패치 개수를 표시 |
| 알려진 CVE와 매칭해 취약 패키지 표시(선택) | **Amazon Inspector** — 설치 패키지 버전을 CVE와 매칭해 findings로 표시 |
| 대시보드에 VM별 "업데이트 필요" 배지 | EC2 콘솔 자체엔 없음 — SSM/Inspector 전용 대시보드에서 확인하는 구조를 그대로 따라감 |

## 왜 지금 방식(콘솔 주입)으로는 못 하는지

- 설치된 패키지 버전을 구조화된 형태로 물어볼 방법이 없음 — 콘솔에 `dpkg -l`/`apk info` 찍고
  텍스트를 파싱해야 하는데, 이건 일회성 실행에는 쓸 만해도 "주기적으로, 여러 VM에 대해 조용히"
  하기엔 적합하지 않음(터미널 세션과 충돌, ANSI 노이즈, VM 하나하나 순회하며 콘솔 여는 비용)
- **선행 조건**: [task-guest-agent-vsock-provisioning.md](task-guest-agent-vsock-provisioning.md)의
  vsock guest agent. 에이전트가 뜨면 host→guest로 구조화된 질의(예: "설치 패키지 목록/버전")를
  보내고 guest는 정해진 응답만 반환하는 구조로 바꿀 수 있음.

## 작업 (선행 task 완료 후)

- guest agent에 "설치 패키지 목록 조회" HostCommand 하나 추가(patch 실행은 아님 — 조회만)
- host가 주기적으로(또는 VM 상세 조회 시) 스냅샷을 받아 최근 상태로 캐시
- 알려진 최신 버전과 비교해 VM 목록/상세 화면에 "업데이트 있음" 배지 표시
- CVE 매칭은 범위 밖(선택 사항, 별도 task로 분리 가능)

## 완료 기준

- guest agent를 통해 설치 패키지 버전을 구조화된 형태로 받아온다(콘솔 텍스트 파싱 아님)
- VM 목록/상세 화면에서 업데이트 필요 여부를 실행 없이 확인할 수 있다
- 이미 있는 `POST /api/vms/{id}/packages/update`가 실행 액션으로 계속 쓰인다(중복 구현 아님)

## 산출물

`firecrab-guest-agent/src`(패키지 조회 command 추가), `firecrab-api/src/guest_agent.rs`,
`firecrab-api-types/src/lib.rs`(PackageInventory 등 타입), `firecrab-frontend/src/components/`
(배지 표시)
