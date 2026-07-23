# 3주차 권장 Tasks - Network, SSH, and UI

- Week 2의 VM별 rootfs, 리소스, process lifecycle과 상태 복구가 완료된 뒤 진행함
- API와 host network helper는 Rust로 구현함
- 권한이 필요한 host 작업은 unprivileged HTTP API와 분리함
- Week 2의 공통 operation과 오류 계약을 그대로 사용함
- 코드 조각은 설계 골격이며 workspace 고정 crate version으로 compile/test해 적용함
- 2026-07-19 우선순위 재조정: 네트워크(TAP 이하)보다 MicroVM 부팅 + Terminal UI를 선행. serial console은 네트워크와 무관해 데모 가능한 최소 경로. 남은 network v2(TAP~접속 API)는 후순위로 유지
- 2026-07-20 재조정: VM 관련 작업(시작 단계별 진행 상황 표시)을 다음 순위로 먼저 하고, 그다음 네트워크는
  TAP 자동화 + Guest 네트워크 설정(DHCP/eth0)까지만 진행한다 — 이미 구현된 NAT/VPC CIDR(bridge
  subnet)/VM IP 할당(IPAM)이 실제 VM에 연결되어 동작하게 만드는 최소 구성. Guest agent/vsock, SSH
  identity, 접속 정보 API는 계속 후순위로 유지
- 2026-07-21: VM 시작 단계별 진행 상황 표시 완료(생성 즉시화 + 상세 모달로 재구현, `docs/tests/vm-detail-modal.md`).
  같은 날 VM 디스크 용량 설정 추가(`task-vm-disk-capacity.md`, 별도 브랜치 `feat/vm-disk-capacity`).
- 2026-07-21(계속): 구축된 VM의 cpu/ram/disk 수정 기능 추가(`task-vm-resource-update.md`, 별도
  브랜치 `feat/vm-resource-update`)
- 2026-07-21(계속): `feat/microvm-terminal`을 `feat/vm-resource-update`에 병합 — `terminal` 버튼
  연결 끊김 버그 수정. 아래 표·각주 갱신
- 2026-07-21(계속): 이미지 템플릿 Alpine Linux 추가를 다음 순위로 등록(`task-alpine-linux-template.md`)
- 2026-07-21(계속) 네트워크 범위 최소화: 대회 일정상 남은 네트워크 작업을 "VM이 실제 IP로 외부와
  통신 가능"까지로 좁힌다 — TAP 자동화 + Guest DHCP 적용 2개만 다음 순위로 유지. Guest agent/vsock,
  SSH identity, 접속 정보 조회 API, 통합 테스트는 **이번 대회 범위 밖으로 보류**(브라우저 터미널로
  이미 guest 접속이 가능해 데모에 필수는 아님) — 재개할 경우를 대비해 표는 남겨두되 상태만 구분
- 2026-07-22: 이미지 템플릿 Alpine Linux 추가 완료. Alpine minirootfs(v3.24.1)를 Docker의
  `apk --root`로 구성해 ext4 이미지로 패키징(host root/sudo 불필요 — `scripts/firecracker-menual/install-alpine-rootfs.sh`),
  기존 Ubuntu 템플릿과 커널을 공유. `templates.rs`에 `alpine-3.24` alias 등록, `CreateVm.tsx`
  드롭다운에 추가. 실제 API+프론트 엔드투엔드(생성→start→console.log)로 Alpine 부팅과 Ubuntu
  무회귀를 모두 확인(`docs/tests/alpine-linux-template.md`). `cargo test --workspace` 83/9/9/31 green
- 2026-07-22(계속): CI(GitHub Actions) 구현. `rust`(fmt/clippy/`cargo-llvm-cov`+Codecov 업로드),
  `docs`(`RUSTDOCFLAGS=-D warnings cargo doc`), `frontend`(oxlint/tsc/vite build) 3개 job.
  `rust-toolchain.toml`에 `llvm-tools` 컴포넌트 추가, 기존 미포맷 코드 `cargo fmt --all`로 정리.
  `CODECOV_TOKEN` 시크릿 등록은 GitHub 저장소 설정에서 별도 필요(`task-cicd-github-actions.md`)
