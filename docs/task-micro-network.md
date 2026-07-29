---
tags:
  - firecrab
  - network
  - micronetwork
status: 완료
scope: 3주차
updated: 2026-07-29
---

# MicroNetwork — 가상 네트워크

> [!summary] 한 줄 요약
> 하나뿐이던 bridge·subnet·gateway를 **사용자가 여러 개 만드는 가상 네트워크**로 일반화하고,
> VM을 그중 하나에 소속시킨다.
> 이 문서는 3주차에 완료된 범위의 기록 — 격리 강화(VRF 등)는 [4주차](week4-tasks.md) 플랜.

## 개요

- 원래는 bridge(`fcbr0`)·subnet(`172.30.0.0/24`)·gateway가 **하나뿐**이었고,
  그것도 5곳(`ipam.rs`, `bridge.rs`, `firewall.rs`, `nat.rs`, `dhcp.rs`)에 따로 하드코딩돼 있었음
- 이 작업의 본질은 새 서비스를 만드는 게 아니라 **그 5곳을 네트워크별로 파라미터화**하는 것
- 기존 VM·기존 동작은 그대로 — MicroNetwork를 안 고르면 예전과 같은 기본 네트워크에 붙음
- [네트워크 구성 대시보드](task-network-configuration-dashboard.md)가 미룬
  "여러 개의 독립된 네트워크 지원"의 후속
- 테스트 절차는 [tests/micro-network](tests/micro-network.md)

> [!note] 계층 구조 — 1계층으로 유지
> MicroNetwork 하나가 subnet 하나(bridge 하나)를 직접 소유한다.
> 2계층(MicroNetwork = 격리 경계 / Subnet = CIDR 조각) 분리는 검토 후 보류:
> 단일 host에서 한 네트워크에 subnet을 여러 개 둘 동기가 아직 없고,
> 분리하면 VRF·2단 UI·스키마까지 함께 커진다.

## AWS로 비유하면

| firecrab | AWS 대응 |
|---|---|
| MicroNetwork | VPC + Subnet (통합) |
| bridge + gateway | Subnet의 암묵적 라우터 |
| DHCP 범위 | VPC DHCP Option Set |
| NAT | VPC NAT Gateway |
| 인터넷 on/off | Internet Gateway attach/detach |
| VM - MicroNetwork | EC2 Instance - VPC/Subnet |
| MicroNetwork 간 격리 | VPC 간 격리 |

AWS 콘솔에서 VPC를 만들고 인스턴스를 그 안에 띄우는 흐름을, firecrab에서 직접 만드는 것.

## 완료 — 리소스 (2026-07-24)

- [x] `micro_networks` 테이블 + `POST/GET/DELETE /api/micro-networks`
- [x] 프론트엔드 "MicroNetwork" 버튼 → 목록/생성/삭제 모달
- [x] 생성 시 실제 host bridge(`mnb<hex>`) 생성, 삭제 시 제거
- [x] `bridge.rs`를 `BridgeConfig`로 파라미터화 — 기존 `ensure_bridge()`는 얇은 wrapper

> [!important] 신뢰 경계
> 인터페이스 이름은 helper가 id로부터 직접 유도한다.
> API가 넘기는 값은 gateway/prefix 숫자뿐 — 문자열이 `ip link`나 nftables 인자로 들어가지 않는다.

## 완료 — 서비스 파라미터화 (2026-07-29)

- [x] **서브넷/IPAM** — `SubnetSpec`으로 lease 할당을 네트워크 CIDR로 스코프,
      `network_leases.micro_network_id` 컬럼(NULL = 기본 네트워크)
- [x] **DHCP** — 네트워크별 `interface=`/`dhcp-range=`,
      집합이 바뀌면 reload가 아니라 **restart**(dnsmasq는 main config를 시작할 때만 읽음),
      `dhcp_release`도 실제 서빙 bridge로 전송
- [x] **NAT** — subnet별 postrouting 규칙, masquerade 체인은 공유
- [x] **방화벽** — bridge별 forward dispatch + 모든 firecrab subnet을 목적지 deny에 넣어
      네트워크 간 라우팅 차단(같은 네트워크 안 east-west는 bridge 테이블이 담당)
- [x] **VM 소속** — `micro_network_id`로 생성, TAP을 그 네트워크 bridge에 attach
- [x] **삭제 가드** — active lease가 있으면 `409`
- [x] **겹침 검증** — 기존·기본 네트워크와 겹치는 CIDR은 필드 검증 오류

## 완료 — 재적용·인터넷 토글·상세 (2026-07-29)

- [x] **재적용** — `ensure_all_networks()`가 기본 bridge + 네트워크별 bridge + 방화벽 + DHCP를
      한 번에 되살림. daemon 시작 1회(best-effort) + VM 시작마다(실패 시 start 실패)
- [x] **인터넷 on/off** — `internet_enabled`. 생성 시 선택, `PATCH /api/micro-networks/{id}`로 변경.
      끄면 masquerade 규칙이 빠지고 forward 경로에서 새 흐름이 drop됨
      (bridge·주소·DHCP는 그대로라 내부 통신은 계속 동작)
- [x] **상세 조회** — `GET /api/micro-networks/{id}`: 네트워크 ID / 서브넷 / 브릿지 / NAT /
      방화벽 / 소속 VM. 전부 id·CIDR에서 유도한 값이라 실제 설치된 것과 어긋날 여지가 없음

> [!warning] 전역 방화벽 적용은 테이블을 flush한다
> 그래서 적용 직후 실행 중인 VM의 개별 정책을 다시 설치한다.
> 안 하면 네트워크 생성·삭제·토글 **한 번에 실행 중인 VM 전부가 조용히 외부 통신을 잃는다.**
> 토글은 적용에 실패하면 저장값을 되돌린다 —
> 호스트가 막지 않고 있는데 "차단됨"으로 보이면 안 되기 때문.

## 남은 범위

[4주차](week4-tasks.md)의 "MicroNetwork — 네트워크 격리 강화" 절에서 태스크로 진행한다.

| 태스크 | 왜 남았나 |
|---|---|
| [VRF](task-micro-network-vrf.md) | 지금 네트워크 간 차단은 nftables 규칙 — 규칙이 빠지면 뚫림 |
| [네트워크별 uplink](task-micro-network-uplink.md) | 지금은 host의 기본 경로 하나를 전부 공유 |
| [호스트 방화벽 연동](task-micro-network-host-firewall.md) | 자기 소유가 아닌 firewall은 안 건드린다는 원칙과 충돌 |
| 2계층 분리 | 단일 host에서는 동기가 없음(위 계층 구조 참고) — 조건부 보류 |
