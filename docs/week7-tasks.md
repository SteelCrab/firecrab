---
tags:
  - firecrab
  - week7
  - snapshot
status: 시작 전
updated: 2026-07-29
---

# 7주차 — Snapshots

> [!summary] 목표
> 실행 중 VM을 같은 시점의 full checkpoint로 떠 두고, 한 번 복원한다.
> [4주차](week4-tasks.md)에 있던 snapshot 4종을 여기로 이월 —
> 격리·관측이 없으면 snapshot 실패를 진단할 수단 자체가 없기 때문.

## 선행 조건

- [4주차](week4-tasks.md) — jailer·cgroup 격리, metrics·tracing·health
  (복원 실패가 어디서 났는지 볼 수 있어야 함)
- [202 비동기 + operation/idempotency 계약](task-api-operation-contracts.md)
  — snapshot 생성/복원은 비동기 operation이라 이 계약이 먼저 있어야 함
- [disk generation·artifact ledger](task-vm-rootfs-and-artifacts.md)
  — writable rootfs의 세대 관리가 snapshot lineage의 기반
  — [4주차](week4-tasks.md) MicroStorage 절에서 진행

## 원칙

- **one-shot** — 하나의 checkpoint는 한 번만 복원된다. 재실행은 차단
- 부분 snapshot은 절대 노출되지 않는다 — 커밋 전에는 아무도 볼 수 없어야 함
- 복원 전에 host·path·무결성을 전부 검증한다

## 태스크

### SNAP-1. [snapshot 저장 모델](task-vm-snapshot-storage.md)

- **작업**: state / memory / writable rootfs + one-shot lineage와 인증 metadata를 원자적으로 저장,
  quota·retention 관리
- **완료 기준**: 부분 snapshot이 노출되지 않고, 외부 resource path·호환성·checksum/HMAC·
  consume 상태·in-use lease가 전부 기록됨

### SNAP-2. [snapshot 생성 API](task-vm-snapshot-create-api.md)

- **작업**: 실행 중 VM을 pause해 memory·state·writable disk가 같은 시점인 full checkpoint를 비동기 생성
- **완료 기준**: 커밋 전 실패는 source가 복구되고, 성공 뒤에는 source가 재개되지 않은
  `checkpointed` 상태가 됨

### SNAP-3. [snapshot 복원 API](task-vm-snapshot-restore-api.md)

- **작업**: `checkpointed` 원본 VM을 호환되는 snapshot에서 1회 복원
- **완료 기준**: host·path·무결성을 검증하고, load 전에 외부 자원과 metrics/log/vsock을 구성하며,
  resume intent 이후의 재실행을 차단

### SNAP-4. [snapshot 관리 UI](task-vm-snapshot-ui.md)

- **작업**: VM별 checkpoint 목록, 생성 operation, one-shot 소비 상태, 복원·폐기 흐름
- **완료 기준**: 상태에 맞는 action만 활성화되고, source 중지·소비 상태·복원 경고·비동기 실패가
  명확히 표시됨

### SNAP-5. 통합 테스트 (snapshot 부분)

- **작업**: [격리·관측·snapshot 통합 테스트](task-isolation-observability-snapshot-tests.md) 중
  snapshot replay·daemon 재시작·UI 재동기화 시나리오
- **완료 기준**: snapshot replay가 차단되고, daemon 재시작 후 host 자원이 정리됨
- **참고**: 같은 문서의 격리·관측 시나리오는 4주차 범위

## 이월 사유

| 날짜 | 내용 |
|---|---|
| 2026-07-29 | 4주차에서 snapshot 4종을 7주차로 이월 — 격리·관측이 선행돼야 실패 진단이 가능하고, 대회 일정상 운영 가능한 release([5주차](week5-tasks.md))가 먼저 |
