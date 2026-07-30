---
tags:
  - firecrab
  - firecracker
status: 미완료
scope: 4주차
updated: 2026-07-23
---

# Firecracker Jailer 격리 구현

## 브랜치 개요

- 브랜치: `feat/firecracker-jailer-isolation` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: HTTP API를 일반 사용자로 유지하면서 Firecracker를 VM별 chroot와 비특권 UID/GID 안에서 실행함.
- Jailer 준비만 제한된 `firecrab-runtime-helper`가 담당함.
- 범위: 실행 순서와 chroot/device ownership은 공식 [Jailer 문서](https://github.com/firecracker-microvm/firecracker/blob/main/docs/jailer.md), production 경계는 [production host setup](https://github.com/firecracker-microvm/firecracker/blob/main/docs/prod-host-setup.md)을 기준으로 함.

## Rust 경계

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct JailerLaunchRequest {
    pub vm_id: Uuid,
    pub operation_id: Uuid,
    pub purpose: LaunchPurpose,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum LaunchPurpose {
    FreshBoot,
    SnapshotRestore { snapshot_id: Uuid },
}
```

- API가 UID/GID, 실행 파일, chroot·cgroup 경로나 Firecracker argument를 보내지 못하게 함.
- helper가 root 소유 설정, VM lease와 `vm_id`로 최종 값을 계산하고 operation ID를 ownership record에 연결함.
- `purpose`는 allowlist enum이며 snapshot ID도 소유 snapshot root에서만 해석함.
- helper protocol에는 shell command 문자열을 두지 않음.

- runtime helper protocol도 version, frame 길이, request ID와 `SO_PEERCRED`를 검증하는 공통 envelope를 사용하고 동시 요청·timeout을 제한함.
- Firecracker와 Jailer는 같은 release의 production static/musl artifact와 digest pair인지 startup에 확인함.
- API가 보낸 resource limit은 그대로 신뢰하지 않고 DB의 검증된 VM spec과 root 소유 host maximum에서 helper가 다시 계산함.

```rust
let mut command = tokio::process::Command::new(&config.jailer_binary);
command
    .arg("--id").arg(request.vm_id.simple().to_string())
    .arg("--exec-file").arg(&config.firecracker_binary)
    .arg("--uid").arg(lease.uid.to_string())
    .arg("--gid").arg(lease.gid.to_string())
    .arg("--chroot-base-dir").arg(&config.chroot_base)
    .arg("--cgroup-version").arg("2")
    .arg("--parent-cgroup").arg(&config.parent_cgroup)
    .arg("--cgroup").arg(format!("cpu.max={}", limits.cpu_max))
    .arg("--cgroup").arg(format!("memory.max={}", limits.memory_max))
    .arg("--cgroup").arg(format!("memory.swap.max={}", limits.memory_swap_max))
    .arg("--cgroup").arg(format!("pids.max={}", limits.pids_max))
    .arg("--resource-limit").arg(format!("no-file={}", limits.no_file))
    .arg("--resource-limit").arg(format!("fsize={}", limits.max_file_size))
    .arg("--new-pid-ns")
    .arg("--")
    .arg("--api-sock").arg("run/firecracker.socket");

if matches!(request.purpose, LaunchPurpose::FreshBoot) {
    command.arg("--config-file").arg("resources/firecracker.json");
}
let child = command.spawn()?;
```

- fresh boot에만 `--config-file resources/firecracker.json`을 추가함.
- snapshot restore는 config file 없이 빈 Firecracker를 시작하고 logger/metrics만 구성한 뒤 `LoadSnapshot`을 호출함.
- 이미 machine/device config를 적용한 process에 snapshot을 load하지 않음.

- kernel, rootfs, config, vsock과 metrics path는 jailer가 볼 수 있는 VM 전용 위치에 준비하고 Firecracker config에는 chroot 내부 상대 경로만 기록함.
- `--exec-file`, `--chroot-base-dir`와 그 parent는 Jailer 실행 전후 모두 unprivileged user가 수정할 수 없어야 함.
- Jailer가 만든 개별 `<chroot_dir>/root`와 jail 내부 `/dev/kvm`, `/dev/net/tun`은 실행 UID/GID로 chown되므로 launch 뒤 jail 내부는 trusted host metadata source로 사용하지 않음.

- API socket과 vsock UDS는 Linux `sockaddr_un.sun_path` 제한을 넘지 않도록 고정된 짧은 jail 내부 이름과 충분히 짧은 chroot base를 startup에 검증함.
- helper가 host 쪽 절대 path로 connect할 때의 최종 byte 길이도 process spawn 전에 확인함.
- PID namespace 내부 PID가 아니라 Jailer pid file과 pidfd가 가리키는 host PID를 runtime identity로 저장함.

- VM UID가 만든 API socket을 일반 API service에 직접 노출하지 않음.
- runtime helper가 socket을 열고 `pause`, `resume`, `snapshot`, `shutdown`, readiness 같은 allowlisted typed operation만 proxy함.
- snapshot 요청도 VM/snapshot/operation ID만 받고 state, memory와 disk 경로는 helper가 소유 root에서 파생함.
- raw Firecracker HTTP, URL, JSON body나 임의 path를 helper protocol로 전달하지 않음.

## 소유권

- VM별 UID/GID lease를 DB에서 원자적으로 할당함.
- numeric UID/GID는 root 소유 config의 전용 range에서만 할당하고 startup에 service account, NSS/local user와 기존 file owner 충돌을 검사함.
- helper도 요청 VM의 lease가 range 안인지 재검증함.
- chroot base와 VM별 jail parent/ownership record는 root만 수정함.
- Jailer가 VM UID로 chown하는 jail root 아래에는 VM-owned `run`과 writable disk/output, root-owned read-only `resources` subdirectory를 분리함.
- UID/GID lease는 stop 동안 유지하고 VM snapshot/state가 해당 GID를 참조하는 동안 재사용하지 않음.
- VM delete에서 process, snapshot과 artifact 정리가 모두 끝난 뒤에만 반환함.
- 다른 VM chroot와 host root를 가리키는 symlink는 거부함.
- Jailer pid file, cgroup과 jail 밖 root-owned ownership record를 operation에 연결함.
- VM-owned jail 내부 marker만으로 host cleanup 권한을 판단하지 않음.

## 테스트 및 검증

- Firecracker process의 UID/GID, PID namespace, cgroup과 root directory를 `/proc/<pid>`에서 확인함.
- VM config에 host 절대 경로를 넣거나 다른 VM artifact를 열려는 test, raw helper API 호출이 실패해야 함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
