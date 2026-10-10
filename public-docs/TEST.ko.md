# Firecrab 테스트 체크리스트 (한국어)

- 영어: [TEST.md](TEST.md)
- 범위: [QA 작업 목록](qa.md)의 모든 ID, 크로스플랫폼 CLI 검증 목록의 모든 항목, [Playwright 모음](../firecrab-e2e/README.md)의 모든 브라우저 테스트 케이스.
- 적용 가능한 각 항목을 **PASS**, **FAILED**, **WARNING**으로 기록한다. 건너뛰거나 잔여물이 있으면 **WARNING**이며 통과로 계산하지 않는다.
- 리소스는 **생성 → 사용/수정 → 삭제 → 잔여물 없음 확인** 순서로 검증한다. 테스트 이름에는 QA 접두어를 붙이고 실행마다 전용 서브넷을 쓴다.
- 공통 API 계약은 Linux, macOS, Windows에 동일하게 적용한다.
- 업데이트 적용은 U1에서만 수행한다. 이번 실행에서 설치한 이미지만 삭제하고, 기존 Docker Hub 로그인은 복원한다.
- API, CLI, 브라우저 검증은 별도 실행으로 기록한다. 브라우저 게스트 부팅을 건너뛴 결과를 부팅 성공으로 표시하지 않는다.

```text
QA 이름 접두어: qa-
API 기본 주소:  http://127.0.0.1:5523
업데이트 범위:  POST /api/update → U1에서만
```

## 목차

