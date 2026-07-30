---
tags:
  - firecrab
  - storage
status: 미완료
scope: 4주차
updated: 2026-07-23
---

# VM별 rootfs 및 artifact 관리 구현

## 브랜치 개요

- 브랜치: `feat/vm-rootfs-artifacts`
- 커밋: `7f0e7df feat: manage VM disk generations and artifacts`
- 상태: 구현 브랜치 존재
- 변경 규모: 7개 파일, 552줄 추가, 20줄 삭제
- 목적: 각 VM이 독립된 persistent rootfs와 실행 세대별 config, API socket, console log를 사용하게 한다.
- stop/start에서 writable rootfs를 보존하고 생성 실패와 삭제 시 소유 범위만 일관되게 정리한다.

## 경로 모델

```rust
#[derive(Debug, Clone)]
pub struct VmArtifactPaths {
    pub dir: PathBuf,
    pub disks: PathBuf,
    pub runtimes: PathBuf,
}

#[derive(Debug, Clone)]
pub struct HostRuntimePaths {
    pub dir: PathBuf,
    pub config: PathBuf,
    pub api_socket: PathBuf,
    pub console_log: PathBuf,
}

impl VmArtifactPaths {
    pub fn for_vm(root: &Path, id: Uuid) -> Self {
        let dir = root.join(id.simple().to_string());
        Self {
            disks: dir.join("disks"),
            runtimes: dir.join("runtimes"),
            dir,
        }
    }

    pub fn rootfs(&self, generation: Uuid) -> PathBuf {
        self.disks.join(format!("{}.ext4", generation.simple()))
    }

    pub fn runtime(&self, runtime_id: Uuid) -> HostRuntimePaths {
        let dir = self.runtimes.join(runtime_id.simple().to_string());
        HostRuntimePaths {
            config: dir.join("firecracker.json"),
            api_socket: dir.join("firecracker.sock"),
            console_log: dir.join("console.log"),
            dir,
        }
    }
}
```

- UUID는 내부에서 생성된 값만 사용한다.
- 사용자 입력으로 artifact 경로를 만들지 않는다.

- Jailer 적용 전부터 host 경로와 Firecracker가 보는 runtime 경로를 분리함.

```rust
pub struct RuntimeArtifactPaths {
    pub kernel: PathBuf, // jail 내부 상대 경로
    pub rootfs: PathBuf, // jail 내부 상대 경로
    pub config: PathBuf,
    pub api_socket: PathBuf,
}
```

- DB에는 사용자가 바꿀 수 있는 절대 경로 대신 rootfs generation과 runtime UUID를 저장하고 host 경로는 설정된 data root에서 파생함.
- helper에는 검증된 host 경로를 전달하고 Firecracker config에는 `RuntimeArtifactPaths`만 사용함.
- 이 경계를 두지 않으면 Jailer 도입 시 기존 config와 snapshot의 backing path가 모두 깨짐.

## 원자적 준비

```rust
pub async fn prepare_initial_rootfs(
    paths: &VmArtifactPaths,
    generation: Uuid,
    template: &TemplateVersion,
) -> anyhow::Result<PathBuf> {
    create_vm_directories_securely(paths).await?;
    let destination = paths.rootfs(generation);
    let temporary = paths.disks.join(format!(".{}.tmp", generation.simple()));
    let source = template.open_rootfs_verified()?;

    let result = async {
        copy_sparse_create_new(&source, &temporary).await?;
        sync_file(&temporary).await?;
        tokio::fs::rename(&temporary, &destination).await?;
        sync_directory(&paths.disks).await?;
        Ok::<_, anyhow::Error>(())
    }.await;

    if result.is_err() {
        if let Err(error) = tokio::fs::remove_file(&temporary).await {
            record_cleanup_required(&temporary, &error);
        }
    }
    result?;
    Ok(destination)
}
```

- 대용량 sparse rootfs를 다룰 때는 reflink 또는 sparse copy를 지원하는 검증된 복사 방식을 사용한다.
- 외부 `cp`를 쓰는 경우 shell을 거치지 않고 `Command::arg`에 검증된 경로만 전달한다.

- 복사 대상은 `create_new`, mode `0600`으로 만들고 file과 parent directory를 `fsync`한 뒤 publish함.
- data root부터 directory handle 기준으로 열어 symlink와 다른 mount로의 탈출을 차단함.
- reflink가 성공해도 source와 destination이 같은 writable inode인지 검사하고 template 원본은 read-only로 유지함.
- disk quota 또는 최소 free-space budget을 먼저 예약해 host disk 고갈을 방지함.

- 초기 rootfs generation은 `preparing` ledger row와 quota를 먼저 예약하고 한 번만 만듦.
- file publish 뒤 transaction에서 generation을 `active`로 전환하며 partial unique index가 VM별 active disk 하나만 허용함.
- `stopped -> starting`에서는 이 active file을 그대로 재사용하고 template에서 다시 복사하거나 stale runtime cleanup에 포함하지 않음.
- 각 start는 새 `runtime_id` directory를 `create_new` 방식으로 만들고 config, socket과 bounded log를 그 안에 둠.
- 이전 runtime directory는 process identity가 종료된 것을 확인한 뒤 retention 정책으로 정리함.

- rename과 DB commit 사이 crash는 `preparing` generation, expected digest와 operation phase로 reconcile함.
- final file이 있으면 open descriptor에서 size/digest/owner를 검증한 뒤 active 전환하고, 검증되지 않은 file이나 temp만 있으면 정리한 뒤 reservation을 반환함.
- UUID 이름만 일치한다고 기존 file을 신뢰하지 않음.

## 삭제 순서

1. DB 상태를 `deleting`으로 전환한다.
2. 프로세스가 없고 socket이 사용 중이 아닌지 확인한다.
3. 실행 세대별 runtime directory를 정리한다.
4. active rootfs와 보존 generation을 정리한다.
5. transaction에서 상태를 `deleted`로 바꾼다.

## 테스트 및 검증

- 동시에 두 VM을 만들었을 때 모든 경로와 rootfs inode가 달라야 한다.
- stop/start 뒤 rootfs inode와 Guest 파일 내용이 유지되어야 하고 runtime ID와 socket 경로는 새로 생성되어야 함.
- 복사 실패를 강제로 발생시킨 뒤 `.tmp` 파일과 부분 DB generation이 남지 않아야 한다.

## 완료 및 후속 범위

- 구현 브랜치와 커밋이 존재함.
- 위 테스트 및 검증 항목을 모두 통과한 뒤 완료로 판정함.
