# VM 생성 즉시화 + VM 상세 모달 테스트

## 자동 테스트

```sh
cargo test -p firecrab-api handlers::vms::tests::console_log
cargo test -p firecrab-api-types vm_log_response
cd firecrab-frontend && npx tsc --noEmit && npm run build && npm run lint
```

## 확인 항목

- `create_vm`이 템플릿 파일을 해싱하지 않고 `resolve_alias`(메모리 조회)만으로 즉시 응답
- `run_start`의 기존 `open_verified` 검증은 그대로 유지 — 변조된 템플릿은 여전히 start 시점에 잡힘
- `GET /api/vms/{id}/log`: 출력 없으면 빈 문자열, 파일 있으면 그대로 반환, 256KiB 초과 시
  `truncated:true`로 앞부분만, 존재하지 않는 VM은 404, non-UTF8 바이트가 있어도 에러 없이
  `from_utf8_lossy`로 처리
- 테이블 STATE 셀은 배지만 표시(인라인 단계 pill 없음) — `starting → running` 전이 동안에도 마찬가지
- VM 이름 클릭 시 모달(라우팅 아님) 오픈, 닫으면 `/log` 폴링이 즉시 멈춤

## 실제 검증됨 (이번 세션에서 완료)

- `cargo test --workspace`: 61(api, +5)/6(api-types, +1)/8/27 전부 green
- **생성 속도 실측**: 실제 2GB rootfs 템플릿 기준, 수정 전 동일 파일의 `sha256sum`(비최적화 CLI)이
  11.16초 — 앱 내부 `sha2`(opt-level 3)는 이보다 빠르지만 여전히 초 단위. 수정 후
  `POST /api/vms` 3회 연속 실측: 0.0006s~0.0016s. 사실상 즉시 응답으로 확인
- **실제 브라우저(headless Chrome + CDP)**로 전체 플로우 확인:
  1. 생성 폼 제출 → 0.5초 뒤 확인 시 이미 목록에 `created`로 반영(지연 체감 없음)
  2. 휴지 상태·`starting→running` 전이 전 구간에서 테이블 셀에 pill 없음(10회 연속 폴링으로 확인)
  3. `starting` 중 이름 클릭 → 모달에서 스테퍼가 `[1 active]-[2 pending]-[3 pending]` 형태로 렌더링,
     로그창에 `[HH:MM:SS] 디스크 준비 중 (rootfs 템플릿 복사)...` 파이프라인 라인 표시 — 스크린샷 확인
  4. `running`인 VM 이름 클릭 → 스테퍼 3단계 전부 체크(✓) 표시, 로그창에 **실제 guest 콘솔 부팅 로그**
     (OSC 3008 shell-integration 마커, `hostname=firecrab`, `root@firecrab:~#` 프롬프트 등)가 그대로
     보임 — 합성 데이터 아님, `console.log` 파일 내용 그대로
  5. 모달 닫기 → `fetch`를 훅킹해 `/log` 호출 횟수 확인: 열려있는 동안 4회 증가, 닫은 직후·2초 후 모두
     동일값 유지(폴링 확실히 정지)

발견해 감안한 것: OSC 이스케이프 바이트(`\x1b]3008;...`)가 일반 `<pre>`에는 그대로 글자로 보임(예:
깨진 네모 문자) — xterm.js가 아닌 읽기 전용 텍스트 로그라 의도된 동작. 실제 텍스트 메시지(부팅 로그,
로그인 프롬프트 등)는 정상적으로 읽힘.

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
curl -w "%{time_total}s\n" -X POST http://127.0.0.1:3000/api/vms \
  -H "Content-Type: application/json" \
  -d '{"name":"demo","template":"ubuntu-26.04","ram":512,"cpu":1}'
```

`http://localhost:8080/`에서 방금 만든 VM의 `start` 클릭 후 곧바로 이름 클릭 → 모달의 단계 스테퍼가
진행되고, 완료되면 로그창에 실제 guest 부팅 텍스트가 이어붙는지 확인.

## 정리

```sh
curl -s -X POST http://127.0.0.1:3000/api/vms/<id>/stop
curl -s -X DELETE http://127.0.0.1:3000/api/vms/<id>
```

세션 1의 두 터미널을 `Ctrl-C`로 종료.
