---
tags:
  - firecrab
  - vm
status: 완료
scope: 2주차
updated: 2026-07-23
---

# MicroVM 생성 API 구현

## 브랜치 개요

- 브랜치: `feat/microvm-create-api`
- 커밋: `09b67bf feat: add vm create API`
- 상태: 구현 브랜치 존재
- 변경 규모: 6개 파일, 125줄 추가
- 목적: `POST /api/vms`의 JSON 요청을 역직렬화하고 UUID와 초기 상태를 부여한 뒤 VM 레코드를 반환한다.
- 범위: `main@2fea1c3`의 완료 기준은 `f64` CPU와 JSON 저장을 사용하는 최소 구현임.
- 아래 코드는 입력 계약과 SQLite store까지 적용한 목표 형태이며 migration 이전 baseline과 혼용하지 않음.

## 요청과 응답 모델

```rust
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateVmRequest {
    pub name: String,
    pub template: String,
    pub cpu: u8,
    pub ram: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateVmResponse {
    pub id: Uuid,
    pub name: String,
    pub state: VmState,
    pub template: String,
    pub template_version: String,
    pub cpu: u8,
    pub ram: u32,
}
```

- Firecracker의 `vcpu_count`는 정수이므로 신규 계약에서는 `cpu`를 `u8`로 둔다.
- 범위 검증은 [관리 API 보안 및 입력 계약 강화](task-api-security-and-input-validation.md)에서 처리한다.

## Handler

```rust
pub async fn create_vm(
    State(state): State<AppState>,
    Json(req): Json<CreateVmRequest>,
) -> Result<(StatusCode, Json<CreateVmResponse>), AppError> {
    let template = state
        .templates
        .resolve_alias(&req.template)
        .cloned()
        .ok_or_else(|| AppError::invalid_template(&req.template))?;

    let vm = NewVmRecord {
        id: Uuid::new_v4(),
        name: req.name,
        state: VmState::Created,
        template: template.name.clone(),
        template_version: template.version.clone(),
        cpu: req.cpu,
        ram: req.ram,
    };

    let stored = state
        .store
        .insert_vm_with_template_digests(
            &vm,
            &template.kernel_sha256,
            &template.rootfs_sha256,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(CreateVmResponse::from(stored))))
}
```

- 저장 성공 이후에만 `201 Created`를 반환한다.
- 저장 실패를 성공 응답으로 숨기거나 handler에서 `unwrap()`으로 프로세스를 종료하면 안 된다.

- template alias는 atomic registry generation에서 owned immutable reference로 resolve하고, 그 version/digest의 VM row와 idempotency 결과를 같은 DB transaction에 저장함.
- client 재시도로 VM이 중복 생성되지 않도록 `Idempotency-Key`를 지원함.
- 같은 key와 같은 body는 기존 `201` 응답을 재사용하고, 같은 key에 다른 body는 `409 idempotency_conflict`를 반환함.
- 세부 계약은 [API 오류·idempotency·비동기 operation 계약](task-api-operation-contracts.md)을 따름.

## 테스트 및 검증

```sh
curl -i -X POST http://127.0.0.1:3000/api/vms \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: create-test-vm-001' \
  -d '{"name":"test-vm","template":"ubuntu-rootfs-26.04","cpu":1,"ram":512}'
```

- 응답 status가 `201`이고 `id`, `name`, `state`, `template`, `templateVersion`, `cpu`, `ram`이 모두 포함되어야 한다.
- alias 승격 뒤 기존 VM row의 version/digest가 바뀌면 안 됨.

## 완료 및 후속 범위

- 구현 브랜치와 커밋이 존재함.
- 위 테스트 및 검증 항목을 모두 통과한 뒤 완료로 판정함.
