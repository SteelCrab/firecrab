<p align="center">
  <a href="https://www.rust-lang.org"><img alt="Rust" src="https://img.shields.io/badge/rust-1.96%2B-orange?logo=rust&logoColor=white"></a>
  <a href="https://codecov.io/gh/SteelCrab/firecrab"><img alt="Codecov" src="https://codecov.io/gh/SteelCrab/firecrab/branch/main/graph/badge.svg"></a>
  <a href="https://www.linux.org"><img alt="Linux" src="https://img.shields.io/badge/platform-linux-blue?logo=linux&logoColor=white"></a>
  <a href="./LICENSE"><img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue"></a>
  <a href="./CHANGELOG.md"><img alt="Changelog" src="https://img.shields.io/badge/changelog-0.3.1-informational"></a>
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
  <a href="./README.ko.md">한국어</a> ·
  <a href="./README.ja.md">日本語</a> ·
  <a href="./README.zh.md">中文</a> ·
  <a href="./README.id.md">Bahasa Indonesia</a>
</p>

![Firecrab 데모](assets/dashboard/firecrab-demo.gif)

## 개요

<details>
<summary>목적</summary>

내 Linux 서버 한 대에서 Firecracker microVM을 만들고 운영합니다. 브라우저 대시보드·CLI·REST API로 관리하며, 개인 서버·홈랩·개발 환경에 적합합니다.

</details>

<details>
<summary>주요 기능</summary>

- **VM 관리** — microVM 생성·시작·중지·삭제와 브라우저 시리얼 콘솔.
- **이미지와 디스크** — M2Image 템플릿, OCI 이미지 가져오기, MicroStorage 디스크 위치 지정.
- **네트워크** — MicroNetwork 서브넷과 VM별 인터넷 허용·격리 정책.
- **호스트 플랫폼** — Linux에서 직접 실행, macOS·Windows에서 microManager 사용(Windows Preview).

</details>

<details>
<summary>플랫폼 비교</summary>

