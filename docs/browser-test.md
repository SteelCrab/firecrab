# VM 대시보드 (브라우저)

`firecrab-frontend`는 React+TypeScript(Vite) 대시보드다. VM 목록/생성/시작/중지/삭제와 상태 polling,
그리고 실행 중인 VM의 serial console(터미널)을 제공한다. `firecrab-api-types`의 응답 타입을 `ts-rs`로
자동 생성해(`src/bindings/`) 그대로 재사용한다 — 손으로 TS 타입을 유지하지 않는다.

## 준비물 (1회)

```sh
cd firecrab-frontend
npm install
```

## 사용

API 서버를 먼저 띄운다.

```sh
cd firecrab-api
cargo run
```

대시보드는 Vite 개발 서버로 띄운다.

```sh
cd firecrab-frontend
npm run dev
# http://localhost:8080/
```

`vite.config.ts`의 proxy 설정이 `/api/*`는 HTTP로, `/ws/*`는 WebSocket으로 각각 `127.0.0.1:3000`에
전달하므로 앱은 same-origin 상대 경로만 사용한다(API 주소 hardcode 없음). 8080 포트는 API 서버 CORS
허용 origin(`http://localhost:8080`)과 정확히 일치해야 한다 — `127.0.0.1:8080`은 다른 origin으로
취급되어 403이 난다.

## 타입 재생성

`firecrab-api-types`의 Rust 타입을 바꿨으면 TS 바인딩을 다시 생성한다(자동 watch 아님, 수동 명령):

```sh
TS_RS_EXPORT_DIR=../firecrab-frontend/src/bindings cargo test -p firecrab-api-types --features ts-rs export_bindings
```

생성된 `firecrab-frontend/src/bindings/*.ts`는 git에 커밋 대상이다(gitignore 안 됨) — 프론트엔드가
Rust 툴체인 없이 `npm run dev`/`build`만으로 동작해야 하기 때문. 손으로 편집하지 않는다.

## 동작

- 3초 간격 polling으로 목록 갱신 — 종료 감시가 바꾼 상태(guest poweroff → `stopped`, crash → `error`)도 자동 반영
- API 연결 실패가 3회 연속되면 15초 간격으로 완화
- 상태별 action만 활성화: `created/stopped/error` → start·delete, `running` → stop, `starting/stopping` → 없음
- 요청 중인 VM은 action이 잠겨 중복 클릭이 차단됨
- `409 invalid_state` 등 실패 시 오류 배너 표시 + 즉시 재조회
- 생성 form은 서버 검증 오류(`fields.name` 등)를 필드별로 표시
- `running` VM 행의 `terminal` 버튼 → serial console을 WebSocket으로 스트리밍(`@xterm/xterm`)

## 파일

- `firecrab-frontend/index.html`, `src/index.css`: 엔트리 + 스타일(외부 JS 없음, 폰트만 Google Fonts)
- `firecrab-frontend/src/App.tsx`: 대시보드 상태(`useReducer`)·polling·action 처리
- `firecrab-frontend/src/api/client.ts`: fetch 기반 API client(오류 envelope 파싱)
- `firecrab-frontend/src/components/`: 생성 form, VM 테이블, 배너, 콘솔(xterm.js)
- `firecrab-frontend/src/bindings/`: `ts-rs` 자동 생성 타입(재생성 명령은 위 참고)

## 프로덕션 배포

`firecrab-api`에는 정적 파일 서빙 코드가 없다 — 지금은 dev/production 구분 없이 두 프로세스(API +
프록시 낀 프론트 dev 서버)로 동작한다. 단일 배포 아티팩트가 필요해지면 `tower-http`의 `ServeDir`로
`npm run build`의 `dist/`를 `firecrab-api`가 직접 서빙하는 옵션을 추가할 수 있다(아직 미구현,
`docs/task-packaging-systemd-upgrades.md` 범위).
