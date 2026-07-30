---
tags:
  - firecrab
  - network
  - firewall
status: 완료
scope: 3주차
updated: 2026-07-23
---

# NAT·firewall 자동화

Firecrab subnet의 외부 통신 NAT를 전용 nftables table로 관리.

## AWS로 비유하면

| firecrab | AWS 대응 |
|---|---|
| 이 task 전체 | **NAT Gateway** — private 서브넷의 인스턴스가 직접 공인 IP 없이도 외부로 나가게 해주는 것과 같은 역할 |
| masquerade rule | NAT Gateway가 출발지 주소를 자신의 공인 IP로 바꿔주는 동작(source NAT) |
| single-writer firewall actor | NAT Gateway 설정을 AWS 콘솔/API 양쪽에서 동시에 안 건드리게 막는 것과 같은 직렬화 |
| VM stop/delete가 다른 VM traffic에 무영향 | 서브넷 안 인스턴스 하나를 종료해도 NAT Gateway 자체나 다른 인스턴스의 아웃바운드는 안 끊기는 것 |
| host 기존 firewall rule 보존 | NAT Gateway를 새로 만들어도 기존 라우팅 테이블의 다른 규칙들을 안 건드리는 것 |

## 작업

- 전용 table 2개만 소유: `inet firecrab`(forward dispatch + NAT), `bridge firecrab_l2`(L2) — host 기존 table/chain flush 금지
- base chain policy는 `accept`, Firecrab bridge/TAP traffic만 전용 regular chain으로 dispatch
- masquerade + established/related 허용 rule을 helper config(bridge/subnet/uplink)에서 생성 — API 요청값 삽입 금지(임의 문자열이 nft 규칙에 그대로 들어가는 것 방지)
- single-writer firewall actor가 apply/remove/reconcile 직렬화 (lost update 방지)
- 적용은 atomic nftables transaction — `nft` 사용 시 stdin으로 ruleset 전달, shell 문자열 금지
- startup에 현재 rule과 비교해 차이만 교체, VM stop/delete 시 공용 NAT 유지 (제거는 명시적 uninstall에서만)

## 완료 기준

- 여러 VM 동시 외부 IP/DNS 통신
- 한 VM stop/delete가 다른 VM 연결·NAT rule에 무영향
- 동시 apply/remove에 lost update 없음, 실패 시 이전 ruleset 유지
- host 기존 nftables rule 불변 (semantic snapshot 비교)

## 산출물

`firecrab-net-helper/src/firewall.rs`