| 핵심 항목 | **Firecrab** | [KVM + libvirt](https://libvirt.org/) | [OpenStack](https://docs.openstack.org/nova/latest/) |
| --- | --- | --- | --- |
| 목적 | 단일 호스트 microVM 운영 | 범용 VM 운영 | 사설 클라우드 |
| 가상화 | Firecracker + KVM | QEMU/KVM | 주로 QEMU/KVM |
| 관리 | 대시보드·CLI·REST | libvirt API·CLI, GUI 별도 | Horizon·CLI·REST |
| 구성 | 호스트 한 대 | 호스트별 관리 | 컨트롤러·컴퓨트 서비스 |

</details>

## 아키텍처

[상세 아키텍처](public-docs/architecture.md): OS별 microManager 구성, VM 시작, 이미지·커널 공급, 게스트 기능, CLI·업데이트 흐름.

### Firecrab 한눈에

![Firecrab 한눈에](assets/architecture/firecrab-at-a-glance.ko.svg)

1. 브라우저나 CLI로 요청합니다. M2Image, MicroNetwork, MicroStorage를 골라 MicroVM을 만듭니다.
2. Firecrab이 M2Image를 확인하고 MicroNetwork를 준비한 뒤, MicroStorage에 VM 전용 디스크를 만듭니다.
3. MicroVM마다 Firecracker 프로세스를 하나씩 띄웁니다. 각자 자기 커널로 부팅합니다.
4. 게스트가 네트워크 준비 완료를 알리면 MicroVM이 실행 중(running)이 됩니다.

### 어디서든 실행

![어디서든 실행](assets/architecture/firecrab-runs-anywhere.ko.svg)

Linux는 `install.sh` 한 번이면 됩니다. macOS와 Windows에서는 `firecrab service install`이 관리용 Debian VM을 만들어 같은 Firecrab을 실행하고, 결과는 똑같이 `localhost:5523` 대시보드로 열립니다.

macOS는 Apple silicon M3 이상이 필요하며 Apple M5에서 검증했습니다. Windows는 WSL2에서 아직 microVM을 시작하지 못하므로 Preview로 표시했습니다.

로고: Linux·Apple·Debian은 simple-icons(CC0), 기어 아이콘은 Lucide(ISC)에서 가져왔습니다. 모든 로고와 상표는 해당 소유자에게 속합니다.

## 설치

<details>
<summary>Linux · macOS · Windows 설치</summary>

### Linux

Linux x86_64 또는 ARM64, 사용 가능한 `/dev/kvm`, 네트워크, `sudo` 권한이 있는 일반 사용자 계정이 필요합니다. **앞에 `sudo`를 붙이지 않고** 그 사용자로 설치기를 실행하세요. KVM이 없다면 하드웨어 가상화 또는 중첩 가상화를 먼저 활성화하세요.

```sh
curl -fsSL https://github.com/SteelCrab/firecrab/releases/latest/download/install.sh | bash
```

진단·제거 옵션은 [설치 가이드](public-docs/installation.md)에 있습니다. Linux에서 원격 CLI만 설치하려면 아래의 체크섬 검증을 포함한 `install-cli.sh` 절차를 사용하세요. GNU/musl을 자동 선택하며 기본 경로는 `~/.local/bin`입니다.

### macOS

Apple silicon, macOS 15 이상, 중첩 가상화를 지원하는 호스트가 필요합니다(M3 이상, 전체 검증은 M5/macOS 26.6.2). 먼저 설치기 체크섬을 확인하고 CLI와 helper를 설치하세요.

```sh
curl -fLO https://github.com/SteelCrab/firecrab/releases/latest/download/install-cli.sh
curl -fLO https://github.com/SteelCrab/firecrab/releases/latest/download/SHA256SUMS
grep ' install-cli.sh$' SHA256SUMS > install-cli.sh.sha256
if command -v sha256sum >/dev/null; then
  sha256sum -c install-cli.sh.sha256
else
  shasum -a 256 -c install-cli.sh.sha256
fi && sh install-cli.sh
```

이어서 호스트 지원 여부를 확인하고 Debian 관리 환경을 설치합니다.

```sh
firecrab service doctor
firecrab service install
```

CLI 설치만으로는 관리 VM이 설치되지 않습니다. microManager는 `Virtualization.framework`, 별도 영속 데이터 디스크, 상주 launchd 서비스를 사용합니다. [macOS 가이드](public-docs/micromanager-macos.md)를 참고하세요.

### Windows

x86_64 또는 ARM64 Windows, Microsoft Store WSL2, 중첩 KVM이 필요합니다. 일반 PowerShell에서 CLI 설치기를 내려받고 체크섬을 확인하세요.

```powershell
Invoke-WebRequest https://github.com/SteelCrab/firecrab/releases/latest/download/install-cli.ps1 -OutFile install-cli.ps1
Invoke-WebRequest https://github.com/SteelCrab/firecrab/releases/latest/download/SHA256SUMS -OutFile SHA256SUMS
$expected = ((Get-Content SHA256SUMS | Where-Object { $_ -match ' install-cli\.ps1$' }) -split '\s+')[0]
if ((Get-FileHash install-cli.ps1 -Algorithm SHA256).Hash -ne $expected) { throw 'installer checksum mismatch' }
& ./install-cli.ps1
```

`firecrab`이 아직 `PATH`에 없다면 새 PowerShell을 연 뒤 지원 여부 확인과 설치를 실행하세요. ARM64 설치는 아직 전체 검증되지 않았습니다. [Windows 가이드](public-docs/micromanager-windows.md)를 참고하세요.

```powershell
firecrab service doctor
firecrab service install
```

**Windows 제약:** v0.3.0 이전 게스트는 net-helper가 필요로 하는 nftables `bridge` 패밀리가 WSL2에 없어 기본 WSL2에서 MicroVM을 시작하지 못합니다. microManager는 이제 수정이 반영된 최신 릴리스를 설치하지만, 이 경로는 아직 Windows에서 검증되지 않았습니다. Windows CLI로 지원되는 원격 Linux 호스트를 관리할 수는 있습니다.

</details>

## 실행

<details>
<summary>Linux · macOS · Windows 시작과 중지</summary>

### Linux

설치기는 두 systemd 서비스를 시작합니다. 이후 시작·상태 확인·중지는 다음 명령을 사용하세요.

```sh
sudo systemctl start firecrab-helper firecrab-api
systemctl status firecrab-helper firecrab-api
sudo systemctl stop firecrab-api firecrab-helper
```

### macOS

microManager가 상주 관리 VM과 localhost API 터널을 시작합니다.

```sh
firecrab service start
firecrab service status
firecrab service debug --logs --tail 100
firecrab service stop
```

### Windows

microManager가 사용자별 예약 작업으로 WSL2 관리 배포판을 유지합니다.

```powershell
firecrab service start
firecrab service status
firecrab service debug --logs --tail 100
firecrab service stop
```

**Windows 제약:** v0.3.0 이전 게스트는 net-helper가 필요로 하는 nftables `bridge` 패밀리가 WSL2에 없어 기본 WSL2에서 MicroVM을 시작하지 못합니다. microManager는 이제 수정이 반영된 최신 릴리스를 설치하지만, 이 경로는 아직 Windows에서 검증되지 않았습니다. Windows CLI로 지원되는 원격 Linux 호스트를 관리할 수는 있습니다.

정상 상태이면 `http://127.0.0.1:5523/`을 여세요. MicroNetwork를 만들고 설치된 이미지를 선택해 VM을 생성·시작한 뒤, `running` 상태에서 터미널을 엽니다. 원격 호스트는 [CLI 호스트 프로필](public-docs/firecrab-cli.md#host-profiles)을 설정하세요.

</details>

## 소스 실행

<details>
<summary>Linux API·대시보드 / macOS·Windows CLI</summary>

저장소 루트에서 지정된 [Rust 도구 체인](rust-toolchain.toml), Node.js 22 이상, npm을 사용하세요. 실제 VM 실행에는 Linux KVM과 [호스트 준비 조건](public-docs/installation.md)이 필요합니다.

### Linux

터미널 세 개를 사용합니다. helper는 특권으로, API는 일반 사용자로 실행합니다. 로컬 데이터 경로는 저장소 루트를 기준으로 합니다.

```sh
# 1
cargo build -p firecrab-helper --locked
sudo -u root -g "$(id -gn)" FIRECRAB_NET_HELPER_ALLOWED_UID="$(id -u)" \
  ./target/debug/firecrab-helper

# 2
cargo run -p firecrab-api --locked

# 3
npm ci --prefix firecrab-frontend
npm run dev --prefix firecrab-frontend
# http://localhost:8080/
```

빌드한 대시보드를 API가 제공하게 하려면 개발용 API를 종료하고 다음을 실행하세요.

```sh
npm run build --prefix firecrab-frontend
FIRECRAB_STATIC_ROOT="$PWD/firecrab-frontend/dist" cargo run -p firecrab-api --locked
# http://127.0.0.1:5523/
```

### macOS

체크아웃의 CLI와 서명된 네이티브 helper를 빌드한 뒤 관리 서비스를 설치합니다. 이미 설치되어 있다면 `service start`를 사용하세요.

```sh
cargo build -p firecrab-cli --locked
scripts/build-micromanager-macos.sh target/debug/firecrab-micromanager-macos
./target/debug/firecrab service install
./target/debug/firecrab service status
```

### Windows

PowerShell에서 체크아웃의 CLI를 빌드하고 관리 서비스를 설치합니다. 이미 설치되어 있다면 `service start`를 사용하세요.

```powershell
cargo build -p firecrab-cli --locked
.\target\debug\firecrab.exe service install
.\target\debug\firecrab.exe service status
```

macOS·Windows의 위 명령은 소스에서 빌드한 CLI/helper와 설치된 게스트 API를 실행합니다. Debian 안에서 수정한 `firecrab-api` 소스를 **다시 빌드하지 않습니다**. 전체 API/net-helper 런타임 개발은 Linux에서 진행하고, 관리 서비스 개발은 위 플랫폼별 가이드를 참고하세요.

</details>

## 테스트

<details>
<summary>기본 검사·커버리지·브라우저 E2E</summary>

저장소 루트에서 실행하는 기본 검사:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
npm ci --prefix firecrab-frontend
npm run lint --prefix firecrab-frontend
npm run build --prefix firecrab-frontend
python3 scripts/check-doc-links.py
python3 scripts/check-changelog.py
```

선택적 로컬 커버리지(`cargo-llvm-cov` 필요):

```sh
cargo llvm-cov --workspace --locked --lcov --output-path lcov.info
```

게스트 부팅을 생략하는 브라우저 E2E:

```sh
npm ci --prefix firecrab-e2e
npm run install-browsers --prefix firecrab-e2e
FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm test --prefix firecrab-e2e
```

PowerShell에서는 `npm test --prefix firecrab-e2e` 전에 `$env:FIRECRAB_E2E_SKIP_GUEST_BOOT="1"`을 설정하세요. 게스트 부팅을 생략하므로 KVM이나 nginx HTTP 동작을 검증하지 않습니다. 실제 게스트 실행과 모든 설치 검사 항목은 [한국어 체크리스트](public-docs/TEST.ko.md), [영어 체크리스트](public-docs/TEST.md), [E2E 가이드](firecrab-e2e/README.md)에 있습니다.

</details>

## 사용 방법: nginx

<details>
<summary>nginx 이미지 가져오기 → microVM 생성 → HTTP 접속</summary>

API와 CLI가 실행 중인 지원 Linux 호스트 또는 macOS microManager를 사용하세요. 아래 명령은 Linux/macOS의 POSIX 셸 기준입니다. 대시보드에서도 이미지 → OCI Import, 네트워크, MicroVM 순서로 같은 작업을 할 수 있습니다.

1. 이미지를 확인하고 가져옵니다. 작업이 성공해 `nginx-1.27`이 설치될 때까지 상태 확인 명령을 반복하세요.

```sh
firecrab image inspect nginx:1.27
firecrab image import nginx:1.27
firecrab image import-status nginx-1.27
```

2. 네트워크를 만듭니다. 겹치지 않는 서브넷을 고르고, 첫 명령이 반환하는 UUID로 아래 `NETWORK_ID`를 바꾸세요.

```sh
firecrab network create --name nginx-net --subnet-cidr 172.31.20.0/24
firecrab vm create --name nginx-demo --template nginx-1.27 --network NETWORK_ID
```

3. VM 생성 결과의 UUID로 `VM_ID`를 바꾼 뒤 시작하고, 목록에서 `running`이 될 때까지 기다리세요.

```sh
firecrab vm start VM_ID
firecrab vm list
```

4. REST API로 TCP 호스트 포트 `8081` → 게스트 포트 `80`을 연결하고 nginx 응답을 확인합니다. 이 PUT은 VM의 포트 포워드 전체 목록을 교체하므로 기존 규칙이 필요하면 요청에 함께 넣으세요.

```sh
curl -fsS -X PUT http://127.0.0.1:5523/api/vms/VM_ID/port-forwards \
  -H 'Content-Type: application/json' \
  -d '{"portForwards":[{"hostPort":8081,"guestPort":80,"protocol":"tcp"}]}'
```

Linux에서는 다른 컴퓨터에서 `http://FIRECRAB_HOST_IP:8081/`로 확인하세요. DNAT는 호스트 자신의 loopback 접속을 제공하지 않습니다. macOS에서는 TCP 릴레이가 제공하는 `http://127.0.0.1:8081/`을 사용합니다. 원격 접근 시 호스트·라우터 방화벽에서도 8081 포트를 허용해야 합니다.

```sh
curl -I http://FIRECRAB_HOST_IP:8081/
# macOS
curl -I http://127.0.0.1:8081/
```

OCI import는 `/etc/firecrab/busybox`를 PID 1로 사용하는 부팅 가능한 rootfs를 만들고 nginx 엔트리포인트를 서비스로 실행합니다. `EXPOSE 80`만으로 호스트 포트가 연결되지는 않습니다. [OCI 이미지](public-docs/oci.md)와 [네트워크 가이드](public-docs/networking.md)를 참고하세요.

예제 VM 중지:

```sh
firecrab vm stop VM_ID
```

<details>
<summary>대시보드 화면 안내</summary>

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

</details>

</details>

## 문서와 기여

기본 문서 언어는 영어이며 위 링크에서 한국어·일본어·중국어·인도네시아어 README를 선택할 수 있습니다. 대시보드는 영어와 한국어를 지원합니다.

- [public-docs/](public-docs/README.md): 설치·API·운영·문제 해결
- [CONTRIBUTING.md](CONTRIBUTING.md): 유지자 노트·개발 환경·PR 검사

[Apache License, Version 2.0](LICENSE)로 배포됩니다.
