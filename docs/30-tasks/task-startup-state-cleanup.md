---
tags:
  - firecrab
status: 완료
scope: 2주차
updated: 2026-07-23
---

# 재시작 정리

서버 재시작 후 유령 실행 상태 제거.

## 작업

- 서버 시작 시 `starting/running/stopping` 레코드를 `stopped`로 갱신
- (프로세스 추적/재연결은 후순위 — [task-vm-state-recovery.md](task-vm-state-recovery.md))

## 완료 기준

- running 상태로 저장된 채 재시작해도 목록에 유령 `running` 없음

## 산출물

`firecrab-api/src/main.rs`, `firecrab-api/src/persistence.rs`
