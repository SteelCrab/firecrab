---
tags:
  - firecrab
  - frontend
status: 완료
scope: 3주차
updated: 2026-07-23
---

# MicroVM 부팅 + 대시보드 Terminal UI

네트워크(TAP/guest 설정/SSH)보다 먼저 "VM이 실제로 부팅되고 브라우저에서 셸을 친다"를 실증. serial console(ttyS0)은 네트워크와 무관해 최소 경로로 데모 가능.

## 작업

- `FIRECRAB_NETWORK_ENABLED`(기본 off)로 시작 시 `EnsureBridge` 요구를 gate — net-helper 없이도 API 기동, VM은 그대로 netless 부팅
- Firecracker stdin/stdout을 pipe로 연결(PTY 불필요 — guest ttyS0가 이미 진짜 tty)
- `ConsoleBroker`: stdout을 읽어 `console.log` tee + `broadcast`로 다중 뷰어 fan-out, backlog(256KiB)로 늦은 접속자도 스냅샷 수신
- `GET /ws/vms/{id}/console` WS — REST(`/api`)와 분리된 서브라우터라 `enforce_limits`의 10초 타임아웃 밖에 위치. 입력 프레임은 guest stdin으로 그대로 전달
- 프론트: vendored xterm.js + `wasm-bindgen inline_js` 최소 바인딩, WS로 출력 스트리밍·입력 전송, VM 목록의 `terminal` 버튼

## 완료 기준

- net-helper 없이 API 기동, VM 생성·시작 성공
- 대시보드 `terminal` 버튼 → 실제 부팅 로그·로그인 프롬프트가 브라우저에 렌더링
- WS 연결이 10초 넘게 유지됨(REST 타임아웃 미적용 확인)
- 타이핑한 입력이 guest 셸에 정확히 도달(원시 WS로 바이트 단위 검증)

## 산출물

`firecrab-api/src/console.rs`, `firecrab-api/src/firecracker.rs`, `firecrab-api/src/handlers/console.rs`, `firecrab-api/src/server.rs`, `firecrab-api/src/main.rs`, `firecrab-frontend/src/xterm.rs`, `firecrab-frontend/src/components/console.rs`, `firecrab-frontend/vendor/`, `docs/tests/microvm-terminal.md`
