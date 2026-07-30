---
tags:
  - firecrab
  - vm
status: 완료
scope: 2주차
updated: 2026-07-23
---

# 종료 감시

Guest 내부 종료(poweroff, crash)를 상태에 반영.

## 작업

- spawn 후 `child.wait()` 감시 task 등록
- 정상 종료 → `stopped`, 비정상 종료 → `error` 저장
- stop API에 의한 종료와 중복 갱신되지 않게 상태 확인 후 기록
- 종료 시 프로세스 map에서 제거

## 완료 기준

- Guest 내부 `poweroff` 시 상태 자동 `stopped`
- 프로세스 kill 시 `error`

## 산출물

`firecrab-api/src/firecracker.rs`, `firecrab-api/src/state.rs`
