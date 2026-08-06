---
tags:
  - firecrab
  - m2image
  - spec
status: 설계 완료
scope: MVP
updated: 2026-08-06
---

# M2Image 상세 패널 — 행 클릭 시 정보+액션 통합 — 설계

> [!summary] 한 줄 요약
> Images 표의 행을 클릭하면 상세 정보(버전·크기·사용 중인 VM)가 열리고,
> 지금은 표 행마다 흩어져 있는 설치/삭제 버튼과 페이지 하단에 항상 떠
> 있는 "배포판 부트스트랩" 패널을 이 상세 안의 **굽기·설치·삭제** 3개
> 액션으로 한데 모은다.

## 왜

- 지금 Images 화면은 이미지 하나를 다루는 데 필요한 정보/액션이 두 곳에
  나뉘어 있다 — 표 행의 설치/삭제 버튼과, 페이지 하단에 항상 보이는
  별도 "배포판 부트스트랩" 패널(3개 alias 버튼을 전부 노출). 어떤
  이미지를 보든 관련 없는 나머지 alias의 부트스트랩 버튼까지 항상
  화면을 차지한다.
- `firecrab-frontend/src/components/MicroNetworks.tsx`/
  `MicroStorages.tsx`는 이미 "행 클릭 → 표 아래 인라인 상세 패널" 패턴을
  쓰고 있다 — Images만 이 패턴에서 벗어나 있다.
- 최근 웹 이미지 빌드 기능(파생 이미지 빌드) 제거로 표 액션 열이 이미
  단순해진 상태라, 지금이 배치를 다시 정리하기 좋은 시점이다.

## 핵심 아이디어

표는 `이미지 · 크기 · 상태` 3열만 남기고 순수 인벤토리로 만들고, 행을
클릭하면 그 alias의 상세 정보 + 액션 3개(**굽기 · 설치 · 삭제**)가 표
아래 인라인 패널에 열린다. 굽기는 지금의 "배포판 부트스트랩", 설치는
지금 행에 있던 가져오기/로컬 패키지 설치, 삭제는 지금의 삭제 버튼과
동일한 동작이다 — **동작 자체는 하나도 바뀌지 않고, 위치와 노출 방식만
정리**한다.

## 의도적 범위 경계

- 백엔드 API·wire 타입 변경 없음. 기존 `install`/`package`/`bootstrap`
  엔드포인트를 그대로 재사용한다.
- 대상은 지금과 동일한 3개 alias(alpine-3.24/ubuntu-26.04/rocky-9)뿐이다
  — 이 셋은 `TemplateRegistry::known_specs()`가 고정으로 반환하며,
  웹 이미지 빌드 기능 제거 이후 이 목록에 새 alias가 추가될 경로 자체가
  없다. alias 목록 확장은 이번 스코프 밖.
- `firecrab-frontend/src/components/Images.tsx` 한 파일만 수정한다.
- 상세 안에서 여러 alias의 진행 상황을 동시에 보여주는 멀티 패널 같은
  것은 만들지 않는다 — MicroNetworks/MicroStorages와 동일하게 한 번에
  하나의 alias만 펼쳐진다.

## UI 변경

### 표

`이미지 / 크기 / 상태` 3열만 유지, **액션 열 제거**. 행 클릭 시 선택
토글(`selectedAlias`) — 이미 선택된 행을 다시 클릭하면 닫힘. 선택된
행에 `selected` 클래스(MicroNetworks/MicroStorages와 동일 CSS).

### 상세 패널 (표 아래, `selectedAlias`가 가리키는 이미지 기준)

1. 기본 정보 `<dl className="detail-fields mono">`: alias · 버전 ·
   최소 디스크(`minDiskGb`) · rootfs 크기(`formatRootfsSize`) · 상태 ·
   설명(`description`) · 패키지 URL(있으면)
2. 이 이미지를 쓰는 VM 목록 — alias 선택 시 `listVms()`를 조회해
   `template === alias`로 클라이언트 필터링(전용 백엔드 엔드포인트
   불필요). 없으면 "없음", 있으면 `handleDelete`가 이미 쓰는 것과 같은
   `이름 [상태]` 형식으로 나열.
3. 액션 3개: **굽기 · 설치 · 삭제** (아래 표)
4. 굽기/설치가 진행 중이면 액션 바로 아래에 그 진행 상황(스텝퍼+
   인라인 콘솔+로그, 또는 설치 로그)이 표시된다 — 지금 있는 컴포넌트
   (`BootstrapStepper`, `InlineConsole`, `LogExportActions`)를 그대로
   재사용.

## 액션 규칙 (동작은 기존 그대로, 위치와 노출 조건만 재구성)

| 버튼 | 동작 (기존 함수 재사용) | 비활성 조건 | 라벨 |
|---|---|---|---|
| 굽기 | `startBootstrap(alias)` | ① 이미 설치됨/패키지 준비됨(기존 `bootstrapBlockedAliases`) 또는 ② 부트스트랩 요청이 응답 대기 중(`starting`, 기존 더블클릭 방지 그대로)이거나 세션이 비종결 상태인데 그게 **다른** alias 것 | 기본 "굽기", ①이면 "이미 설치됨/패키지 준비됨", ②면 "다른 배포판 굽는 중", 선택된 alias 자신이 진행 중이면 "굽는 중…" |
| 설치 | packageStaged→`handleInstallStaged`("로컬 패키지 설치"), 아니면 packageUrl→`handleFetchPackage`("가져오기 (파일명)", 다운로드 후 자동 설치까지 이어짐) | 이미 설치됨, 또는 스테이징·URL 둘 다 없음("패키지 URL 없음") | 위 셋 중 하나, 진행 중이면 "가져오는 중…"/"설치 중…" |
| 삭제 | 기존 `handleDelete`(확인창 → `in_use`면 사용 중 VM 목록 보여주고 정리 여부 재확인) | 미설치 상태 | "삭제 중…" 진행 시 |