- 2026-07-22(계속): rustdoc 문서화율을 75% 목표까지 끌어올림(별도 브랜치 `feat/rustdoc-coverage`,
  main에서 분기). `firecrab-api-types`/`firecrab-helper-protocol` 100%, `firecrab-net-helper`
  97.2%, `firecrab-api` 76.2% — 워크스페이스 전체 84.1%(503/598 아이템). `cargo fmt --all` +
  `cargo clippy --workspace --all-targets` + `cargo test --workspace`(83/9/9/31) +
  `RUSTDOCFLAGS=-D warnings cargo doc` 전부 green 확인
- 2026-07-22(계속): VM별 TAP 자동화 완료 후 코드 리뷰에서 나온 실제 버그 3건 수정(exit monitor가
  guest poweroff/kill 시 네트워크 정리를 안 하던 것, `create_tap`의 이름 충돌 시 소유권 미확인,
  `setup_vm_network`의 policy 실패 시 TAP만 정리하던 것) + Codecov patch coverage 78%→약 91%로
  보강. 이어서 Guest 네트워크 설정(DHCP) 구현: lease 세대(`PRAGMA user_version`) 도입,
  `firecrab-net-helper/src/dhcp.rs`(dnsmasq 자식 프로세스 supervise, hosts 파일 원자 교체+
  `dnsmasq --test` 검증+reload), `specialize_guest`(실제 ext4 이미지에 `debugfs -w`로 hostname
  기록·SSH host key/random-seed 제거, root 불필요), guest agent 없이 serial console sentinel
  (`FIRECRAB_NETWORK_READY`/`FIRECRAB_NETWORK_FAILED`)로 network readiness 판정(`StartupStep::
  ConfiguringNetwork`), golden image 스크립트 갱신(Ubuntu 고정 IP→DHCP 전환, 두 배포판 모두
  sentinel 유닛/init.d 추가). `cargo test --workspace` 104/9/12/39 green, 프론트엔드
  `tsc -b`/`oxlint`/`vite build` 전부 통과. 실제 dnsmasq bind+lease 발급, 실제 golden image
  재빌드 후 부팅은 root/CAP_NET_ADMIN 필요라 미검증(`docs/guest-network-smoke.md` 참고)
- 2026-07-23: 네트워크 구성 대시보드 구현 완료(`task-network-configuration-dashboard.md`). `EgressPolicy`를
  `firecrab-api-types`로 옮겨 VM 생성·수정 화면에서 Internet/Isolated를 실제로 고를 수 있게 하고(기존엔
  `setup_vm_network`가 항상 `EgressPolicy::default()`로 고정), VM 상세에 실제 할당된 ip/mac과 결정론적
  hostname을 노출. `GET /api/network`(bridge/subnet/gateway — 고정값 노출만, 런타임 편집은 IPAM/bridge
  재구성이 필요해 범위 밖으로 유지)와 `GET /api/host`(load average/memory/disk/uptime, `/proc`+`df -kP`
  기준)를 신규 추가하고 헤더 "HOST 정보" 버튼+모달로 노출. 검증 중 발견한, 이번 기능과 무관한 기존
  clippy 실패 4건(dead_code 2건, let_and_return, suspicious_open_options, unused Json)도 같이 정리.
  `cargo test --workspace` 112/16/12/39 green, `cargo llvm-cov`로 신규 `handlers/network.rs` 커버리지
  확인 후 74.75%→100%(라인 기준)까지 테스트 보강. 프론트엔드 `tsc -b`/`oxlint`/`vite build` 통과, Playwright로
  실제 dev 서버 골든 패스 확인(VM 생성 시 격리 정책 선택 → 상세에서 ip/mac/hostname/정책 확인 → 수정 →
  HOST 정보 모달에서 실제 값 확인)
