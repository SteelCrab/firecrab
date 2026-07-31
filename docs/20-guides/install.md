---
tags:
  - firecrab
  - guide
updated: 2026-07-31
---

# 설치 (`install.sh`)

`install.sh`는 리눅스 호스트 하나에 firecrab을 올린다. 없는 것을 찾아 설치하고, 빌드하고,
systemd 데몬 2개(`firecrab-net-helper`, `firecrab-api`)를 띄우고, 대시보드까지 서빙한다.

```sh
git clone https://github.com/SteelCrab/firecrab && cd firecrab
sudo ./install.sh
```

끝나면 `http://127.0.0.1:3000/`에서 대시보드가 뜬다. 개발용 3터미널 실행은
[web.md](web.md)를 참고한다.

## 요구사항

| 항목 | 필요 | 없으면 |
|---|---|---|
| 리눅스 + systemd | 필수 | 유닛을 설치할 수 없어 중단 |
| KVM (`/dev/kvm`) | 필수 | 설치는 계속되지만 VM이 시작되지 않음 — **스크립트가 대신 설치할 수 없는 유일한 항목** |
| 네트워크 | 필수 | 패키지·firecracker·이미지를 받지 못함 |
| root | 필수 (`--check` / `--doctor`는 예외) | `sudo ./install.sh` |
| 패키지 관리자 | apt-get / dnf / zypper / pacman / apk | 감지 실패 시 부족한 것을 안내만 하고 직접 설치해야 함 |

KVM이 없다는 건 보통 BIOS에서 가상화가 꺼져 있거나, 이 호스트 자체가 VM인데 중첩 가상화가
없는 경우다.

## 먼저 확인만 하기

```sh
./install.sh --check
```

- **root 불필요**, 파일을 하나도 바꾸지 않는다
- 무엇이 없고 무엇을 설치할지, KVM·systemd·이미지·UFW 상태까지 보고한다

설치 후(또는 네트워크/DB 이상 시) **런타임 host 진단**:

```sh
./install.sh --doctor            # 또는 ./scripts/firecrab-doctor.sh
# 설치 후: firecrab-doctor
```

- 역시 root 불필요·무변경. UFW 브리지 규칙, helper 소켓 권한, 이중 DB 경로, nft 테이블 등을
  한 번에 보고, 문제마다 조치 한 줄을 붙인다. 전부 정상이면 통과 요약만 출력.
- 자세한 검증: [tests/host-doctor](../40-tests/host-doctor.md), 태스크 [Host 진단](../30-tasks/task-host-doctor.md)

## 옵션

| 옵션 | 동작 |
|---|---|
| (없음) | 없는 것을 설치하고 빌드·배치·기동까지 |
| `--check` | 설치 readiness 보고만. root 불필요, 무변경 |
| `--doctor` | 런타임 host 진단 (`scripts/firecrab-doctor.sh`). root 불필요, 무변경 |
| `--no-deps` | 아무것도 설치하지 않고 부족한 것만 보고 (직접 준비한 호스트용) |
| `--no-images` | 게스트 이미지를 준비하지 않음 |
| `--with-ubuntu-image` | Alpine 외에 Ubuntu 이미지도 빌드 (용량 큼, root chroot 필요) |
| `--no-frontend` | 대시보드를 빌드·설치하지 않음 (API만) |
| `--uninstall` | 유닛·바이너리·대시보드 제거. **데이터는 보존** |
| `--uninstall --purge` | 데이터 디렉터리까지 삭제 (VM 디스크·DB 전부) |
| `-h`, `--help` | 도움말 |

## 환경 변수

경로와 계정 이름을 바꿀 수 있다. 지정하지 않으면 아래 기본값을 쓴다.

| 변수 | 기본값 |
|---|---|
| `FIRECRAB_USER` / `FIRECRAB_GROUP` | `firecrab` / `firecrab` |
| `PREFIX` | `/usr/local` (바이너리 `$PREFIX/lib/firecrab`, 대시보드 `$PREFIX/share/firecrab`) |
| `DATADIR` | `/var/lib/firecrab` |
| `CONFDIR` | `/etc/firecrab` |
| `UNITDIR` | `/etc/systemd/system` |

```sh
sudo DATADIR=/srv/firecrab PREFIX=/opt ./install.sh
```

## 설치가 하는 일

