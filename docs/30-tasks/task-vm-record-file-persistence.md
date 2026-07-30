---
tags:
  - firecrab
  - vm
status: 완료
scope: 2주차
updated: 2026-07-23
---

# VM 레코드 파일 저장 및 복원

## 브랜치 개요

- 브랜치: `feat/vm-record-file-persistence`
- 커밋: `09b67bf feat: add vm create API`
- 상태: 구현 브랜치 존재
- 변경 규모: 6개 파일, 125줄 추가
- 목적: SQLite 도입 전 단계에서 VM map을 `data/vms.json`에 저장하고 서버 시작 시 복원한다.
- 현재 `main`은 direct `fs::write`, 모든 read 오류의 빈 map 처리와 process panic을 사용하므로 최소 동작만 완료된 상태임.
- 아래 코드는 SQLite 전환 전에도 유지해야 한다면 적용할 임시 hardening 목표이며 장기 저장 계약은 SQLite task로 대체함.

## Rust 구현

```rust
use std::{collections::HashMap, path::Path};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::model::VmRecord;

const DATA_FILE: &str = "data/vms.json";

pub async fn load() -> anyhow::Result<HashMap<Uuid, VmRecord>> {
    match fs::read(DATA_FILE).await {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
        Err(error) => Err(error.into()),
    }
}

pub async fn save(vms: &HashMap<Uuid, VmRecord>) -> anyhow::Result<()> {
    let path = Path::new(DATA_FILE);
    fs::create_dir_all(path.parent().expect("data file has parent")).await?;

    let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let bytes = serde_json::to_vec_pretty(vms)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .await?;
    file.write_all(&bytes).await?;
    file.sync_all().await?;
    drop(file);

    fs::rename(&temporary, path).await?;
    sync_parent_directory(path).await?;
    Ok(())
}
```

- `NotFound`만 빈 저장소로 취급한다.
- 권한 오류나 손상된 JSON을 빈 map으로 바꾸면 기존 VM 정보가 조용히 유실되므로 시작 오류로 올려야 한다.

- 저장은 bounded single-writer channel로 직렬화하고 각 request에 monotonic revision과 oneshot ack를 둠.
- handler는 mutex 안에서 map을 clone한 뒤 guard를 해제하고 writer에 snapshot을 넘겨야 하며, 해당 revision의 fsync ack 전에는 성공 응답을 반환하지 않음.
- queue full은 구조화된 overload 오류로 처리하고 오래된 snapshot이 최신 file을 덮어쓰지 않게 writer가 revision을 검증함.
- lock을 잡은 채 `save().await`하지 않음.
- 임시 파일과 parent directory를 `fsync`해야 rename이 전원 장애 후에도 durable하다고 볼 수 있음.

## 한계

- VM 생성마다 전체 map을 다시 기록한다.
- 여러 API 프로세스가 동시에 쓰는 상황을 지원하지 않는다.
- 상태 변경과 event를 원자적으로 저장할 수 없다.
- 메모리 변경과 파일 publish 사이에 daemon이 종료되면 마지막 요청이 유실될 수 있다.

- 이 한계는 [SQLite migration 및 상태 모델 구현](task-sqlite-migration-and-state-model.md)에서 제거한다.

## 테스트 및 검증

- VM을 두 개 생성한 뒤 서버를 재시작하고 두 UUID가 그대로 복원되는지 확인한다.
- 저장 도중 프로세스를 종료해도 기존 `vms.json`이 유효해야 한다.

## 완료 및 후속 범위

- 구현 브랜치와 커밋이 존재함.
- 위 테스트 및 검증 항목을 모두 통과한 뒤 완료로 판정함.
