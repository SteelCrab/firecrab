---
tags:
  - firecrab
  - template
status: 미완료
scope: 5주차
updated: 2026-07-23
---

# 재현 가능한 template build pipeline 구현

## 브랜치 개요

- 브랜치: `feat/reproducible-template-builds` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: Ubuntu Base, kernel, package 목록과 build 설정을 versioned manifest로 고정하고 동일 입력에서 동일한 filesystem 내용을 생성함.

## Build manifest

```rust
#[derive(Debug, Deserialize, Serialize)]
pub struct TemplateBuildSpec {
    pub name: String,
    pub version: String,
    pub ubuntu_base_url: Url,
    pub ubuntu_base_sha256: String,
    pub kernel_url: Url,
    pub kernel_sha256: String,
    pub packages: Vec<String>,
    pub rootfs_size_mib: u32,
    pub source_date_epoch: i64,
}
```

- 입력 archive는 upstream checksum과 고정된 manifest checksum을 모두 확인함.
- package repository snapshot이나 version pinning 없이 최신 package를 설치하면 같은 manifest가 다른 결과를 만들 수 있으므로 허용하지 않음.

## Rust builder 순서

1. KVM과 image build 도구를 사전 점검함.
2. HTTPS scheme와 allowlist host만 허용하고 redirect, resolved private/link-local address와 응답 크기를 제한해 격리된 staging directory에 입력을 다운로드한 뒤 SHA-256을 검증함.
3. absolute path, `..`, symlink/hardlink 탈출, device node, file count·개별 크기·총 추출 크기 초과를 거부하며 Ubuntu Base를 추출함. 최소 `systemd`, `udev`, `kmod`, `util-linux`, SSH server, CA/DNS/DHCP 구성에 필요한 package의 정확한 version을 lock해 설치함.
4. `systemd-networkd`, serial console, DNS, SSH 정책과 versioned Guest agent를 적용함.
5. `/etc/machine-id`, random seed와 SSH host key를 비워 VM별 첫 boot 생성을 보장함. 사용자별 `authorized_keys`는 image에 bake하지 않음.
6. 고정 timestamp와 정렬된 입력으로 ext4 image를 생성함.
7. boot smoke test와 manifest 생성을 완료한 뒤 publish staging으로 이동함.

- package maintainer script와 image tool은 host credential, host filesystem mount와 `/dev/kvm` 외 장치가 없는 disposable VM/namespace에서 실행함.
- download가 끝난 build 단계에는 불필요한 network를 차단함.
- 외부 도구는 shell을 거치지 않고 `std::process::Command` argument로 호출하고 secret을 제거한 bounded stdout/stderr를 build log로 저장함.

## 검증

```rust
pub struct BuildOutput {
    pub kernel: PathBuf,
    pub rootfs: PathBuf,
    pub kernel_sha256: String,
    pub rootfs_sha256: String,
    pub package_lock_sha256: String,
    pub guest_agent_sha256: String,
}
```

- KVM smoke test는 systemd boot 완료, serial console, `eth0`, gateway, DNS 조회와 SSH daemon 준비를 확인함.
- rescue shell 문구는 boot 성공으로 처리하지 않음.

## 테스트 및 검증

- 깨끗한 두 build 환경에서 같은 manifest를 실행하고 normalized filesystem manifest가 일치해야 함.
- binary image byte 동일성이 불가능한 filesystem metadata는 명시적으로 정규화하거나 비교 대상에서 이유와 함께 제외함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