순서대로 진행하고, 각 단계는 이미 되어 있으면 건너뛴다.

1. **의존성** — `ip`, `nft`, `dnsmasq`, `mkfs.ext4`, `curl`을 감지된 패키지 관리자로 설치
   (`apt-get update`는 한 번만 실행)
2. **firecracker** — 없으면 [`scripts/install-firecracker.sh`](../../scripts/install-firecracker.sh)로 설치
3. **빌드 도구** — `cargo`가 없으면 rustup 비대화형 설치, 대시보드를 만들 때만 `npm`
4. **검사** — KVM(경고만), systemd(없으면 중단)
5. **빌드** — `cargo build --release --workspace`, `npm ci && npm run build`
6. **계정** — 시스템 사용자 `firecrab` 생성 + `kvm` 그룹 편입(firecracker가 `/dev/kvm`을 연다)
7. **디렉터리** — `$DATADIR/{data,images}`(0750, firecrab 소유), `$CONFDIR`(0750, root:firecrab)
8. **배치** — 바이너리, 대시보드, `$CONFDIR/api.env`(이미 있으면 보존)
9. **유닛** — [`packaging/systemd/`](../../packaging/systemd) 템플릿을 치환해 설치 후 `daemon-reload`
10. **이미지** — 저장소에 있으면 복사, 없으면 docker로 Alpine 이미지 빌드
11. **기동** — `systemctl enable --now` 후 두 유닛이 실제로 active인지 확인

이미지 단계는 실패해도 경고만 남기고 계속 간다. 이미지가 없는 템플릿은 API가 건너뛰므로
대시보드와 MicroNetwork는 그대로 동작하고, 이미지는 나중에 넣으면 된다.

## 설치 후 배치

| 경로 | 내용 |
|---|---|
| `/usr/local/lib/firecrab/` | `firecrab-api`, `firecrab-net-helper` |
| `/usr/local/bin/firecrab-doctor` | host 진단 스크립트 (`install.sh --doctor`와 동일) |
| `/usr/local/share/firecrab/dashboard/` | 빌드된 대시보드 정적 파일 |
| `/var/lib/firecrab/data/` | SQLite DB, VM 아티팩트(`vms/<id>/`) |
| `/var/lib/firecrab/images/` | 커널·rootfs 이미지 |
| `/etc/firecrab/api.env` | API 설정(재설치해도 보존) |
| `/run/firecrab/net-helper.sock` | helper 소켓 (`srw-rw---- root firecrab`) |
| `/etc/systemd/system/firecrab-*.service` | 유닛 2개 |

## 확인

```sh
systemctl status firecrab-net-helper firecrab-api   # 둘 다 active
ls -l /run/firecrab/net-helper.sock                 # 그룹이 firecrab이어야 함
sudo -u firecrab id                                 # kvm 그룹 포함
firecrab-doctor                                     # host 진단 (또는 ./install.sh --doctor)
curl -s -o /dev/null -w '%{http_code}\n' localhost:3000/   # 200 (대시보드)
curl -s localhost:3000/api/vms                             # []
curl -s localhost:3000/api/micro-networks                  # [] (신규 설치 — 기본 서브넷 없음)
```

**첫 네트워크를 만든 뒤** VM을 만든다. 암시적 `fcbr0`/기본 서브넷은 없다.

```sh
curl -s -X POST localhost:3000/api/micro-networks \
  -H 'Content-Type: application/json' \
  -d '{"name":"lab","subnetCidr":"172.30.0.0/24","internetEnabled":true}'
```

브라우저에서 `http://127.0.0.1:3000/` → MicroNetwork 생성 → VM 생성 → start → `running` 도달까지 확인한다.
자세한 절차는 [tests/host-install.md](../40-tests/host-install.md).  
M2 게스트 부팅 매트릭스(nightly): [m2-ci-boot-matrix.md](m2-ci-boot-matrix.md).

## 운영

```sh
journalctl -u firecrab-api -f          # 로그
journalctl -u firecrab-net-helper -f
systemctl restart firecrab-api         # 설정 변경 후
```

- **설정 변경**: `/etc/firecrab/api.env` 수정 → `systemctl restart firecrab-api`
  (사용 가능한 변수는 [api.md](api.md)의 환경 변수 표 참고)
