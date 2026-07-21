# Yew/Wasm → React/TypeScript 프론트엔드 전환 테스트

## 자동 테스트

```sh
cargo test -p firecrab-api-types                                          # 기존 4개, 무변경 통과
cargo check -p firecrab-api-types                                          # ts-rs off, 클린
cargo check -p firecrab-api                                                # 서버 빌드 그래프 무영향
TS_RS_EXPORT_DIR=../firecrab-frontend/src/bindings \
  cargo test -p firecrab-api-types --features ts-rs export_bindings        # 재생성, 커밋본과 diff
cd firecrab-frontend && npx tsc --noEmit && npm run build && npm run lint  # 타입체크 + 빌드 + oxlint
```

## 확인 항목

- wire 포맷 일치: `VmState.ts`는 소문자 문자열 유니온, `VmResponse`/`ErrorResponse`/`ApiError`는 camelCase,
  `id`/`requestId`는 `string`, `ApiError.fields`는 `fields?: Record<string,string>`(optional로 생성됨)
- `/api/*`는 HTTP, `/ws/*`는 WebSocket으로 둘 다 8080 Vite 프록시를 통해 정상 동작
- 생성→목록 반영(이름순 정렬)→시작→`running`→terminal 버튼 노출→정지→삭제 전체 흐름
- 필드 에러는 인풋 아래, 미매핑 에러는 배너
- 3회 연속 실패 후 "API 연결 안 됨 — 15s 간격 재시도"로 전환
- `127.0.0.1:8080` Origin은 403, `localhost:8080`만 허용(서버 측 정확매칭, 프론트 변경과 무관하게 유지)
- **핵심 회귀 확인**: 실제 MicroVM의 serial console을 대량 출력(WS 메시지 여러 개에 걸쳐 전송)으로
  스트리밍했을 때 xterm.js 렌더링이 깨지지 않는지 — 이 전환 자체의 존재 이유

## 실제 검증됨 (이번 세션에서 완료)

net-helper/nftables 계열과 달리 이 마이그레이션은 **실제 브라우저 + 실제 부팅된 MicroVM으로 end-to-end
검증했다**(headless Chrome + CDP, 스크린샷 확인). 이전 Yew 프론트엔드에서 발견된 원본 버그(xterm.js가
wasm 선형 메모리의 zero-copy view를 비동기로 읽다가 레이스로 깨짐)를 재현했던 **동일한 VM, 동일한
systemd OSC 3008 shell-integration 데이터**를 그대로 재사용해 확인했다.

- `cargo test --workspace`: 70(api) + 4(api-types) + 8(helper-protocol) + 27(net-helper), 전부 green —
  `ts-rs` 추가·워크스페이스 멤버 변경 후에도 기존 테스트 회귀 없음
- `ts-rs` 바인딩 생성: `BTreeMap<String,String>`·`Uuid`(uuid-impl feature) 둘 다 문제없이 지원됨을 직접
  확인, `#[serde(skip_serializing_if)]`는 예상대로 TS에서 `fields?:`(optional)로 반영됨
- `npx tsc --noEmit`은 통과했지만 `npm run build`(`tsc -b`)에서 별도로 `TS1294
  erasableSyntaxOnly` 에러 발견 — `ApiClientError` 생성자의 TS parameter-property 축약(`readonly x: T`
  직접 선언)이 원인. 명시적 필드 선언 + 생성자 본문 대입으로 수정 후 빌드 통과 — `--noEmit`과 `-b`가
  서로 다른 규칙을 검사한다는 걸 실제로 확인한 사례
- Origin 정확매칭(`127.0.0.1:8080` → 403, `localhost:8080` → 200): 백엔드 변경 없이 그대로 유지 확인
- 실제 대시보드에서 VM 생성 폼 제출 → `생성 중…` 비활성 상태 → (2GB rootfs 복사로 실제로 수 초 소요)
  → 목록에 `created` 상태로 반영 → 삭제까지 REST 흐름 전체 확인
- **콘솔 회귀 확인**: 이미 부팅되어 있던 VM(`c54525b1-…`)에 대시보드에서 접속 → 대량 backlog(과거
  boot 로그 전체) 재생이 깨짐 없이 렌더링됨 → 별도 raw WebSocket 연결로 `yes stress-test-line | head -c
  80000`을 주입해 WS 메시지 여러 개에 걸친 8만 바이트 burst를 발생시킴 → 화면에 `stress-test-line`이
  글자 하나 안 깨지고 수백 줄 렌더링됨(스크린샷 확인) → 서버 측 `console.log`의 동일 구간과 대조해
  `machineid=`/`hostname=` 등 OSC 3008 페이로드가 (예전 Yew 버전에서 사용자가 직접 목격한 것과 동일한
  guest 데이터인데도) 화면에는 전혀 노출되지 않고 정상적으로 감춰짐을 확인 — 원본 버그 리포트와 같은
  "서버 로그는 완벽, 화면만 깨짐" 기준으로 검증했고, 이번엔 화면도 완벽함

발견된 부수 이슈(수정 완료, 버그 아님): 콘솔 WS에 두 번째 뷰어(테스트용 raw WS injector)가 붙는
시점에 guest tty의 DSR/CPR 커서 위치 질의에 브라우저의 실제 xterm.js가 자동 응답하면서, 그 응답이
동시에 주입 중이던 입력 바이트와 같은 guest stdin에서 섞여 명령 앞부분이 깨진 적이 있었다 — 이는
"여러 뷰어가 stdin을 공유"하는 기존 설계상 당연한 특성이고 렌더링 버그가 아니다(입력 주입 사이에
짧은 지연을 두는 것으로 테스트 스크립트만 수정해 해결).

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

curl -s -X POST http://127.0.0.1:3000/api/vms/<id>/start
```

브라우저로 `http://localhost:8080/`(꼭 `localhost`, `127.0.0.1` 아님) 접속 → `running` 행의 `terminal`
클릭 → 부팅 로그가 즉시, 깨짐 없이 렌더링되는지 확인 → 타이핑 시 guest 셸에 정확히 도달하는지 확인.

## 정리

```sh
curl -s -X POST http://127.0.0.1:3000/api/vms/<id>/stop
curl -s -X DELETE http://127.0.0.1:3000/api/vms/<id>
```

세션 1의 두 터미널을 `Ctrl-C`로 종료.
