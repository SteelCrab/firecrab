---
tags:
  - firecrab
  - m2image
  - spec
status: 설계 완료
scope: MVP
updated: 2026-08-05
---

# 부트스트랩 진행 상황 UI — 단계 타임라인 + 라이브 콘솔 — 설계

> [!summary] 한 줄 요약
> 부트스트랩 패널이 지금은 상태뱃지 5종 + 평문 로그 한 덩어리뿐이라
> 실제로 어디까지 진행됐는지 알 수 없다. VM 생성 화면이 이미 쓰는
> "단계별 성공/실패 타임라인"(`StartupStep`) 패턴을 부트스트랩에도
> 이식하고, 빌더 VM이 살아있는 동안은 기존 콘솔 WebSocket 메커니즘을
> 재사용한 경량 인라인 터미널로 실제 게스트 출력을 실시간으로 보여준다.

## 왜

- 실사용 중(Task 8 수동 검증) 사용자가 실제로 겪은 문제: 30분까지 걸릴
  수 있는 설치 스크립트 실행 동안 화면엔 60초마다 반복되는 한글
  heartbeat 한 줄(`여전히 실행 중… (N분 경과)`)만 보였고, "현재
  어디까지 되고 있는지를 알 수가 없어"라는 직접 피드백을 받았다.
- 같은 자리에서 두 가지를 명시적으로 요청받았다: **VM처럼 설치 과정을
  단계(stage)로 보여줄 것**, **로그에서 한글을 빼고 더 디테일하게 쓸
  것**.
- VM 상세 화면은 이미 정확히 이 문제를 풀어놓은 기존 패턴
  (`StartupStep`/`StartupStepRun`, `PipelineStepper`)이 있다 — 새로
  설계하지 않고 그 패턴을 그대로 미러링한다.
- `BootstrapResponse`의 `vm_id` 필드 doc comment(firecrab-api-types)에
  이미 "콘솔 WebSocket을 재사용해 라이브 출력을 보여줄 수 있다"는
  주석이 있었지만 실제로 구현된 적은 없다 — 이번 설계로 그 주석을
  실현한다.

## 핵심 아이디어

두 축을 독립적으로 추가한다:

1. **백엔드 4단계 타임라인** — `BootstrapStep`/`BootstrapStepRun`을
   VM의 `StartupStep`/`StartupStepRun`과 동일한 패턴으로 신설하고,
   `handlers/bootstrap.rs`의 실제 12개 phase를 4개 사용자 의미 단위로
   묶어 각 전환 지점에서 기록한다. 응답 스키마에 `current_step`/
   `step_timeline`을 추가해 프론트가 VM 화면과 똑같은 스테퍼 UI를
   그릴 수 있게 한다.
2. **경량 인라인 라이브 터미널** — 빌더 VM이 살아있는 동안(상태가
   `booting`/`running`일 때)만, 기존 `Console.tsx`의 xterm+WebSocket
   연결 로직만 떼어내 부트스트랩 패널 안에 직접 임베드한다. 원시
   게스트 콘솔 출력을 그대로 보여주므로, 가장 오래 걸리는
   "시스템 설치" 단계의 내부 디테일은 별도 백엔드 계측 없이 해결된다.

