# VM 시작 단계별 진행 상황 표시 테스트

## 자동 테스트

```sh
cargo test -p firecrab-api-types startup_step
cargo test -p firecrab-api handlers::vms::tests
cd firecrab-frontend && npx tsc --noEmit && npm run build && npm run lint
```

## 확인 항목

- `StartupStep`은 camelCase로 직렬화됨(`preparingDisk`/`generatingConfig`/`startingProcess`)
- `VmResponse.startupStep`은 `state != starting`일 때 항상 `null`
- `run_start`의 각 단계(디스크 준비 → 설정 생성 → 프로세스 시작) 진입 시 `startup_step`이 갱신됨
- `Starting`을 벗어나는 모든 경로(성공 → `Running`, 실패 → `Error`, claim 시 초기화)에서 `startup_step`이 `None`으로 정리됨 — 이전 시작의 stale 값이 남지 않음
- 대시보드에서 `starting` 행에 단계 pill 3개(완료/진행중/대기)가 표시되고, 폴링 주기 내에 갱신됨

## 실제 검증됨 (이번 세션에서 완료)

- `cargo test --workspace`: 56(api, +1) + 5(api-types, +1) + 8(helper-protocol) + 27(net-helper), 전부 green
- 실제 API로 확인: VM 생성 → `POST /start`를 백그라운드로 실행하며 0.1초 간격으로 `GET /api/vms/{id}`를 폴링 →
  2GB rootfs 복사가 끝날 때까지 수십 번의 응답 모두 `state:"starting"`, `startupStep:"preparingDisk"`를
  정확히 반환. 최종적으로 `state:"running"`, `startupStep:null`로 정리됨을 확인
- **실제 브라우저(headless Chrome + CDP)로 대시보드에서 확인**: VM 생성 폼 제출 → `start` 클릭 →
  `starting` 배지 옆에 "디스크 준비"(진행중, 호박색 pulsing) · "설정 생성"(대기) · "프로세스 시작"(대기)
  pill 3개가 정확히 렌더링됨을 스크린샷으로 확인
- 이 과정에서 실제 UI 버그 하나를 발견해 수정: 좁은 테이블 셀에서 한글 라벨이 pill 안에서 글자 단위로
  줄바꿈되던 문제 — `.startup-steps li`에 `white-space: nowrap` + `flex-shrink: 0`, 부모에
  `flex-wrap: wrap` 추가로 pill 전체가 하나의 단위로 줄바꿈되도록 수정 후 재검증 완료

### 터미널 세션 1 — API + 프론트 실행

```sh
cargo run -p firecrab-api
```

다른 터미널에서:

```sh
cd firecrab-frontend
npm run dev
```

### 터미널 세션 2 — 수동 확인

```sh
curl -s -X POST http://127.0.0.1:3000/api/vms \
  -H "Content-Type: application/json" \
  -d '{"name":"demo","template":"ubuntu-26.04","ram":512,"cpu":1}'
```

브라우저로 `http://localhost:8080/` 접속 → 생성된 VM의 `start` 클릭 → `starting` 배지 아래 단계 pill이
"디스크 준비"부터 순서대로 진행되는지 확인(2GB 복사라 몇 초간 "디스크 준비"에 머무는 게 정상).

## 정리

```sh
curl -s -X POST http://127.0.0.1:3000/api/vms/<id>/stop
curl -s -X DELETE http://127.0.0.1:3000/api/vms/<id>
```

세션 1의 두 터미널을 `Ctrl-C`로 종료.
