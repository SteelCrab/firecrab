---
tags:
  - firecrab
  - template
status: 미완료
scope: 5주차
updated: 2026-07-23
---

# Template 무결성 검증 및 승격 구현

## 브랜치 개요

- 브랜치: `feat/template-integrity-and-promotion` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: 서명된 manifest와 checksum을 통과한 immutable template version만 staging에서 active registry로 승격함.

## Signed manifest

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct TemplateReleaseManifest {
    pub schema_version: u32,
    pub name: String,
    pub version: semver::Version,
    pub architecture: String,
    pub kernel_sha256: String,
    pub rootfs_sha256: String,
    pub boot_args_sha256: String,
    pub guest_agent_sha256: String,
    pub build_manifest_sha256: String,
    pub compatibility: TemplateCompatibility,
}
```

```rust
pub fn verify_release(
    manifest: &[u8],
    signature: &Signature,
    key: &VerifyingKey,
) -> anyhow::Result<TemplateReleaseManifest> {
    key.verify_strict(manifest, signature)?;
    Ok(serde_json::from_slice(manifest)?)
}
```

- signature는 canonical manifest bytes에 대해 확인하고 duplicate JSON key와 비정규 encoding을 거부함.
- staging root directory handle에서 `openat2`의 beneath/no-symlink 정책 또는 동등한 API로 kernel, rootfs, canonical boot args와 build manifest를 열고, 열린 file descriptor의 내용을 streaming SHA-256으로 계산함.
- rootfs 안의 Guest agent digest도 build manifest와 대조함.
- manifest 안의 경로나 요청 이름을 그대로 filesystem path로 사용하지 않음.

## Promotion

- 검증한 bytes를 digest 기반 immutable store에 `create_new`로 복사하고 file과 directory를 fsync한 뒤 read-only로 publish함.
- boot smoke test도 이 content-addressed kernel/rootfs를 사용해 path 검증과 실행 사이의 교체를 막음.
- test를 통과하면 digest만 참조하는 새 registry generation을 임시 파일로 작성하고 atomic rename 및 parent fsync로 active generation을 바꿈.
- 기존 generation은 rollback용으로 유지함.

- 실행 중 VM은 생성 당시 template version과 digest를 계속 참조함.
- active version 변경이 기존 VM의 rootfs나 snapshot 호환성을 바꾸면 안 됨.

## Key 정책

- 검증 public key는 package 또는 systemd credential로 배포함.
- signing private key는 API host에 저장하지 않음.
- key rotation은 이전·신규 key가 겹치는 transition generation을 지원함.

## 테스트 및 검증

- manifest, kernel, rootfs, signature 각각을 변조하고 hash 검증 직후 staging path를 symlink나 다른 inode로 교체해 승격이 실패하는지 확인함.
- promotion 중 process를 종료해도 active registry가 이전 또는 새 generation 중 하나의 완전한 상태여야 함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
