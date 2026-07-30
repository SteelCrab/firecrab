---
tags:
  - firecrab
  - network
status: 완료
scope: 3주차
updated: 2026-07-23
---

# 공용 bridge 네트워크

Firecrab 전용 bridge/subnet/gateway를 idempotent하게 준비.

## AWS로 비유하면

| firecrab | AWS 대응 |
|---|---|
| `fcbr0` + `172.30.0.0/24` | VPC + 그 안의 서브넷 CIDR |
| gateway `172.30.0.1` | 서브넷의 암묵적 라우터(VPC 라우터) |
| `ensure_bridge`(있으면 그대로, 없으면 생성) | Terraform/CloudFormation으로 VPC를 재적용해도 중복 생성 안 되는 것과 동일한 idempotency |
| 여러 VM이 공용 gateway 사용 | 같은 서브넷의 모든 인스턴스가 같은 라우터를 쓰는 것 |

이 task는 "VM들이 속할 VPC 서브넷 하나를 만드는 것" 그 자체 — 이후 모든 네트워크 작업(IPAM, NAT, 방화벽, TAP)이 이 위에서 이뤄짐.

## 작업

- 고정 구성: `fcbr0`, `172.30.0.0/24`, gateway `172.30.0.1`, MTU 1500 — helper config 전용, API로 변경 불가
- `ensure_bridge`(rtnetlink): 있으면 그대로 사용, 없으면 생성 → gateway 주소 → link up
- host 기존 주소·route·container/VPN subnet과 겹치는 구성 거부 (다른 서브넷과 CIDR 충돌 방지)
- IPv4 forwarding은 별도 host 설정으로 명시 (daemon 종료 시 원복하지 않음 — 전역 sysctl)
- 초기 scope IPv4 — bridge에서 IPv6 traffic 차단
- API 시작 시 bridge 재검증, 누락 주소·전용 rule만 복원 (host 재부팅 후에도 자동 복구되게)

## 완료 기준

- `ensure_bridge` 반복 실행에도 interface/주소 중복 없음
- host 재부팅 후 동일 구성 복구, 기존 host bridge·route 보존
- 여러 VM이 공용 gateway 사용

## 산출물

`firecrab-net-helper/src/bridge.rs`
