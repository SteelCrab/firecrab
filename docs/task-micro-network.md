# MicroNetwork — VPC형 가상 네트워크 구현

지금은 bridge(`fcbr0`)·subnet(`172.30.0.0/24`)·gateway가 코드 전체(`firecrab-api`의 `ipam.rs`,
`firecrab-net-helper`의 `bridge.rs`/`firewall.rs`/`dhcp.rs`)에 **하나뿐**, 그것도 5곳에 독립적으로
하드코딩돼 있다. 이 단일 네트워크를 사용자가 여러 개 만들고 VM을 그중 하나에 명시적으로
소속시킬 수 있는 가상 네트워크 단위로 일반화한다. 이 기능의 이름을 **MicroNetwork**로 한다
(`task-network-configuration-dashboard.md`가 미룬 "여러 개의 독립된 네트워크 지원" 후속).

## AWS로 비유하면

| firecrab | AWS 대응 |
|---|---|
| MicroNetwork | VPC + Subnet (통합) |
| MicroNetwork - Gateway | Subnet - 암묵적 라우터 |
| MicroNetwork - RouteTable | VPC - Route Table |
| VM - MicroNetwork | EC2 Instance - VPC/Subnet |
| MicroNetwork - MicroNetwork 격리 | VPC - VPC 격리 |

## 설계 결정: VPC/Subnet을 왜 하나로 합쳤나

AWS에서 VPC와 Subnet이 분리된 핵심 이유는 **AZ(가용 영역)별로 서브넷을 나눠 장애 도메인을
분리**하기 위해서다. firecrab은 단일 host라 AZ 개념 자체가 없고, "VPC 하나에 subnet 여러 개"를
만들 이유가 없다 — 어차피 bridge 하나 : subnet 하나로 1:1 대응한다. 그래서 **MicroNetwork를
VPC+Subnet 통합 엔티티로 유지**하기로 결정함(2026-07-24, 사용자 확인). 나중에 실제로 여러
subnet이 필요한 경우가 생기면(예: 멀티 host로 확장) 그때 `MicroNetwork`(상위, 격리 경계)와
`Subnet`(하위, CIDR 조각) 2계층으로 분리하는 걸 재검토 — 지금은 과설계로 보지 않기로 함.

## 진행 상황 (2026-07-24)

**1단계(MicroNetwork ↔ VPC 기본 리소스) 완료**: 이름 있는 CIDR 예약만 다루는 최소 슬라이스 —
`micro_networks` 테이블(id/name/subnet_cidr만), `POST/GET/DELETE /api/micro-networks`, 프론트엔드에
"MicroNetwork" 버튼 → 모달(목록 + 생성 폼 + 삭제, `HostInfoModal`/`CreateVm` 컨벤션 그대로 재사용).
실제 dev 서버 브라우저에서 생성→목록→삭제→검증 에러 표시까지 확인,
`cargo fmt/clippy/test --workspace` 137/20/12/46 green, 프론트 `tsc -b`/`oxlint`/`vite build` 통과.

**2단계(실제 host bridge 프로비저닝) 완료**: MicroNetwork 생성/삭제가 이제 실제 Linux bridge를
만들고 지운다.

- `firecrab-helper-protocol`: `micro_network_bridge_name(id)`(기존 `tap_name`과 동일한 패턴 —
  `mnb` + sha256(id) 앞 6바이트hex, IFNAMSIZ 이내) + `NetworkRequest::{EnsureMicroNetworkBridge,
  RemoveMicroNetworkBridge}` 신설. 인터페이스 이름은 helper가 id로부터 직접 유도하고, API가
  넘기는 건 `gateway`/`prefix` 숫자값뿐(기존 `ApplyVmPolicy`의 `ipv4` 필드와 같은 신뢰 경계 —
  "helper가 인터페이스 이름을 직접 유도한다"는 `tap_name`의 원칙은 그대로 유지, CIDR 형태의
  raw text만 넘어가지 않음)
- `firecrab-net-helper/src/bridge.rs`: `BridgeConfig`(name/gateway/network/prefix)로 내부
  파라미터화 — 기존 `ensure_bridge()`는 그대로 두고(기본 네트워크 값으로 부르는 얇은 wrapper),
  `ensure_micro_network_bridge`/`delete_micro_network_bridge` 신설. `BridgeError`의 각 variant가
  하드코딩된 상수 대신 실제 subnet/이름을 필드로 담도록 변경(에러 메시지가 이제 어떤
  bridge/subnet인지 정확히 보여줌). 기존 overlap 검사(`assert_subnet_available`)가 그대로
  재사용되어 새 MicroNetwork끼리도, 기존 `fcbr0`와도 자동으로 겹침 방지됨
- helper `dispatch`에서 prefix를 8-30 범위로 재검증(API가 이미 16-28로 걸러도 helper가 신뢰
  경계로서 다시 확인 — `egress_policy` allowlist 검증과 같은 이유)
- `handlers/micro_networks.rs`: 생성 시 DB insert 후 bridge 프로비저닝(실패하면 방금 넣은 row
  롤백, `create_vm`의 lease 롤백과 동일한 순서), 삭제 시 bridge 제거 후 DB delete(bridge 제거
  실패 시 row는 남겨서 재시도 가능하게 함). gateway는 저장하지 않고 subnet_cidr에서 매번
  계산(network 주소 + 1) — 응답에는 아직 안 실림(산출물 이후 단계)
