---
tags:
  - firecrab
  - api
status: 완료
scope: 2주차
updated: 2026-07-23
---

# Rust API 서버 기반 구현

## 브랜치 개요

- 브랜치: `feat/rust-api-server-foundation`
- 커밋: `09b67bf feat: add vm create API`
- 상태: 구현 브랜치 존재
- 변경 규모: 6개 파일, 125줄 추가
- 목적: Tokio runtime 위에서 axum Router를 실행하고 모든 handler가 공유할 `AppState`를 주입한다.
- 현재 `main` 브랜치에서 완료된 최소 기반에 해당한다.
- 범위: 아래 코드는 현재 baseline을 설명함.
- 이후 task의 `state.store`, `state.operations`, helper client는 SQLite와 공통 operation 계약을 적용한 목표 구조이며 이 `HashMap` 구조와 동시에 유지하지 않음.

## 모듈

```text
firecrab-api/src/
├── main.rs
├── handlers.rs
├── handlers/vms.rs
├── model.rs
├── persistence.rs
└── state.rs
```

## Rust 구현

```rust
// state.rs
use std::{collections::HashMap, sync::{Arc, Mutex}};
use uuid::Uuid;

use crate::model::VmRecord;

#[derive(Clone, Default)]
pub struct AppState {
    pub vms: Arc<Mutex<HashMap<Uuid, VmRecord>>>,
}
```

```rust
// main.rs
use axum::{routing::post, Router};

let state = AppState::new();
let app = Router::new()
    .route("/api/vms", post(handlers::vms::create_vm))
    .with_state(state);

let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
axum::serve(listener, app).await?;
```

- `AppState`는 clone할 때 실제 VM map을 복사하지 않고 `Arc`의 참조만 늘린다.
- 이후 SQLite로 전환하면 `Mutex<HashMap<...>>` 대신 clone 비용이 낮은 `SqlitePool`을 보관한다.

- `std::sync::Mutex` guard를 잡은 상태로 파일이나 network I/O를 수행하거나 `.await`하지 않음.
- 현재 JSON baseline에서는 lock 안에서 map snapshot을 clone한 뒤 lock을 해제하고 단일 persistence writer에 저장함.
- SQLite 전환 후에는 이 map과 mutex를 제거함.

## 테스트 및 검증

```sh
cd firecrab-api
cargo fmt --check
cargo test
cargo run
```

- `curl -i http://127.0.0.1:3000/api/vms`가 Router까지 도달하면 서버 기반 구성이 유효하다.
- 현재 `main`에는 GET route가 없으므로 이 단계에서는 `405 Method Not Allowed`가 정상이다.

## 완료 및 후속 범위

- 구현 브랜치와 커밋이 존재함.
- 위 테스트 및 검증 항목을 모두 통과한 뒤 완료로 판정함.
