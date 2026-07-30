---
tags:
  - firecrab
  - host
  - systemd
status: 완료
caveat: 실 호스트 기동 미검증
scope: 4주차
updated: 2026-07-29
---

# Host 데몬 — systemd 유닛 구성

> [!summary] 한 줄 요약
> 지금은 터미널 3개(net-helper / API / 프론트 dev 서버)를 손으로 띄운다.
> 부팅 시 자동으로 뜨고, 죽으면 살아나고, 순서가 보장되게 만든다.

## 왜

- 지금 실행 방법 — 터미널 3개
  ([browser-test](../20-guides/browser-test.md), [tests/micro-network](../40-tests/micro-network.md))
  1. `sudo … firecrab-net-helper` (root, 소켓 그룹 지정 필요)
  2. `cargo run -p firecrab-api` (저장소 루트에서)
  3. `npm run dev` (Vite dev 서버 — 8080에서 `/api`·`/ws`를 3000으로 프록시)
- host를 재부팅하면 아무것도 안 뜸 — 네트워크 재적용 로직이 있어도 데몬이 없으면 무의미
- helper가 먼저 떠 있어야 API의 시작 시 재적용이 성공함(지금은 순서가 사람 손에 달림)

## 작업

- `firecrab-net-helper.service` — root, 필요한 capability만(`CAP_NET_ADMIN`),
  `/run/firecrab` 소켓 디렉터리 준비, 소켓 그룹을 API 계정으로
- `firecrab-api.service` — 비특권 계정, `After=firecrab-net-helper.service`,
  작업 디렉터리를 데이터 루트로 고정(cwd에 따라 다른 DB를 여는 사고 방지)
- 재시작 정책(`Restart=on-failure`), 로그는 journald로
- 샌드박싱: `ProtectSystem`, `PrivateTmp`, `NoNewPrivileges`(helper는 필요한 만큼만 완화)
- `firecrab-api.service`가 `dnsmasq`·`nftables`와 충돌하지 않는지 확인
- 3번(dev 서버)은 유닛으로 만들지 않는다 — [프론트엔드 서빙](task-host-frontend-serving.md)에서
  API가 정적 자산을 직접 서빙하게 바꿔 데몬 2개로 끝낸다

## 구현 (2026-07-29)

`packaging/systemd/`에 템플릿 2개. `install.sh`가 `@PLACEHOLDER@`를 실제 경로/계정으로 치환해 설치.

- `firecrab-net-helper.service` — `User=root` + `Group=firecrab`.
  **그룹이 핵심**: helper가 소켓을 0660으로 만들고 그룹은 프로세스에서 가져가므로,
  이걸 빠뜨리면 API가 소켓에 못 붙는다(이번 주에 실제로 겪은 실패)
  - `RuntimeDirectory=firecrab`(0750)로 `/run/firecrab` 생성·정리
  - `Before=firecrab-api.service` — API의 시작 시 재적용이 helper를 찾을 수 있도록
- `firecrab-api.service` — 비특권 `firecrab` 계정, `WorkingDirectory=/var/lib/firecrab`
  - `FIRECRAB_IMAGE_ROOT`, `FIRECRAB_STATIC_ROOT` 지정, `EnvironmentFile=-/etc/firecrab/api.env`
- 샌드박싱은 `ProtectSystem=full`까지만 — `strict`는 `/var`를 잠가 dnsmasq의 lease 파일을 깨뜨린다
- `CapabilityBoundingSet`으로 uid 0을 유지한 채 쓰지 않는 권한을 전부 회수:
  `NET_ADMIN` `NET_RAW` `NET_BIND_SERVICE` `SETUID` `SETGID` `KILL` `CHOWN`
  - 하나가 빠지면 **기동은 되고 특정 동작만 런타임에 깨진다** — 그래서 CI가 실제로 확인한다
  - `KILL`: dnsmasq가 비특권 사용자로 내려간 뒤 시그널을 보내야 함(커널은 uid가 아니라
    capability를 본다)
  - `CHOWN`: dnsmasq가 pid 파일 소유자를 그 사용자로 넘긴다. **CI가 이걸 잡아냈다** —
    없으면 매 기동마다 `chown of PID file ... Operation not permitted`

## 완료 기준

- 재부팅 후 사람 개입 없이 두 데몬(net-helper, api)이 뜨고, 기존 MicroNetwork bridge가 복구됨
- 대시보드 접속에 별도 프로세스가 필요 없음(프론트엔드 서빙 태스크와 함께)
- API를 강제 종료해도 자동으로 살아나고, 소켓 권한 문제로 helper 연결이 끊기지 않음
- `systemctl status`/`journalctl -u`로 상태와 로그를 볼 수 있음

> [!warning] cwd 함정
> API는 DB·VM 아티팩트 경로를 cwd 기준으로 잡는다.
> 유닛의 `WorkingDirectory`를 고정하지 않으면 빈 DB를 여는 사고가 그대로 재현된다.
