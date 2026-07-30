---
tags:
  - firecrab
  - vm
status: 완료
scope: 2주차
updated: 2026-07-23
---

# VM 상태 모델

`VmState` lifecycle 확장 + 전이 검사 함수.

## 작업

- `VmState`에 `starting/running/stopping/stopped/error` 추가 (기존 `created` 포함 6개)
- 전이 검사 함수 (`can_transition(from, to)` 또는 동등한 method)

전이표:

| from | to |
|---|---|
| created | starting |
| starting | running, error |
| running | stopping, stopped(내부 종료), error |
| stopping | stopped, error |
| stopped | starting |
| error | starting |

삭제는 상태가 아니라 레코드 제거 (`created/stopped/error`에서만 허용).

## 완료 기준

- 전이표대로 허용/거부되는 단위 테스트

## 산출물

`firecrab-api-types/src/lib.rs`
