---
tags:
  - firecrab
  - network
  - micronetwork
status: 미완료
scope: 4주차
updated: 2026-07-29
---

# 호스트 방화벽(UFW) 연동 정책

> [!summary] 한 줄 요약
> UFW를 쓰는 호스트에서는 MicroNetwork를 만들 때마다 손으로 규칙을 넣어야 한다.
> 자동화할지 말지를 **먼저 정하는** 태스크.

## 왜

- UFW의 허용 규칙은 인터페이스 이름에 묶여 있어 `fcbr0`만 열려 있음
- 새 브리지(`mnb<hex>`)는 DHCP/DNS가 막혀 guest가 `no-ipv4-address`로 실패
- 증상과 우회는 [troubleshooting](troubleshooting.md)에 있지만, 사용자가 직접 겪어야 안다는 게 문제

## 선택지

| 안 | 내용 | 대가 |
|---|---|---|
| (a) 현행 유지 + 진단 강화 | 규칙은 안 건드리고, 실패 시 UFW를 원인으로 지목 | 사용자가 여전히 손으로 넣어야 함 |
| (b) opt-in 자동 삽입 | 설정 플래그가 켜졌을 때만 규칙 삽입·제거 | 자기 소유가 아닌 firewall을 건드림 |

> [!warning] 원칙 충돌
> firecrab은 자기가 만든 nftables 테이블만 건드린다.
> UFW 규칙 자동 삽입은 그 원칙을 깨므로, 켤 거라면 명시적 opt-in이어야 한다.

## 작업

- 위 두 안 중 하나를 결정하고 근거를 이 문서에 기록
- (a)면: 네트워크 준비 실패 시 UFW 상태를 읽어 원인 후보로 응답/로그에 노출
- (b)면: `FIRECRAB_MANAGE_HOST_FIREWALL` 같은 opt-in 설정 + 네트워크 삭제 시 규칙 회수

## 완료 기준

- 결정한 쪽이 문서화되고, 어느 쪽이든 **실패 원인이 로그나 API 응답에서 즉시 드러남**
- (b)를 택한 경우 네트워크를 지우면 넣었던 규칙도 사라짐

## 참고

- 완료된 범위는 [MicroNetwork](task-micro-network.md)
