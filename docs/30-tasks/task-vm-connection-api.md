---
tags:
  - firecrab
  - vm
status: 보류
updated: 2026-07-23
---

# VM 접속 정보 API

`GET /api/vms/{id}` 응답에 상태별 SSH 접속 정보 추가.

## AWS로 비유하면

| firecrab | AWS 대응 |
|---|---|
| `connection` 필드(ip/user/port/fingerprint) | EC2 콘솔의 **Connect** 버튼이 보여주는 정보(퍼블릭 IP, 사용자, 키 이름) |
| running + network ready + provisioning 확인돼야 값 존재 | EC2 인스턴스 **상태 검사(status check) 2/2 통과**해야 콘솔의 Connect가 활성화되는 것과 동일 |
| 그 외엔 전부 `null` | 상태 검사 통과 전엔 Connect 버튼이 비활성/회색으로 뜨는 것 |

## 작업

- `connection` 필드 추가: `{ ipv4, sshUser, sshPort, authentication: "public_key", hostKeyFingerprint }`
- running + network ready + 현재 runtime generation/agent session의 provisioning 확인 시에만 값 — 그 외 전부 `null` (DB에 IP가 남아 있어도)
- private key·TAP 이름·bridge·lease ID·host 경로 미노출
- `docs/api.md` 갱신

## 완료 기준

- 준비 전·비running 상태에서 `connection: null`
- 응답의 주소·사용자로 실제 SSH 접속 성공

## 산출물

`firecrab-api/src/handlers/vms.rs`, `firecrab-api/src/model.rs`, `docs/api.md`, `docs/vm-connection-api-smoke.md`
