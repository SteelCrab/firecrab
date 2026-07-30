---
tags:
  - firecrab
status: 미완료
scope: 4주차
updated: 2026-07-23
---

# cgroup 리소스 회계 및 제한 구현

## 브랜치 개요

- 브랜치: `feat/cgroup-resource-governance` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: cgroup v2 아래에 VM별 subtree를 만들고 CPU, memory, process와 지원 storage의 block I/O 사용량을 제한·측정함.

## 모델

```rust
#[derive(Debug, Clone)]
pub struct CgroupLimits {
    pub cpu_quota_us: u64,
    pub cpu_period_us: u64,
    pub memory_max_bytes: u64,
    pub memory_swap_max_bytes: u64,
    pub pids_max: u32,
    pub io_max: Option<IoMax>,
}
```

- guest RAM과 VMM overhead는 다르므로 `memory.max`는 `guest_ram + configured_overhead`로 계산함.
- memory overhead가 없는 값으로 guest RAM만 강제하면 정상 VM도 OOM으로 종료될 수 있음.

```rust
fn jailer_cgroup_args(limits: &CgroupLimits) -> Vec<String> {
    let mut args = vec![
        format!("cpu.max={} {}", limits.cpu_quota_us, limits.cpu_period_us),
        format!("memory.max={}", limits.memory_max_bytes),
        format!("memory.swap.max={}", limits.memory_swap_max_bytes),
        format!("pids.max={}", limits.pids_max),
    ];
    if let Some(io_max) = &limits.io_max {
        args.push(format!("io.max={}", io_max.to_validated_cgroup_value()));
    }
    args
}
```

- runtime helper는 startup에 cgroup v2 unified hierarchy와 `cpu`, `memory`, `pids` controller가 parent의 `cgroup.subtree_control`에 활성화됐는지 확인함.
- limit은 Jailer `--cgroup-version=2`, `--parent-cgroup`, `--cgroup` argument로 넘겨 Firecracker가 실행되기 전에 적용함.
- swap 정책은 명시적으로 고정하고 host overcommit과 snapshot latency에 포함함.
- 사용자 문자열을 cgroup path나 argument에 넣지 않음.

- `io.max`는 root 소유 config의 device major/minor와 검증된 BPS/IOPS 값으로만 구성함.
- VM rootfs path가 실제 어느 block device에 있는지 helper가 확인하고 overlay/network filesystem처럼 의미 있는 throttle을 적용할 수 없으면 조용히 다른 device에 rule을 걸지 않고 unsupported/degraded readiness로 처리함.

## 수명주기

1. helper가 parent hierarchy와 controller 상태를 검증함.
2. Jailer가 root 소유 config의 systemd-delegated parent 아래 `<vm_uuid>` cgroup을 생성하고 limit 적용 뒤 process를 배치함.
3. 종료 시 `cgroup.events`의 `populated 0`을 확인함.
4. 비어 있는 Firecrab 소유 cgroup만 제거함.

- x86 host에서 guest가 만든 `kvm-pit/<firecracker-pid>` kernel thread는 자동으로 VM cgroup에 들어가지 않을 수 있음.
- 지원 host에서는 외부 agent가 정확한 process identity를 확인한 뒤 해당 thread를 VM cgroup으로 이동하고 이 동작을 test함.

## 테스트 및 검증

- CPU stress, guest memory limit 초과, thread/process 증가와 지원 block device의 read/write saturation을 각각 test함.
- `pids.max`는 Firecracker의 API/VMM/vCPU thread overhead를 포함해 계산함.
- 한 VM의 제한 초과가 API와 다른 VM에 영향을 주지 않고 `memory.events`, `cpu.stat`, `io.stat`가 metrics에 반영되어야 함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
