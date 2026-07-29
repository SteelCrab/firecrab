---
tags:
  - firecrab
  - host
  - install
status: 구현 완료 (실 호스트 설치 미검증)
scope: 4주차
updated: 2026-07-30
---

# Host 설치 — `install.sh`

> [!summary] 한 줄 요약
> **네트워크만 되는 리눅스 머신**에서 `sudo ./install.sh` 한 번으로
> 없는 것을 찾아 설치하고, 빌드하고, 데몬 2개를 띄우고, 대시보드까지 뜨게 한다.
> 별도의 부트스트랩 옵션 없이 **기본 실행**이 그 일을 한다.

## 왜

- 흩어져 있던 것들: [firecracker 설치](../scripts/install-firecracker.sh),
  [KVM 확인](../scripts/kvm-check.sh), rootfs 빌드 스크립트, net-helper 실행 방법,
  `/run/firecrab` 디렉터리와 소켓 그룹 권한
- 실제로 이번 주에 소켓 그룹을 잘못 줘서 API가 helper에 붙지 못하는 일이 있었음
  (`sudo -u root -g <group>`을 빠뜨리면 재현)
- 데모·심사 환경에서 "clone → 한 줄 → 브라우저"가 안 되면 아무것도 보여줄 수 없음

## 구현 (2026-07-30)

### 모드

| 실행 | 동작 |
|---|---|
| `sudo ./install.sh` | 없는 것을 **설치**하고 빌드·배치·기동까지 |
| `./install.sh --check` | 무엇이 없고 무엇을 설치할지 보고만. **root 불필요, 아무것도 바꾸지 않음** |
| `sudo ./install.sh --uninstall` | 유닛·바이너리·대시보드 제거 (데이터 보존) |
| `… --uninstall --purge` | 데이터 디렉터리까지 삭제 |

억제 플래그: `--no-deps`(설치는 하지 않고 부족한 것만 보고), `--no-images`, `--no-frontend`,
`--with-ubuntu-image`(큰 Ubuntu 이미지도 함께)

### 스스로 채우는 것

| 대상 | 방법 |
|---|---|
| `ip`, `nft`, `dnsmasq`, `mkfs.ext4`, `curl` | 패키지 관리자 자동 감지 — apt-get / dnf / zypper / pacman / apk |
| `firecracker` | `scripts/install-firecracker.sh` 호출 |
| Rust 툴체인 | `cargo`가 없으면 rustup 비대화형 설치 |
| Node/npm | 대시보드를 빌드할 때만 필요 |
| **게스트 이미지** | 저장소에 있으면 복사, 없으면 docker로 Alpine 이미지 빌드 |
| KVM | 설치 대상이 아님 — BIOS/중첩 가상화 문제라 안내만 |

패키지 이름은 관리자별로 매핑한다(apt는 `docker.io`, apk는 `e2fsprogs-extra` 등).
`apt-get update`는 한 번만 돈다.

### 배치

- 계정: 시스템 사용자 `firecrab` + `kvm` 그룹 편입(firecracker가 `/dev/kvm`을 연다)
- `/var/lib/firecrab/{data,images}`(0750, firecrab 소유), `/etc/firecrab`(0750, root:firecrab)
- 바이너리 → `/usr/local/lib/firecrab`, 대시보드 → `/usr/local/share/firecrab/dashboard`
- 이미지는 **복사**(심볼릭 링크 아님) — 저장소를 옮기거나 지워도 살아 있어야 한다. 이미 있으면 건너뜀
- `/etc/firecrab/api.env` 생성(0640), 이미 있으면 보존
- 유닛은 [systemd 태스크](task-host-systemd-daemons.md)의 템플릿을 `sed`로 치환해 설치
- 경로는 전부 환경 변수로 덮어쓸 수 있음 — `PREFIX`, `DATADIR`, `CONFDIR`, `UNITDIR`,
  `FIRECRAB_USER/GROUP`

> [!important] 이미지가 없어도 설치는 성공한다
> 이미지 빌드는 실패해도 경고만 남기고 계속 간다. 이를 위해
> `TemplateRegistry::load_default()`가 **파일이 없는 템플릿은 건너뛰도록** 바뀌었다
> (예전엔 아티팩트가 하나라도 없으면 API 기동 자체가 실패했다).
> 대시보드·MicroNetwork는 이미지 없이도 동작하고, 이미지는 나중에 넣으면 된다.
> 있는 파일은 여전히 전부 해시 검증한다.

## 완료 기준

- 깨끗한 머신에서 `sudo ./install.sh` 한 번으로 데몬 2개가 뜨고 브라우저에서 VM 생성까지
- 두 번 실행해도 같은 결과(오류·중복 없음)
- 설치할 수 없는 것(KVM)은 **무엇이 문제인지** 알려주고, 나머지는 알아서 채움
- `--check`는 root 없이 돌고 **아무것도 바꾸지 않음**
- `--uninstall` 후 firecrab이 만든 것만 사라지고 host의 다른 설정은 그대로

> [!warning] 검증 상태
> `--check`(비특권·무변경)·`--help`·문법(`bash -n`)·유닛 렌더링(`systemd-analyze verify`)은 확인했다.
> **깨끗한 머신에서의 전체 설치는 아직 안 돌려봤다** — 개발 머신에 시스템 계정과 유닛을 만들면
> 지금의 수동 실행 방식과 충돌하기 때문. 절차는 [tests/host-install](tests/host-install.md).

## 참고

- 패키지(.deb/.rpm)·업그레이드·롤백은 [5주차](week5-tasks.md)의
  [패키징·systemd·upgrade](task-packaging-systemd-upgrades.md) 범위 —
  이 태스크는 그 전 단계인 **소스에서의 설치**
- 진단만 따로 떼어낸 것이 [Host 진단](task-host-doctor.md) — 지금은 `--check`가 그 일부를 한다
