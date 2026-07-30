---
tags:
  - firecrab
  - operations
status: 미완료
scope: 5주차
updated: 2026-07-23
---

# Backup 및 restore 구현

## 브랜치 개요

- 브랜치: `feat/backup-and-restore` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: SQLite와 운영에 필요한 manifest, 설정, 선택 artifact를 일관된 bundle로 백업하고 offline staging을 거쳐 복원함.

## Manifest

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct BackupManifest {
    pub schema_version: u32,
    pub firecrab_version: String,
    pub created_at: OffsetDateTime,
    pub database_sha256: String,
    pub files: Vec<BackupFile>,
    pub required_key_fingerprints: Vec<KeyFingerprint>,
    pub external_dependencies: Vec<ExternalArtifact>,
    pub encrypted: bool,
}
```

## 생성 순서

1. maintenance mode로 변경 작업을 차단함.
2. 진행 중 transaction과 snapshot publish를 drain하고 retention GC를 pause함.
3. backup inventory와 durable lease를 한 transaction으로 고정하고 모든 artifact를 검증된 directory handle에서 open함.
4. SQLite online backup API로 inventory revision과 일치하는 DB 사본을 생성함.
5. 필요한 immutable template artifact, quiesced VM의 active/retained disk generation과 모든 `ready` snapshot을 열린 descriptor에서 staging에 복사함.
6. 각 파일 checksum과 전체 manifest를 작성함.
7. 검증된 `age` 같은 형식으로 bundle을 암호화함.
8. 임시 bundle을 최종 이름으로 atomic rename하고 parent를 fsync함.
9. backup lease를 반환하고 GC pause를 해제함. crash 시 recovery가 staging을 정리한 뒤 같은 순서로 lease를 반환함.

```rust
pub async fn backup_database(source: PathBuf, destination: PathBuf) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || {
        let source = open_verified_source_database(&source)?;
        let mut destination = create_new_backup_database(&destination)?;
        let backup = rusqlite::backup::Backup::new(&source, &mut destination)?;
        backup.run_to_completion(100, Duration::from_millis(10), None)?;
        drop(backup);
        destination.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        sync_database_and_parent(&destination)?;
        Ok::<_, anyhow::Error>(())
    }).await??;
    Ok(())
}
```

- source와 destination은 data/staging root directory handle 기준으로 열고 symlink를 따라가지 않음.
- destination이 이미 존재하면 실패하며 이전 backup DB를 재사용하지 않음.
- SQLite backup 완료 뒤 integrity check, file과 parent fsync를 통과해야 다음 artifact 단계로 이동함.

## Restore

- 서비스를 중지한 상태에서 기존 root와 같은 filesystem의 새 data root에 decrypt·extract함.
- absolute path, `..`, symlink/hardlink 탈출, device/FIFO, 중복 entry, file count·개별 크기·총 크기 초과를 거부함.
- checksum, schema compatibility, template/snapshot 참조와 key fingerprint를 검증하고 backup 시점의 transient runtime, shutdown과 backup lease를 recovery 대상으로 초기화함.
- 그 뒤 기존 data root와 atomic 교체하고 parent를 fsync함.
- 기존 root는 rollback용으로 보존함.

- private signing key, backup decrypt key와 발급된 token 원문은 backup 대상이 아님.
- token pepper와 snapshot HMAC key도 별도 secret escrow 정책으로 복구하며 bundle에는 필요한 key version/fingerprint만 기록함.
- 해당 key가 없으면 token을 일괄 revoke하거나 snapshot을 unusable로 명확히 표시해야 하고 조용히 검증을 건너뛰면 안 됨.
- token hash와 audit data 보존 정책은 manifest에 명시함.

- maintenance mode는 API 변경을 막을 뿐 실행 중 Guest의 writable rootfs 쓰기를 멈추지 못함.
- self-contained backup은 running VM이 하나라도 있으면 시작을 거부하고 먼저 one-shot checkpoint 또는 stop을 요구함.
- quiesced VM의 active/retained disk generation과 immutable ready snapshot은 backup lease로 보호해 DB가 참조하는 artifact를 모두 포함함.
- 명시적인 metadata-only mode를 제공한다면 boot 불가능한 VM ID와 누락 generation을 manifest에 기록하고 restore 후 해당 VM을 `error`로 표시하며 일반 backup 성공과 혼동하지 않음.
- manifest-only template mode도 digest가 일치하는 external registry dependency를 명시하고 restore preflight가 artifact를 확보하기 전에는 service를 시작하지 않음.
- runtime process/socket/TAP은 backup 대상이 아니며 restore recovery가 `running`으로 잘못 간주하지 않게 함.

## 테스트 및 검증

- 정상·손상·archive traversal/bomb·잘못된 key·누락 key version·구버전 schema bundle을 test함.
- running VM이 self-contained backup에서 거부되고 모든 DB disk generation, `ready` snapshot과 active template digest가 bundle 또는 선언된 external dependency에 존재하는지 확인함.
- 복원 후 DB integrity, VM 목록, template registry, snapshot checksum/HMAC과 기존·새 VM boot가 정상이어야 함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