- 2026-07-24: 실사용 중 발견한 버그 4건 수정 + OS 패키지 업데이트 기능 추가.
  콘솔 브로드캐스트 채널의 `Lagged`(컨슈머 지연)를 `Closed`(진짜 종료)와 구분 못 해 여러 VM
  동시 부팅 시 멀쩡한 VM을 SIGKILL하던 버그(`docs/bugs/vm-killed-mid-boot-under-concurrent-load.md`),
  DHCP가 거의 항상 실패하던 원인 3건 추가 발견·수정(dnsmasq base config/hosts 파일 경로 충돌,
  `dhcp-hostsfile` 잘못된 접두어, IP 빠른 재사용 시 dnsmasq 예전 리스와 충돌 —
  `docs/bugs/dhcp-never-reaches-guest.md`), Alpine 템플릿만 매번 `no-ipv4-address`로 실패하던
  버그(OpenRC `after dhcpcd`가 시작 순서만 보장, dhcpcd 완료를 보장하지 않음 —
  `docs/bugs/alpine-network-ready-races-dhcpcd.md`), VM 내부에서 나가는 새 아웃바운드 연결이
  타임아웃되던 버그(호스트 UFW가 라우팅을 기본 거부, 프로젝트 자체 nftables와 별개로 독자 drop —
  `docs/bugs/vm-outbound-forward-blocked-by-ufw.md`, 코드 아닌 호스트 설정 문제)까지 총 4건.
  `POST /api/vms/{id}/packages/update`(`firecrab-api/src/handlers/packages.rs`)로 실행 중 VM에
  apt/apk update·upgrade를 콘솔 sentinel 방식으로 실행하는 기능도 추가(guest agent 없이
  `wait_for_network_ready`와 같은 패턴 재사용) — 실제 Ubuntu/Alpine VM에서 exit code 0으로 검증,
  프론트엔드 버튼은 아직 없음(백엔드만). CI(GitHub Actions)에 `dnsmasq` 미설치로 나던 테스트 실패
  수정, Codecov patch coverage 90.00%→92.24%로 보강. `cargo test --workspace` 126/18/12/46 green

