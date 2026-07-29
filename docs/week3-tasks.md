---
tags:
  - firecrab
  - week3
  - network
status: 완료
updated: 2026-07-29
---

# 3주차 — Network, SSH, UI

> [!summary] 목표
> VM이 실제 IP로 외부와 통신하고,
> 브라우저에서 생성·관리·접속까지 되는 상태 — 달성.
> [MicroNetwork](task-micro-network.md)는 쓸 수 있는 상태까지 완료 —
> 격리 강화(VRF 등)는 [4주차](week4-tasks.md) 플랜.

## 원칙

- API는 unprivileged — 권한이 필요한 host 작업만 [net-helper](task-host-network-privileges.md)로 분리
- Week 2의 operation·오류 계약을 그대로 사용
- 문서의 코드 조각은 설계 골격 — workspace 고정 crate 버전으로 compile/test 후 적용
- 각 항목의 작업 내용·완료 기준·산출물은 링크된 task 문서에 있음

## 완료 — 네트워크

- [x] [호스트 네트워크 권한 분리](task-host-network-privileges.md)
      — bridge/TAP/firewall만 helper에서, UUID로 검증된 자원만 조작
- [x] [공용 bridge·subnet·gateway](task-shared-bridge-network.md)
      — idempotent, 재부팅 후에도 같은 구성으로 복구
- [x] [IP·MAC 할당(IPAM)](task-vm-ip-mac-allocation.md)
      — SQLite 원자 할당, stop 중 유지 delete 후 반환
- [x] [NAT·forwarding 자동화](task-nat-firewall-automation.md)
      — 전용 nftables table, 기존 host 규칙 보존
- [x] [격리·anti-spoofing](task-vm-network-isolation.md)
      — east-west 기본 거부, lease 기반 source 검증
- [x] [VM별 TAP 자동화](task-vm-tap-automation.md)
      — start 시 생성·bridge 연결, stop/delete/실패 시 정리
- [x] [Guest 네트워크 설정(DHCP)](task-guest-network-configuration.md)
      — DB의 IP = Firecracker MAC = guest `eth0`
- [x] [네트워크 구성 대시보드](task-network-configuration-dashboard.md)
      — `GET /api/network`·`/api/host` 조회 + VM별 egress 정책 선택
- [x] [MicroNetwork — 가상 네트워크](task-micro-network.md)
      — 사용자가 만드는 가상 네트워크. 네트워크별 bridge·서브넷·DHCP·NAT·방화벽,
      VM 소속, daemon 시작 시 재적용, 네트워크별 인터넷 on/off
      — VRF·네트워크별 uplink·2계층 분리는 [4주차](week4-tasks.md) 플랜

## 완료 — VM·UI·빌드

- [x] [MicroVM 부팅 + Terminal UI](task-microvm-terminal.md) — serial console을 WebSocket으로
- [x] [VM 대시보드](task-vm-dashboard.md) — 목록/생성/시작/중지/삭제 + 상태 polling
- [x] [시작 단계별 진행 표시](task-vm-startup-progress.md)
- [x] [디스크 용량 설정](task-vm-disk-capacity.md) — `resize2fs`로 실제 확장
- [x] [CPU·MEM·DISK 수정](task-vm-resource-update.md) — 다음 시작에 반영
- [x] [Alpine Linux 템플릿](task-alpine-linux-template.md)
- [x] [배포판 표준 커널](task-distro-standard-kernels.md) — Alpine은 initrd 필요
- [x] [CI(GitHub Actions)](task-cicd-github-actions.md) — fmt·clippy·test+coverage·rustdoc·frontend


### 우선순위 조정

| 날짜 | 내용 |
|---|---|
| 2026-07-19 | 네트워크보다 MicroVM 부팅 + Terminal UI를 선행 — serial console은 네트워크와 무관 |
| 2026-07-20 | 네트워크는 TAP 자동화 + Guest DHCP까지만 — 이미 있는 NAT/CIDR/IPAM을 실제 VM에 연결하는 최소 구성 |
| 2026-07-21 | 대회 일정상 SSH 계열(guest agent·identity·접속 API·통합 테스트)을 범위 밖으로 보류 |

### 구현

| 날짜 | 내용 |
|---|---|
| 2026-07-21 | 시작 진행 표시, 디스크 용량, 리소스 수정 |
| 2026-07-22 | Alpine 템플릿, CI 3-job, rustdoc 84.1%, TAP 자동화, Guest DHCP |
| 2026-07-23 | 네트워크 구성 대시보드 — `GET /api/network`·`/api/host`, VM별 egress 정책 |
| 2026-07-24 | 배포판 표준 커널, 패키지 업데이트 API, MicroNetwork 1·2단계(CIDR 예약 → 실제 bridge) |
| 2026-07-29 | MicroNetwork 3단계 — 네트워크 서비스 5종 파라미터화, daemon 시작 시 재적용, 네트워크별 인터넷 on/off |

### 실사용에서 나온 버그 (2026-07-24)

- [동시 부팅 시 멀쩡한 VM이 SIGKILL됨](bugs/vm-killed-mid-boot-under-concurrent-load.md)
      — broadcast의 `Lagged`(컨슈머 지연)를 `Closed`로 오인
- [DHCP가 거의 항상 실패](bugs/dhcp-never-reaches-guest.md)
      — 원인 3건: dnsmasq 설정/hosts 파일 경로 충돌, `dhcp-hostsfile` 접두어, IP 빠른 재사용
- [Alpine만 `no-ipv4-address`](bugs/alpine-network-ready-races-dhcpcd.md)
      — OpenRC `after dhcpcd`는 시작 순서만 보장, 완료를 보장하지 않음
- [VM 아웃바운드 타임아웃](bugs/vm-outbound-forward-blocked-by-ufw.md)
      — 호스트 UFW가 라우팅을 기본 거부. 코드가 아니라 호스트 설정 문제
- [Alpine 공식 커널 부팅 실패](bugs/alpine-official-kernel-cant-mount-root.md)
      — ext4가 모듈이라 `rootfstype=ext4` 없이는 root mount 실패

## 검증 상태

- `cargo fmt` / `clippy --all-targets` / `test --workspace` (154/20/16/56) green
- `RUSTDOCFLAGS=-D warnings cargo doc` clean
- 프론트엔드 `tsc -b` / `oxlint` / `vite build` 통과
- 테스트 절차는 [MicroNetwork 테스트 문서](tests/micro-network.md) 등 `docs/tests/` 참고
