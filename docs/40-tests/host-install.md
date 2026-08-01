---
tags:
  - firecrab
  - test
  - host
updated: 2026-08-01
---

# Host 설치 테스트

`install.sh`와 systemd 유닛, API의 대시보드 서빙을 확인한다
([task-host-install-script](../30-tasks/task-host-install-script.md),
[task-host-systemd-daemons](../30-tasks/task-host-systemd-daemons.md),
[task-host-frontend-serving](../30-tasks/task-host-frontend-serving.md)).

사용법(옵션·경로·운영·제거)은 [install.md](../20-guides/install.md).

> [!warning] 개발 머신에서 전체 설치를 돌리지 말 것
> 시스템 계정과 유닛이 생겨서 지금의 수동 실행(터미널 3개)과 충돌한다.
> 전체 설치는 VM이나 컨테이너 같은 **버려도 되는 호스트**에서 한다.

> [!note] CI가 대신 돌린다
> `.github/workflows/ci.yml`의 `install` job이 일회용 러너 VM에서 전체 설치를 실행한다 —
> shellcheck(`install.sh` + `firecrab-doctor.sh`), `--check`/`--doctor` 무변경, 설치,
> 데몬 2개 기동, 설치 후 doctor(이미지 FAIL만 허용, `--no-images`), capability 런타임 확인,
> 대시보드 200, 재실행 멱등성, `--uninstall`/`--purge`까지. 게스트 이미지 빌드(=VM 부팅)만 범위 밖.

## 자동 테스트 (root 불필요)

```sh
cargo test -p firecrab-api server::                    # 6
cargo test -p firecrab-api templates::                 # 9
bash -n install.sh scripts/firecrab-doctor.sh          # 문법
./install.sh --help
./install.sh --check                                   # 비특권 + 무변경이어야 함
./install.sh --doctor                                  # 비특권 + 무변경 (FAIL 있어도 exit 0/1)
```

전체: `cargo test --workspace` → 216/20/16/56 (합계 308)

확인 항목:

- `FIRECRAB_STATIC_ROOT`가 없거나 `index.html`이 없으면 정적 서빙이 꺼지고 모든 경로가 JSON 404
- 지정돼 있으면 `/`와 자산은 파일로, 없는 경로는 `index.html`(SPA fallback)
- 그 상태에서도 `/api/*`·`/ws/*`의 없는 경로는 **JSON 404**(HTML 아님)
- 이미지가 없는 image root로도 API가 뜨고, 파일이 갖춰진 템플릿만 resolve됨
  (`load_from_skips_templates_whose_images_are_not_built_yet`)

`--check`가 정말 아무것도 바꾸지 않는지도 같이 본다:

```sh
BEFORE=$(ls -la /var/lib/firecrab 2>&1); ./install.sh --check >/dev/null
[ "$BEFORE" = "$(ls -la /var/lib/firecrab 2>&1)" ] && echo "무변경 OK"
```

## 정적 서빙 확인 (root 불필요)

```sh
cd firecrab-frontend && npm run build && cd ..
FIRECRAB_STATIC_ROOT="$PWD/firecrab-frontend/dist" FIRECRAB_ALLOWED_ORIGINS="" \
  cargo run -p firecrab-api

# 다른 터미널에서
curl -s -o /dev/null -w '%{http_code} %{content_type}\n' localhost:3000/          # 200 text/html
curl -s -o /dev/null -w '%{http_code} %{content_type}\n' localhost:3000/vms/abc   # 200 text/html
curl -s -o /dev/null -w '%{http_code}\n'                localhost:3000/api/vms    # 200
curl -s localhost:3000/api/nope                                                   # JSON 404
```

브라우저에서 `http://localhost:3000/` — **Vite dev 서버 없이** 대시보드가 뜨고,
VM 목록·생성·터미널이 그대로 동작해야 한다.

## 유닛 파일 확인 (root 불필요)

```sh
# 설치와 같은 방식으로 치환해 문법만 본다
sed -e 's|@LIBDIR@|/usr/local/lib/firecrab|g' -e 's|@SHAREDIR@|/usr/local/share/firecrab|g' \
    -e 's|@DATADIR@|/var/lib/firecrab|g' -e 's|@CONFDIR@|/etc/firecrab|g' \
    -e 's|@FIRECRAB_USER@|firecrab|g' -e 's|@FIRECRAB_GROUP@|firecrab|g' -e 's|@FIRECRAB_UID@|998|g' \
    packaging/systemd/firecrab-api.service > /tmp/firecrab-api.service
grep -c '@[A-Z_]*@' /tmp/firecrab-api.service     # 0 이어야 함
systemd-analyze verify /tmp/firecrab-api.service  # 바이너리 없음 경고만 나오면 정상
```

## 전체 설치 (버려도 되는 호스트에서, root 필요)

네트워크만 되는 머신이면 된다 — 나머지는 스크립트가 채운다.

```sh
git clone <repo> && cd firecrab
sudo ./install.sh          # 없는 것(nft/dnsmasq/firecracker/rust/node/docker+이미지)을 알아서 설치
```

먼저 무엇이 설치될지 보고 싶으면:

```sh
./install.sh --check       # root 없이, 아무것도 바꾸지 않음
```

확인 항목:

```sh
systemctl status firecrab-net-helper firecrab-api    # 둘 다 active
ls -l /run/firecrab/net-helper.sock                  # srw-rw---- root firecrab
sudo -u firecrab id                                  # kvm 그룹 포함
curl -s -o /dev/null -w '%{http_code}\n' localhost:3000/       # 200 (대시보드)
curl -s localhost:3000/api/vms                                 # []
```

- 브라우저에서 대시보드 접속 → VM 생성 → start → `running` 도달
- **재부팅** 후 두 데몬이 자동으로 뜨고, MicroNetwork bridge가 복구되는지
  (`ip -br addr show type bridge`)
- `sudo ./install.sh` 재실행 → 오류 없이 같은 상태(idempotent)
- `sudo ./install.sh --uninstall` → 유닛·바이너리 사라지고 `/var/lib/firecrab`은 남음
- `sudo ./install.sh --uninstall --purge` → 데이터까지 삭제

## 완료 기준 대조

- 깨끗한 머신에서 한 번의 실행으로 데몬 2개가 뜨고 브라우저에서 VM 생성까지 — **미검증**
- 재실행해도 같은 결과(idempotent) — 미검증
- 의존성을 스스로 설치(패키지 관리자 5종, firecracker, rustup, node, docker+이미지) — **미검증**
- `--check`가 비특권으로 돌고 아무것도 바꾸지 않음 — **확인**(2026-07-30)
- 이미지가 없어도 설치가 끝나고 API가 뜸 — 자동 테스트로 확인
- 정적 서빙(dev 서버 없이 대시보드) — **확인**(2026-07-29)
- 유닛 문법·플레이스홀더 치환 — **확인**(2026-07-29)