- [자동 점검](#자동-점검-및-준비)
- [플랫폼 및 호스트](#플랫폼-및-호스트)
- [MicroNetwork](#micronetwork)
- [MicroStorage](#microstorage)
- [Shells](#shells)
- [이미지·커널·OCI·레지스트리](#이미지커널oci레지스트리)
- [MicroVM 및 SSH](#microvm-및-ssh)
- [VM 수명](#vm-수명)
- [nginx 통합 시나리오](#nginx-통합-시나리오)
- [CLI](#cli)
- [브라우저 E2E](#브라우저-e2e-playwright의-모든-케이스)
- [제거](#제거)
- [정리 및 CI 범위](#정리-및-ci-범위)

## 자동 점검 및 준비

- [ ] **A1 — Rust 서식:** 서식 변경 사항이 없다.
- [ ] **A2 — Rust 린트:** 전체 대상에서 경고가 0개다.
- [ ] **A3 — Rust 테스트:** 워크스페이스 테스트가 통과한다. 필요하면 CLI만 별도로 실행한다.
- [ ] **A4 — 커버리지:** 도구가 있는 환경에서 워크스페이스 보고서를 생성한다.
- [ ] **A5 — 프런트엔드:** 의존성 설치, 린트, 타입 검사, 빌드가 통과한다.
- [ ] **A6 — 브라우저 준비:** 독립 E2E 패키지와 Chromium을 설치한다.
- [ ] **A7 — 브라우저 가져오기 검사:** 게스트 부팅 없이 실행하고 게스트 케이스는 건너뜀으로 기록한다.
- [ ] **A8 — 브라우저 전체 검사:** KVM, Firecracker, 네트워크 helper와 함께 실행한다.
- [ ] **A9 — 문서:** 링크, 변경 이력 형식, rustdoc 경고를 검사한다.
- [ ] **A10 — 설치 스크립트:** 해당 OS에서 shell 린트와 릴리스/CLI 설치 검사를 실행한다.
- [ ] **A11 — Swift/macOS:** 단위 테스트, 서명된 helper, CLI, 진단 JSON을 확인한다.
- [ ] **A12 — Windows 빌드:** CLI 린트·테스트·빌드 및 진단 JSON을 확인한다.
- [ ] **A13 — 네이티브 릴리스 대상:** 아래 일곱 대상에서 테스트와 빌드를 검증한다.
- [ ] **A14 — 문서 단위 점검:** 변경 이력·릴리스 노트·PR 보고 테스트를 실행하고 CI의 rustdoc 커버리지 하한을 확인한다.
- [ ] **A15 — 설치 진단:** 구문·스모크·도움말·점검·진단이 통과하고, 점검·진단은 호스트 상태를 바꾸지 않는다.
- [ ] **A16 — 설치 생명주기:** 설치, 데몬·소켓 권한, KVM 그룹, CLI, 진단, 안전한 재설치, 제거, purge를 확인한다.
- [ ] **A17 — 배포판 의존성:** Debian 12, Fedora, Arch, openSUSE Tumbleweed에서 의존성을 점검·설치한다.
- [ ] **A18 — 개발 배포 계약:** 공통 게스트 빌드·배포·rollback 회귀 검사를 실행한다. 실제 `service dev` 검증은 별도로 기록한다.
- [ ] **A19 — QA 실행기 계약:** VM 없이 게스트 실패·정리, 네이티브 Windows 단계별 결과 수집, 브라우저 모드·API 조건을 검증한다.

Linux의 저장소 루트에서 워크스페이스 점검을 실행한다.
주석의 ID는 해당 체크리스트 항목이다:

```sh
# A1–A4
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
cargo test -p firecrab-cli --locked
cargo llvm-cov --workspace --locked --lcov --output-path lcov.info

# A18 (Linux 또는 macOS; 빌드·서비스 명령은 stub 사용)
python3 scripts/test-micromanager-dev.py
python3 scripts/test-ci-qa-guest.py # A19

# A5
npm ci --prefix firecrab-frontend
npm run lint --prefix firecrab-frontend
npm run build --prefix firecrab-frontend

# A9, A14
python3 scripts/check-doc-links.py
python3 scripts/check-changelog.py
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --document-private-items
python3 -m unittest scripts/test_check_changelog.py scripts/test_write_release_notes.py scripts/test_micromanager_pr_report.py
```

설치 및 플랫폼 명령:

```sh
# A10, A15
shellcheck install.sh scripts/firecrab-release.sh
bash scripts/test-firecrab-release.sh
bash scripts/test-install-cli.sh
bash scripts/test-install-cli-release.sh
bash scripts/test-smoke-release-exec.sh
bash -n install.sh
./install.sh --help
./install.sh --check
./install.sh --doctor

# A11 (macOS)
swift test --package-path micromanager-macos --scratch-path target/swift-micromanager-tests

# A17 (지원하는 각 배포판 컨테이너에서 실행)
./install.sh --check
./install.sh --deps-only
```

PowerShell에서 네이티브 Windows CLI를 점검한다(A12):

```powershell
cargo clippy -p firecrab-cli --all-targets -- -D warnings
cargo test -p firecrab-cli --locked
cargo build -p firecrab-cli --locked
pwsh -File scripts/test-install-cli.ps1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/test-ci-qa-windows.ps1
.\target\debug\firecrab.exe service doctor --json
```

macOS는 같은 CLI Clippy·테스트 명령과 [CI 안내](ci.md)의 Swift·helper 검사를 실행한다.
API·helper 워크스페이스 검사는 Linux에서 수행하고 소스 배포 빌드는 관리 게스트에서 실행한다.
호스팅 doctor 검사는 유효한 JSON과 종료 코드 0 또는 1을 허용하지만 런타임 G2/G3는 `ready: true`를 요구한다.

```text
A13 릴리스 대상:
x86_64-unknown-linux-gnu    x86_64-unknown-linux-musl
aarch64-unknown-linux-gnu   aarch64-unknown-linux-musl
aarch64-apple-darwin        x86_64-pc-windows-msvc
aarch64-pc-windows-msvc

A17 필수 도구: ip nft dnsmasq mkfs.ext4 firecracker sha256sum
```

## 플랫폼 및 호스트

- [ ] **G1 — Linux:** 진단, 서비스 설치·시작·상태 확인 성공.
- [ ] **G2 — macOS:** 중첩 가상화 환경에서 서비스 진단·설치·상태 확인 성공.
- [ ] **G3 — Windows:** WSL2 중첩 가상화 환경에서 서비스 진단·설치·상태 확인 성공.
- [ ] **G4 — 전체:** 호스트 API는 200, 대시보드는 HTML 200.
- [ ] **G5 — 전체:** 없는 경로는 요청 ID를 포함한 JSON 404.
- [ ] **G6 — macOS/Windows:** `service shell`은 관리 Debian 게스트에서 명령을 root로 실행하고, 각 인자를 그대로 전달하며, 명령의 종료 코드를 돌려준다. 명령 없이 실행하면 root 로그인 셸이 열린다.
- [ ] **G6a — Windows:** 명령은 root로 실행되고 작업 디렉터리는 `/root`다.
- [ ] **G6b — Windows:** 기본 셸은 stdin을 읽고 종료 코드 19를 전달한다.
- [ ] **G6c — Windows:** `service run` 별칭도 같은 셸을 연다.
- [ ] **G6d — Windows:** 한글·따옴표·빈 문자열·공백·달러 기호·백슬래시·셸 메타 문자가 리터럴 인자로 유지된다.
- [ ] **G6e — Windows:** stdout/stderr가 분리되고 종료 코드 23이 호스트에 전달된다.
- [ ] **G6f — Windows:** 명령 stdin의 한글과 줄이 유지되고 EOF로 명령이 종료된다.
- [ ] **G6g — Windows:** 없는 명령은 오류를 내며 실패하고 후속 셸은 정상 실행된다.
- [ ] **G6h — Windows:** 셸 종료 후 API/helper가 active이고 호스트 API가 응답한다.
- [ ] **G7 — macOS 복구:** `service repair`는 다운로드 없이 서비스와 localhost API를 복구한다. 설치 바이너리, 디스크, SSH 키, 개발 실행 설정을 보존한다. 없거나 손상된 데몬 스크립트와 launchd plist도 복구한다. 필수 설치 파일이 없으면 서비스를 멈추기 전에 실패한다. SSH, 게스트 서비스, API가 정상 상태가 아니면 0이 아닌 종료 코드를 반환한다.
- [ ] **G7a — macOS 복구 안내:** 서비스 실행 오류는 `firecrab service repair`를 제안한다. 복구를 자동 실행하지 않는다. 설치가 완료됐는데 서비스 등록 파일이 없으면 repair를 안내한다. 새 호스트에는 install을 안내한다. 소스·컴파일 오류는 기존 오류를 유지한다. `debug --json`은 JSON 형식을 유지하고 필드 안에 복구 안내를 넣는다.
- [ ] **H1 — 업데이트 상태:** 읽기 전용 상태 확인 성공.
- [ ] **H2 — 호스트 네트워크:** uplink가 있으며 loopback·내부 helper 인터페이스는 선택 목록에 없다.
- [ ] **U1 — 업데이트 적용(별도 선택 실행):** 적용 후 호스트가 복구되고 API가 200을 반환한다.

호스팅 macOS·Windows 러너는 빌드·단위 테스트·진단만 수행한다.
런타임 E2E는 M3 이상 네이티브 Mac 또는 WSL2가 있는 Windows 호스트에서 준비 상태를 확인한 뒤 수행한다.
첫 부팅이 느리면 QA 대기 배수로 nginx/SSH 대기 시간을 늘린다. 패키지 설치나 Playwright 제한 시간은 늘어나지 않는다.

```sh
# G1: Linux
firecrab doctor
firecrab service install
firecrab service start
firecrab service status

# G2/G3: 네이티브 macOS 또는 Windows 호스트
firecrab service doctor
firecrab service install
firecrab service status

# G6: 네이티브 macOS 또는 Windows 호스트
firecrab service shell -- id -un                                     # root
firecrab service shell -- printf '[%s]\n' "it's here" 'a b' '$HOME'   # [it's here] [a b] [$HOME]
firecrab service shell -- sh -c 'exit 7'; echo $?                    # 7
firecrab service shell                                               # root 로그인, `exit`로 종료
```

Windows G6a–G6h: WSL2 microManager가 설치된 호스트에서 `cargo test -p firecrab-cli --test windows_service_shell -- --ignored --test-threads=1`을 실행한다. 기본 셸 검사는 stdin으로 수행하며 터미널 키보드·크기 변경·Ctrl-C는 별도 대화형 검사다.

G7은 설치된 Mac에서 실행한다. 실행 중인 MicroVM을 멈출 수 있을 때 진행한다.

```sh
cargo build -p firecrab-cli --locked
./target/debug/firecrab service stop
./target/debug/firecrab service repair
./target/debug/firecrab service status
./target/debug/firecrab service shell -- systemctl is-active firecrab-api
curl -fsS http://127.0.0.1:5523/api/host
```

G7/G7a 오류 재현과 복구는 `scripts/ci-qa-macos-e2e.sh repair`로 반복 검사한다.
파일 보존과 원상 복구를 확인하고 실행 로그와 결과를 저장한다.
준비 조건과 검사 항목은 [repair QA](micromanager-repair-qa.md)에 정리한다.

종료 코드 0, 정상 게스트 서비스, HTTP 200을 확인한다. 실행 전후에 설치
바이너리와 SSH 키의 체크섬을 비교한다. 디스크 파일의 식별값도 비교한다.
부팅은 디스크 내용을 바꾸지만 디스크 파일을 교체하면 안 된다. VM을 멈춘
상태에서 plist와 데몬 스크립트를 백업 이름으로 옮긴다. `repair`를 다시 실행하고
두 파일이 생성되는지 확인한다. 복구가 끝날 때까지 백업을 보존한다. 단위 테스트는
사용자의 launchd 서비스를 바꾸지 않고 설치 파일 누락과 파일 보존을 확인한다.

```text
런타임 조건: doctor → ready: true
느린 게스트: FIRECRAB_QA_WAIT_FACTOR
H2 제외 대상: lo, fct*, mnb*
```

리소스 테스트 전에 공통 API를 확인한다:

```sh
API=http://127.0.0.1:5523
curl -i "$API/api/host"             # G4: 200
curl -i "$API/"                    # G4: HTML 200
curl -i "$API/api/no-such-route"    # G5: requestId가 포함된 JSON 404
curl -i "$API/api/update"           # H1: 읽기 전용 확인
curl -i "$API/api/network"          # H2: uplink 목록
```

## MicroNetwork

- [ ] **N1 — IPv4 생성/삭제:** 생성은 201, 삭제는 204, 이후 상세 조회는 404.
- [ ] **N2 — 목록/상세:** 두 화면에 생성한 네트워크가 나온다.
- [ ] **N3 — 인터넷 전환:** 인터넷을 끈 뒤 다시 켠다.
- [ ] **N4 — IPv6 SLAAC:** ULA /64 네트워크를 SLAAC로 생성한 뒤 삭제한다.
- [ ] **N5 — 잘못된 uplink:** 빈 uplink는 400으로 거부되고 행이 남지 않는다.
- [ ] **N6 — 사용 중 삭제:** VM이 연결된 네트워크 삭제는 409; VM을 먼저 삭제한 뒤 네트워크를 삭제한다.

사용하지 않는 테스트 서브넷에서 N1–N3을 실행하는 예시다. 기존 네트워크와 겹치면 CIDR을 바꾼다:

```sh
API=http://127.0.0.1:5523
NET_ID=$(curl -fsS -X POST "$API/api/micro-networks" \
  -H "Origin: $API" -H 'content-type: application/json' \
  -d '{"name":"qa-doc-net","subnetCidr":"172.31.230.0/24","internetEnabled":true}' \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
curl -fsS "$API/api/micro-networks/$NET_ID"
curl -fsS -X PATCH "$API/api/micro-networks/$NET_ID" \
  -H "Origin: $API" -H 'content-type: application/json' \
  -d '{"internetEnabled":false}'
curl -fsS -X PATCH "$API/api/micro-networks/$NET_ID" \
  -H "Origin: $API" -H 'content-type: application/json' \
  -d '{"internetEnabled":true}'
curl -sS -o /dev/null -w '%{http_code}\n' -X DELETE "$API/api/micro-networks/$NET_ID"
```

추가 네트워크 요청과 기대 결과:

```http
N4  POST /api/micro-networks
    {"name":"qa-ipv6","subnetCidr":"172.31.231.0/24","internetEnabled":true,"ipv6AddressMode":"slaac"}
    → 201; IPv6 ULA /64
N5  POST /api/micro-networks with "uplink":"" → 400; 잔여 행 없음
N6  DELETE /api/micro-networks/{id} with attached VM → 409
```

## MicroStorage

- [ ] **S1 — 저장소 루트:** 목록에 기본 풀이 있다.
- [ ] **S2 — 장치:** 호스트 장치 목록이 열린다.
- [ ] **S3 — 풀 생명주기:** 호스트 절대 경로 등록(201), 삭제(204), 이후 상세 조회 404. macOS/Windows에서는 Debian 관리 게스트 내부 경로를 쓴다.

```http
S1  GET    /api/storage                         → "default" 포함
S2  GET    /api/storage/devices
S3  POST   /api/micro-storages                  → 201
    DELETE /api/micro-storages/{id}             → 204
    GET    /api/micro-storages/{id}             → 404
```

## Shells

- [ ] **L1 — 생성/삭제:** 이름이 있는 POSIX shell 생성(201) 후 삭제(204).
- [ ] **L2 — 리비전:** 새 shell 리비전을 추가한다.
- [ ] **L3 — 본문 조회:** shell과 해당 리비전 본문을 조회한다.
- [ ] **L4 — VM 고정:** VM 생성·수정 때 shell을 지정하고 게스트의 실행 파일을 확인한다.

```http
L1  POST   /api/shells                           → 201; 본문은 /bin/sh
    DELETE /api/shells/{id}                     → 204
L2  POST   /api/shells/{id}/revisions
L3  GET    /api/shells/{id} 및 해당 리비전
L4  VM 생성: shellIds
    PUT    /api/vms/{id}/shells
    게스트: /var/lib/firecrab/shells/00.sh
```

## 이미지·커널·OCI·레지스트리

- [ ] **I1 — 카탈로그:** 이미지·커널·레지스트리 목록이 열린다.
- [ ] **I2 — 커널 생명주기:** 설치 성공, 미사용 커널 삭제 가능, 연결된 커널 삭제 시 충돌.
- [ ] **I3 — M2Image 생명주기:** 패키징·설치 후 임시 자료와 이번 실행에서 설치한 이미지만 정리한다.
- [ ] **I4 — 커널 연결:** 이미지를 커널과 연결하고 캐시 누락 시 충돌을 확인한다.
- [ ] **I5 — OCI 검사:** blob 저장 없이 참조를 검사한다.
- [ ] **I6 — OCI 가져오기:** 가져오기·상태 폴링 후 alias를 삭제한다.
- [ ] **I7 — 레지스트리:** 설치된 사용자 지정 alias를 등록하고 I6과 함께 정리한다.
- [ ] **I8 — Docker Hub:** 조회 시 비밀값이 보이지 않는다. 선택적으로 가짜 로그인을 설정·삭제하고 기존 상태를 복원한다.
- [ ] **I9 — 없는 이미지:** 알 수 없는 alias는 404.
- [ ] **I10 — 부트스트랩(느린 독립 실행):** 작업을 시작하고 해당 작업과 빌더 VM을 삭제한다.
- [ ] **I11 — OCI 배포판 부팅 목록:** 아래 세 참조를 각각 검사·가져오기·시작·중지·삭제한다. 기존 설치 alias는 보존하고 수행하지 않은 가져오기는 **WARNING**으로 표시한다.

```http
I1   GET    /api/images | /api/kernels | /api/microregistry
I2   POST   /api/kernels/{version}/install       → succeeded
     DELETE 연결된 커널                        → 409 in_use
I3   POST   /api/images/{alias}/package, 이후 /install
I4   PUT    /api/images/{alias}/kernel           → 캐시 누락 시 409 kernel_required
I5   GET    /api/oci/inspect?reference=…
I6   POST   /api/oci/import; /import/{alias} 폴링
     DELETE /api/images/{alias}
I7   POST   /api/microregistry/register
I8   Docker Hub: GET; 선택적 PUT, 이후 DELETE → configured=false 또는 기존 로그인
I9   GET    /api/images/does-not-exist           → 404
I10  POST   /api/images/{alias}/bootstrap
     DELETE /api/images/bootstrap/{id}          → 빌더 VM 제거
```

```text
I11 OCI 참조: alpine:3.21 | ubuntu:24.04 | fedora:42
OCI ext4 패킹: Firecrab 호스트에 fakeroot 필요
```

I2, I3, I4, I7, I10, U1은 GitHub Ubuntu CI 범위 밖이다.

## MicroVM 및 SSH

VM 생명주기 전에 설치된 템플릿(I3/I6)과 네트워크(N1)를 준비한다.

- [ ] **V1 — 생성:** 이름, 설치된 템플릿, CPU, RAM, 디스크, 네트워크 ID, 환경 변수를 제출한다.
- [ ] **V2 — 목록/상세:** 두 화면에 새 VM이 생성 상태로 표시된다.
- [ ] **V3 — 중지 상태 환경 변수:** 생성·수정 때 설정하고 게스트 파일을 확인한다.
- [ ] **V4 — 포트 포워딩:** 생성·수정 때 규칙을 설정하고 시작 후 호스트 포트에 요청한다.
- [ ] **V5 — Shell 고정:** shell ID를 지정하고 게스트 파일을 확인한다.
- [ ] **V6 — 스토리지:** VM 스토리지를 성공적으로 수정한다.
- [ ] **V7 — 시작:** 실행 상태와 네트워크 준비 로그를 확인한다.
- [ ] **V8 — SSH:** VM 실행 후 네 가지 하위 검사를 모두 수행한다.
  - [ ] **V8a — 개인 키:** VM 전용 OpenSSH PEM을 내려받는다.
  - [ ] **V8b — 호스트 키:** 지문과 공개 키를 받는다.
  - [ ] **V8c — 호스트 키 검증:** 키가 일치할 때까지 폴링한다.
  - [ ] **V8d — 로그인:** 키 전용 root 로그인에 성공하고 OS 이름이 비어 있지 않다. macOS는 관리 VM SSH 프록시를 쓴다.
- [ ] **V9 — 콘솔:** WebSocket 업그레이드가 101을 반환한다.
- [ ] **V10 — 실행 중 환경 변수:** 실행 중 값을 수정하고 게스트 파일 변경 및 앱 서비스 재시작을 확인한다.
- [ ] **V11 — 중지:** 중지 상태에 도달한다.
- [ ] **V12 — 실행 중 삭제:** VM을 중지하기 전 삭제가 거부된다.
- [ ] **V13 — 중지 후 삭제:** 삭제는 204, 이후 상세 조회는 404이며 목록에서도 사라진다.
- [ ] **V14 — 네트워크 누락:** 생성은 400이고 행이 남지 않는다.

```http
V1   POST   /api/vms                         → name, template, cpu, ram, disk, microNetworkId, env
V2   GET    /api/vms; GET /api/vms/{id}      → created
V3   생성 또는 PUT env                      → 게스트 /etc/firecrab/vm.env
V4   생성 또는 PUT portForwards             → 시작 후 호스트 포트 요청
V5   생성 또는 PUT shellIds                 → 게스트 /var/lib/firecrab/shells/00.sh
V6   PUT    /api/vms/{id}/storage
V7   POST   /api/vms/{id}/start              → running; FIRECRAB_NETWORK_READY
V8a  GET    /api/vms/{id}/ssh-key            → firecrab-<name>.pem
V8b  GET    /api/vms/{id}/ssh-host-key       → fingerprint, publicKey
V8c  GET    /api/vms/{id}/ssh-host-key/check → status=match
V9   GET    /ws/vms/{id}/console             → 101
V10  실행 중 PUT env                         → services.d/app 재시작
V11  POST   /api/vms/{id}/stop               → stopped
V12  실행 중 DELETE /api/vms/{id}            → 거부
V13  DELETE /api/vms/{id}                    → 204; 이후 GET → 404
V14  microNetworkId 없이 POST /api/vms      → 400
```

```sh
# V8d: V8a의 키 경로와 V7의 게스트 주소로 바꾼다
GUEST_IP=172.31.230.2  # 게스트 IPv4 주소로 교체
ssh -i firecrab-qa-vm.pem -o IdentitiesOnly=yes "root@$GUEST_IP" true
ssh -i firecrab-qa-vm.pem -o IdentitiesOnly=yes "root@$GUEST_IP" uname -s
```

CPU, RAM, 디스크, egress는 created, stopped, error에서만 수정한다. 환경 변수는 running에서도 수정할 수 있다.

## VM 수명

실행 중인 VM(V7)과 API 호스트의 root 권한으로 수행한다. macOS에서는 호스트 명령을 management VM에 SSH로 접속해 실행한다.
모든 VM은 각자의 `firecrab-vm-<simple id>.service` unit에서 실행된다.

- [ ] **R1 — unit 안의 shim:** `firecrab-api vm-shim --vm-id <id>`가 Firecracker의 부모이자 active 상태인 `firecrab-vm-<simple id>.service`의 메인 프로세스이고, 부모는 PID 1, 실행 사용자는 API 사용자이며, 런타임 디렉터리에 `shim.sock`과 `console.log`가 있고, unit의 `MemoryMax`는 게스트 RAM에 256 MiB와 RAM의 1/8 중 큰 값을 더한 값, `CPUQuota`는 vCPU당 1코어에 1코어를 더한 값이다.
- [ ] **R2 — API 밖에서 중지:** unit을 `systemctl stop`하면 `stopped`가 기록되고 `exit.json`에 `"stop_requested":true`가 있으며 TAP이 제거된다.
- [ ] **R3 — 바로 재시작:** 중지 직후 시작하면 `running`이 된다.
- [ ] **R4 — API 재시작:** shim과 Firecracker PID가 그대로이고 VM이 `running`을 유지하며, journal에 `adopted=1`이 남고 TAP이 bridge에 붙어 있고 V9와 V11이 동작한다.
- [ ] **R4b — nft 변경:** helper가 살아 있는 상태에서 API를 중단하고 QA VM의 DNAT/egress/L2 규칙을 손상시킨다. API 시작 후 정책과 ping·SSH·포트포워딩 HTTP가 복구되고 VM PID와 다른 호스트 테이블이 유지된다.
- [ ] **R4c — TAP/브리지 변경:** TAP 분리·down, 브리지 down·잘못된 MTU·gateway prefix·forwarding을 API 시작 시 복구한다. VM PID가 유지되고 게스트 통신이 동작한다.
- [ ] **R4d — DHCP 변경:** 예약·기본 설정 파일 손상과 dnsmasq 종료 후 같은 lease revision으로 서비스를 복구한다. 게스트가 예약 IP를 다시 받고 통신한다.
- [ ] **R4e — 복구 재시도:** helper 중단 시 `networkFailed`와 재시도 503을 확인한다. helper 복구 후 `POST /api/network/reconcile`은 204를 반환하고 결과를 `reconnected`로 갱신한다. VM PID와 게스트 통신이 유지된다.
- [ ] **R4f — helper 이름 변경 (일회용 Linux CI 전용):** VM 실행 중 기존 서비스를 전환한다. 두 서비스 이름의 helper PID가 같고 기존 실행 경로·drop-in이 동작하며, VM PID와 ping·SSH·포트포워딩 HTTP가 복구된다.
- [ ] **R5 — API가 꺼진 동안 crash:** Firecracker를 `kill -9`한 뒤 시작한 API가 `error`를 기록하고 TAP과 nft 규칙을 제거하며, VM은 IPv4 주소를 그대로 유지한다.
- [ ] **R6 — 중단된 시작:** shim이 뜬 뒤 `running` 전에 API를 재시작하면 `error`가 기록되고 unit이 남지 않는다.
- [ ] **R7 — 정상 중지:** V11이 `stopped`를 기록하고 `exit.json`에 `"stop_requested":true`가 있으며 failed 상태의 `firecrab-vm-*` unit이 없다.

```sh
# R1: shim, 부모 프로세스, unit 확인
VM=<vm id>
ps -o pid,ppid,user,args -p "$(pgrep -f "[v]m-shim --vm-id $VM")"
systemctl list-units --all --plain --no-legend 'firecrab-vm-*'
systemctl show -p MemoryMax -p CPUQuotaPerSecUSec "firecrab-vm-$(echo "$VM" | tr -d -).service"

# R2: API 밖에서 VM 중지
sudo systemctl stop "firecrab-vm-$(echo "$VM" | tr -d -).service"

# R4: API 재시작 전후 PID 비교
pgrep -f "[v]m-shim --vm-id $VM"
sudo systemctl restart firecrab-api
pgrep -f "[v]m-shim --vm-id $VM"
journalctl -u firecrab-api -b | grep 'startup reconciliation finished' | tail -1

# R5: API가 꺼진 동안 VM crash
sudo systemctl stop firecrab-api
sudo pkill -9 -f "firecracker --api-sock .*/$(echo "$VM" | tr -d -)/"
sudo systemctl start firecrab-api

# R7, X7
systemctl --failed --plain --no-legend | grep firecrab-vm- || echo none
```

`scripts/ci-qa-lifetime.sh [OCI 참조]`가 R1–R7과 X7을 실행하고, 이미지가 없으면 import한다.

## nginx 통합 시나리오

nginx QA 스크립트로 OCI, Shell, VM, 환경 변수, 포트 포워딩, SSH를 함께 검증한다.

- [ ] **NGX1:** nginx 이미지 검사·가져오기 후 alias 설치 확인.
- [ ] **NGX2:** POSIX shell 생성 후 201과 shell ID 확인.
- [ ] **NGX3:** 환경 변수, 고정 shell, HTTP 포트 포워딩으로 VM 생성 후 201 확인.
- [ ] **NGX4:** VM 시작 후 IPv4가 있는 실행 상태이며 ping 또는 네트워크 준비 로그 확인.
- [ ] **NGX5:** 호스트 포트의 HTTP 요청이 200 반환.
- [ ] **NGX6:** V8a–V8d: PEM, 호스트 키 일치, root SSH 로그인 통과.
- [ ] **NGX6b:** SSH로 게스트의 초기 환경 변수 값을 확인한다.
- [ ] **NGX7:** SSH로 고정된 shell이 실행 가능한지 확인한다.
- [ ] **NGX8:** 실행 중 환경 변수를 수정하고 게스트 파일 갱신을 확인한다.
- [ ] **NGX9:** VM 중지·삭제, 가져온 alias·shell·네트워크 삭제; 잔여물 없음.

```sh
scripts/ci-qa-nginx.sh nginx:1.27-alpine
curl -i http://127.0.0.1:18080/  # NGX5 → 200
```

```text
NGX3:  QA_NGINX=ci; shellIds; portForwards 18080→80/tcp
NGX4:  FIRECRAB_NETWORK_READY
NGX6b: cat /etc/firecrab/vm.env → QA_NGINX=ci
NGX7:  test -x /var/lib/firecrab/shells/00.sh
NGX8:  PUT env QA_NGINX=two → 게스트 파일 갱신
```

macOS에서는 NGX5를 관리 VM 내부와 Mac의 loopback relay에서 모두 확인하고, NGX6–NGX8의 SSH는 관리 VM을 통해 프록시한다.

## CLI

- [ ] **C1 — VM:** 목록·생성·시작·중지·삭제 후 테스트 VM이 없다.
- [ ] **C2 — 콘솔:** 연결과 분리가 정상 동작한다.
- [ ] **C3 — 네트워크:** 목록·생성·삭제 후 테스트 네트워크가 없다.
- [ ] **C4 — 이미지:** 목록·검사·가져오기·상태 확인 후 가져온 alias를 삭제한다.
- [ ] **C5 — 호스트 프로필:** 테스트 프로필을 추가·목록·사용·조회·제거한다.
- [ ] **C6 — 설정 경로:** Unix/Windows 기본 경로와 디렉터리 재지정을 확인한다.
- [ ] **C7 — 프로필 검증:** 프로필 변경은 직렬화되며 알 수 없는 키를 거부한다.
- [ ] **C8 — 엔드포인트 선택:** 명시적 API와 저장된 호스트의 우선순위를 검증한다.
- [ ] **C9 — 플랫폼 명령:** Linux 전용 호스트 관리와 Apple silicon macOS 서비스 생명주기를 검증한다.
- [ ] **C10 — 터미널:** 콘솔 종료 후 모든 플랫폼에서 raw 터미널 모드 복원.
- [ ] **C11 — 설치 무결성:** 릴리스 해시, 안전한 symlink 교체, 쓰기 가능한 설치 경로, 실행 파일 검색 경로를 확인한다.
- [ ] **C12 — 원격 수동 흐름:** Linux 데몬·API 확인 후 어느 클라이언트 OS에서나 SSH로 loopback API를 포워딩하고 저장된 호스트 프로필 및 설정을 확인한다.

Linux 전용 명령은 Linux 또는 관리 게스트에서 실행한다. macOS/Windows 호스트 CLI에서는 해당 없음으로 표시한다.

```text
C1   firecrab vm list | create | start | stop | delete
C2   firecrab vm console
C3   firecrab network list | create | delete
C4   firecrab image list | inspect | import | import-status
C5   firecrab host add | list | use | show | remove
C6   Unix: ~/firecrab/config.toml
     Windows: %USERPROFILE%\firecrab\config.toml
     재지정: FIRECRAB_CONFIG_DIR
C8   --api와 --host
C9   macOS: service install | start | stop | status | reinstall | uninstall
     Linux 전용: doctor | info | status | update --check | update --apply
     Linux systemd: service start | stop | restart | enable | disable
C11  SHA-256 검증; 설치 프로그램 symlink 안전 교체; 사용자 쓰기 가능 설치; PATH
C12  설정: current_host = "dev"
```

C12 실행 전 SSH 호스트 주소를 바꾼다:

```sh
# Linux 호스트에서
sudo systemctl status firecrab-api firecrab-helper
curl -fsS http://127.0.0.1:5523/api/host

# SSH가 있는 Linux, macOS 또는 Windows 클라이언트에서
ssh -fN -L 15523:127.0.0.1:5523 user@firecrab-host
firecrab host add dev http://127.0.0.1:15523 --use
firecrab host show
firecrab vm list
firecrab host list
```

## 브라우저 E2E: Playwright의 모든 케이스

로컬 레지스트리 fixture를 사용하며 Docker Hub는 필요하지 않다. 필요하면 테스트 모음이 API와 Vite를 시작한다.

- [ ] **B1 — OCI 검사/가져오기:** 로컬 참조를 입력하고 아키텍처 호환성을 검사·가져온 뒤 등록되고 설치된 alias 확인.
- [ ] **B2 — OCI 게스트 부팅:** 가져온 이미지로 VM을 생성·시작하고 네트워크·게스트 서비스 표식을 확인한다.
- [ ] **B3 — MicroRegistry 가져오기:** fixture를 검사·가져와 설치된 이미지로 확인.
- [ ] **B4 — MicroRegistry 등록/충돌:** 등록 후 버전이 있는 카탈로그 행 하나를 확인하고 중복 등록의 충돌을 확인한다.
- [ ] **B5 — MicroRegistry 실패 작업:** 실패한 등록 작업 후 현재 카탈로그 행이 없어야 한다. **제품 기능 미구현으로 건너뜀:** 실제 실패를 유발하는 경로가 아직 없다.
- [ ] **B6 — MicroRegistry 재설치/부팅:** 템플릿 삭제, 로컬 카탈로그 행으로 재설치, 네트워크 준비까지 부팅.
- [ ] **B7 — IPv6 폼:** Off일 때 IPv6 입력이 숨겨지고 활성화하면 주소/모드가 보이며 SLAAC가 선택된다.
- [ ] **B8 — IPv6 네트워크:** IPv4 전용 및 자동 ULA 듀얼스택 생성. 전자는 IPv6 설정이 없고 후자는 ULA /64, SLAAC, NAT66 확인.
- [ ] **B9 — DHCP fixture 가져오기:** 로컬 DHCP 부팅 이미지를 검사·가져오기.
- [ ] **B10 — DHCP 듀얼스택 게스트:** HTTP 포트 포워딩으로 부팅하고 네트워크 준비 로그, IPv4·IPv6, 포워딩된 IPv4와 직접 IPv6의 키 전용 root SSH 확인.
- [ ] **B11 — 콘솔 세션 종료:** 모의 게스트 종료 후 Session ended를 유지하고 New session으로 다시 연결한다.
- [ ] **B12 — 콘솔 자동 재연결:** 다른 모의 소켓 종료에서는 자동으로 다시 연결한다.
- [ ] **B13 — 실제 콘솔 재접속:** exit로 두 화면의 세션이 종료되고, 새 세션에서 명령을 실행하며 VM은 계속 실행된다.
- [ ] **B14 — 영어 재연결 UI:** 목록·상세에서 API 결과 6종, 툴팁, 확인 시각, 미확인 VM을 검증한다.
- [ ] **B15 — 한국어 재연결 UI:** 같은 목록·상세 결과와 진단을 한국어로 검증한다.
- [ ] **B16 — 재연결 상태 갱신:** 새 시작 후 목록·상세의 이전 API 결과가 제거된다.
- [ ] **B17 — 좁은 재연결 패널:** 한국어 진단이 넘치지 않고 줄바꿈되며 상태 툴팁을 이용할 수 있다.

가져오기·폼 전용 모드에서는 B2, B6, B8, B10, B13을 건너뛰며 B5도 계속 제외한다. 예상 결과는 **11개 통과, 6개 제외**다.
전체 게스트 실행은 **16개 통과, 1개 기존 제외**를 예상하며 KVM, Firecracker, 작동하는 네트워크 helper 소켓, B10용 SSH 도구가 필요하다.
테스트 소유 VM·네트워크·alias·패키지·로컬 카탈로그 등록을 정리하며, 등록이 남으면 준비 단계에서 실패한다.
worker는 하나이고 재시도는 없다. 게스트 부팅 제외는 명시적인 설정으로만 수행한다.

```text
대시보드 origin: http://localhost:8080
API:             http://127.0.0.1:5523
B2/B6:           FIRECRAB_NETWORK_READY
B2:              FIRECRAB_OCI_E2E_READY
B4:              중복 등록 시 409 alias_collision
B8:              ULA /64; SLAAC; NAT66
B10:             80:18888/tcp
```

먼저 가져오기·폼 케이스를 실행한다:

```sh
npm ci --prefix firecrab-e2e
npm run test:runner --prefix firecrab-e2e # A19
npm ci --prefix firecrab-frontend
npm run install-browsers --prefix firecrab-e2e
FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm test --prefix firecrab-e2e
```

게스트 부팅 케이스는 한 터미널에서 helper를 시작하고 다른 터미널에서 테스트를 실행한다:

```sh
# 터미널 1
./scripts/dev-net-helper.sh

# 터미널 2 (KVM과 Firecracker 준비)
npm test --prefix firecrab-e2e
```

Windows에서는 PowerShell로 소스 배포와 브라우저 E2E를 실행한다:

```powershell
cargo build -p firecrab-cli --locked
.\scripts\ci-qa-windows-e2e.ps1 -Phase browser -Cli .\target\debug\firecrab.exe -Source .
.\scripts\ci-qa-windows-e2e.ps1 -Phase nginx -Cli .\target\debug\firecrab.exe -Source . -WaitFactor 3
```

브라우저·Vite·fixture는 관리 WSL 안에서 실행하고 실제 게스트 부팅과 API 재사용을 활성화한다.
nginx는 네이티브 Windows의 포워딩된 포트에서도 HTTP 200을 요구한다.
Windows `all`은 gate/setup 성공 후 API → nginx → guest → browser 결과를 실패 후에도 수집하고, 한 단계라도 실패하면 전체 실패로 종료한다.
게스트 스크립트도 앞선 이미지 실패 후 나머지 참조를 검사한다.
Windows 로그·요약·브라우저 archive는 `target/qa/windows/<run-id>` 또는 `-ResultsDir`에 보관한다.
macOS 브라우저 명령과 관리 VM SSH 설정은 [E2E 안내](../firecrab-e2e/README.md)를 참고한다.

## 제거

UFW가 켜져 있고 MicroNetwork 하나와 실행 중인 MicroVM 하나가 있는 일회용 Linux 호스트에서, 제거 뒤의 호스트를 설치 전의 호스트와 비교한다.

- [ ] **UN1:** `firecrab service uninstall` 또는 `./install.sh --uninstall`이 VM을 먼저 멈춘다. `firecracker`·`firecrab-api` 프로세스와 `firecrab-*` unit이 남지 않는다.
- [ ] **UN2:** `/usr/local/lib/firecrab`과 `/usr/local/bin/firecrab`이 없다.
- [ ] **UN3:** `mnb*`·`fct*`·`fcbr0` 링크, firecrab nftables 테이블, MicroNetwork 서브넷의 MASQUERADE 규칙이 없다.
- [ ] **UN4:** `ufw status numbered`에 firecrab 브리지 이름이 없고, 이전부터 있던 규칙은 그대로다.
- [ ] **UN5:** `/etc/firecrab/host-baseline.env`가 없고, `getfacl -p /dev/kvm`이 설치 전과 같으며, 다른 브리지나 컨테이너 네트워크가 없으면 포워딩이 복원된다.
- [ ] **UN6:** `--purge` 없이는 데이터와 설정이 남고 재설치하면 네트워크와 VM이 `stopped`로 보인다. `--purge`면 모두 없다.
- [ ] **UN7:** 두 번째 제거는 아무것도 바꾸지 않고 `0`으로 끝난다.

```sh
sudo firecrab service uninstall            # 또는: sudo ./install.sh --uninstall
pgrep -x firecracker || echo none          # UN1
pgrep -x firecrab-api || echo none
systemctl list-units --all --plain --no-legend 'firecrab-*' | grep . || echo none
ls /usr/local/lib/firecrab 2>&1 | head -1  # UN2: No such file or directory
ip -br link | grep -E '^(mnb|fct|fcbr)' || echo none                        # UN3
sudo iptables -t nat -S POSTROUTING | grep MASQUERADE || echo none
sudo ufw status numbered | grep -E 'mnb[0-9a-f]{12}' || echo none          # UN4
getfacl -p /dev/kvm; sysctl net.ipv4.ip_forward net.ipv6.conf.all.forwarding # UN5
```

## 정리 및 CI 범위

- [ ] **X1:** QA VM이 남지 않는다.
- [ ] **X2:** QA 네트워크가 남지 않는다.
- [ ] **X3:** QA 스토리지 풀이 남지 않는다.
- [ ] **X4:** QA shell이 남지 않는다.
- [ ] **X5:** 사용자 지정 OCI alias가 없고 카탈로그 fixture는 명시적으로 보존할 때만 남긴다.
- [ ] **X6:** QA Docker Hub 비밀값이 남지 않고 기존 로그인이 복원된다.
- [ ] **X7:** QA VM의 `firecrab-vm-*` unit이 남지 않는다.
- [ ] **X8:** QA VM 정지·삭제 후 포워딩된 TCP 포트가 닫힌다.
- [ ] **X9:** 수동 변경한 QA DNS·관리 SSH 설정을 복구하고 로그와 결과 요약을 보관한다.

정리 후 네 종류의 리소스 목록을 확인한다:

```sh
API=http://127.0.0.1:5523
curl -fsS "$API/api/vms"
curl -fsS "$API/api/micro-networks"
curl -fsS "$API/api/micro-storages"
curl -fsS "$API/api/shells"
```

CI 스크립트 범위:

```text
scripts/ci-qa-api.sh: G4–G5 H1–H2 N1–N5 S1–S3 L1–L3 I1 I8–I9 V14 C3 C5 X1–X4 X6
scripts/ci-qa-nginx.sh: V8a–V8d를 포함한 NGX1–NGX9
scripts/ci-qa-ssh.sh: 게스트·nginx 실행 중 V8a–V8d
scripts/ci-qa-guest.sh: I5–I6 V1–V2 V6–V9 V11–V13 N6 C1–C2 C4 X5
scripts/ci-qa-lifetime.sh: R1–R7 X7, API 호스트의 root 명령 (macOS: management VM SSH)
scripts/ci-qa-macos-e2e.sh: 네이티브 Mac의 수동 gate/browser/API/nginx/guest 단계 (G6 포함)
scripts/ci-qa-windows-e2e.ps1: 수동 Windows gate, 선택적 소스 배포, WSL browser/API/nginx/guest 및 네이티브 HTTP
```

확장 게스트 검사는 첫 OCI 참조에 적용한다.
현재 CI 워크플로는 Linux API/nginx/공개 이미지 guest QA와 전체 Chromium E2E를 실행한다. ARM64 KVM 런타임 작업은 등록되지 않았다.
Linux CI는 로그·JSON/JUnit 결과·실패 trace를 14일 보관한다.
[Windows 소스 검증 결과](qa.md#windows-source-validation-2026-10-03)에 브라우저 9개 통과, 기존 B5 제외 및 별도 Ubuntu/Fedora SSH 실패를 기록했다.
이 결과로 Windows `all`이나 고정된 설치 릴리스의 런타임이 통과했다고 판단하지 않는다.

## 관련 문서

- [QA 원본 목록](qa.md)
- [CI 및 런타임 E2E](ci.md)
- [브라우저 E2E 실행 안내](../firecrab-e2e/README.md)
- [기여 안내](../CONTRIBUTING.md)

`FIRECRAB_QA_HELPER_UPGRADE=1` enables the R4f installer migration in the
Linux CI lifetime job; it requires a disposable GitHub Actions host.
