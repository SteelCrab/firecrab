---
tags:
  - firecrab
  - firecracker
status: 완료
scope: 2주차
updated: 2026-07-23
---

# Firecracker 프로세스 모듈

Firecracker 프로세스 spawn/종료 관리.

## 작업

- spawn: `firecracker --api-sock data/vms/{id}/firecracker.sock --config-file ...` (shell 미사용)
- stdout/stderr → `data/vms/{id}/console.log`
- readiness: socket 연결 + API 응답 확인까지 대기 (timeout)
- stop: `SIGTERM` → timeout(5s) → `SIGKILL`
- `AppState`에 실행 중 프로세스 map (`Uuid → Child` handle)

## 완료 기준

- 스폰 후 socket API 응답 확인
- stop 시 프로세스 실제 종료
- readiness timeout 시 프로세스 정리 후 오류 반환

## 산출물

`firecrab-api/src/firecracker.rs`, `firecrab-api/src/state.rs`