| 상태 | 제목 | 작업 | 완료 기준 | 산출물 |
|---|---|---|---|---|
| ✅ | [호스트 네트워크 권한 및 자원 소유권 구현](task-host-network-privileges.md) | API 전체를 root로 실행하지 않고 bridge, TAP, firewall 작업만 제한된 helper에서 수행한다. | 검증된 UUID 기반 자원만 조작하고 Firecrab 소유가 아닌 interface와 firewall rule은 변경하거나 삭제하지 않는다. | `firecrab-api/src/network.rs`, `firecrab-net-helper/src/main.rs`, `docs/net-helper.md` |
| ✅ | [공용 bridge 및 네트워크 기반 구현](task-shared-bridge-network.md) | Firecrab 전용 bridge, subnet(VPC CIDR), gateway와 host forwarding 구조를 idempotent하게 준비한다. | 반복 실행과 host 재부팅 후에도 동일한 bridge 구성이 복구되며 여러 VM이 공용 gateway를 사용한다. | `firecrab-net-helper/src/bridge.rs` |
| ✅ | [VM IP 및 MAC 할당 관리 구현](task-vm-ip-mac-allocation.md) | SQLite에서 VM별 IP와 MAC을 원자적으로 할당하고 중복과 pool 고갈을 처리한다. | 동시에 여러 VM을 생성해도 IP와 MAC이 중복되지 않는다. 할당값은 stop 동안 유지되고 delete 성공 후 반환된다. | `firecrab-api/src/ipam.rs`, `firecrab-api/src/model.rs`, `firecrab-api/src/persistence.rs` |
| ✅ | [외부 통신 NAT 및 firewall 자동화 구현](task-nat-firewall-automation.md) | subnet 단위 NAT와 forwarding rule을 전용 nftables table로 관리한다. | 여러 VM이 동시에 외부 통신하고 한 VM의 stop/delete가 다른 VM의 연결을 끊지 않는다. 기존 host firewall rule은 보존된다. | `firecrab-net-helper/src/firewall.rs` |
| ✅ | [VM network 격리 및 anti-spoofing 구현](task-vm-network-isolation.md) | VM 간 통신과 host·reserved subnet 접근을 기본 거부하고 lease 기반 source 검증을 적용한다. | 허용 정책 없는 east-west·spoofed traffic이 차단되고 DNS, gateway, 명시적 SSH와 외부 응답 traffic만 통과한다. | `firecrab-api/src/network_policy.rs`, `firecrab-net-helper/src/firewall.rs` |
| ✅ | [MicroVM 부팅 + 대시보드 Terminal UI 구현](task-microvm-terminal.md) | net-helper 없이 VM을 부팅하고, 대시보드에서 serial console(ttyS0)에 실시간 접속한다. | `terminal` 버튼으로 실제 부팅 로그·셸이 브라우저에 보이고 타이핑이 guest에 도달하며, WS는 REST 타임아웃과 무관하게 유지된다. | `firecrab-api/src/console.rs`, `firecrab-api/src/handlers/console.rs`, `firecrab-frontend/src/components/Console.tsx` |
| ✅ | [프론트엔드 VM 대시보드 구현](task-vm-dashboard.md) | 목록, 생성, 시작, 중지, 삭제와 상태 polling을 제공한다(React/TypeScript로 재구현, 원래 Wasm 계획에서 전환). | 상태별 action만 활성화되고 중복 클릭, 전이 상태, `409`와 비동기 실패가 사용자에게 표시된다. | `firecrab-frontend/src/App.tsx`, `firecrab-frontend/src/components/`, `docs/browser-test.md` |
| ✅ | [VM 시작 단계별 진행 상황 표시 구현](task-vm-startup-progress.md) | `starting` 상태를 rootfs 준비·Firecracker 기동·부팅 확인 등 이름 붙은 단계로 나눠 로그·UI에 노출한다. | 단계 전환이 순서대로 로그에 남고, 대시보드에서도 폴링 주기 내에 단계별로 반영된다. | `firecrab-api-types/src/lib.rs`, `firecrab-api/src/firecracker.rs`, `firecrab-api/src/rootfs.rs`, `firecrab-frontend/src/components/VmDetailModal.tsx` |
| ✅ | [VM 디스크 용량 설정 구현](task-vm-disk-capacity.md) | VM 생성 시 디스크 용량(GiB)을 지정하고 rootfs 템플릿 복사 후 `e2fsck`+`resize2fs`로 실제 확장한다. | 지정한 용량으로 디스크가 만들어지고 guest가 그 크기를 실제로 인식하며, 템플릿 크기 미만은 검증 오류로 거부된다. | `firecrab-api-types/src/lib.rs`, `firecrab-api/src/rootfs.rs`, `firecrab-api/src/handlers/vms.rs`, `firecrab-frontend/src/components/CreateVm.tsx` |
| ✅ | [구축된 VM CPU/MEM/DISK 수정 구현](task-vm-resource-update.md) | `PUT /api/vms/{id}`로 프로세스가 안 떠 있는 VM의 cpu/ram/disk를 다음 시작에 반영되게 수정한다. | `running`/`starting`/`stopping`은 거부되고, disk는 축소가 거부되며, 수정 후 시작하면 Firecracker config와 실제 디스크 크기에 반영된다. | `firecrab-api-types/src/lib.rs`, `firecrab-api/src/handlers/vms.rs`, `firecrab-api/src/rootfs.rs`, `firecrab-frontend/src/components/VmDetailModal.tsx` |
| ✅ | [이미지 템플릿 Alpine Linux 추가](task-alpine-linux-template.md) | Alpine 커널·rootfs를 두 번째 템플릿으로 등록하고 생성 폼에서 고를 수 있게 한다. | Alpine 선택 시 실제로 Alpine 커널이 부팅되고, 기존 Ubuntu 템플릿 동작에 회귀가 없다. | `firecrab-api/src/templates.rs`, `firecrab-frontend/src/components/CreateVm.tsx`, `images/` |
| ✅ | [CI 구현(GitHub Actions)](task-cicd-github-actions.md) | PR/push마다 fmt·clippy·test+coverage(Codecov)·rustdoc(Rust)와 lint·typecheck·build(frontend)를 자동 검증한다. | `rust`/`docs`/`frontend` 3개 job 모두 green(rustdoc 문서화율 게이트 75% 대비 `feat/rustdoc-coverage`에서 84.1% 달성). | `.github/workflows/ci.yml`, `codecov.yml`, `rust-toolchain.toml` |
| ✅ | [네트워크 구성 대시보드](task-network-configuration-dashboard.md) | 웹 대시보드에서 host 네트워크 정보(bridge/subnet/gateway)와 host 리소스 상태를 보여주고, VM별 egress 정책을 생성·수정 화면에서 선택할 수 있게 한다. | 대시보드에서 실제 bridge/subnet/gateway와 host load average/memory/disk/uptime을 확인할 수 있고, VM 생성·상세 화면에서 egress 정책을 보고 바꿀 수 있다. subnet/uplink 자체의 런타임 편집은 범위 밖(IPAM/bridge 재구성 필요, 후속 작업으로 유지). | `firecrab-api/src/handlers/network.rs`, `firecrab-api-types/src/lib.rs`, `firecrab-frontend/src/components/HostInfoModal.tsx`, `firecrab-frontend/src/components/CreateVm.tsx`, `firecrab-frontend/src/components/VmDetailModal.tsx` |

