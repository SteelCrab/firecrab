# 테스트 체크리스트

[영어](testing.md) · [QA 스크립트](qa.md)

각 항목을 **PASS**, **FAILED**, **WARNING**으로 기록한다.
생략과 잔여 리소스는 WARNING으로 기록한다.
`qa-` 이름과 전용 서브넷을 사용한다.
생성·사용·삭제 후 남은 리소스를 확인한다.
업데이트는 U1에서만 적용한다. 이번 실행에서 설치한 이미지만 삭제한다.
기존 Docker Hub 로그인을 복원한다.
API·CLI·브라우저 검사 결과를 따로 기록한다.

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

명령은 [CI 로컬 검사](ci.md#local-checks)와 [설치·플랫폼 검사](ci.md#installer-and-platform-checks)를 참고한다.

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

## 플랫폼 및 호스트

[플랫폼 설치 가이드](../README.md#get-started), [복구 QA](macos-repair.md), [Windows 셸 테스트](ci.md#windows-runtime-e2e)를 사용한다.
런타임 진단은 `ready: true`를 반환해야 한다.
호스팅 빌드·단위 테스트는 실제 VM 실행을 검증하지 않는다.

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
- [ ] **G7 — macOS 복구:** 등록 정보와 API를 복구한다.
  바이너리·데이터·키·개발 설정을 보존한다.
  OS 복사본을 백업하고 검사한 뒤 검증된 복구본만 적용한다.
  필수 파일이 없으면 중지 전에 실패한다. 부팅 보고와 API 검사 실패는 오류로 반환한다.
  보존 검사는 [복구 QA](macos-repair.md)를 따른다.
- [ ] **G7a — macOS 복구 안내:** 서비스 실행 오류는 `firecrab service repair`를 제안한다. 복구를 자동 실행하지 않는다. 설치가 완료됐는데 서비스 등록 파일이 없으면 repair를 안내한다. 새 호스트에는 install을 안내한다. 소스·컴파일 오류는 기존 오류를 유지한다. `debug --json`은 JSON 형식을 유지하고 필드 안에 복구 안내를 넣는다.
- [ ] **G7b — macOS 복구 진행 표시:** repair는 검증·중지와 helper 정리·OS 백업과 복구·등록·시작·게스트 부팅과 API 검사 단계를 작업 전에 출력한다. VM/API 준비와 중지 대기는 10초마다 경과 시간과 현재 상태를 출력한다. 로컬 VM/API 준비 제한은 120초이다. 중지 제한은 40초를 유지한다. 정상 상태 검사와 실패 종료 코드는 유지한다.
- [ ] **G7c — macOS OS 파일시스템 복구:** 작은 테스트 디스크의 연결되지 않은 inode 오류를 고친다. 복구 뒤 읽기 전용 재검사를 통과한다. 정상 디스크는 원본 inode를 유지한다. 실패와 시간 초과에서는 원본, 백업, 데이터 디스크, SSH 설정을 유지한다. 별도 복구 검사 제한은 120초이다.
- [ ] **G7d — macOS helper 정리:** 사용자, 실행 파일, 관리 홈이 일치하는 helper만 정리한다. 다른 관리 홈의 helper는 유지한다. 관리 홈별 잠금으로 VM 동시 실행과 OS 교체를 막는다.
- [ ] **G7e — 누락된 Debian 파일:** 검증된 누락 파일을 복원한다.
  링크·패키지 기록은 버전이 일치할 때만 복원한다.
  기존 파일·`/etc`·앱 데이터와 OS·데이터·SSH·참조 이미지를 보존한다.
  검증 실패를 거부한다. 두 번째 복구는 파일을 복원하지 않는다.
  복구본에서 SSH·dbus·네트워크·API 준비를 120초 안에 확인한다.
- [ ] **G7f — macOS SSH 경로:** `Application Support` 경로를 OpenSSH가 하나의 호스트 키 파일 경로로 해석한다. 실제 `ssh -G`와 데몬 연결 복구 테스트로 확인한다. shell, dev, 포트 전달도 같은 경로 처리를 사용한다. repair의 마지막 검사는 SSH 명령 없이 이번 부팅의 게스트 보고를 확인한다. 필드 누락·비활성·중복·주소 불일치를 거부한다. 기존 호스트 키를 보존하고 변경된 키는 거부한다.
- [ ] **G8 — Windows 복구:** WSL 디스크를 백업하고 별도 WSL2 복구 배포판에서 분리된 복사본을 검사한다.
  검증된 복구본만 적용한다. 예약 작업과 XML을 복구하고 설치된 API/helper를 재시작한다.
  이전 helper 이름도 검사한다. 바이너리·설치 정보·데이터·개발 설정을 보존한다.
  다운로드·재가져오기·전체 WSL 종료를 수행하지 않는다.
  관리자 권한이나 복구 도구가 없으면 중지 전에 실패한다. 각 단계 실패는 오류로 반환한다.
  실제 서비스와 Windows API 준비를 5분 안에 확인한다.
- [ ] **H1 — 업데이트 상태:** `GET /api/update`가 성공한다.
  `GET /api/update/progress`는 `idle` 또는 마지막 실행 상태를 반환한다.
- [ ] **H2 — 호스트 네트워크:** uplink가 있으며 loopback·내부 helper 인터페이스는 선택 목록에 없다.
- [ ] **H3 — Firecrab 정보:** API가 버전·설치 경로·Firecracker 버전·API/helper 서비스 상태를 알려 준다. Host 탭에서 표시와 상태 갱신을 확인한다. 조회 실패 시 이전 실행 상태를 표시하지 않는다.
- [ ] **U1 — 업데이트 적용(별도 선택 실행):** 적용 후 호스트가 복구되고 API가 200을 반환한다.
  대시보드 팝업이 릴리스 노트를 보여 준다.
  새 API가 응답할 때까지 게이지·퍼센트·단계를 표시한다.

## MicroNetwork

요청은 [네트워크 가이드](../guides/networking.md)와 [API 필드 규칙](../reference/api.md#micronetwork)을 참고한다.

- [ ] **N1 — IPv4 생성/삭제:** 생성은 201, 삭제는 204, 이후 상세 조회는 404.
- [ ] **N2 — 목록/상세:** 두 화면에 생성한 네트워크가 나온다.
- [ ] **N3 — 인터넷 전환:** 인터넷을 끈 뒤 다시 켠다.
- [ ] **N4 — IPv6 SLAAC:** ULA /64 네트워크를 SLAAC로 생성한 뒤 삭제한다.
- [ ] **N5 — 잘못된 uplink:** 빈 uplink는 400으로 거부되고 행이 남지 않는다.
- [ ] **N6 — 사용 중 삭제:** VM이 연결된 네트워크 삭제는 409; VM을 먼저 삭제한 뒤 네트워크를 삭제한다.

## MicroStorage

요청과 배치 제한은 [스토리지 가이드](../guides/storage.md)를 참고한다.

- [ ] **S1 — 저장소 루트:** 목록에 기본 풀이 있다.
- [ ] **S2 — 장치:** 호스트 장치 목록이 열린다.
- [ ] **S3 — 풀 생명주기:** 호스트 절대 경로 등록(201), 삭제(204), 이후 상세 조회 404. macOS/Windows에서는 Debian 관리 게스트 내부 경로를 쓴다.

## Shells

경로는 [API](../reference/api.md#other-endpoints)를 참고한다.
게스트 동작은 [셸 버전 고정](../reference/guest-runtime.md#shell-revisions)을 참고한다.

- [ ] **L1 — 생성/삭제:** 이름이 있는 POSIX shell 생성(201) 후 삭제(204).
- [ ] **L2 — 리비전:** 새 shell 리비전을 추가한다.
- [ ] **L3 — 본문 조회:** shell과 해당 리비전 본문을 조회한다.
- [ ] **L4 — VM 고정:** VM 생성·수정 때 shell을 지정하고 게스트의 실행 파일을 확인한다.

## 이미지·커널·OCI·레지스트리

절차는 [이미지](../guides/images.md), [커널](../guides/kernels.md), [OCI](../guides/oci.md)를 참고한다.
I11은 `alpine:3.21`, `ubuntu:24.04`, `fedora:42`를 검사한다.
I2/I3/I4/I7/I10/U1은 GitHub Ubuntu CI 밖에서 별도로 검사한다.

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

## MicroVM 및 SSH

이미지를 설치하고 네트워크를 먼저 만든다.
[VM API](../reference/api.md#vm-endpoints)를 사용한다.
CPU·RAM·디스크·송신 정책 변경은 비활성 VM에서 수행한다.
SSH는 실제 인증을 확인한다. macOS는 관리 VM 프록시를 사용한다.

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
- [ ] **V15 — 콘솔 로그아웃:** `exit`는 소켓을 `4000 session_ended`로 종료한다. VM은 실행을 유지한다.

## VM 수명

API 호스트에서 root로 실행한다.
실행 중인 QA VM이 필요하다.
macOS 호스트 명령은 관리 VM SSH로 실행한다.

- [ ] **R1 — shim 유닛:** shim은 Firecracker의 부모이며 유닛의 주 프로세스다.
  PID 1이 관리하고 API 사용자로 실행한다. `shim.sock`과 `console.log`를 확인한다.
  `MemoryMax`는 게스트 RAM에 max(256 MiB, RAM/8)을 더한 값이다.
  `CPUQuota`는 vCPU 수에 코어 1개를 더한 값이다.
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
VM=VM_ID
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

자동 검사는 `scripts/ci-qa-lifetime.sh [OCI reference]`로 실행한다.

## nginx 통합 시나리오

`scripts/ci-qa-nginx.sh nginx:1.27-alpine`을 실행한다.
`QA_NGINX=ci`, POSIX 셸, TCP `18080 → 80`을 설정한다.
환경 변수를 `QA_NGINX=two`로 바꾼다.
SSH로 `/etc/firecrab/vm.env`와 실행 가능한 `/var/lib/firecrab/shells/00.sh`를 확인한다.
macOS는 Debian 내부와 Mac 전달 포트에서 HTTP를 검사한다.
Windows는 네이티브 localhost에서도 HTTP 200을 확인한다.

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

## CLI

명령·설정 경로·선택 순서는 [CLI 참조](../reference/cli.md)를 참고한다.
C12는 [호스트 프로필](../reference/cli.md#host-profiles)의 SSH 터널을 사용한다.

- [ ] **C1 — VM:** 목록·생성·시작·중지·삭제 후 테스트 VM이 없다.
- [ ] **C2 — 콘솔:** 연결과 분리가 정상 동작한다.
- [ ] **C2b — 콘솔 로그아웃:** CLI는 `guest session ended`를 출력하고 0으로 종료한다. VM은 실행을 유지한다.
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

## 브라우저 E2E: Playwright의 모든 케이스

명령과 테스트 이미지는 [브라우저 E2E](../../firecrab-e2e/README.md)를 참고한다.
작업자 1개와 재시도 0회를 사용한다.
가져오기·폼 모드는 **11개 성공, 6개 생략**이다.
B2/B6/B8/B10/B13과 제품 제약으로 막힌 B5를 생략한다.
전체 실행 모드는 **16개 성공, 명시적 생략 1개**이다. B5를 생략한다.
실패 시 추적 파일과 게스트 로그를 보존한다.
테스트 등록 정보와 리소스를 모두 삭제한다.

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

## 제거

UFW, 네트워크 1개, 실행 중인 VM이 있는 일회용 Linux 호스트를 사용한다.
ACL·전달 설정·방화벽 규칙을 설치 전 기록과 비교한다.

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

정리 후 VM·네트워크·스토리지·셸 목록을 확인한다.
로그와 결과를 보존한다.
[QA 스크립트 범위](qa.md#script-coverage)를 참고한다.

- [ ] **X1:** QA VM이 남지 않는다.
- [ ] **X2:** QA 네트워크가 남지 않는다.
- [ ] **X3:** QA 스토리지 풀이 남지 않는다.
- [ ] **X4:** QA shell이 남지 않는다.
- [ ] **X5:** 사용자 지정 OCI alias가 없고 카탈로그 fixture는 명시적으로 보존할 때만 남긴다.
- [ ] **X6:** QA Docker Hub 비밀값이 남지 않고 기존 로그인이 복원된다.
- [ ] **X7:** QA VM의 `firecrab-vm-*` unit이 남지 않는다.
- [ ] **X8:** QA VM 정지·삭제 후 포워딩된 TCP 포트가 닫힌다.
- [ ] **X9:** 수동 변경한 QA DNS·관리 SSH 설정을 복구하고 로그와 결과 요약을 보관한다.

## 관련 문서

- [CI](ci.md)
- [과거 검증 기록](validation-history.md)
- [기여 가이드](../../CONTRIBUTING.md)
