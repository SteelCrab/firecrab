<p align="center">
  <a href="https://www.rust-lang.org"><img alt="Rust" src="https://img.shields.io/badge/rust-1.96%2B-orange?logo=rust&logoColor=white"></a>
  <a href="https://codecov.io/gh/SteelCrab/firecrab"><img alt="Codecov" src="https://codecov.io/gh/SteelCrab/firecrab/branch/main/graph/badge.svg"></a>
  <a href="https://www.linux.org"><img alt="Linux" src="https://img.shields.io/badge/platform-linux-blue?logo=linux&logoColor=white"></a>
  <a href="./LICENSE"><img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue"></a>
  <a href="./CHANGELOG.md"><img alt="Changelog" src="https://img.shields.io/badge/changelog-0.2.0-informational"></a>
</p>

```text
███████ ██ ██████  ███████  ██████ ██████   █████  ██████
██      ██ ██   ██ ██      ██      ██   ██ ██   ██ ██   ██
█████   ██ ██████  █████   ██      ██████  ███████ ██████
██      ██ ██   ██ ██      ██      ██   ██ ██   ██ ██   ██
██      ██ ██   ██ ███████  ██████ ██   ██ ██   ██ ██████
```

<p align="center">내 서버에서 바로 쓰는 경량 microVM 플랫폼</p>

<p align="center">
  <a href="./README.md">English</a> ·
  <a href="./README.zh.md">中文</a> ·
  <a href="./README.ja.md">日本語</a>
</p>

