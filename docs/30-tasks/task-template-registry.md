---
tags:
  - firecrab
  - template
status: 완료
scope: 2주차
updated: 2026-07-23
---

# Template registry 구현

## 브랜치 개요

- 브랜치: `feat/template-registry`
- 커밋: `31ec1d8 feat: add immutable template registry`
- 상태: 구현 브랜치 존재
- 변경 규모: 8개 파일, 534줄 추가, 14줄 삭제
- 목적: API에 노출되는 template 이름을 관리자가 등록한 kernel과 rootfs에만 연결한다.
- 요청값을 host 경로나 명령 문자열로 직접 사용하지 않는다.

## 타입

```rust
use std::{collections::HashMap, path::{Path, PathBuf}};

#[derive(Clone)]
pub struct TemplateVersion {
    pub name: String,
    pub version: String,
    pub kernel_sha256: String,
    pub rootfs_sha256: String,
    pub boot_args_sha256: String,
    pub kernel: PathBuf, // image_root 기준 검증된 상대 경로
    pub rootfs: PathBuf,
    pub boot_args: String,
}

#[derive(Clone)]
pub struct TemplateRegistry {
    image_root: Arc<OwnedFd>,
    aliases: HashMap<String, TemplateVersion>,
    versions: HashMap<(String, String), TemplateVersion>,
}
```

## 등록 시 검증

```rust
fn open_template_artifact(
    image_root: &OwnedFd,
    candidate: &Path,
) -> anyhow::Result<VerifiedArtifact> {
    validate_normal_relative_path(candidate)?;
    let file = openat2_read_only(
        image_root,
        candidate,
        Resolve::BENEATH
            | Resolve::NO_SYMLINKS
            | Resolve::NO_MAGICLINKS
            | Resolve::NO_XDEV,
    )?;
    let metadata = file.metadata()?;
    anyhow::ensure!(metadata.file_type().is_file(), "not a regular file");
    VerifiedArtifact::from_open_file(candidate, file, metadata)
}
```

- registry 설정을 읽을 때 image root directory FD 기준으로 모든 artifact를 열고 검증함.
- absolute path, `..`, symlink/magic-link와 다른 mount로의 탈출을 거부함.
- image root와 artifact는 API·helper·VM UID가 수정할 수 없고 관리자만 새 generation을 publish할 수 있어야 함.
- 요청 처리 시에는 이미 검증된 entry를 이름으로 clone한다.

```rust
impl TemplateRegistry {
    pub fn resolve_alias(&self, name: &str) -> Option<&TemplateVersion> {
        self.aliases.get(name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.aliases.contains_key(name)
    }
}
```

- 기본 registry에는 문서와 UI가 사용하는 `ubuntu-rootfs-26.04` 이름을 하나만 등록한다.
- template 이름은 셸, SQL, 파일 경로에 삽입하지 않고 HashMap 조회에만 사용한다.

- create 요청의 이름은 alias일 뿐임.
- 생성 transaction에서 alias를 immutable version과 kernel/rootfs digest로 해석해 VM row에 모두 저장함.
- 이후 start, rootfs 생성과 snapshot compatibility는 active alias를 다시 조회하지 않고 저장된 version/digest가 정확히 일치하는 `versions` entry만 사용함.
- registry promotion이나 daemon restart가 기존 VM의 boot artifact를 바꾸면 안 됨.

- 실제 복사 시에도 같은 directory handle 정책으로 다시 열고 등록 시 저장한 device/inode와 SHA-256이 일치하는지 확인함.
- 검증한 열린 file handle에서 복사해 검증과 사용 사이의 TOCTOU를 제거함.
- 지원 kernel에 `openat2`가 없으면 문자열 canonicalization으로 약화하지 않고 startup을 실패시키거나 동등한 capability filesystem 구현을 사용함.

## 테스트 및 검증

- 미등록 이름은 `400 validation_failed`로 거부한다.
- `../`, 절대 경로, root 밖을 가리키는 symlink가 포함된 registry 설정은 서버 시작을 실패시킨다.
- 등록된 kernel과 rootfs가 없거나 일반 파일이 아니면 서버 시작을 실패시킨다.
- registry 검증 직후 symlink나 artifact를 교체해도 VM 생성이 거부되는지 확인한다.
- alias를 새 version으로 바꿔도 기존 VM은 이전 digest를 사용하고 새 VM만 새 version을 사용해야 한다.

## 완료 및 후속 범위

- 구현 브랜치와 커밋이 존재함.
- 위 테스트 및 검증 항목을 모두 통과한 뒤 완료로 판정함.
