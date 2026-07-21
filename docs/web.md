# 웹 대시보드 사용법

`http://localhost:8080/`에서 VM 생성부터 콘솔 접속까지 전부 브라우저로 처리한다. 개발 서버 띄우는
방법과 아키텍처는 [browser-test.md](browser-test.md) 참고 — 이 문서는 실제 사용 흐름만 다룬다.

## 실행

```sh
# 터미널 1
cargo run -p firecrab-api

# 터미널 2
cd firecrab-frontend && npm run dev
```

`http://localhost:8080/` 접속(꼭 `localhost`, `127.0.0.1` 아님 — Origin 불일치로 403).

## 1. VM 생성

상단 "VM 생성" 패널에서 이름·template·cpu·ram 입력 후 생성. 목록에 `created` 상태로 즉시 반영된다.

| 필드 | 제약 |
|---|---|
| name | 1–64자, 영문/숫자/`.`/`_`/`-` |
| template | `ubuntu-26.04`(현재 유일 선택지) |
| cpu | 1–32 |
| ram | 128–32768 MiB |
| disk | 템플릿 rootfs 크기(현재 2GiB) 이상 500GiB 이하 |

값이 잘못되면 해당 입력 아래 빨간 글씨로 서버 검증 오류가 바로 표시된다.

## 2. 시작 및 진행 상황

목록에서 `start` 클릭 → 테이블 STATE 셀은 배지만 바뀐다(`starting`). 단계별 진행 상황을 보려면
**VM 이름을 클릭**해 상세 모달을 연다(§3).

## 3. VM 상세 모달

목록에서 **VM 이름 클릭**(상태와 무관하게 언제든 가능) → 모달로 template/cpu/ram/disk/id 상세 정보,
상단에 단계 스테퍼(`[✓]─[✓]─[ ]` 형태), 아래에 로그창이 뜬다.

`created`/`stopped`/`error` 상태에서는 필드 옆에 **"수정"** 버튼이 뜬다 — 누르면 cpu/ram/disk가
입력 필드로 바뀌고 저장하면 즉시 반영된다(다음 시작부터 적용, 떠 있는 프로세스를 실시간으로 바꾸는
게 아님). disk는 축소 불가 — 현재 값보다 작은 값은 검증 오류. `running`/`starting`/`stopping`
중에는 "수정" 버튼 자체가 안 보인다.

스테퍼는 `디스크 준비` → `설정 생성` → `프로세스 시작` 순서. 첫 시작은 rootfs 템플릿(2GB)을 VM 전용
디스크로 복사하는 "디스크 준비" 단계가 대부분을 차지해 몇 초간 머무는 게 정상이다 — 이후 재시작은
이미 복사된 디스크를 재사용해 훨씬 빠르다. `running`에 도달하면 3단계 모두 체크 표시로 바뀐다.

로그창은 진행 단계 메시지(클라이언트에서 생성) 뒤에 **실제 guest 콘솔 부팅 로그**(서버가 캡처한
`console.log` 그대로)가 이어붙는다 — 부팅 완료 후 다시 열어도 캡처된 로그 전체가 즉시 보인다.
750ms 주기로 갱신되며, 모달을 닫으면 갱신도 멈춘다.

**참고**: `terminal` 버튼(`running` 상태 VM에 표시)은 이 브랜치에서 백엔드 WebSocket 라우트가 아직
연결되지 않아 클릭해도 붙지 않는다 — 별도 브랜치(`feat/microvm-terminal`)에 구현이 있고, 병합은
검증됐지만 이번 작업 범위에는 포함하지 않았다. 실제 guest 출력은 위 상세 모달의 로그창으로 확인할 수
있다.

## 4. 상태 배지

| 배지 | 의미 | 가능한 다음 action |
|---|---|---|
| `created` | 생성됨, 아직 시작 안 함 | start, delete |
| `starting` | 시작 파이프라인 진행 중(이름 클릭 시 단계 확인) | 없음(진행 중 대기) |
| `running` | 부팅 완료, 콘솔 접속 가능 | stop |
| `stopping` | 종료 처리 중 | 없음(진행 중 대기) |
| `stopped` | 정상 종료됨 | start, delete |
| `error` | 비정상 종료(크래시 등) | start, delete |

## 5. 삭제

`stopped`/`error`/`created` 상태에서만 `delete` 가능. 클릭 시 확인 대화상자가 뜨고, 확인하면 VM
레코드와 디스크 파일이 함께 삭제된다(복구 불가).

## 문제가 생기면

- 목록이 갱신 안 되고 "API 연결 안 됨 — 15s 간격 재시도"가 뜨면 `firecrab-api`가 살아있는지 확인
- 상세 모달이 계속 "불러오는 중…"이면 `firecrab-api`가 재시작되지 않았는지, VM id가 여전히
  존재하는지(삭제된 VM은 아닌지) 확인
- 그 외 상세 디버깅은 [tests/microvm-terminal.md](tests/microvm-terminal.md),
  [tests/vm-startup-progress.md](tests/vm-startup-progress.md),
  [tests/vm-detail-modal.md](tests/vm-detail-modal.md),
  [tests/vm-disk-capacity.md](tests/vm-disk-capacity.md),
  [tests/vm-resource-update.md](tests/vm-resource-update.md),
  [tests/frontend-react-migration.md](tests/frontend-react-migration.md) 참고
