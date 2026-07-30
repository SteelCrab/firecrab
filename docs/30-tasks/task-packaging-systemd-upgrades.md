---
tags:
  - firecrab
  - release
status: 미완료
scope: 5주차
updated: 2026-07-23
---

# 패키징·systemd·upgrade 구현

## 브랜치 개요

- 브랜치: `feat/packaging-systemd-upgrades` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: Rust workspace binary, service 계정, directory와 systemd unit을 versioned package로 배포하고 안전한 DB migration·rollback 절차를 제공함.

## Package 구성

```text
/usr/bin/firecrab-api
/usr/libexec/firecrab-runtime-helper
/usr/libexec/firecrab-net-helper
/usr/bin/firecrab-admin
/usr/share/firecrab/frontend/
/usr/lib/systemd/system/firecrab-api.service
/usr/lib/systemd/system/firecrab-*.socket
/usr/lib/sysusers.d/firecrab.conf
/usr/lib/tmpfiles.d/firecrab.conf
/etc/firecrab/config.toml
/var/lib/firecrab/
/var/log/firecrab/
```

## systemd hardening

- API unit은 `NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`, 제한된 `ReadWritePaths`를 사용함.
- frontend 정적 asset은 read-only로 API와 same-origin에서 제공함.
- network/runtime helper는 별도 socket activation unit과 필요한 capability만 가짐.

- 실행 VM을 API restart에서 보존하는 정책이면 Jailer cgroup을 systemd가 위임한 전용 `firecrab-vm.slice` 또는 transient scope 아래에 만들고 API/helper unit stop이 VM process를 kill하지 않는지 검증함.
- unit의 `KillMode`만 느슨하게 바꿔 service cgroup에 orphan child를 남기는 방식은 사용하지 않음.
- 해당 host integration을 지원하지 않으면 installer가 `drain_on_shutdown=true`를 강제함.

- API, runtime helper와 network helper package에는 protocol version range를 기록함.
- socket activation이 upgrade 중 임의 version helper를 시작하지 않도록 unit과 binary를 같은 package transaction으로 교체하고, 새 API가 설치된 helper protocol 및 DB schema와 호환되는지 offline preflight함.
- 호환되지 않는 조합에서는 migration이나 service start를 진행하지 않음.

- package manifest는 `requires_vm_drain`을 명시함.
- runtime helper, Firecracker/Jailer, snapshot format 또는 jail/cgroup contract가 바뀌면 모든 VM을 stop하거나 one-shot checkpoint한 뒤 교체함.
- API-only이며 이전 helper protocol과 양방향 호환되는 upgrade만 실행 VM을 유지할 수 있고, recovery test로 기존 process를 다시 attach함.
- 실행 중 helper binary를 덮어쓴 뒤 새 socket activation process와 구 protocol을 섞는 방식은 허용하지 않음.

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct UpgradePlan {
    pub from_version: semver::Version,
    pub to_version: semver::Version,
    pub database_backup: PathBuf,
    pub migrations: Vec<String>,
    pub rollback_supported: bool,
}
```

## Upgrade 순서

1. host, KVM, disk와 config preflight를 실행함.
2. maintenance mode와 upgrade rollback용 SQLite/config backup을 완료하고 package 정책이 요구하면 VM drain을 확인함. 실행 VM을 유지하는 API-only upgrade의 backup은 self-contained disaster-recovery bundle로 표시하지 않음.
3. 새 binary, frontend asset과 unit을 staging하고 signature, owner, mode와 API/helper protocol compatibility를 확인함.
4. package transaction으로 binary와 unit을 교체함.
5. backward-compatible migration을 실행함. migration binary와 target schema version을 기록하고 single-writer lock을 사용함.
6. helper handshake, API readiness와 smoke test를 확인함.
7. 실패 시 binary와 config를 되돌림. destructive schema migration은 자동 downgrade하지 않고 backup restore 절차를 사용함.

- installer는 기존 config와 data를 덮어쓰지 않으며 file owner와 mode를 검증함.

## 테스트 및 검증

- 깨끗한 설치, 같은 version 재설치, API/helper version mismatch, 한 version upgrade, 각 단계 중간 실패와 rollback을 disposable host에서 test함.
- 실행 VM과 DB가 정책대로 보존되고 stale socket activation binary가 실행되지 않으며 helper 권한이 확대되지 않아야 함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
