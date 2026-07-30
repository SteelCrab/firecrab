---
tags:
  - firecrab
status: 보류
updated: 2026-07-23
---

# Guest agent·vsock provisioning

제한된 vsock protocol로 SSH key provisioning·readiness·shutdown 교환.

## AWS로 비유하면

| firecrab | AWS 대응 |
|---|---|
| vsock으로 host↔guest 통신(네트워크 경유 안 함) | **AWS Systems Manager(SSM) Agent** — 인스턴스 안에 떠서 host(AWS 제어 영역)와만 제한된 채널로 통신, SSH 포트를 열지 않고도 명령 전달 |
| HostCommand(Hello/Provision/Shutdown)만 허용 | SSM이 딱 정의된 문서(Document)/명령만 실행하고 임의 셸 접근은 아닌 것과 같은 구조 |
| guest는 host shell/파일/network 명령 실행 불가 | guest(에이전트)가 host를 조작할 방법이 원천적으로 없는 것 — 통제는 항상 host→guest 방향 |
| public host key만 host로 전송, private key는 guest 내부 생성 | EC2 키페어 모델 — private key는 절대 AWS/API를 거치지 않음 |

## 작업

- Rust guest agent + host protocol: HostCommand(Hello/Provision/Shutdown), GuestEvent(Hello/Ready/Provisioned/ShutdownAccepted/Error)만 허용
- frame ≤64 KiB, protocol version·CID·runtime_generation(매 start 발급)·agent_session_id(매 reconnect 발급)·challenge·message 순서 검증
- Guest 불신뢰: agent 요청으로 host shell/파일/network/helper 명령 실행 금지, 정해진 status message만 수신
- `vsock_leases` 테이블: guest CID unique 할당(immediate transaction), stop 동안 유지·delete cleanup 후 반환
- reconnect 시 이전 session의 ready/fingerprint를 stale 처리 후 새 session/challenge로 handshake
- agent는 공개키 형식·algorithm·길이 검증 후 전용 `authorized_keys`만 atomic 갱신 (다른 key file 불변)
- SSH host private key는 Guest 내부 생성, public host key만 host로 전송
- graceful shutdown: deadline 포함 Shutdown 명령 (architecture 무관 종료 경로)

## 완료 기준

- malformed guest message가 host process·다른 VM 상태에 무영향
- 재부팅·reconnect·snapshot 복원 후 새 session으로 재연결·재확인
- 동시 CID 할당 중복 없음, 이전 session replay 차단

## 산출물

`firecrab-guest-protocol/`, `firecrab-guest-agent/src`, `firecrab-api/src/guest_agent.rs`, `firecrab-api/src/vsock.rs`, `docs/guest-agent-vsock-smoke.md`