②의 "다른 alias 진행 중" 판단은 지금 `BootstrapPanel`의 전역 `busy`
플래그(백엔드가 세션을 하나만 허용하므로 어차피 전역 제약)를 그대로
쓰되, 선택된 alias 자신의 세션인지 아닌지에 따라 라벨/툴팁만 분기한다
(기존엔 3버튼이 항상 같이 보였으니 이 구분이 필요 없었다 — 이번에
새로 필요해지는 문구).

## 컴포넌트/상태 재구성

- `Images()`에 `selectedAlias: string | null` 신설.
- 지금 `BootstrapPanel`이 갖고 있는 `session`/`starting`/
  `pollBootstrap`/`start` 로직을 `Images()`로 끌어올린다(부트스트랩은
  alias와 무관하게 전역 단일 세션이라 컴포넌트가 언마운트되지 않는 한
  세션 상태가 유지돼야 하고, 지금처럼 항상 마운트돼 있던 별도 패널이
  아니라 조건부로 열리는 상세 안에 렌더링되므로 상위로 옮겨야 한다).
  `BootstrapPanel`은 독립 `<section>`을 그리는 컴포넌트에서, 상세 패널
  내부에 끼워 넣는 조각으로 성격이 바뀐다 — 별도 훅으로 추출할 만큼
  다른 곳에서 쓰이지 않으므로 그대로 `Images()` 본문 함수들로 합친다.
- `install`(설치 진행 상태, 기존에도 `Images()` 소유) 렌더링 조건에
  `install.alias === selectedAlias`를 추가 — 로그 블록 자체는 그대로.
- 사용 중 VM 목록은 `selectedAlias` 변경 시 `useEffect`로 조회하는 새
  상태(`usedByVms`/`usedByError`) — MicroNetworks의
  `getMicroNetwork(selectedId)` 패턴과 동일한 모양.
- 상세 패널 자체는 이 파일 안의 새 서브컴포넌트(예: `ImageDetail`)로
  뽑아, `MicroNetworkDetail`처럼 표시 전용 + 액션 콜백을 props로 받는
  형태로 구성한다.

## 트레이드오프 (사용자 확인 완료)

다른 alias 상세를 보는 동안 A alias의 **설치** 작업이 진행 중이면, 그
로그는 A를 다시 선택하기 전까지 화면에 보이지 않는다(작업 자체는
계속 폴링되며 끊기지 않음). 부트스트랩은 전역에 하나만 돌 수 있어
이 문제가 없다. MicroNetworks/MicroStorages도 선택 해제 시 상세를
숨기는 동일 패턴이라 이 코드베이스 안에서 일관된 선택이다 — 논의 후
승인됨.

## 테스트 전략

- 프론트: 이 컴포넌트는 기존에도 무거운 렌더 테스트가 없다(수동 검증
  위주) — 이번 변경도 동일 관례를 따른다. `keepNewestJobSnapshot`류
  이미 존재하는 순수 함수는 그대로 두고 건드리지 않는다.
- 수동 검증: 3개 alias 각각에서 (1) 행 클릭 → 상세 열림/재클릭 → 닫힘,
  (2) 미설치 alias 굽기 → 상세 안에서 스텝퍼+콘솔+로그 진행 확인 →
  완료 후 설치 버튼 활성화, (3) 설치 → 로그 → 완료 후 상태 "설치됨",
  (4) 삭제 → 사용 중 VM 있는 경우/없는 경우 각각, (5) 굽기 진행 중
  다른 alias 선택 시 그 alias의 굽기 버튼이 "다른 배포판 굽는 중"으로
  비활성.

## 완료 기준 (MVP)

- [ ] 표에서 액션 열 제거, 행 클릭으로 `selectedAlias` 토글 + `selected`
      클래스
- [ ] 상세 패널 서브컴포넌트: 기본 정보 `<dl>` + 사용 중 VM 목록(신규
      `listVms()` 조회) + 굽기·설치·삭제 3버튼
- [ ] `BootstrapPanel`의 세션 상태/폴링을 `Images()`로 이관, 렌더링을
      상세 패널 내부(선택된 alias가 세션 alias와 같을 때)로 이동
- [ ] "다른 alias 굽기 진행 중" 비활성 라벨/툴팁 추가
- [ ] `install` 로그 블록 표시 조건에 `selectedAlias` 일치 추가
- [ ] 3개 alias 전체에서 위 테스트 전략 항목 수동 검증

## 참고

- `firecrab-frontend/src/components/MicroNetworks.tsx`,
  `MicroStorages.tsx` — 재사용하는 행 클릭/인라인 상세 패턴의 원본.
- `firecrab-frontend/src/components/Images.tsx` — 이번에 수정하는
  파일 전체(`BootstrapPanel`, `BootstrapStepper`, 표/액션 로직).
- `docs/superpowers/specs/2026-08-05-bootstrap-progress-ui-design.md` —
  `BootstrapStepper`/`InlineConsole`가 왜 지금 모양인지의 원 설계.
