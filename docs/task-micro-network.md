# MicroNetwork — 가상 네트워크

## 개요

- 원래는 bridge(`fcbr0`)·subnet(`172.30.0.0/24`)·gateway가 **하나뿐**이었음
- 그것도 5곳(`ipam.rs`, `bridge.rs`, `firewall.rs`, `nat.rs`, `dhcp.rs`)에 따로 하드코딩
- 이걸 사용자가 여러 개 만들 수 있는 가상 네트워크(**MicroNetwork**)로 일반화하고,
  VM을 그중 하나에 소속시킴
- 이 작업의 본질은 네트워크 서비스를 새로 만드는 게 아니라, **하드코딩된 5곳을
  네트워크별로 파라미터화**하는 것
- 기존 VM·기존 동작은 그대로 — MicroNetwork를 안 고르면 예전과 같은 기본 네트워크에 붙음
- `task-network-configuration-dashboard.md`가 미룬 "여러 개의 독립된 네트워크 지원"의 후속
- 테스트 절차는 `docs/tests/micro-network.md`

### 계층 구조

- 지금은 **1계층** — MicroNetwork 하나가 subnet 하나(bridge 하나)를 직접 소유
- 2계층(MicroNetwork = 격리 경계 / Subnet = CIDR 조각) 분리는 검토했으나 보류
  - 단일 host에서는 한 네트워크에 subnet을 여러 개 둘 실사용 동기가 아직 없음
  - 분리하면 VRF·2단 UI·스키마까지 함께 커져서, 지금 얻는 것 대비 비용이 큼
  - 멀티 host로 확장하거나 한 네트워크 안에서 용도별 대역이 필요해지면 그때 재검토

## AWS로 비유하면

| firecrab | AWS 대응 |
|---|---|
| MicroNetwork | VPC + Subnet (통합) |
| MicroNetwork - bridge + gateway | Subnet의 암묵적 라우터 |
| MicroNetwork - DHCP 범위 | VPC DHCP Option Set |
| MicroNetwork - NAT | VPC의 NAT Gateway |
| VM - MicroNetwork | EC2 Instance - VPC/Subnet |
| MicroNetwork 간 격리 | VPC 간 격리 |

즉 AWS 콘솔에서 VPC를 만들고 인스턴스를 그 안에 띄우는 흐름을, firecrab에서 직접 만드는 것.

## 작업

### 완료 — MicroNetwork 리소스 (2026-07-24)

- `micro_networks` 테이블(id/name/subnet_cidr) + `POST/GET/DELETE /api/micro-networks`
- 프론트엔드 "MicroNetwork" 버튼 → 목록/생성/삭제 모달
- 생성 시 실제 host bridge(`mnb<hex>`) 생성, 삭제 시 제거
- 인터페이스 이름은 helper가 id로부터 유도, API가 넘기는 건 gateway/prefix 숫자값뿐
- `bridge.rs`를 `BridgeConfig`로 파라미터화 — 기존 `ensure_bridge()`는 얇은 wrapper로 유지

### 완료 — 네트워크 서비스 파라미터화 (2026-07-29)

- **서브넷/IPAM**: `SubnetSpec`(network/prefix에서 gateway·broadcast·호스트 범위 유도)로
  lease 할당을 네트워크별로 스코프, `network_leases.micro_network_id` 컬럼 추가
- **DHCP**: dnsmasq base config에 네트워크별 `interface=`/`dhcp-range=` 블록
  - 네트워크 집합이 바뀌면 reload가 아니라 **restart** — dnsmasq는 main config를
    시작할 때만 읽고 SIGHUP은 `dhcp-hostsfile`만 다시 읽음
  - `dhcp_release`도 그 lease를 실제로 서빙하는 bridge로 보냄
- **NAT**: postrouting에 네트워크별 `ip saddr <subnet> oifname <uplink>` 규칙, masquerade 공유
- **방화벽**: bridge별 forward dispatch 규칙 + 모든 firecrab subnet을 목적지 deny 목록에 넣어
  네트워크 간 라우팅 차단(같은 네트워크 안 east-west는 기존 bridge 테이블이 담당)
- **VM 소속**: `CreateVmRequest`/`VmResponse`에 `micro_network_id`, TAP을 그 네트워크 bridge에
  attach, 생성 폼에 MicroNetwork 선택 추가
- **삭제 가드**: active lease가 있는 MicroNetwork는 `409`로 삭제 거부
- **겹침 검증**: 기존 MicroNetwork·기본 네트워크와 겹치는 CIDR은 필드 검증 오류로 거부
- **재적용(부분)**: VM 시작 때마다 모든 MicroNetwork bridge를 다시 ensure — 재부팅으로
  사라진 bridge가 복구됨
- **상세 조회**: `GET /api/micro-networks/{id}` — 네트워크 ID, 서브넷(CIDR/gateway/주소
  사용량/DHCP), 브릿지(이름/TAP 수), NAT(출발 대역/업링크), 방화벽(차단 항목), 소속 VM 목록.
  전부 id·CIDR에서 유도한 값이라 실제 설치된 것과 따로 저장돼 어긋날 여지가 없음.
  프론트엔드는 MicroNetwork 목록에서 행 클릭 시 상세 패널로 표시

### 남은 범위

- **VRF**(`vrf.rs` 신규): MicroNetwork별 라우팅 테이블 분리
  - 지금 네트워크 간 차단은 nftables 규칙이라, 규칙이 빠지면 뚫림
  - VRF는 경로 자체가 없어서 규칙 누락으로 뚫릴 수 없음 — 같은 결과의 더 강한 보장
- **daemon 시작 시 재적용**: 지금은 VM 시작이 트리거라, VM이 하나도 없는 MicroNetwork는
  host 재부팅 후 bridge가 사라진 채로 남음
- **2계층 분리**(MicroNetwork / Subnet): 위 "계층 구조" 참고 — 필요해지면 재검토
- MicroNetwork별 uplink 지정(지금은 host의 기본 경로 하나를 모두 공유)
- **네트워크별 인터넷 on/off**: 지금은 모든 네트워크가 NAT를 받음. 상세의 `nat.enabled`는
  항상 `true` — AWS의 IGW attach/detach에 해당하는 토글이 아직 없음
- **호스트 UFW 연동**: UFW를 쓰는 호스트에서는 새 브리지마다 DHCP/DNS 허용 규칙을 손으로
  넣어야 함(`docs/troubleshooting.md`). 자기 소유가 아닌 firewall은 건드리지 않는 원칙이라
  자동화하지 않았고, 대신 실패 증상을 문서로 남김
