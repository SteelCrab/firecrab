---
tags:
  - firecrab
  - network
status: 완료
scope: 3주차
updated: 2026-07-23
---

# VM network 격리·anti-spoofing

VM 간 통신과 lease 밖 source address를 기본 차단.

## AWS로 비유하면

| firecrab | AWS 대응 |
|---|---|
| MAC/IP/ARP를 active lease와 대조, 불일치 drop | ENI의 **Source/Destination Check** — 자신에게 할당된 IP가 아니면 트래픽을 내보내거나 받지 못하게 막는 기본 동작 |
| VM 간 east-west 기본 drop | **Security Group**의 기본값(명시적으로 허용 안 하면 전부 차단), 인스턴스끼리도 서로 안 열려있음 |
| gateway DHCP/DNS·허용 egress·established/related만 통과 | Security Group의 아웃바운드 허용 + stateful 연결 추적(나간 응답은 자동 허용) |
| host→VM SSH는 IP+포트 명시 허용 | Security Group의 인바운드 규칙 하나 추가하는 것과 동일(포트 22만 특정 소스에 열기) |
| API는 CIDR 대신 egress policy ID만 선택 | 사용자가 방화벽 규칙을 직접 문자열로 못 쓰고 미리 정의된 Security Group만 선택하는 것과 같은 안전장치 |

## 작업

- `bridge firecrab_l2` prerouting: TAP source MAC·ARP sender MAC/IP·IPv4 source를 active lease와 대조, 불일치 drop
- DHCP 예외 2건만: discover(`0.0.0.0:68→255.255.255.255:67`, source MAC 일치), address-conflict probe(sender IP `0.0.0.0`)
- bridge forward: Firecrab TAP 간 east-west frame 기본 drop (VM끼리는 기본적으로 서로 안 보임)
- `inet firecrab`: gateway DHCP/DNS·허용 egress·established/related만 통과, loopback·link-local·host 관리망·reserved subnet 차단
- host→VM SSH는 해당 VM IP + TCP 22만 명시 허용
- IPv6/VLAN 등 비허용 ethertype 차단 (IPv4-only 초기 scope)
- 순서: rule 설치 후 TAP up, rule 실패 시 start rollback; delete는 process → rule → TAP → lease 순
- API는 arbitrary CIDR 대신 helper config의 egress policy ID만 선택

## 완료 기준

- IP/MAC/ARP spoofing, VLAN/IPv6 우회, VM 간 ping/SSH, host 관리 IP 접근 차단
- DNS·gateway·명시적 SSH·허용 외부 응답 traffic만 통과
- container/VPN 등 기존 host forwarding 유지, 한 VM rule 교체가 다른 VM traffic에 무영향

## 산출물

`firecrab-api/src/network_policy.rs`, `firecrab-net-helper/src/firewall.rs`
