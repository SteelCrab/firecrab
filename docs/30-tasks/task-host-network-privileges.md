---
tags:
  - firecrab
  - network
status: 미완료
scope: 3주차
updated: 2026-07-23
---

# 호스트 네트워크 권한 분리

API는 비특권으로 유지하고 bridge/TAP/firewall 작업만 helper 프로세스로 분리.

## AWS로 비유하면

| firecrab | AWS 대응 |
|---|---|
| `firecrab-api` (root 아님, 요청만 받음) | EC2 API(control plane) — 사용자가 직접 부르는 쪽 |
| `firecrab-net-helper` (CAP_NET_ADMIN만 보유) | AWS 내부의 실제 하이퍼바이저 조작 계층 — 고객은 절대 직접 못 건드림 |
| typed protocol(정해진 operation만) | EC2 API의 구조화된 액션(`CreateNetworkInterface` 등) — "임의 명령 실행"이 아니라 "정해진 액션 + 파라미터"만 존재 |
| `SO_PEERCRED` UID 검증 | AWS IAM의 API 호출자 인증(누가 부르는지 반드시 확인 후 실행) |

즉 "고객(API)은 하이퍼바이저를 직접 만지지 못하고, 정해진 액션만 내부 시스템(helper)에 요청할 수 있다"는 AWS의 기본 구조를 그대로 가져온 것.

## 작업

- `firecrab-net-helper`: CAP_NET_ADMIN만 가진 별도 프로세스, UDS(`/run/firecrab/net-helper.sock`) 통신
  - **왜 분리하나**: API 프로세스가 뚫려도(예: 파싱 취약점) 공격자가 얻는 건 이 좁은 protocol뿐 — 커널 네트워크 권한 자체를 못 가져감
- typed protocol(serde enum): EnsureBridge / CreateTap / DeleteTap / EnsureFirewall / ApplyVmPolicy / RemoveVmPolicy — 임의 명령·interface 이름·CIDR·nftables 텍스트는 받지 않음
- length-prefixed frame(≤64 KiB) + protocol version + `SO_PEERCRED` UID/GID 검증, 동시 연결·timeout 제한
- bridge/subnet/uplink는 root 소유 helper config에서만 읽고 TAP 이름은 helper가 UUID에서 계산 (API가 이름을 정해서 넘기지 못함)
- 소유권: interface alias `firecrab:<vm_uuid>` + root 소유 ownership record 일치 시에만 조작
- systemd unit에 필요 capability만 부여, API·helper protocol version 불일치 시 시작 실패

## 완료 기준

- API 프로세스 EUID ≠ root
- 검증된 UUID 자원만 조작, Firecrab 소유 아닌 interface/rule 변경·삭제 거부
- API가 침해돼도 임의 host 명령을 실행할 protocol 부재

## 산출물

`firecrab-api/src/network.rs`, `firecrab-net-helper/src/main.rs`, `docs/net-helper.md`
