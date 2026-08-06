---
tags:
  - firecrab
  - guide
  - frontend
updated: 2026-08-02
---

# 웹 대시보드

> [!summary] 한 줄 요약
> `firecrab-frontend`는 React + TypeScript(Vite) 대시보드임.
> VM 생성부터 시리얼 콘솔까지 브라우저 하나로 처리함.

## 실행

```sh
npm install --prefix firecrab-frontend     # 최초 1회

./scripts/dev-net-helper.sh                # 터미널 1 — 특권 helper
pkill -f 'target/debug/firecrab-api'       # 터미널 2 — 이전 인스턴스 정리
cargo run -p firecrab-api                  #            저장소 루트에서
pkill -f 'vite'                            # 터미널 3 — 이전 dev 서버 정리(선택)
npm run dev --prefix firecrab-frontend     # 터미널 3 — http://localhost:8080/
```

### 개발 환경에서 M2Image 설치 확인

M2Image 설치는 API가 `FIRECRAB_IMAGE_BASE_URL`의 패키지를 받아야 한다. 로컬에서 확인할 때는
패키지 호스트와 API를 각각 아래처럼 실행한다. 빈 `FIRECRAB_IMAGE_ROOT`를 쓰면 저장소의
사전 빌드 이미지가 이미 설치된 것으로 보이는 일을 피할 수 있다.

```sh
# 터미널 4 — 먼저 ./scripts/build-m2images.sh 로 dist/m2images를 만든 뒤 실행
python3 -m http.server --bind 127.0.0.1 --directory dist/m2images 8765

# 터미널 2 — 기존 API를 내린 뒤, 같은 저장소 루트에서 실행
FIRECRAB_IMAGE_ROOT=/tmp/firecrab-m2image-test \
FIRECRAB_IMAGE_BASE_URL=http://127.0.0.1:8765 \
cargo run -p firecrab-api
```

`GET /api/images`의 각 항목에 `packageUrl`이 보이면 Packer의 **패키지 설치** 버튼이
활성화된다. 패키지 설치가 끝나면 Store의 **이미지 가져오기** 목록에 준비된 패키지가 나타나며,
가져오기 로그는 Store에서 표시된다. 설정 없이 API를 띄우면 package API는 `503`을 반환한다.

### 종료

| 프로세스 | 붙어 있는 터미널 | 다른 셸에서 정리 |
|---|---|---|
| Vite (`npm run dev`) | `Ctrl-C` | `pkill -f 'vite'` |
| `firecrab-api` | `Ctrl-C` | `pkill -f 'target/debug/firecrab-api'` |
| net-helper | `Ctrl-C` | `sudo pkill -f firecrab-net-helper` |

포트 8080이 비었는지 확인: `ss -ltnp | grep 8080` (또는 `lsof -i :8080`).

> [!warning] 자주 걸리는 셋
> - **`localhost:8080`으로 접속** — `127.0.0.1:8080`은 다른 origin이라 403
> - **API는 저장소 루트에서** — DB(`data/firecrab.db`)와 VM 아티팩트 경로가 cwd 기준.
>   `firecrab-api/`에서 띄우면 빈 DB가 새로 생기고 기존 VM이 안 보임
> - **이전 인스턴스를 죽일 것** — 포트 3000(API)·8080(Vite)이 잡혀 있으면 새 프로세스가
>   못 뜨고, 브라우저는 계속 **재빌드 전 바이너리**를 상대함. 새 필드가 응답에 없으면 대개 이 경우

net-helper가 없으면 bridge/TAP/DHCP를 대신할 프로세스가 없어 VM 시작이
`network helper is unavailable`로 즉시 실패함.

## 화면

| 위치 | 무엇 |
|---|---|
| 상단 폼 | VM 생성 — 이름·이미지·cpu·ram·disk·MicroNetwork·외부 통신 |
| 목록 | 상태 배지 + 상태별 action. 3초 폴링 |
| VM 이름 클릭 | 상세 모달 — 정보·시작 타임라인·로그 |
| `terminal` 버튼 | 시리얼 콘솔 **새 창** (`#/console/<id>`, WebSocket + xterm.js). 하단 VM 세부정보 · `터미널만` 토글. `running`일 때만 |
| 좌측 `이미지` | M2Image-Packer의 패키지 설치 + M2Image-Store의 이미지 가져오기 · 로그 · 삭제 |
| 헤더 `MicroNetwork` | 가상 네트워크 목록/생성/삭제, 행 클릭 시 상세 |
| 헤더 `HOST 정보` | bridge/subnet/gateway/uplink + load·memory·disk·uptime |

