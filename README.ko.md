<p align="center">
  <a href="https://www.rust-lang.org"><img alt="Rust" src="https://img.shields.io/badge/rust-1.94%2B-orange?logo=rust&logoColor=white"></a>
  <a href="https://codecov.io/gh/SteelCrab/firecrab"><img alt="Codecov" src="https://codecov.io/gh/SteelCrab/firecrab/branch/main/graph/badge.svg"></a>
  <a href="https://www.linux.org"><img alt="Linux" src="https://img.shields.io/badge/platform-linux-blue?logo=linux&logoColor=white"></a>
  <a href="./LICENSE"><img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue"></a>
</p>

<h1 align="center">firecrab</h1>

<p align="center">내 서버에서 바로 쓰는 경량 microVM 플랫폼</p>

<p align="center">
  <a href="./README.md">English</a> ·
  <a href="./README.zh.md">中文</a> ·
  <a href="./README.ja.md">日本語</a>
</p>

firecrab은 사용자가 관리하는 Linux 호스트에서 격리된
[Firecracker](https://firecracker-microvm.github.io/) microVM을 실행·관리합니다.
Rust API, 브라우저 대시보드, 두 개의 작은 시스템 서비스가 결합되어 VM을 만들 때 이미지,
네트워크, 디스크 위치, 외부 통신 정책을 함께 선택할 수 있습니다.

강한 격리가 필요하지만 완전한 클라우드 컨트롤 플레인까지는 필요 없는 사설 단일 호스트
microVM 환경을 위한 도구입니다. 호스팅 서비스나 멀티 호스트 스케줄러는 아닙니다.

## 핵심 기능

- **MicroVM 실행:** VM 생성·조회·비활성 VM 수정·시작·중지·삭제와 VM별 브라우저 시리얼
  콘솔을 제공합니다.
- **격리 네트워크 선택:** 명시적 **MicroNetwork**를 만들고 VM을 하나에 배치합니다. IPv4,
  MAC, hostname은 유지되며 네트워크끼리는 격리됩니다. VM별 인터넷 허용 또는 격리 egress를
  선택할 수 있습니다.
- **이미지·디스크 관리:** 템플릿 설치·삭제, 임시 builder VM에서 지원 배포판 부트스트랩,
  설정된 저장소 루트 또는 **MicroStorage** 풀에 VM 디스크 배치를 지원합니다.
- **상태 확인:** 대시보드에서 시작 진행 상황, 콘솔 로그, 호스트 상태를 확인합니다. 대시보드는
  영어와 한국어를 지원합니다.
- **작은 권한 범위:** API는 비특권으로 실행하며, 별도 `firecrab-net-helper`가 호스트
  네트워크에 필요한 capability만 가집니다.

## 구조

```text
브라우저 대시보드 / REST 클라이언트
              │ HTTP + WebSocket
              ▼
  firecrab-api (Rust, SQLite, Firecracker 프로세스 관리자)
       │                         │
       │ Unix socket             └── Firecracker → VM마다 하나의 프로세스
       ▼
firecrab-net-helper (특권, capability 제한)
       └── bridge · TAP · nftables · dnsmasq
```

API는 템플릿 아티팩트를 검증한 뒤 사용하며, 설치 환경에서는 빌드된 대시보드도 직접
서빙합니다. 자세한 내용은 [아키텍처](docs/10-overview/architecture.md)를 참고하세요.

## Linux 호스트에 설치

`/dev/kvm`, 네트워크, `sudo` 권한이 있는 일반 사용자 계정이 필요합니다. 설치기는 **`sudo`를
앞에 붙이지 않고** 일반 사용자로 실행하세요. 소스 빌드와 사용자 도구는 호출한 사용자의 소유로
유지하고, 패키지·systemd·호스트 설정처럼 필요한 개별 단계에서만 내부적으로 `sudo`를 사용합니다.

```sh
git clone https://github.com/SteelCrab/firecrab.git
cd firecrab
./install.sh
```

자주 쓰는 설치기 옵션:

```sh
./install.sh --check                 # 필요 조건과 예정 변경 사항 확인
./install.sh --doctor                # KVM, 방화벽, 소켓, 호스트 설정 진단
./install.sh --with-ubuntu-image
./install.sh --with-rocky-image
./install.sh --uninstall         # 기본적으로 데이터 유지
./install.sh --uninstall --purge # /var/lib/firecrab도 제거
```

기본 설치는 대시보드와 Alpine 게스트 이미지를 빌드합니다. `/dev/kvm`이 없다면 스크립트가
대신 활성화할 수 없으므로 하드웨어 가상화(또는 중첩 가상화)를 먼저 켜야 합니다. 모든 옵션,
설치 경로, 업그레이드, 문제 해결은 [설치 가이드](docs/20-guides/install.md)에 있습니다.

## 빠른 시작

설치 후 `http://127.0.0.1:3000/`을 열고 다음 순서로 진행하세요.

1. **MicroNetwork**를 만듭니다.
2. 설치된 이미지를 고르고, 그 네트워크에 VM을 생성합니다.
3. VM을 시작해 `running`이 되면 **Terminal**을 엽니다.

네트워크를 먼저 만드는 것은 의도된 흐름입니다. firecrab에는 숨은 기본 서브넷이 없으므로,
모든 VM은 운영자가 선택한 네트워크에 배치됩니다.

## 대시보드 화면 안내

![firecrab M2 대시보드 데모](assets/dashboard/firecrab-m2.gif)

대시보드는 왼쪽 메뉴의 **MicroVM**, VM별 **터미널**, **네트워크**, **이미지** 화면으로
일상적인 운영 흐름을 나눕니다.

### MicroVM

상단 폼에서 이름, 이미지, CPU, RAM, 디스크, 저장소 위치, MicroNetwork, 외부 통신 정책을 고른 뒤
VM을 생성합니다. 아래 목록은 상태, 이미지, 리소스, ID를 3초마다 갱신하며, 실행 중인 VM에는
**터미널**과 **중지** 작업을 제공합니다. VM 이름을 누르면 시작 과정, 로그, 네트워크와 저장소를
포함한 상세 정보를 확인할 수 있습니다.

![MicroVM 생성과 목록 화면](assets/dashboard/microvm.png)

### 터미널

실행 중인 VM의 **터미널**은 별도 탭에서 열리는 브라우저 시리얼 콘솔입니다. 부팅 로그와 로그인
프롬프트를 실시간으로 확인하고 명령을 입력할 수 있습니다. 상단 도구 모음에서 표시 설정을 바꾸고,
콘솔 로그를 복사·저장하거나 **터미널만** 보기로 전환할 수 있으며, 아래 패널에는 VM 일반 정보,
사양, 네트워크, 저장소가 표시됩니다.

![VM 브라우저 시리얼 터미널](assets/dashboard/terminal.png)

### 네트워크

**MicroNetwork**를 이름, 서브넷 CIDR, 인터넷 정책으로 생성합니다. 목록에서 각 네트워크의
게이트웨이, 인터넷 연결 상태, ID를 확인하고, **인터넷 차단/연결**로 해당 네트워크 전체의 NAT
외부 통신을 바꾸거나 삭제할 수 있습니다. 행을 선택하면 서브넷 주소 사용량, bridge/TAP, NAT,
방화벽과 소속 VM을 자세히 봅니다.

![MicroNetwork 생성과 목록 화면](assets/dashboard/networks.png)

### 이미지

**M2Image** 목록은 이미지별 크기와 `패키지 준비됨`·`설치됨` 같은 상태를 표시합니다. 행을 선택하면
별칭, 버전, 최소 디스크, rootfs 크기, 상태와 해당 이미지를 사용하는 VM을 확인할 수 있습니다.
오른쪽 `…` 메뉴에서는 상태에 따라 패키지 설치, 이미지 가져오기, 부트스트랩 또는 삭제 작업을
수행합니다. VM 생성에는 설치된 이미지만 사용할 수 있습니다.

![M2Image 목록 화면](assets/dashboard/images.png)

요청 형식, 생명주기 의미, 오류 envelope는 [API 가이드](docs/20-guides/api.md)를, 이미지 패키지와
브라우저 부트스트랩은 [이미지 가이드](docs/20-guides/m2image-builder.md)를 참고하세요.

## 소스에서 개발

network helper, API, Vite 대시보드를 위해 세 개의 터미널을 사용합니다. 로컬 데이터
경로가 작업 디렉터리 기준이므로 API는 반드시 저장소 루트에서 실행하세요.

```sh
# 터미널 1 — 특권 네트워크 작업
cargo build -p firecrab-net-helper
sudo -u root -g "$(id -gn)" FIRECRAB_NET_HELPER_ALLOWED_UID="$(id -u)" \
  ./target/debug/firecrab-net-helper

# 터미널 2 — API와 Firecracker 관리자
cargo run -p firecrab-api

# 터미널 3 — 대시보드: http://localhost:8080/
npm install --prefix firecrab-frontend
npm run dev --prefix firecrab-frontend
```

로컬에서 설치 환경과 비슷하게 실행하려면 대시보드를 빌드한 뒤 API가 직접 서빙하게 합니다.

```sh
npm run build --prefix firecrab-frontend
FIRECRAB_STATIC_ROOT="$PWD/firecrab-frontend/dist" cargo run -p firecrab-api
# http://127.0.0.1:3000/
```

Rust 테스트는 다음처럼 실행합니다.

```sh
cargo test --workspace
```

더 많은 개발 노트와 브라우저 워크플로는 [웹 대시보드 가이드](docs/20-guides/web.md)에 있습니다.

## 문서

한국어 문서 볼트 [`docs/`](docs/HOME.md)에는 아키텍처, 가이드, API 계약, 검증 절차, 버그
기록이 있으며 Obsidian 볼트로 바로 열 수 있습니다.

## 라이선스

[Apache License, Version 2.0](./LICENSE)로 배포됩니다.