### 네트워크 — 최소 범위(VM이 실제 IP로 외부와 통신 가능한 상태까지)

| 상태 | 제목 | 작업 | 완료 기준 | 산출물 |
|---|---|---|---|---|
| ✅ | [VM별 TAP 디바이스 자동화 구현](task-vm-tap-automation.md) | VM start 시 고유 TAP을 생성해 bridge에 연결하고 stop, delete, 실패 시 정리한다. | 두 VM이 서로 다른 TAP을 사용하며 daemon 복구 후에도 고아 TAP이 남지 않는다. | `firecrab-api/src/network.rs`, `firecrab-net-helper/src/tap.rs` |
| ✅ | [Guest 네트워크 설정 적용 구현](task-guest-network-configuration.md) | DHCP와 MAC 예약으로 할당 IP를 Guest `eth0`에 적용한다. | DB IP, Firecracker MAC, Guest `eth0` 주소가 일치하고 gateway와 DNS 설정이 재부팅 후에도 유지된다. | `firecrab-api/src/dhcp.rs`, `firecrab-net-helper/src/dhcp.rs`, Guest template 설정 |

### 네트워크 이후

| 상태 | 제목 | 작업 | 완료 기준 | 산출물 |
|---|---|---|---|---|
| 미완료 (네트워크 이후) | [배포판 표준 커널 사용](task-distro-standard-kernels.md) | Ubuntu/Alpine 템플릿이 공유하는 자체 빌드 vanilla 커널 대신, 각 배포판이 실제 배포하는 공식 커널(`linux-image-generic`, `linux-virt`)을 추출해 쓴다. | 두 템플릿 모두 실제 배포판 공식 커널로 부팅되고 기존 동작에 회귀가 없다(Alpine은 virtio_blk/ext4가 모듈이라 initrd 필요). | `firecrab-api/src/templates.rs`, `firecrab-api/src/firecracker.rs`, `images/kernel/` |
| 미완료 (네트워크 이후) | [네트워크 구성 대시보드 — 다중 subnet/uplink 편집](task-network-configuration-dashboard.md) | (읽기 전용 조회 + VM별 egress 정책은 위 표에서 ✅ 완료됨) 대시보드에서 host 네트워크(subnet/uplink) 자체를 **직접 설정**하고, 여러 네트워크 중 VM이 속할 네트워크를 명시적으로 고를 수 있게 한다 — IPAM/bridge 다중화 리팩터가 선행돼야 함. | 대시보드에서 네트워크 설정을 바꿀 수 있고, VM 생성·상세 화면에서 소속 네트워크(복수 중 하나)를 확인할 수 있다. | `firecrab-api/src/handlers/network.rs`, `firecrab-api-types/src/lib.rs`, `firecrab-net-helper/src/nat.rs`, `firecrab-frontend/src/components/` |

### 네트워크 — 범위 밖으로 보류 (재개 시 참고용, 이번 대회 일정에는 포함 안 함)

| 상태 | 제목 | 보류 사유 |
|---|---|---|
| 범위 밖 | [Guest agent 및 vsock provisioning 구현](task-guest-agent-vsock-provisioning.md) | Guest agent 자체가 별도 crate(`firecrab-guest-protocol`/`firecrab-guest-agent`) 신규 개발이 필요한 큰 작업. 브라우저 터미널로 이미 guest 접속 가능해 데모엔 불필요 |
| 범위 밖 | [VM별 SSH identity 및 접근 정책 구현](task-vm-ssh-identity.md) | 위 Guest agent가 선행돼야 키 배포 경로가 생김 |
| 범위 밖 | [VM 접속 정보 조회 API 구현](task-vm-connection-api.md) | SSH 미지원 상태에서는 조회할 접속 정보 자체가 없음 |
| 범위 밖 | [Network, SSH, UI 통합 테스트 구현](task-network-ssh-ui-tests.md) | 위 항목들이 모두 범위 밖이라 테스트 대상 자체가 없음 — TAP·Guest DHCP는 각 task 문서의 자체 완료 기준으로 검증 |

`feat/microvm-terminal`은 `feat/vm-resource-update`에 병합 완료(충돌 1건, `server.rs`의 라우트
목록 — 손으로 정리). `cargo test --workspace` 82/9/8/27 green, 실제 VM으로 터미널 연결·부팅 로그
렌더링·키보드 입력 왕복까지 확인됨(자세한 내용은 [web.md](web.md) 참고).
