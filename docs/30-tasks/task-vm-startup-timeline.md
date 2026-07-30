---
tags:
  - firecrab
  - frontend
  - observability
  - vm
status: 진행 중
scope: 4주차
updated: 2026-07-30
---

# VM 시작 타임라인 — 단계별 소요 시간과 타임스탬프 로그

> [!summary] 한 줄 요약
> VM 생성 화면을 **CI 빌드 로그처럼** 만든다.
> 가로로 늘어선 단계마다 소요 시간과 ✓가 붙고, 아래에는 시각이 찍힌 로그가 흐른다.
> "왜 아직 안 뜨나"를 눈으로 짚을 수 있게.

## 왜

- 지금도 단계는 보이지만([task-vm-startup-progress](task-vm-startup-progress.md)) **얼마나 걸렸는지**가 없다
- 느릴 때 어느 단계가 느린지 알 수 없다 — 디스크 복사인지, 부팅인지, DHCP 대기인지
- 실제로 이 구분이 필요했던 적이 두 번 있다:
  [디스크 I/O 병목](../50-bugs/vm-startup-stuck-under-concurrent-load.md),
  [DHCP 실패](../50-bugs/dhcp-never-reaches-guest.md).
  둘 다 "starting에서 멈춤"으로만 보여서 원인 구분에 시간을 썼다
- 로그에 시각이 없어 콘솔 출력과 대조가 안 된다

## AWS로 비유하면

| firecrab | AWS 대응 |
|---|---|
| 단계별 소요 시간 막대 | CodeBuild의 phase별 duration |
| 단계 ✓ / 진행 중 / 대기 | CodePipeline 스테이지 상태 |
| 시각이 찍힌 스트리밍 로그 | CloudWatch Logs의 이벤트 타임스탬프 |
| 실패한 단계에서 멈춤 | CodeBuild `PHASE_FAILED` |

## 지금 무엇이 있고 무엇이 없나

| 요소 | 현재 |
|---|---|
| 단계 정의 | ✅ `StartupStep` 4종 — 디스크 준비 / 설정 생성 / 프로세스 시작 / 네트워크 확인 |
| 단계 표시 | ✅ `VmDetailModal`에 세로 목록 + ✓ |
| 로그 | ✅ 파이프라인 라인 + 게스트 콘솔, 클라이언트가 시각을 찍음 |
| **단계별 소요 시간** | ❌ |
| **단계 전이 시각(서버 기준)** | ❌ — `startup_step`은 현재 단계만 알려주고 메모리에만 있음 |
| 가로 진행 바 | ❌ 세로 목록 |

> [!important] 3초 폴링으로는 시간을 못 잰다
> `startup_step`은 폴링으로만 관측된다(3초 간격). 디스크 준비처럼 2~3초 만에 끝나는 단계는
> 아예 안 보이거나 3초로 뭉뚱그려진다.
> **정확한 시간은 서버가 전이 시각을 함께 줘야** 나온다.

## 작업

### 1단계 — 서버가 단계 전이 시각을 준다 ✅ (2026-07-30)

- [x] `StartupStepRun { step, startedAtMs, endedAtMs, outcome, detail }` 신설
- [x] `set_startup_step`이 이전 단계를 닫고(`succeeded`) 다음 단계를 연다
- [x] `VmResponse.startupTimeline` 추가 — 시작이 끝난 뒤에도 유지(그때가 제일 볼 만함)
- [x] 실패 시 열린 단계를 `failed` + 사유로 닫는다. 그 뒤 단계는 아예 안 생김
- [x] 새 시작을 선점할 때만 타임라인을 비운다(`claim_transition`)
- [x] 시각은 `SystemTime` epoch millis — 새 의존성 없음, 포맷은 클라이언트가

### 2단계 — 프론트엔드 타임라인 ✅ (2026-07-30)

- [x] 단계를 **가로 배치**로, 각 칸에 소요 시간(`820ms` / `3s` / `1m 32s`)과 ✓/✕
- [x] 진행 중인 단계는 1초 타이머로 경과 시간이 올라감(폴링과 무관)
- [x] 각 단계 아래에 시작 시각(`00:14:48.827`)
- [x] 실패한 단계는 `--error` 색 + 사유 표시, 이후 단계는 흐리게
- [ ] 로그 라인의 시각을 **서버 타임스탬프**로 교체(아직 클라이언트 추정)

### 3단계 — 생성 직후부터 보이게

- [ ] VM 생성 즉시 이 화면을 열어 시작까지 이어지게 (지금은 생성과 시작이 따로)
- [ ] `stopped` → `start` 재시작에서도 같은 타임라인이 다시 그려질 것

## 완료 기준

- [x] 단계별 소요 시간이 **정확**하다 — 서버 타임스탬프라 폴링 주기와 무관, 1초 미만은 `ms`로
- [x] 실패하면 **어느 단계에서** 실패했는지가 화면에 남는다
- [x] 동시에 여러 대를 시작해도 각자의 타임라인이 섞이지 않는다(VM 레코드별 배열)
- [ ] 로그의 시각과 게스트 콘솔 출력의 시각을 대조할 수 있다 — 로그 타임스탬프가 아직 클라이언트 기준

> [!note] 자동 테스트로 덮은 범위
> `a_successful_start_records_every_step_with_its_own_span` — 4단계가 순서대로,
> 각자 닫히고, 서로 겹치지 않음.
> `a_failed_start_marks_the_step_it_died_on` — 죽은 단계가 `failed` + 사유로 닫히고
> 그 뒤로 아무 단계도 안 생김.

## 선행·연관

- **선행 없음** — 2단계(프론트엔드)만 먼저 해도 폴링 기준의 근사 시간은 나온다.
  다만 정확도 때문에 1단계를 같이 하는 것을 권장
- [lifecycle event log API](task-lifecycle-log-api.md) — 단계 전이를 영속 이벤트로 남기려면 이쪽과 합쳐야 한다.
  이 태스크는 **진행 중인 시작 한 건**만 다루므로 메모리로도 성립
- [SQLite 확장 스키마](task-sqlite-migration-and-state-model.md) — 위와 함께 갈 때만 필요
- [VM 관측 대시보드](task-observability-dashboard.md) — 같은 화면에 metrics를 얹는 후속

## 참고

- 기존 단계 구현: [task-vm-startup-progress](task-vm-startup-progress.md),
  검증 절차는 [tests/vm-startup-progress](../40-tests/vm-startup-progress.md)
- 상세 모달 구조: [tests/vm-detail-modal](../40-tests/vm-detail-modal.md)