## 생성 폼 제약

| 필드 | 제약 |
|---|---|
| name | 1–64자, 영문/숫자/`.`/`_`/`-` |
| template | `ubuntu-26.04` · `alpine-3.24` · `rocky-9` |
| cpu | 1–32 |
| ram | 128–32768 MiB, 2의 거듭제곱만 |
| disk | 템플릿 rootfs 크기(2 GiB) 이상, 500 GiB 이하 |
| MicroNetwork | 기본 네트워크 또는 만들어 둔 것 중 하나 |
| 외부 통신 | 인터넷 허용(기본) / 격리 |

서버 검증 오류는 해당 입력 아래에 필드별로 표시됨.

## VM 상세 모달

- 상태와 무관하게 언제든 열림
- **시작 타임라인** — 단계별 소요 시간(`820ms` / `3s` / `1m 32s`)과 시작 시각.
  실패하면 죽은 단계가 붉게 표시되고 사유가 붙음
- **로그** — 파이프라인 메시지 뒤에 실제 게스트 콘솔 출력이 이어짐. 부팅 후 다시 열어도 전체가 보임
- ip/mac은 실제 할당된 lease. hostname은 VM id에서 유도(`fc-<hex>`)라 시작 전에도 표시됨
- `created`/`stopped`/`error`에서만 **수정** 버튼 — cpu/ram/disk/외부 통신 변경.
  **다음 시작부터** 적용되고 disk 축소는 거부됨

## 상태 배지

| 배지 | 의미 | 가능한 action |
|---|---|---|
| `created` | 생성만 됨 | start · delete |
| `starting` | 시작 진행 중 | 없음 |
| `running` | 부팅 완료 | stop · terminal |
| `stopping` | 종료 처리 중 | 없음 |
| `stopped` | 정상 종료 | start · delete |
| `error` | 비정상 종료·시작 실패 | start · delete |

삭제는 `created`/`stopped`/`error`에서만 가능하고, 레코드와 디스크가 함께 지워짐(복구 불가).

## 동작

- 3초 폴링. 연속 3회 실패하면 15초로 완화
- 요청 중인 VM은 action이 잠겨 중복 클릭이 막힘
- `409 invalid_state` 등은 배너로 띄우고 즉시 재조회
- 종료 감시가 바꾼 상태(게스트 poweroff → `stopped`, 크래시 → `error`)도 폴링에 반영됨
- `vite.config.ts`가 `/api`는 HTTP, `/ws`는 WebSocket으로 `127.0.0.1:3000`에 프록시 —
  앱은 상대 경로만 씀(API 주소 하드코딩 없음)

## 프로덕션 서빙

dev 서버 없이 `firecrab-api`가 직접 서빙함. 같은 origin이라 CORS 설정도 불필요.

```sh
npm run build --prefix firecrab-frontend
FIRECRAB_STATIC_ROOT="$PWD/firecrab-frontend/dist" cargo run -p firecrab-api
# http://localhost:3000/
```

[설치 스크립트](install.md)가 이 경로를 systemd 유닛에 넣어 주므로,
설치한 호스트에서는 데몬 2개만으로 대시보드가 뜸.

## 파일

| 경로 | 내용 |
|---|---|
| `src/App.tsx` | 대시보드 상태(`useReducer`)·폴링·action |
| `src/api/client.ts` | fetch 기반 클라이언트, 오류 envelope 파싱 |
| `src/components/` | 생성 폼 · VM 테이블 · 배너 · 콘솔 · 상세/네트워크/호스트 모달 |
| `src/bindings/` | `firecrab-api-types`에 대응하는 TS 타입 |
| `src/index.css` | 스타일 전부(외부 JS 없음, 폰트만 Google Fonts) |

> [!warning] 바인딩 재생성 경로가 끊겨 있음
> `src/bindings/*.ts`는 `ts-rs`가 생성한 형식이지만, 지금 `firecrab-api-types`에는
> `ts-rs` 의존성도 feature도 없음 — 즉 **현재는 손으로 유지되고 있음**.
> Rust 타입을 바꾸면 대응 `.ts`도 같이 고쳐야 함. `ts-rs`를 되살리거나 이 사실을 명시하거나 택 1.

## 문제가 생기면

증상별 대응은 [트러블슈팅](troubleshooting.md).
기능별 검증 절차는 [tests](../40-tests/MOC-tests.md), 겪은 버그는 [bugs](../50-bugs/MOC-bugs.md).
