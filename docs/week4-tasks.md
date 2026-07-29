---
tags:
  - firecrab
  - week4
  - micro-network
  - micro-storage
  - m2image
  - host
  - isolation
  - observability
status: 진행 중
updated: 2026-07-29
---

# 4주차 — MicroNetwork · MicroStorage · M2Image · Host · 격리 · 관측

> [!summary] 목표
> 3주차까지 "동작하는 것"을 만들었다면, 4주차는 **남에게 넘길 수 있는 것**으로 만든다.
> 설치(Host)·이미지(M2Image)·스토리지 선택을 갖추고,
> MicroNetwork 격리를 규칙이 아닌 구조로 끌어올리고, 실행 경계와 관측을 붙인다.
> snapshot은 [7주차](week7-tasks.md)로 이월 — 격리·관측이 없으면 실패를 진단할 수 없기 때문.

## 개념 참조

| AWS | firecrab | 설명 |
|---|---|---|
| EC2 | **M2 (MicroMachine)** | MicroVM 기반 컴퓨팅 |
| VPC | **MicroNetwork** | 가상 네트워크 |
| EBS | **MicroStorage** | 영구 스토리지 |
| AMI | **M2Image** | 인스턴스 이미지 |
| Snapshot | **Snapshot** | 스토리지 백업 |
| RDS | **MicroDB** | 관리형 데이터베이스 |

## 원칙

- 문서의 코드 조각은 설계 골격 — 지원 Firecracker·kernel·crate 버전으로 compile/KVM 테스트 후 적용
- 각 항목의 작업 내용·완료 기준·산출물은 링크된 task 문서에 있음
- Host → MicroStorage → MicroNetwork → M2Image → 격리 → 관측 순서.
  그다음이 snapshot([7주차](week7-tasks.md))
- Host가 먼저인 이유: 데몬으로 안 뜨면 나머지를 재부팅·재현 환경에서 검증할 수 없음
- MicroNetwork는 [3주차](week3-tasks.md)에서 쓸 수 있는 상태까지 갔고, 4주차는 그 격리를 단단하게 하는 단계

## Host — 설치·데몬 기반

터미널 **3개**(net-helper / API / 프론트 dev 서버)를 손으로 띄우던 것을
**설치 한 번 + 데몬 2개**로 바꾼다. 아래 셋은 2026-07-29 구현 완료 — 검증 절차는
[tests/host-install](tests/host-install.md).

- [x] [Host 설치 — `install.sh`](task-host-install-script.md)
      — **네트워크만 되는 머신에서 한 줄**: 없는 의존성(nft·dnsmasq·firecracker·rustup·node·docker)과
      게스트 이미지까지 스스로 채우고 데몬 2개를 띄움. `--check`는 비특권·무변경
      — 실 호스트 전체 설치는 아직 미검증(버려도 되는 머신 필요)
- [x] [Host 데몬 — systemd 유닛](task-host-systemd-daemons.md)
      — `packaging/systemd/` 템플릿 2종. helper의 `Group=`이 소켓 접근을 좌우, API는 `WorkingDirectory` 고정
      — 실 호스트 기동은 미검증
- [x] [프론트엔드 서빙](task-host-frontend-serving.md)
      — `FIRECRAB_STATIC_ROOT`로 `dist/` 서빙 + SPA fallback, `/api`·`/ws`는 JSON 404 유지
      — 실제로 dev 서버 없이 대시보드 동작 확인
- [ ] [Host 진단 — `firecrab doctor`](task-host-doctor.md)
      — KVM·ip_forward·nft·dnsmasq·UFW·소켓 권한·이미지 digest를 한 번에 점검
      — 지금까지의 실패가 대부분 코드가 아니라 host 설정이었음

## MicroStorage — 영구 스토리지

> [!important] 2026-07-21 실측 — 다음 순위
> VM을 동시에 시작하면 디스크 I/O가 한 물리 디스크에 몰려 병목이 된다
> (`iostat` `%util` ~100%, `w_await` 수백 ms — NVMe 하드웨어 자체는 문제 없음).
> 자세한 내용은 [vm-startup-stuck-under-concurrent-load](bugs/vm-startup-stuck-under-concurrent-load.md).

- [ ] [VM 생성 시 물리 디스크 선택](task-vm-physical-disk-selection.md)
      — rootfs를 등록된 여러 디스크 중 지정한 위치에 생성
      — 여유 공간 부족은 생성 시점에 거부
- [ ] [disk generation·artifact ledger](task-vm-rootfs-and-artifacts.md)
      — 디스크 세대 관리. [7주차](week7-tasks.md) snapshot lineage의 기반

## MicroNetwork — 네트워크 격리 강화

[3주차](week3-tasks.md)에서 리소스·서비스 파라미터화·VM 소속·재적용·인터넷 토글까지 끝나
**쓸 수 있는 상태**가 됐다. 아래는 그 격리를 규칙 의존에서 구조 의존으로 바꾸는 단계 —
셋은 서로 독립이라 순서 없이 진행 가능.

