---
tags:
  - firecrab
  - host
  - frontend
status: 완료
scope: 4주차
updated: 2026-07-29
---

# Host 프론트엔드 서빙 — dev 서버 없이 띄우기

> [!summary] 한 줄 요약
> 지금 대시보드는 Vite **dev 서버**로만 뜬다.
> 빌드된 정적 자산을 API가 직접 서빙해서, 운영에서 프로세스를 하나 줄인다.

## 왜

- 지금 firecrab을 띄우려면 터미널 3개가 필요하다
  1. `firecrab-net-helper` (root)
  2. `cargo run -p firecrab-api`
  3. `npm run dev` (Vite, 8080 → 3000 프록시)
- 3번은 개발용이다. `/api`·`/ws` 프록시가 `vite.config.ts`에 있어서
  이게 없으면 브라우저가 API에 닿지 못함
- `firecrab-api`에는 정적 파일 서빙 코드가 아예 없음([web](../20-guides/web.md))
- 데몬화([systemd 유닛](task-host-systemd-daemons.md))를 해도 이게 남으면
  운영 환경에서 Node를 계속 띄워야 함

## 작업

- `tower-http`의 `ServeDir`로 `firecrab-frontend/dist/`를 API가 서빙
  - SPA fallback(알 수 없는 경로 → `index.html`), `/api`·`/ws`는 기존 라우터 우선
  - 자산 경로는 설정으로(기본값은 설치 디렉터리 기준)
- `install.sh`가 `npm ci && npm run build`를 돌려 `dist/`를 배치
  ([Host 설치](task-host-install-script.md)와 연결)
- dev 흐름은 그대로 유지 — dist가 없으면 서빙을 끄고 안내만(개발자는 계속 `npm run dev` 사용)
- 같은 origin에서 서빙되므로 CORS 허용 origin이 필요 없어짐 — 설정 문서 갱신

## 구현 (2026-07-29)

- `HttpConfig.static_root` — `FIRECRAB_STATIC_ROOT`로 지정. **`index.html`이 있을 때만** 켜짐
  (반쯤 빌드된 `dist/`가 라우터 fallback을 가로채지 않게, 없으면 경고만 남기고 API만 서빙)
- `ServeDir(root).fallback(ServeFile(index.html))`을 라우터 fallback으로 —
  실제 파일이 있으면 그 파일, 없으면 `index.html`(브라우저에서 `/vms/<id>` 새로고침이 404가 되면 안 됨)
- `/api/{*rest}`·`/ws/{*rest}` catch-all을 먼저 둬서 **오타 난 API 경로는 계속 JSON 404** —
  HTML이 돌아오면 클라이언트가 파싱에서 죽는다
- dev 흐름 무변경: `FIRECRAB_STATIC_ROOT`를 안 주면 예전과 똑같이 동작

## 완료 기준

- 운영에서 **데몬 2개**(net-helper, api)만으로 브라우저 대시보드가 동작
- 터미널 접속·WebSocket 콘솔이 같은 포트에서 그대로 동작
- `npm run dev` 개발 흐름에 회귀 없음

> [!note] 실제 확인 (2026-07-29)
> 빌드된 `dist/`를 가리켜 API를 띄우고 확인:
> `/` → 200 text/html, `/assets/*.js` → 200 text/javascript,
> `/vms/abc` → 200(SPA fallback), `/api/vms` → 200,
> `/api/nope` → JSON 404, `/ws/vms/<id>/console` → WebSocket 핸들러 도달(catch-all에 안 가려짐).

## 참고

- 패키지에 자산을 넣는 것과 업그레이드 시 교체는 [5주차](weeks/week5-tasks.md)의
  [패키징·systemd·upgrade](task-packaging-systemd-upgrades.md)