- 실제 host에서 확인: MicroNetwork 생성 → `mnb<hex>` bridge가 실제로 `ip link`에 나타나고
  주소가 정확함(`172.31.0.1/24`) → 기존 `fcbr0`와 실행 중이던 VM들은 무영향 → 삭제 시 bridge
  완전히 제거 → 기존 `fcbr0`와 겹치는 CIDR로 생성 시도 시 거부(롤백까지 확인, orphan 없음)
- `cargo fmt/clippy/test --workspace` 140/20/14/47 green

## 작업 (남은 범위)

- SQLite `micro_networks` 테이블에 subnet CIDR 외 컬럼 추가(uplink, created_at) — gateway는
  subnet_cidr에서 계산되므로 저장 불필요
- MicroNetworkResponse에 계산된 `gateway` 필드 노출(지금은 내부적으로만 계산해서 씀)
- `firecrab-net-helper`에 VRF 관리 신설(`vrf.rs`?) — MicroNetwork별 VRF device 생성
  (`ip link add vrf-<id> type vrf table <id>`) 후 해당 bridge를 `master`로 편입, 필요시
  uplink를 통한 default route를 그 VRF 테이블 안에 추가 — namespace 없이 라우팅 테이블만 분리
  (지금은 bridge만 만들고 라우팅 테이블 분리는 안 함 — 여러 MicroNetwork를 만들면 host의
  단일 라우팅 테이블에 connected route가 전부 잡혀서, `ip_forward=1` 상태에서 서로 라우팅될
  수 있음)
- `firecrab-net-helper/src/firewall.rs`/`nat.rs`: `TABLE_INET`/`TABLE_BRIDGE` 이름과
  `BRIDGE_SUBNET`을 MicroNetwork별로 네임스페이스 분리(예: `firecrab_<network_id>`) — 지금처럼
  전역 테이블 하나로 flush하면 다른 MicroNetwork의 규칙까지 날아감. 역할은 VRF가 막아주는
  "다른 네트워크로 가는 경로 차단"이 아니라 지금처럼 "같은 네트워크 안 spoofing 방지"에 한정.
  NAT도 안 붙어 있어서 지금 MicroNetwork의 bridge에 VM을 붙여도 인터넷 egress 안 됨
- `firecrab-net-helper/src/dhcp.rs`: dnsmasq 설정에 MicroNetwork별 `interface=`/`dhcp-range=`
  블록 추가(dnsmasq 하나로 여러 인터페이스를 서빙하는 구성 vs MicroNetwork별 별도 프로세스 —
  택 1 필요)
- `firecrab-api/src/ipam.rs`: lease 테이블에 `micro_network_id` FK 추가, allocation을
  네트워크별로 스코프
- `CreateVmRequest`/`VmResponse`에 `micro_network_id` 추가(지금은 암묵적으로 하나뿐, VM을
  MicroNetwork의 bridge에 실제로 붙이는 경로 자체가 없음)
- 프론트엔드: VM 생성 폼에 MicroNetwork 선택 추가(목록/생성/삭제 UI는 완료, 1단계 —
  `firecrab-frontend/src/components/MicroNetworksModal.tsx`)
- active lease(또는 VM 소속)가 있는 MicroNetwork는 삭제 거부 — VM 연동 자체가 아직 없어서
  지금은 항상 삭제 가능

## 완료 기준

- MicroNetwork를 여러 개 만들 수 있고 각자 독립된 subnet/bridge/gateway를 가진다 ✅(1·2단계)
- VM 생성 시 소속 MicroNetwork를 선택하며, 서로 다른 MicroNetwork의 VM은 기본적으로 서로
  통신할 수 없다 — firewall 규칙이 아니라 **VRF로 라우팅 테이블 자체가 분리**돼 있어서
  (규칙 누락으로 뚫릴 수 없음), 같은 MicroNetwork 안에서의 east-west 차단은 지금처럼 firewall이
  담당 (남은 범위 — VM 소속 자체가 아직 없음)
- 하나의 MicroNetwork를 정리/재적용해도 다른 MicroNetwork의 bridge·firewall·lease는 그대로다
  ✅(bridge는 확인됨 — 각자 독립된 이름이라 서로 안 건드림. firewall/lease는 아직 해당 없음)
- active lease가 있는 MicroNetwork 삭제는 거부된다(남은 범위 — VM 연동 자체가 아직 없어 해당 없음)

## 산출물

**1·2단계 완료**: `firecrab-helper-protocol/src/network.rs`, `firecrab-net-helper/src/bridge.rs`,
`firecrab-net-helper/src/main.rs`, `firecrab-api/src/network.rs`, `firecrab-api/src/persistence.rs`,
`firecrab-api/src/handlers/micro_networks.rs`, `firecrab-api-types/src/lib.rs`,
`firecrab-frontend/src/components/MicroNetworksModal.tsx`, `firecrab-frontend/src/api/client.ts`

**남은 범위**: `firecrab-api/src/ipam.rs`, `firecrab-net-helper/src/vrf.rs`(신규),
`firecrab-net-helper/src/firewall.rs`, `firecrab-net-helper/src/nat.rs`,
`firecrab-net-helper/src/dhcp.rs`, `firecrab-frontend/src/components/CreateVm.tsx`
(MicroNetwork 선택 추가)
