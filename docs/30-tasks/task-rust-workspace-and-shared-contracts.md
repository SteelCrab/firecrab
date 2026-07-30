---
tags:
  - firecrab
status: 완료
scope: 2주차
updated: 2026-07-23
---

# Rust workspace 및 shared contract 구성

## 브랜치 개요

- 브랜치: `feat/rust-workspace-shared-contracts`
- 커밋: `a6843e1 feat: add Rust workspace and shared contracts`
- 상태: 구현 및 자동 테스트 완료
- 변경 규모: 12개 파일, 201줄 추가, 27줄 삭제
- 목적: 단일 API crate를 Cargo workspace로 전환하고 API 및 helper 통신 계약을 공용 crate로 분리함.
- 범위: 새 API 엔드포인트를 추가하지 않고 crate 구조와 데이터 계약을 정리함.

## Workspace 구성

- 루트 `Cargo.toml`에서 세 crate를 하나의 workspace로 관리함.
- 공통 package 정보와 `serde`, `serde_json`, `uuid` 버전을 루트에서 관리함.
- `Cargo.lock`을 `firecrab-api/`에서 저장소 루트로 이동함.
- Rust `1.94.1`과 `clippy`, `rustfmt` component를 `rust-toolchain.toml`에 고정함.

```toml
[workspace]
resolver = "3"
members = [
    "firecrab-api",
    "firecrab-api-types",
    "firecrab-helper-protocol",
]

[workspace.package]
version = "0.1.0"
edition = "2024"
rust-version = "1.94.1"
license = "MIT"
```

- `firecrab-api`: Axum API 서버 실행과 VM 상태 관리 담당.
- `firecrab-api-types`: 외부 API 요청과 응답 타입 담당.
- `firecrab-helper-protocol`: API와 권한 helper 사이의 요청 규격 담당.

## API 공유 타입

- `firecrab-api-types/src/lib.rs`에 VM API의 공용 타입을 정의함.
- `CreateVmRequest`는 정의되지 않은 JSON 필드를 거부함.
- `VmState::Created`는 JSON에서 `"created"`로 직렬화함.
- `VmResponse`는 응답 필드 이름을 camelCase로 직렬화함.

```rust
#[serde(rename_all = "lowercase")]
pub enum VmState {
    Created,
}

#[serde(deny_unknown_fields)]
pub struct CreateVmRequest {
    pub name: String,
    pub template: String,
    pub ram: u32,
    pub cpu: f64,
}

#[serde(rename_all = "camelCase")]
pub struct VmResponse {
    pub id: Uuid,
    pub name: String,
    pub state: VmState,
    pub template: String,
    pub cpu: f64,
    pub ram: u32,
}
```

- 기존 API의 중복 타입을 제거하고 공유 타입을 다시 내보냄.

```rust
pub use firecrab_api_types::CreateVmRequest;
pub use firecrab_api_types::VmState;
```

- 현재 생성 handler는 `VmResponse`가 아니라 내부 `VmRecord`를 반환함.
- `VmResponse`를 실제 응답에 적용하는 작업은 후속 범위임.

## Helper 통신 계약

- `firecrab-helper-protocol/src/lib.rs`에 helper 요청과 protocol version을 정의함.
- `PrepareRuntime`과 `RemoveRuntime` 요청은 VM ID와 runtime ID를 전달함.
- `RequestEnvelope`는 version과 실제 요청을 함께 전달함.
- 지원하지 않는 version은 `ProtocolError::UnsupportedVersion`으로 거부함.

```rust
pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_INTERFACE_NAME_LEN: usize = 15;

#[serde(tag = "type", rename_all = "snake_case")]
pub enum HelperRequest {
    PrepareRuntime { vm_id: Uuid, runtime_id: Uuid },
    RemoveRuntime { vm_id: Uuid, runtime_id: Uuid },
}

pub struct RequestEnvelope {
    pub version: u16,
    pub request: HelperRequest,
}
```

```rust
pub fn validate(self) -> Result<HelperRequest, ProtocolError> {
    if self.version != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion(self.version));
    }
    Ok(self.request)
}
```

- 이 crate는 메시지 규격만 정의함.
- VM, TAP, 네트워크를 직접 조작하는 helper binary는 아직 구현하지 않음.

## 테스트 및 검증

- `vm_response_round_trips`: `VmResponse`의 JSON 직렬화와 역직렬화 결과를 검증함.
- `rejects_unknown_protocol_version`: 지원하지 않는 protocol version을 거부하는지 검증함.
- 현재 workspace 자동 테스트는 총 2개이며 모두 통과함.

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

## 완료 및 후속 범위

- Cargo workspace와 공통 dependency 구성을 완료함.
- API 요청·응답 타입과 helper 요청 계약 분리를 완료함.
- 기존 API가 공용 `CreateVmRequest`, `VmState`를 사용하도록 변경함.
- `docs/api.md`의 상태 응답 예시 `"Created"`는 실제 값 `"created"`와 동기화해야 함.
- 실제 helper 실행 경로와 `VmResponse` 적용은 후속 작업으로 남아 있음.
