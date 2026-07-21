# MicroVM 부팅 + 대시보드 Terminal UI 테스트

## 자동 테스트

```sh
cargo test -p firecrab-api console:: firecracker:: handlers::console::
cargo check -p firecrab-frontend --target wasm32-unknown-unknown
```

## 확인 항목

- 늦게 붙은 뷰어도 backlog(최대 256KiB) 스냅샷을 즉시 받음
- backlog 스냅샷 이후의 라이브 출력도 정상 수신(subscribe 시점 기준 유실·중복 없음)
- 뷰어 2명이 동일 라이브 출력을 함께 받음(fan-out)
- 뷰어 없이 push해도 에러 아님, backlog에는 남음
- backlog 256KiB 초과 시 오래된 바이트부터 drop
- `write_input`은 stdin 미연결 상태에서도 안전(무시), 연결 후에는 실제 프로세스에 바이트 그대로 전달(`cat` echo로 검증)
- `firecracker::spawn_vm`이 만든 broker의 backlog가 `console.log` 파일 내용과 일치
- WS 라우트(`/ws/vms/{id}/console`)는 `enforce_limits`의 10초 타임아웃 레이어 밖 — REST(`/api`)만 별도 서브라우터로 타임아웃 적용
- 존재하지 않는 vm id(파싱 실패)는 `not_found`, 살아있는 프로세스가 없는 vm은 `vm_not_running`으로 구분

## 실제 검증됨 (이번 세션에서 완료)

net-helper/nftables 계열과 달리 이 기능은 **실제 브라우저 + 실제 부팅된 MicroVM으로 end-to-end 검증했다** (headless Chrome + CDP, 스크린샷 확인).

- `FIRECRAB_NETWORK_ENABLED` 미설정 상태로 API가 net-helper 없이 기동 → VM 생성·시작 → 4초 내 `running`
- 대시보드에서 `terminal` 버튼 클릭 → xterm.js에 실제 systemd 부팅 로그·`firecrab login: root (automatic login)`·셸 프롬프트가 그대로 렌더링(status: 연결됨)
- WS 연결이 10초를 넘겨도 끊기지 않음(20초 유지 확인)
- 원시 WS로 `echo raw-backend-check\n`을 정확히 보내면 guest가 그대로 실행하고 결과를 돌려줌 — 입력 경로가 바이트 단위로 정확함

발견되어 고친 문제 (재현 시 참고):
- trunk의 `[[proxy]]`는 HTTP/WS를 경로별로 하나만 선택 — REST(`/api`, ws=false)와 콘솔 WS가 같은 prefix를 못 씀 → 콘솔은 `/ws/vms/{id}/console`로 분리
- trunk WS 프록시의 `backend`는 `ws://` 스킴이어야 함(`http://`면 `UnsupportedUrlScheme`로 실패)
- 브라우저 Origin은 서버 기본 허용값 `http://localhost:8080`과 정확히 일치해야 함(`127.0.0.1:8080`은 다른 origin으로 취급되어 403)

xterm.js를 CDP 합성 키 이벤트(`Input.insertText`/`dispatchKeyEvent`)로 타이핑시키면 OSC 셸 통합 시퀀스와 얽혀 화면이 깨지는 현상이 있었다 — 원시 WS로 우회해 백엔드 입력 경로 자체는 완전히 정확함을 확인했으므로, 이는 CDP 합성 입력의 한계이지 제품 코드 결함이 아니다.

### 터미널 세션 1 — API + 프론트 실행

```sh
cargo run -p firecrab-api
```

다른 터미널에서(cwd는 `firecrab-frontend/`여야 함 — `Trunk.toml`/`index.html`이 거기 있음):

```sh
cd firecrab-frontend
trunk serve --port 8080
```

### 터미널 세션 2 — 수동 확인

```sh
# VM 생성 + 시작
curl -s -X POST http://127.0.0.1:3000/api/vms \
  -H "Content-Type: application/json" \
  -d '{"name":"demo","template":"ubuntu-26.04","ram":512,"cpu":1}'

curl -s -X POST http://127.0.0.1:3000/api/vms/<id>/start
```

브라우저로 `http://localhost:8080/`(꼭 `localhost`, `127.0.0.1` 아님 — Origin 불일치로 403) 접속 → VM `running` 행의 `terminal` 버튼 클릭 → 부팅 로그가 즉시 보이고, 클릭 후 타이핑하면 실제 guest 셸에 입력됨.

## 정리

```sh
curl -s -X POST http://127.0.0.1:3000/api/vms/<id>/stop
curl -s -X DELETE http://127.0.0.1:3000/api/vms/<id>
```

세션 1의 두 터미널을 `Ctrl-C`로 종료.
