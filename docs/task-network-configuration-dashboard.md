# 네트워크 구성 대시보드 (Host 네트워크 설정 + VM 네트워크 소속)

지금은 bridge(`fcbr0`)·subnet(`172.30.0.0/24`)·uplink가 전부 코드에 고정돼 있고, 사용자는 대시보드에서 host의 네트워크 설정을 보거나 바꿀 수 없고 VM이 어느 네트워크에 속하는지도 확인할 수 없다. 웹 대시보드에서 host 네트워크를 설정하고, VM을 그 네트워크에 명시적으로 포함시키는 UI/API를 추가한다.

## AWS로 비유하면

| firecrab | AWS 대응 |
|---|---|
| 대시보드에서 host bridge/subnet/uplink 설정 | AWS VPC 콘솔에서 VPC·서브넷 생성/설정 |
| VM 생성 시 네트워크 선택 | EC2 인스턴스 launch 시 VPC/서브넷 선택 |
| VM 상세 화면에 소속 네트워크 표시 | EC2 인스턴스 상세의 "Networking" 탭(VPC ID·서브넷 ID) |

## 작업

- API: 네트워크 설정 조회/수정 엔드포인트(`GET/PUT /api/network`) — 지금 하드코딩된 subnet/uplink를 설정 가능한 값으로 노출
- API: `CreateVmRequest`에 네트워크 선택 필드 추가(초기엔 네트워크가 1개뿐이어도 필드는 명시적으로 존재), `VmResponse`에 소속 네트워크 정보 포함
- 프론트엔드: 설정 페이지에 "네트워크" 섹션(subnet CIDR·uplink 표시/수정), VM 생성 폼과 상세 모달에 소속 네트워크 노출
- 설정 변경은 실제 host에 반영 — 기존 `ensure_bridge`/`ensure_firewall` 재사용. subnet 변경 시 활성 lease와 충돌 안 나게 방어(활성 lease 있으면 축소·변경 거부)
- 선행 필요: `net.ipv4.ip_forward` 자동 활성화(지금은 수동, `task-shared-bridge-network.md` 참고), `firewall.rs`의 NAT 로직 `nat.rs` 분리 — 설정이 실제로 여러 값을 오갈 수 있으려면 이 정리가 먼저 되어 있는 게 안전
- 범위 조정 여지: 초기 버전은 네트워크 1개(현재 구조 그대로)를 "설정 가능하게 노출"만 하고, 여러 개의 독립된 네트워크(멀티 브릿지/서브넷) 지원은 후속으로 미룰 수 있음

## 진행 상황 (2026-07-24)

탐색 결과 subnet/gateway/bridge 이름이 `firecrab-api`(ipam.rs, handlers/network.rs)와
`firecrab-net-helper`(bridge.rs, firewall.rs, dhcp.rs) 양쪽에 5곳 독립적으로 하드코딩돼 있고,
설정을 영속화할 저장소 자체가 없으며, `ensure_gateway`는 기존 주소를 교체하는 로직이 없어
실제 subnet 변경 지원엔 새 로직이 필요함을 확인. 지금 host에 실제 running VM이 떠 있는 상태라
subnet을 실제로 바꾸는 기능은 라이브 시스템에 위험 부담이 있어, 이번 라운드는 문서가 명시한
안전한 선행 작업만 진행함:

- ✅ `net.ipv4.ip_forward` 자동 활성화 (`firecrab-net-helper/src/bridge.rs`의
  `enable_ip_forward()`, daemon 시작 시 1회 호출)
- ✅ `firewall.rs`의 NAT 로직을 `nat.rs`로 분리(동작 변화 없는 순수 리팩터 — `detect_uplink`,
  `validate_uplink`, postrouting 체인 렌더링, `BRIDGE_SUBNET`)
- ✅ `GET /api/network` 응답에 `uplink` 필드 추가(`/proc/net/route` 읽기, net-helper IPC 안 늘림)
  및 `HostInfoModal.tsx`에 표시

**남은 범위** (다음 단계): `PUT /api/network`로 subnet/uplink 실제 편집, 설정 영속화 저장소,
5곳 하드코딩을 런타임 설정 하나로 통합, `ensure_gateway`의 주소 교체 로직, 활성 lease 충돌 방어.

## 완료 기준

- 대시보드에서 host 네트워크 설정(subnet/uplink)을 보고 바꿀 수 있다
- VM 생성 시 어느 네트워크에 속하는지 선택·확인할 수 있고, 상세 화면에서도 확인 가능하다
- 활성 lease가 있는 상태에서 위험한 네트워크 변경(subnet 축소 등)은 거부된다

## 산출물

`firecrab-api/src/handlers/network.rs`, `firecrab-api-types/src/lib.rs`, `firecrab-net-helper/src/nat.rs`(신규, 완료), `firecrab-frontend/src/components/HostInfoModal.tsx` — subnet/uplink 실제 편집용 설정 페이지는 남은 범위