- [ ] [VRF — 네트워크별 라우팅 테이블 분리](task-micro-network-vrf.md)
      — nftables deny 규칙을 지운 상태에서도 다른 네트워크에 도달 못 할 것
      — 기존 규칙은 유지(대체가 아니라 이중 방어)
- [ ] [네트워크별 uplink 지정](task-micro-network-uplink.md)
      — 지금은 host의 기본 경로 하나를 전부 공유
      — 선행: uplink가 2개 이상인 검증 환경
- [ ] [호스트 방화벽(UFW) 연동 정책](task-micro-network-host-firewall.md)
      — 자동 삽입 여부를 **먼저 결정**하는 태스크
      — 어느 쪽이든 실패 원인이 로그·응답에서 즉시 드러날 것
- [ ] [2계층 분리 (MicroNetwork / Subnet)](task-micro-network.md) — 조건부 보류
      — 멀티 host 확장이나 용도별 대역이 필요해지면 재검토

## M2Image — 인스턴스 이미지

서버에 [템플릿 레지스트리](task-template-registry.md)가 있는데도 대시보드의 목록은
프론트엔드 코드 상수다. 이미지를 코드 수정 없이 다룰 수 있게 만든다.

- [ ] [M2Image 카탈로그 API](task-m2image-catalog-api.md)
      — `GET /api/images`로 레지스트리 노출, `CreateVm.tsx`의 하드코딩 목록 제거
- [ ] [M2Image 등록·삭제](task-m2image-registration.md)
      — 재빌드·재시작 없이 이미지 추가, 사용 중인 이미지는 삭제 거부
- [ ] [M2Image 캡처](task-m2image-capture-from-vm.md)
      — 설정을 끝낸 VM의 디스크를 새 이미지로(AMI 생성 대응). hostname·SSH host key 정리 포함

> [!note] 재현 가능한 빌드는 5주차
> 이미지의 서명·승격·재현 빌드는 [5주차](week5-tasks.md) 범위.
> 4주차는 "있는 이미지를 다룰 수 있게" 까지.

## Firecracker - 실행 격리

 프로세스·파일시스템 경계.

- [ ] [Firecracker Jailer 격리](task-firecracker-jailer-isolation.md)
      — VM별 chroot + 비특권 UID/GID. API는 root가 아니어야 함
- [ ] [cgroup 리소스 회계·제한](task-cgroup-resource-governance.md)
      — cgroup v2로 CPU/memory/process(+block I/O). 종료·복구 후 stale cgroup 없음
- [ ] [VM runtime 권한·filesystem 격리](task-vm-runtime-permissions.md)
      — artifact 권한, symlink 방어, seccomp, 실행 파일 검증
- [ ] [pidfd 기반 process identity 복구](task-vm-state-recovery.md)
      — daemon 재시작 후 그 pid가 정말 그 VM인지 확인([프로세스 관리](task-firecracker-process-management.md)와 함께)
      — 지금은 pid 재사용을 구분할 방법이 없음

## MicroObservability — 수집·관측

- [ ] [Firecracker metrics 수집](task-firecracker-metrics-collection.md)
      — helper가 metrics FIFO를 drain → bounded 지표 + Prometheus endpoint
      — 느린 consumer가 Firecracker를 막지 않을 것
- [ ] [구조화된 logging·tracing](task-structured-logging-and-tracing.md)
      — request ID ↔ VM ID를 잇는 JSON log/span, secret은 남기지 않음
- [ ] [서비스 health·readiness API](task-service-health-readiness.md)
      — `/health/live`(프로세스 생존) / `/health/ready`(DB·KVM·helper·template) 분리
- [ ] [VM 관측 대시보드](task-observability-dashboard.md)
      — CPU/memory/I/O/network·오류·의존성 상태. bounded polling + downsampling
- [ ] [lifecycle event log API](task-lifecycle-log-api.md)
      — 상태 전이·실패를 조회 가능한 이벤트로. 대시보드가 "왜 error인지"를 보여주려면 필요
- [ ] [SQLite 확장 스키마](task-sqlite-migration-and-state-model.md)
      — 위 이벤트 로그의 저장소(events / runtime instances). 선행 작업

## 통합 테스트·기타

- [ ] [격리·관측 통합 테스트](task-isolation-observability-snapshot-tests.md)
      — 권한 탈출, 자원 초과, daemon 재시작, UI 재동기화
      — 같은 문서의 snapshot replay 시나리오는 [7주차](week7-tasks.md)
- [ ] [lifecycle 통합 테스트 suite](task-lifecycle-api-tests.md)
      — 생성→시작→중지→삭제 왕복과 실패·복구를 API 레벨에서 자동 검증
- [ ] [패키지 최신 상태 확인·알림](task-package-update-notification.md)
      — guest agent가 패키지 버전을 구조화된 형태로 보고(AWS SSM Patch Compliance 대응)
      — 지금은 콘솔 텍스트 파싱 방식의 실행 API만 있음
