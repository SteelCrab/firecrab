---
tags:
  - firecrab
  - host
  - frontend
status: 미완료
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
- `firecrab-api`에는 정적 파일 서빙 코드가 아예 없음([browser-test](browser-test.md))
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

## 완료 기준

- 운영에서 **데몬 2개**(net-helper, api)만으로 브라우저 대시보드가 동작
- 터미널 접속·WebSocket 콘솔이 같은 포트에서 그대로 동작
- `npm run dev` 개발 흐름에 회귀 없음

## 참고

- 패키지에 자산을 넣는 것과 업그레이드 시 교체는 [5주차](week5-tasks.md)의
  [패키징·systemd·upgrade](task-packaging-systemd-upgrades.md)
