---
tags:
  - firecrab
  - network
  - micronetwork
status: 미완료
scope: 4주차
updated: 2026-07-29
---

# MicroNetwork 업링크 지정 — 네트워크별 외부 경로

> [!summary] 한 줄 요약
> 지금은 host의 기본 경로 하나를 모든 네트워크가 공유한다.
> 네트워크마다 나가는 인터페이스를 고를 수 있게 한다.

## 왜

- `nat.rs`의 `detect_uplink()`가 `/proc/net/route`의 기본 경로 하나를 찾아 전부 거기로 masquerade
- 관리망/서비스망이 물리적으로 나뉜 호스트에서, 네트워크별로 나갈 곳을 정할 수단이 없음
- AWS로 치면 VPC마다 다른 NAT Gateway를 붙이는 것에 해당

## 작업

- `micro_networks`에 uplink 컬럼 추가(비우면 지금처럼 기본 경로 자동 탐지)
- `MicroNetworkSpec`에 uplink 전달 — 문자열이 넘어가므로 helper가 `validate_uplink()`로 재검증
- `nat.rs`의 postrouting을 네트워크별 uplink로 렌더링(masquerade 체인은 계속 공유)
- 생성/상세 UI에 uplink 표시·선택, `GET /api/network`의 인터페이스 목록 재사용

## 완료 기준

- 서로 다른 uplink를 지정한 두 네트워크의 트래픽이 실제로 다른 인터페이스로 나감
- 지정하지 않은 네트워크는 기존과 동일하게 동작(무회귀)
- 존재하지 않는/이름이 이상한 인터페이스는 helper가 `invalid_request`로 거부

> [!important] 선행 확인
> 검증하려면 host에 uplink가 2개 이상 있어야 한다.
> 없으면 dummy 인터페이스로 대체 가능한지부터 확인할 것.

## 참고

- 완료된 범위는 [MicroNetwork](task-micro-network.md)