이 둘은 서로 보완적이다: 스테퍼는 거시적 위치("지금 4단계 중 어디")를,
라이브 터미널은 미시적 디테일("지금 이 순간 게스트에서 뭐가 실행되고
있는지")을 담당한다.

## 아키텍처 — 무엇이 바뀌는가

```
BootstrapPanel (Images.tsx)
  │
  ├─ 상태뱃지 (기존 유지)
  │
  ├─ [신규] BootstrapStepper
  │    4박스: 빌더 VM 준비 → 시스템 설치 → 패키징 → 마무리
  │    각 박스: succeeded/running/failed/pending + 경과시간
  │    데이터 소스: BootstrapResponse.stepTimeline (새 폴링 응답 필드)
  │
  ├─ [신규] InlineConsole (status ∈ {booting, running}일 때만 마운트)
  │    xterm + FitAddon + WebSocket, Console.tsx에서 로직만 재사용
  │    (툴바/설정/VM상세패널/터미널만모드/로그내보내기는 제외)
  │    연결 대상: ws://.../ws/vms/{bootstrapResponse.vmId}/console
  │    status가 packaging 이상으로 넘어가면 언마운트,
  │    "터미널 종료 (빌더 VM 정리됨)" 정적 문구로 대체
  │
  └─ 기존 curated 이벤트 로그 <pre> (유지, 한글 heartbeat만 영어로 교체)
```

백엔드 쪽 변경은 `firecrab-api/src/bootstrap.rs`(`BootstrapTracker`)와
`firecrab-api/src/handlers/bootstrap.rs`의 각 phase 전환 지점에
국한된다. 스크립트 실행/패키징/등록 등 기존 부트스트랩 로직 자체는
전혀 바뀌지 않는다 — 오직 "지금 몇 단계인지"를 옆에서 기록하는
계측만 추가된다.

## 의도적 범위 경계

- 4단계는 코드상 실제 12개 phase를 사용자에게 의미 있는 단위로 묶은
  것이다 — VM의 `StartupStep`(4단계)과 시각적 균형을 맞추기 위한
  선택이며, phase 하나하나를 전부 노출하지 않는다(예: 콘솔 셸 대기,
  MicroBoot 소스 확보 등은 "빌더 VM 준비"에 흡수됨).
- `InstallingSystem` 단계 내부의 세부 진행률(예: "몇 % 다운로드됨")은
  구조화하지 않는다 — 그 디테일은 라이브 터미널의 원시 출력으로
  충분하다는 게 이번 설계의 핵심 판단이다.
- VM 타임라인과 동일하게, 이번 타임라인도 **메모리 전용**이다
  (`BootstrapTracker`가 이미 그렇듯 SQLite에 영속화하지 않음) — 서버
  재시작 시 진행 중이던 부트스트랩 세션 자체가 유실되는 기존 known
  gap은 이번 설계로 악화되지도, 해결되지도 않는다.
- `clock()`의 로그 타임스탬프 포맷(`[NNNNs]` 절대 epoch초 →
  `[+NNs]` 경과초)은 이번 설계에 곁들이는 작은 개선이다 — 별도
  설계가 필요할 만큼 크지 않아 여기 포함한다.
- MicroBoot 최초 다운로드(세션 생성 전, 부트스트랩 세션 자체가 아직
  없는 시점)는 이번에도 계측하지 않는다 — 범위 밖.
- 취소(`cancel_bootstrap`)와 서버 재시작 중단은 기존 동작을 그대로
  따른다 — 세션이 사라지면 프론트는 GET 404를 "세션 종료"로 해석해
  스테퍼/터미널을 정리한다(기존에도 이미 그래야 하는 동작).

## 데이터 흐름 / 컴포넌트

### 데이터 모델 (firecrab-api-types)

```rust
pub enum BootstrapStep {
    StartingBuilderVm,
    InstallingSystem,
    Packaging,
    Finalizing,
}

pub enum BootstrapStepOutcome { Running, Succeeded, Failed }

pub struct BootstrapStepRun {
    pub step: BootstrapStep,
    pub started_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    pub outcome: BootstrapStepOutcome,
    pub detail: Option<String>,
}
```

`BootstrapResponse`에 `current_step: Option<BootstrapStep>`,
`step_timeline: Vec<BootstrapStepRun>` 추가. `BootstrapStepOutcome`은
`StartupStepOutcome`과 구조가 동일하지만 별도 타입으로 신설한다 —
`BootstrapStatus`와 `VmState`를 별개로 유지해 온 이 코드베이스의
기존 관례를 따른다.

TS 바인딩 3개 신규(`BootstrapStep.ts`, `BootstrapStepOutcome.ts`,
`BootstrapStepRun.ts`) + `BootstrapResponse.ts` 갱신, 기존
`StartupStep.ts` 3형제와 동일한 수작업 관례.

### 백엔드 단계 전환 지점

`BootstrapTracker`(firecrab-api/src/bootstrap.rs)에 `set_step`/
`close_open_step` 헬퍼를 추가한다. VM 쪽(`handlers/vms.rs`)은 호출부
여러 곳에서 `VmRecord`를 직접 건드리지만, 부트스트랩은 이미
`append_log`/`finish_ok`/`finish_err`/`finish_err_from`이 전부 tracker
메서드로 모여 있으므로 그 안에 자연스럽게 통합한다 — 종료 시 열려있는
단계를 자동으로 close하는 로직도 `finish_ok`/`finish_err`/
`finish_err_from` 내부에 넣으면 호출부마다 따로 챙길 필요가 없다.

| 전환 | 호출 지점 |
|---|---|
| 세션 생성 → `StartingBuilderVm` 시작 | `insert_session`(소스 확보+VM 생성/부팅 대기+콘솔 대기를 포괄) |
| → `InstallingSystem` | `wait_for_console_shell` 성공 직후, 스크립트 push 직전 |
| → `Packaging` | 기존 `Running → Packaging` CAS와 같은 지점(`package_bootstrap` 진입, VM stop 시작) |
| → `Finalizing` | 덤프/tar/zstd 완료 후, 빌더 VM 삭제 직전 |
| 종료(성공) | `finish_ok` — 마지막 단계를 `Succeeded`로 close |
| 종료(실패) | `finish_err`/`finish_err_from` — 열려있는 단계를 `Failed`+동일 reason으로 close (VM의 `close_open_step`과 같은 멱등 패턴) |

### 인라인 라이브 터미널

- 새 컴포넌트(가칭 `InlineConsole.tsx`) — `Console.tsx`에서 `Terminal`
  + `FitAddon` + WebSocket 연결/재연결 로직만 재사용. 툴바, 설정
  팝오버, 하단 VM 상세정보 패널, "터미널만" 모드, `LogExportActions`는
  전부 제외 — 이 컴포넌트는 항상 부트스트랩 패널 내부의 한 섹션일 뿐,
  독립 페이지가 아니다.
- props: `{ vmId: string }` — 부모(`BootstrapPanel`)가 마운트/언마운트
  자체로 연결 여부를 제어하므로 컴포넌트 내부에 null 분기를 둘 필요는
  없다.
- 백엔드 변경 불필요 — 기존 `/ws/vms/{vmId}/console`을 빌더 VM id로
  그대로 사용한다. 빌더 VM도 `VmRecord`로 취급되는 평범한 VM이라
  이 엔드포인트가 이미 그대로 동작한다.
- 마운트 조건: `BootstrapResponse.status`가 `booting` 또는 `running`일
  때만. `packaging`으로 전환되는 시점은 이미 `stop_vm`이 끝난 뒤이므로
  (기존 코드에서 `Running → Packaging` CAS가 stop_vm 시작과 같은
  자리) 그 이후엔 라이브 콘솔이 원천적으로 의미가 없다 — `vmId` 값의
  존재 여부가 아니라 `status`만으로 연결 여부를 판단한다(세션 종료
  후에도 `vmId` 필드 자체는 남아있지만 이미 삭제된 VM을 가리킨다).

### 로그 내용 (한글 제거 + 디테일)

- 기존 curated 로그 문자열 중 한글은 heartbeat 한 줄
  (`여전히 실행 중… (N분 경과)`)뿐 — 영어로 교체
  (`still running install script (+Nm)`). 라이브 터미널이 생긴 뒤로는
  이 줄의 역할이 "실시간 안내"에서 "로그 내보내기용 요약 기록"으로
  격하된다.
- `clock()`(현재 `format!("{}s", epoch_ms / 1000)`, 절대 epoch초를
  그대로 찍어 `[1785900123s]`처럼 나와 가독성이 없음)을 세션 시작
  기준 경과초로 변경(`[+42s]`).
- UI 라벨(스테퍼 박스 이름, 상태뱃지 등)은 한글을 유지한다 — 사용자
  요청은 로그 **본문**의 한글 제거였지 UI 크롬이 아니다.

### 프론트 BootstrapPanel 레이아웃 (Images.tsx)

상태뱃지 → `BootstrapStepper`(4박스, `PipelineStepper`와 동일 패턴) →
`InlineConsole`(조건부 마운트) → 기존 curated 로그 `<pre>`(영어 갱신).

## 에러 처리 / 엣지케이스

- 각 단계 실패 시 해당 박스가 `Failed`로 표시되고 이후 단계는
  `pending`으로 남는다 — VM 스테퍼의 기존 규칙과 동일.
- 취소(`cancel_bootstrap`): 세션 레코드 자체가 삭제되므로 단계 종료
  처리가 따로 필요 없다. 프론트는 GET 404를 세션 종료 신호로 받아
  스테퍼/터미널을 정리한다.
- 서버 재시작 중 진행 중이던 부트스트랩: 기존에도 세션이 유실되는
  known gap — 이번 작업으로 악화되지도 개선되지도 않는다.
- 라이브 터미널 연결이 실패/끊겨도(예: TAP/네트워크 일시 문제)
  스테퍼는 독립적으로 계속 정확하다 — 두 축이 서로의 가용성에
  의존하지 않는다.

## 테스트 전략

- 백엔드: `BootstrapTracker::set_step`/종료 시 자동 close 단위테스트.
  성공 경로 1개(4단계 모두 정상 전환 후 마지막 단계 `Succeeded`) +
  실패 경로 각 전환 지점당 최소 1개(그 시점에 열려있던 단계가
  정확히 `Failed`+reason으로 닫히는지).
- `clock()` 포맷 변경: 경과초 계산 단위테스트.
- 프론트: 기존 관례상 무거운 컴포넌트 테스트는 두지 않음(수동검증
  위주) — `stepStatus()`류 순수 함수를 추가하면 그 부분만 단위테스트.
- 3개 배포판 각각에서 스테퍼가 4단계를 실제로 순서대로 통과하고
  라이브 터미널에 실제 게스트 출력이 보이는지는 수동 검증(실제
  네트워크 필요, 기존 MicroBoot 설계와 동일한 한계).

## 완료 기준 (MVP)

- [ ] `BootstrapStep`/`BootstrapStepOutcome`/`BootstrapStepRun` 타입
      신설 + `BootstrapResponse`에 `current_step`/`step_timeline` 추가
      + TS 바인딩
- [ ] `BootstrapTracker`에 `set_step`/`close_open_step` 추가,
      `insert_session`/`finish_ok`/`finish_err`/`finish_err_from`과
      `handlers/bootstrap.rs`의 4개 전환 지점에 배선
- [ ] `clock()` 포맷을 절대 epoch초 → 세션 시작 기준 경과초로 변경
- [ ] heartbeat 로그 문자열 한글 → 영어 교체
- [ ] `BootstrapStepper` 프론트 컴포넌트 (`PipelineStepper` 패턴 재사용)
- [ ] `InlineConsole` 컴포넌트 (`Console.tsx`에서 xterm+WS 로직만 발췌)
- [ ] `BootstrapPanel`(Images.tsx)에 스테퍼 + 조건부 인라인 터미널 배선
- [ ] 3개 배포판(alpine-3.24/ubuntu-26.04/rocky-9) 웹 부트스트랩에서
      스테퍼 정상 진행 + 라이브 터미널 실제 출력 수동 검증

## 참고

- `docs/superpowers/specs/2026-08-05-m2image-microboot-design.md` —
  이번에 UI를 붙이는 대상인 MicroBoot 부트스트랩 흐름 자체의 설계.
- VM 쪽 참조 패턴: `firecrab-api-types/src/lib.rs`의 `StartupStep`/
  `StartupStepOutcome`/`StartupStepRun`, `firecrab-api/src/handlers/vms.rs`의
  `set_startup_step`/`close_open_step`/`finish_startup_timeline`,
  `firecrab-frontend/src/components/VmDetailModal.tsx`의 `PipelineStepper`.
- `firecrab-api/src/bootstrap.rs`(`BootstrapTracker`),
  `firecrab-api/src/handlers/bootstrap.rs`(12개 phase 전체 흐름),
  `firecrab-frontend/src/components/Console.tsx`(재사용 대상 WebSocket
  연결 로직), `firecrab-frontend/src/components/Images.tsx`의
  `BootstrapPanel`.
- `docs/30-tasks/task-vm-startup-timeline.md` — VM 타임라인이 왜
  메모리 전용/서버 타임스탬프 기반인지의 원 설계 근거, 이번 설계도
  동일 근거를 그대로 상속.
