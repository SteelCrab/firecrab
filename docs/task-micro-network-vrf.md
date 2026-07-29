---
tags:
  - firecrab
  - network
  - micronetwork
status: 미완료
scope: 4주차
updated: 2026-07-29
---

# MicroNetwork VRF — 네트워크별 라우팅 테이블 분리

> [!summary] 한 줄 요약
> 네트워크 간 차단을 **nftables 규칙에서 라우팅 구조로** 옮긴다.
> 규칙이 빠져도 뚫리지 않게.

## 왜

- 지금 MicroNetwork 간 차단은 `firecrab_egress`의 목적지 deny 규칙 하나에 달려 있음
- host는 모든 MicroNetwork 서브넷에 connected route를 갖고 `ip_forward`도 켜져 있어서,
  그 규칙이 빠지거나 flush 타이밍이 어긋나면 그대로 통함
- VRF는 **경로 자체가 없으므로** 규칙 누락으로 뚫릴 수 없음 — 같은 결과의 더 강한 보장

## 작업

- `firecrab-net-helper/src/vrf.rs` 신설
  - MicroNetwork마다 VRF 장치 + 전용 라우팅 테이블(테이블 id는 network id에서 유도)
  - 그 네트워크의 bridge를 VRF에 enslave
- `EnsureMicroNetworkBridge` 처리 경로에서 bridge 생성 직후 VRF에 붙이기
- 삭제 시 VRF 장치·라우팅 테이블도 함께 정리
- uplink로 나가는 경로만 VRF 밖으로 leak(인터넷이 켜진 네트워크에 한해)

## 완료 기준

- nftables의 cross-network deny 규칙을 **지운 상태에서도** 다른 MicroNetwork VM에 도달 못 함
- 인터넷이 켜진 네트워크는 여전히 외부와 통신됨
- 네트워크 삭제 후 `ip vrf show`에 잔여 장치가 없음

> [!warning] 대체가 아니라 이중 방어
> 기존 nftables 규칙은 그대로 남긴다. VRF는 그 위에 얹는 두 번째 방어선.

## 참고

- 완료된 범위는 [MicroNetwork](task-micro-network.md)
- 테스트 절차는 [tests/micro-network](tests/micro-network.md)의 "격리 확인"을 확장
