---
tags:
  - firecrab
  - network
status: 완료
scope: 3주차
updated: 2026-07-23
---

# VM별 TAP 자동화

VM start 시 고유 TAP 생성·bridge 연결, stop/delete/실패 시 정리.

## 작업

- TAP 이름: UUID hash 기반 ≤15byte(IFNAMSIZ), helper가 재계산 — API의 임의 이름 전달 불가
- interface alias에 `firecrab:<vm_uuid>` 기록, 최종 이름은 DB 저장
- 생성 순서: 이름 확인 → TAP 생성(/dev/net/tun) → alias → default-deny policy → bridge attach → link up
- config 작성·spawn 실패 시 compensation으로 policy·TAP 삭제 (async `Drop` 의존 금지)
- 삭제 전 ownership record + alias UUID + 요청 UUID 일치 재확인
- stop 시 TAP 제거, 다음 start에서 같은 lease로 재생성 (초기 고정 정책)

## 완료 기준

- 두 VM이 서로 다른 TAP으로 같은 bridge에 연결
- start 단계별 실패 주입에도 고아 TAP 없음
- daemon 복구 후 고아 TAP 없음

## 산출물

`firecrab-api/src/network.rs`, `firecrab-net-helper/src/tap.rs`, `docs/vm-tap-automation-smoke.md`
