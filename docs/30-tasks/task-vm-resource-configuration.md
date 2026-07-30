---
tags:
  - firecrab
  - vm
status: 보류
updated: 2026-07-23
---

# VM 리소스 설정 적용 구현

## 브랜치 개요

- 브랜치: `feat/vm-resource-configuration`
- 커밋: `6c0a2ff feat: generate Firecracker resource config`
- 상태: 구현 브랜치 존재
- 변경 규모: 2개 파일, 184줄 추가
- 목적: 검증된 CPU와 RAM 값을 Firecracker machine config JSON에 그대로 반영한다.

## Firecracker config 타입

```rust
#[derive(Serialize)]
struct FirecrackerConfig {
    #[serde(rename = "boot-source")]
    boot_source: BootSource,
    drives: Vec<Drive>,
    #[serde(rename = "machine-config")]
    machine_config: MachineConfig,
}

#[derive(Serialize)]
struct MachineConfig {
    vcpu_count: u8,
    mem_size_mib: u32,
    smt: bool,
    track_dirty_pages: bool,
}
```

```rust
fn build_config(
    vm: &VmRecord,
    template: &TemplateVersion,
    paths: &RuntimeArtifactPaths,
) -> FirecrackerConfig {
    FirecrackerConfig {
        boot_source: BootSource {
            kernel_image_path: paths.kernel.clone(),
            boot_args: template.boot_args.clone(),
        },
        drives: vec![Drive {
            drive_id: "rootfs".into(),
            path_on_host: paths.rootfs.clone(),
            is_root_device: true,
            is_read_only: false,
        }],
        machine_config: MachineConfig {
            vcpu_count: vm.cpu,
            mem_size_mib: vm.ram,
            smt: false,
            track_dirty_pages: false,
        },
    }
}
```

- JSON key 이름은 Firecracker API schema와 정확히 맞춰야 하므로 수동 문자열 조립 대신 Serde를 사용한다.
- config는 임시 파일에 쓴 뒤 rename하고 권한은 owner만 쓸 수 있게 제한한다.

- host path를 config에 직접 넣지 않고 Jailer chroot에서 해석되는 runtime path를 사용함.
- Firecracker version별 OpenAPI schema로 config를 검증하고 지원 version을 startup에 고정함.
- CPU template을 사용한다면 VM 레코드와 snapshot manifest에도 이름과 digest를 저장함.

- 초기 full snapshot만 지원하므로 `track_dirty_pages`는 `false`로 둠.
- diff snapshot을 도입할 때는 developer-preview 상태와 memory chain 관리 계약을 별도 검토한 뒤 변경함.

## 테스트 및 검증

- CPU `1`, `32`와 RAM `128`, `32768` 경계값이 config에 그대로 기록되어야 한다.
- 범위 밖 값은 artifact를 만들기 전에 `400`으로 거부해야 한다.
- 생성된 JSON을 다시 `FirecrackerConfig` 또는 schema 검증기로 읽을 수 있어야 한다.

## 완료 및 후속 범위

- 구현 브랜치와 커밋이 존재함.
- 위 테스트 및 검증 항목을 모두 통과한 뒤 완료로 판정함.