- **업그레이드**: `git pull` 후 `sudo ./install.sh` 재실행 — 빌드·배치만 갱신되고
  데이터와 `api.env`는 그대로
- **이미지 추가**: `$DATADIR/images/`에 넣고 `systemctl restart firecrab-api`
  (템플릿 파일명은 `firecrab-api/src/templates.rs`의 `default_specs()` 기준)

## 제거

```sh
sudo ./install.sh --uninstall           # 유닛·바이너리·대시보드만
sudo ./install.sh --uninstall --purge   # 데이터까지 (되돌릴 수 없음)
```

- 남는 것: 서비스 계정, `$CONFDIR`, `$DATADIR` (`--purge` 없이는)
- bridge와 nftables 테이블은 helper 소유라 데몬이 멈추면 다음 재부팅에 사라진다
- 패키지(dnsmasq, firecracker 등)는 지우지 않는다 — 다른 용도로 쓰고 있을 수 있으므로

## 문제 해결

| 증상 | 원인·조치 |
|---|---|
| `--check`가 `/dev/kvm missing` | BIOS에서 가상화 활성화. 이 호스트가 VM이면 중첩 가상화 필요 |
| `firecrab-api`가 helper에 연결 실패 | `./install.sh --doctor` → helper socket. 소켓 그룹 확인(`ls -l /run/firecrab/net-helper.sock`). 유닛의 `Group=`이 API 계정과 같아야 함 |
| VM 생성 폼에 템플릿이 없음 | 이미지 미설치. `$DATADIR/images/` 확인 후 API 재시작 |
| guest가 `no-ipv4-address`로 실패 | `./install.sh --doctor` → UFW. 새 브리지 DHCP 차단 → [troubleshooting.md](troubleshooting.md) |
| VM은 뜨는데 외부 통신 불가 | `./install.sh --doctor` → UFW route. [bugs/vm-outbound-forward-blocked-by-ufw.md](../50-bugs/vm-outbound-forward-blocked-by-ufw.md) |
| `no such column` 류 DB 오류 | `./install.sh --doctor` → multiple DB. 잘못된 작업 디렉터리에서 실행. 유닛은 `WorkingDirectory`가 고정돼 있으므로 손으로 띄운 프로세스가 남아 있는지 확인 |
| arm64에서 VM이 안 뜸 | 이미지 파일명과 `console=ttyS0`가 x86_64 전용 — 아직 미지원 |

## CI 검증

`.github/workflows/ci.yml`의 `install` job이 PR마다 일회용 러너에서 실제로 설치한다.

| 단계 | 확인하는 것 |
|---|---|
| shellcheck | `install.sh` + `scripts/firecrab-doctor.sh` 인용·단어 분리 실수 |
| `--check` | 비특권 동작 + `/var/lib/firecrab`을 만들지 않음 |
| `--doctor` | 비특권·무변경, exit 0/1 |
| `sudo ./install.sh --no-images` | 계정·디렉터리·유닛·기동, `firecrab-doctor` 배치 |
| 설치 후 doctor | host 경로 정상; 이미지 FAIL만 허용(`--no-images`) |
| 소켓·그룹 확인 | `root firecrab 660`, firecrab이 kvm 그룹 |
| capability | `fcbr0` 주소, dnsmasq 생존, 67번 포트 바인딩, `operation not permitted` 없음 |
| HTTP | 대시보드 200, `/api/vms` `[]`, 같은 origin POST 통과 / 타 사이트 403 |
| 재실행 | 멱등성 + 기존 MicroNetwork 보존 |
| 제거 | `--uninstall`은 데이터 보존·doctor 제거, `--purge`는 삭제 |

게스트 이미지 빌드(=VM 부팅)는 docker와 10분 이상이 필요해 범위 밖이다.

## 관련 문서

- 설계 배경과 완료 기준: [task-host-install-script.md](../30-tasks/task-host-install-script.md),
  [task-host-systemd-daemons.md](../30-tasks/task-host-systemd-daemons.md),
  [task-host-frontend-serving.md](../30-tasks/task-host-frontend-serving.md)
- 검증 절차: [tests/host-install.md](../40-tests/host-install.md)
- API 설정·엔드포인트: [api.md](api.md)
- 개발용 실행(3터미널): [web.md](web.md)
- 특권 helper: [net-helper.md](net-helper.md)
