---
tags:
  - firecrab
  - vm
status: 미완료
scope: 4주차
updated: 2026-07-23
---

# VM runtime 권한 및 filesystem 격리 강화

## 브랜치 개요

- 브랜치: `feat/vm-runtime-permissions` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: VM artifact의 Unix 권한과 경로 해석을 제한하고 Firecracker의 seccomp 정책 및 실행 파일 무결성을 검증함.

## 안전한 경로 접근

```rust
pub struct VmResourcesDirectory {
    resources: cap_std::fs::Dir,
}

impl VmResourcesDirectory {
    pub fn create_config(&self) -> anyhow::Result<cap_std::fs::File> {
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        Ok(self.resources.open_with("firecracker.json", &options)?)
    }
}
```

- 검증된 directory handle 기준의 상대 경로만 사용함.
- 기존 파일 덮어쓰기, symlink follow, `..`와 절대 경로를 차단함.

## 권한 정책

| 대상 | mode | owner |
|---|---:|---|
| chroot base와 VM jail parent | `0750` | root, service/helper만 관리 |
| Jailer가 만든 VM jail root/`run` | 최소 쓰기 권한 | VM 전용 UID/GID, launch 후 untrusted |
| jail `resources`와 kernel/config | directory `0550`, file `0440` | root:VM 전용 GID, publish 후 immutable |
| writable rootfs/snapshot staging output | `0600` | VM 전용 UID |
| published snapshot state/memory | `0440` | root:VM 전용 GID, immutable |
| API socket | `0600` | VM UID, runtime helper만 접근 |
| metrics FIFO | `0600` | VM UID, helper가 reader를 먼저 준비 |
| bounded console log | `0600` | 서비스 계정 |

- Jailer는 개별 jail root와 jail 내부 device를 VM UID/GID로 chown하므로 그 내부 파일명이나 marker를 host 권한 판단에 신뢰하지 않음.
- trusted config/kernel은 VM UID가 쓸 수 없는 `resources` subdirectory에 두고 writable `run`, disk/output과 분리함.
- Firecracker binary와 jailer binary 및 chroot base는 root 소유이며 일반 사용자가 수정할 수 없어야 함.
- startup에서 capability path, parent directory owner/mode와 설정된 SHA-256을 확인함.
- API는 jail 내부 socket을 직접 열지 않고 runtime helper의 allowlisted protocol을 사용함.

## Seccomp

- production용 release/musl Firecracker의 내장 seccomp filter를 사용하고 `--no-seccomp`를 API나 config에 노출하지 않음.
- debug binary와 experimental GNU target은 기본 filter가 없을 수 있으므로 production startup에서 binary flavor와 seccomp 적용 여부를 검증함.
- custom filter가 필요하면 관리자 설정에 등록되고 별도 검증된 versioned artifact만 허용함.

- 지원 target별 기본 filter 차이는 공식 [seccomp 문서](https://github.com/firecracker-microvm/firecracker/blob/main/docs/seccomp.md)를 기준으로 release마다 다시 확인함.

## 테스트 및 검증

- symlink swap, path traversal, 다른 VM 파일 읽기, world-readable artifact, 변조된 Firecracker binary를 test함.
- 모든 실패가 process spawn 전에 감지되어야 함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
