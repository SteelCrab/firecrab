---
tags:
  - firecrab
  - security
status: 미완료
scope: 5주차
updated: 2026-07-23
---

# Release 보안 및 운영 검증 구현

## 브랜치 개요

- 브랜치: `feat/release-security-and-operations` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: Rust 품질 gate, dependency·license 정책, SBOM, artifact signature와 운영 부하·장애 test를 release 조건으로 만듦.

## CI gate

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
cargo check -p firecrab-frontend --target wasm32-unknown-unknown
(cd firecrab-frontend && trunk build --release)
cargo deny check
cargo audit
```

- release build는 lockfile 고정, isolated runner, pinned toolchain과 commit SHA로 고정된 CI action을 사용함.
- binary와 package별 SHA-256, CycloneDX SBOM, build provenance를 생성하고 release key로 서명함.

- KVM, root helper와 host network 권한이 있는 self-hosted runner에서는 fork PR이나 승인되지 않은 code를 실행하지 않음.
- 일반 PR은 fake runtime job만 사용하고 실제 KVM/release job은 보호된 branch의 검증된 commit에 대해 secret이 없는 ephemeral host에서 실행한 뒤 폐기함.

## Rust 운영 test harness

```rust
pub struct SoakScenario {
    pub duration: Duration,
    pub concurrent_vms: usize,
    pub lifecycle_rate: usize,
    pub snapshot_rate: usize,
    pub fault_schedule: Vec<FaultInjection>,
}
```

- load·soak test는 생성/list/start/stop/delete/snapshot 비율과 host capacity를 명시함.
- fault에는 Firecracker crash, helper restart, DB busy, disk pressure, network rule 손실과 API restart를 포함함.

## Release 기준

- API error rate와 lifecycle latency SLO 충족
- memory, file descriptor, task, cgroup, TAP 누수 없음
- backup restore와 한 version upgrade/rollback 성공
- KVM 2-VM network/SSH와 one-shot checkpoint/restore replay 방지 test 성공
- browser session, CSRF/CSP와 role별 WebAssembly UI test 성공
- high/critical dependency 취약점 예외는 owner와 만료일을 기록함
- artifact signature와 SBOM 검증 성공

## Runbook

- KVM 장애, disk full, DB lock/corruption, orphan process/TAP/cgroup, snapshot mismatch, token 유출, failed upgrade 절차를 `docs/operations/`에 작성함.
- 명령은 Firecrab 소유 자원만 대상으로 함.

## 테스트 및 검증

- release workflow가 위 gate와 운영 scenario를 통과한 경우에만 version tag artifact를 publish해야 함.
- runbook은 disposable host에서 다른 운영자가 그대로 재현해 검증함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
