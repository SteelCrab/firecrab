---
tags:
  - firecrab
  - api
status: 미완료
scope: 4주차
updated: 2026-07-23
---

# Lifecycle API 자동 테스트 구현

## 브랜치 개요

- 브랜치: `feat/lifecycle-api-tests`
- 커밋: `8979eed test: add lifecycle API coverage`
- 상태: 구현 브랜치 존재
- 변경 규모: 8개 파일, 869줄 추가, 78줄 삭제
- 목적: KVM이 없는 개발 환경에서도 HTTP 계약, migration, 상태 전이, artifact rollback과 process manager를 반복 검증한다.

## 테스트 가능한 경계

```rust
#[async_trait::async_trait]
pub trait VmRuntime: Send + Sync {
    async fn start(&self, spec: RuntimeSpec) -> anyhow::Result<ProcessIdentity>;
    async fn stop(&self, id: Uuid, identity: ProcessIdentity) -> anyhow::Result<()>;
    async fn is_alive(&self, identity: &ProcessIdentity) -> anyhow::Result<bool>;
}
```

- 운영 환경은 `FirecrackerRuntime`, 일반 test는 결과와 지연을 제어할 수 있는 `FakeRuntime`을 주입한다.
- SQLite test는 VM마다 임시 디렉터리의 독립 DB를 사용한다.

## HTTP 통합 테스트

```rust
#[tokio::test]
async fn concurrent_start_accepts_only_one_request() {
    let app = test_app().await;
    let id = create_vm(&app).await;

    let responses = send_concurrent_start_requests(&app, id, 20).await;
    assert_eq!(count_status(&responses, StatusCode::ACCEPTED), 1);
    assert_eq!(count_status(&responses, StatusCode::CONFLICT), 19);
}
```

- 최소 test matrix:

- 생성, 목록, 상세, start, stop, delete의 status와 JSON schema
- 입력 경계값, unknown field, body limit, CORS
- migration 반복 실행과 이전 schema 업그레이드
- 허용·금지 상태 전이와 동시 요청
- idempotency key, starting 중 stop cancellation, operation lease/phase와 daemon 재시작 복구
- rootfs 복사 및 network 준비 실패 시 rollback
- 정상 종료, crash, readiness timeout, daemon 복구

## 실제 KVM 테스트

```sh
FIRECRAB_KVM_TEST=1 cargo test --test kvm_lifecycle -- --nocapture
```

- 환경 변수가 없으면 명시적으로 skip한다.
- unit test가 KVM 권한이나 host network를 변경하면 안 된다.

## 테스트 및 검증

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

- 실패 test가 host에 process, socket, 임시 rootfs, TAP을 남기지 않는지도 teardown에서 검사함.
- test cleanup 오류를 무시하지 않고 원래 test 실패와 함께 보고하며, 전역 KVM/network test는 process 간 file lock으로 직렬화함.

## 완료 및 후속 범위

- 구현 브랜치와 커밋이 존재함.
- 위 테스트 및 검증 항목을 모두 통과한 뒤 완료로 판정함.