**firecrab은 사용자가 관리하는 Linux 호스트 한 대에서
[Firecracker](https://firecracker-microvm.github.io/) microVM을 실행합니다.**
VM을 만들 때 이미지, 네트워크, 디스크 위치, 외부 통신 정책을 함께 선택하며,
브라우저 대시보드·CLI·REST 중 무엇으로도 조작할 수 있습니다.

컨테이너보다 강한 격리가 필요하지만 완전한 클라우드 컨트롤 플레인까지는 필요 없는
사설 단일 호스트 환경을 위한 도구입니다. 호스팅 서비스도, 멀티 호스트 스케줄러도 아닙니다.

![firecrab M2 대시보드 데모](assets/dashboard/firecrab-m2.gif)

## 설치

`/dev/kvm`, 네트워크, `sudo` 권한이 있는 일반 사용자 계정이 필요합니다. 설치기는 **`sudo`를
앞에 붙이지 않고** 일반 사용자로 실행하세요. 릴리스 바이너리를 받고, 패키지·systemd·호스트
설정처럼 꼭 필요한 개별 단계에서만 내부적으로 `sudo`를 사용합니다.

```sh
curl -fsSL https://github.com/SteelCrab/firecrab/releases/latest/download/install.sh | bash
```

```sh
./install.sh --check              # 필요 조건과 예정 변경 사항 확인
./install.sh --doctor             # KVM, 방화벽, 소켓, 호스트 설정 진단
./install.sh --libc musl          # gnu/musl 자동 감지 대신 직접 지정
./install.sh --uninstall          # 기본적으로 데이터 유지
./install.sh --uninstall --purge  # /var/lib/firecrab도 제거
```

KVM은 스크립트가 대신 켤 수 없습니다. `/dev/kvm`이 없다면 하드웨어 가상화(또는 중첩
가상화)를 먼저 활성화하세요. 모든 옵션·설치 경로·문제 해결은
[설치 가이드](public-docs/installation.md)에 있습니다.

## 빠른 시작

설치 후 `http://127.0.0.1:5523/`을 열고 다음 순서로 진행하세요.

1. **MicroNetwork**를 만듭니다.
2. 설치된 이미지를 고르고, 그 네트워크에 VM을 생성합니다.
3. VM을 시작해 `running`이 되면 **터미널**을 엽니다.

네트워크를 먼저 만드는 것은 의도된 흐름입니다. firecrab에는 숨은 기본 서브넷이 없으므로,
모든 VM은 운영자가 선택한 네트워크에 배치됩니다.

## 제공 기능

- **microVM 수명주기** — 생성·조회·비활성 VM 수정·시작·중지·삭제, VM별 브라우저 시리얼 콘솔.
- **격리 네트워크** — 명시적 **MicroNetwork**에 VM을 배치하고 IPv4·MAC·hostname을 유지합니다.
  네트워크끼리는 격리되며, VM별로 인터넷 허용 또는 격리 egress를 고릅니다.
- **이미지와 디스크** — M2Image 템플릿 설치, 레지스트리에서 OCI 이미지 import, 임시 builder
  VM에서 배포판 부트스트랩, 설정된 저장소 루트 또는 **MicroStorage** 풀에 VM 디스크 배치.
- **관측** — 시작 진행 상황, 콘솔 로그, 호스트 상태를 대시보드에서 확인합니다(영어·한국어).
- **작은 권한 범위** — API는 비특권으로 실행하고, 호스트 네트워크에 필요한 capability는
  별도 `firecrab-net-helper`만 가집니다.

## 구조

호스트는 하나, API는 비특권, helper만 네트워크 capability를 가집니다. 실행 중인 게스트마다
Firecracker 프로세스 하나이며, 멀티 호스트 스케줄러는 없습니다.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/architecture/firecrab-architecture-dark.svg">
  <img alt="firecrab 5계층 아키텍처: 외부 클라이언트와 이미지 레지스트리, 비특권 firecrab-api 컨트롤 계층, capability가 제한된 firecrab-net-helper 네트워크 계층, Firecracker 런타임 계층, MicroStorage 스토리지 계층" src="assets/architecture/firecrab-architecture-light.svg">
</picture>

| 계층 | 구성 | 역할 |
| --- | --- | --- |
| External | `firecrab-frontend` | VM, 네트워크, 이미지, 스토리지, 콘솔 UI |
| External | `firecrab-cli` | 같은 작업을 터미널에서 수행 |
| Control | `firecrab-api` | REST, WebSocket, 수명주기, SQLite, 아티팩트 검증 |
| Network | `firecrab-net-helper` | 브리지, TAP, DHCP, DNS, NAT, 방화벽, 포트 포워드 |
| Runtime | Firecracker | 실행 중 게스트마다 프로세스 하나 |
| Storage | MicroStorage | 커널, rootfs 이미지, VM 디스크, SQLite 상태 |

MicroNetwork는 브리지 하나와 IPv4 서브넷 하나입니다. 같은 네트워크의 VM은 그 브리지에서
통신하고, 다른 네트워크끼리는 막습니다. 인터넷 NAT는 네트워크 `internetEnabled`와 VM
`egressPolicy`가 둘 다 허용해야 동작합니다.

OCI 이미지는 그 자체로 부팅 가능한 OS가 아닙니다. firecrab이 레지스트리 트리를 Firecracker
rootfs로 만들고, busybox를 PID 1로 부팅한 뒤 이미지 엔트리포인트를 서비스로 실행합니다.
따라서 `/proc/1/exe`는 이미지의 `init`이 아니라 `/etc/firecrab/busybox`입니다.

자세한 내용: [아키텍처](public-docs/architecture.md) ·
[MicroNetwork](public-docs/micro-network.md) · [OCI 이미지](public-docs/oci.md) ·
[API](public-docs/api.md).

## 다른 플랫폼과 비교

firecrab은 `firecracker`를 직접 띄우는 것과 OpenStack을 구축하는 것 사이를 노립니다.
서버 한 대, 웹 대시보드, 그리고 이미지(**M2Image**)·네트워크(**MicroNetwork**)·
디스크(**MicroStorage**)를 위한 이름 붙은 기본 단위를 제공합니다. 클러스터와 HA를 포기하는
대신, 하루면 전체를 파악할 수 있는 컨트롤 플레인을 택했습니다.

<details>
<summary>전체 비교표</summary>

| 구분 | **Firecrab** | VMware / ESXi | KVM + libvirt | OpenStack | Firecracker 단독 |
| --- | --- | --- | --- | --- | --- |
| 기본 단위 | **microVM** | 일반 VM | 일반 VM | 일반 VM | microVM |
| 가상화 기반 | Firecracker + KVM | VMware Hypervisor | KVM/QEMU | 주로 KVM/QEMU | KVM |
| 주요 목표 | **한 서버에서 간단한 microVM 운영** | 기업 가상화 | 범용 Linux 가상화 | 대규모 Private Cloud | microVM 실행 |
| 관리 난이도 | **낮게 지향** | 중간 | 중간~높음 | **매우 높음** | 높음 |
| 웹 대시보드 | ✅ | ✅ | 별도 구축 | ✅ | ❌ |
| VM 이미지 관리 | **M2Image** | Template/Image | qcow2 등 | Glance | 직접 관리 |
| 가상 네트워크 | **MicroNetwork** | vSwitch | bridge/libvirt network | Neutron | 직접 구현 |
| 디스크 관리 | **MicroStorage** | Datastore/VMDK | qcow2/LVM 등 | Cinder | 직접 구현 |
| 브라우저 콘솔 | ✅ | ✅ | 구성 필요 | ✅ | ❌ |
| VM 격리 | **강함** | 강함 | 강함 | 강함 | **강함** |
| 부팅 속도 | **매우 빠름** | 상대적으로 느림 | 상대적으로 느림 | 상대적으로 느림 | **매우 빠름** |
| 리소스 오버헤드 | **낮음** | 높음 | 중간 | 높음 | **매우 낮음** |
| Control Plane | **최소화** | 있음 | 거의 없음 | **대규모** | 없음 |
| 단일 서버 운영 | **핵심 목표** | 가능 | 가능 | 비효율적 | 가능 |
| 클러스터/HA | 제한적 / 확장 영역 | ✅ | 별도 구성 | ✅ | ❌ |
| Kubernetes 연계 | 향후 Runtime 가능 | 가능 | 가능 | 가능 | containerd 연계 가능 |
| 적합한 환경 | **개인 서버, 홈랩, Edge, 개발 서버** | 기업 데이터센터 | Linux 서버 | 대규모 클라우드 | 서버리스/컨테이너 인프라 |

</details>

## 대시보드

왼쪽 메뉴가 일상 운영을 **MicroVM**, VM별 **터미널**, **네트워크**, **이미지**로 나눕니다.

### MicroVM

이름, 이미지, CPU, RAM, 디스크, 저장소 위치, MicroNetwork, 외부 통신 정책을 고른 뒤 VM을
생성합니다. 목록은 상태·이미지·리소스·ID를 3초마다 갱신하며, 실행 중인 VM에는 **터미널**과
**중지**를 제공합니다. VM 이름을 누르면 시작 과정, 로그, 네트워크, 저장소를 볼 수 있습니다.

![MicroVM 생성과 목록 화면](assets/dashboard/microvm.png)

### 터미널

**터미널**은 실행 중인 VM의 시리얼 콘솔을 별도 탭에서 열어 부팅 로그와 로그인 프롬프트를
실시간으로 보여 줍니다. 도구 모음에서 표시 설정 변경, 콘솔 로그 복사·저장, 터미널 전용 보기
전환을 할 수 있습니다.

![VM 브라우저 시리얼 터미널](assets/dashboard/terminal.png)

### 네트워크

**MicroNetwork**를 이름, 서브넷 CIDR, 인터넷 정책으로 생성합니다. **인터넷 차단/연결**은 해당
네트워크 전체의 NAT 외부 통신을 바꿉니다. 행을 선택하면 서브넷 주소 사용량, bridge/TAP, NAT,
방화벽, 소속 VM을 확인할 수 있습니다.

![MicroNetwork 생성과 목록 화면](assets/dashboard/networks.png)

### 이미지

**M2Image** 목록은 이미지별 크기와 `패키지 준비됨`·`설치됨` 같은 상태를 표시합니다. 오른쪽
`…` 메뉴에서 상태에 맞는 패키지 설치·부트스트랩·삭제를 수행합니다. VM 생성에는 설치된
이미지만 사용할 수 있습니다.

같은 화면에서 OCI 레퍼런스(`nginx:1.27`)가 이 호스트 아키텍처와 맞는지 검사한 뒤 템플릿으로
import합니다. import는 백그라운드 작업이며 진행 상황·오류·등록된 별칭을 보여 줍니다.

![M2Image 목록 화면](assets/dashboard/images.png)

[이미지 가이드](public-docs/images.md), [OCI 이미지 가이드](public-docs/oci.md),
[API 가이드](public-docs/api.md)를 참고하세요.

## 소스에서 개발

network helper, API, Vite 대시보드를 위해 세 개의 터미널을 사용합니다. 로컬 데이터 경로가
작업 디렉터리 기준이므로 API는 반드시 저장소 루트에서 실행하세요.

```sh
# 터미널 1 — 특권 네트워크 작업
cargo build -p firecrab-net-helper
sudo -u root -g "$(id -gn)" FIRECRAB_NET_HELPER_ALLOWED_UID="$(id -u)" \
  ./target/debug/firecrab-net-helper

# 터미널 2 — API와 Firecracker 관리자
pkill -x firecrab-api 2>/dev/null || true
cargo run -p firecrab-api

# 터미널 3 — 대시보드: http://localhost:8080/
pkill -f '[f]irecrab-frontend/node_modules/.bin/vite' 2>/dev/null || true
npm install --prefix firecrab-frontend
npm run dev --prefix firecrab-frontend
```

로컬에서 설치 환경과 비슷하게 실행하려면 대시보드를 빌드한 뒤 API가 직접 서빙하게 합니다.

```sh
npm run build --prefix firecrab-frontend
FIRECRAB_STATIC_ROOT="$PWD/firecrab-frontend/dist" cargo run -p firecrab-api
# http://127.0.0.1:5523/
```

테스트:

```sh
cargo test --workspace

# OCI inspect → import 브라우저 E2E (로컬 레지스트리 fixture, Docker Hub 없음)
npm install --prefix firecrab-e2e
npm run install-browsers --prefix firecrab-e2e
FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm test --prefix firecrab-e2e
```

E2E 기대 결과는 1 passed, 1 skipped입니다. skip된 테스트는 VM을 만들어 부팅하므로, 플래그
없이 실행하려면 KVM·Firecracker와 `./scripts/dev-net-helper.sh`가 필요합니다.
[firecrab-e2e/README.md](firecrab-e2e/README.md)와
[웹 대시보드 가이드](public-docs/dashboard.md)를 참고하세요.

## 문서

영문 기술 문서 [`public-docs/`](public-docs/README.md)에 아키텍처, 설치, 운영, API 계약,
문제 해결이 정리되어 있습니다.

## 기여

<p align="center">
  <a href="./CONTRIBUTING.md#a-note-from-the-maintainer">
    <img src="assets/icons/contributors.png" alt="Contributors" width="96" />
  </a>
</p>

[유지자 노트](./CONTRIBUTING.md#a-note-from-the-maintainer)와 함께, 개발 환경·검사 항목·PR 기대
사항·문서 규칙은 [CONTRIBUTING.md](./CONTRIBUTING.md)를 보세요.

## 라이선스

[Apache License, Version 2.0](./LICENSE)로 배포됩니다.
