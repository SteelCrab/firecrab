---
tags:
  - firecrab
  - api
status: 미완료
scope: 5주차
updated: 2026-07-23
---

# API 권한 및 감사 기록 구현

## 브랜치 개요

- 브랜치: `feat/api-authorization-and-audit` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: 인증된 principal의 역할에 따라 action을 제한하고 허용·거부·실패 결과를 append-only audit event로 기록함.

## 권한 모델

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Viewer,
    Operator,
    Admin,
}

#[derive(Debug, Clone, Copy)]
pub enum Action {
    VmRead,
    VmCreate,
    VmStart,
    VmStop,
    VmDelete,
    SnapshotManage,
    TemplatePromote,
    TokenManage,
    AuditRead,
    QuotaManage,
    MaintenanceManage,
    BackupManage,
}
```

```rust
pub fn minimum_role(action: Action) -> Role {
    match action {
        Action::VmRead => Role::Viewer,
        Action::VmCreate | Action::VmStart | Action::VmStop
        | Action::VmDelete | Action::SnapshotManage => Role::Operator,
        Action::TemplatePromote | Action::TokenManage | Action::AuditRead
        | Action::QuotaManage | Action::MaintenanceManage
        | Action::BackupManage => Role::Admin,
    }
}
```

- 권한 검사는 route 이름이 아니라 명시적인 domain action 기준으로 수행함.
- `minimum_role`은 `Action`을 exhaustive match하므로 신규 action 추가 시 compiler가 정책 결정을 요구함.
- role 비교 함수는 명시적인 rank만 사용하고 VM 소유권을 추가하면 role과 resource scope를 함께 검사함.

## Audit event

```rust
#[derive(Serialize)]
pub struct AuditEvent {
    pub sequence: i64,
    pub id: Uuid,
    pub occurred_at: OffsetDateTime,
    pub principal_id: Option<Uuid>,
    pub action: String,
    pub resource_type: String,
    pub resource_id: Option<Uuid>,
    pub request_id: Uuid,
    pub outcome: AuditOutcome,
    pub previous_hash: String,
    pub event_hash: String,
}
```

- 허용된 변경 작업의 authorization/접수 audit와 state transition은 가능한 한 같은 DB transaction에 기록함.
- `202 Accepted`를 실제 VM 작업 성공으로 기록하지 않고 operation terminal 시 별도 completion/failure audit를 같은 operation ID로 연결함.
- 거부 요청과 인증 실패도 secret 없이 별도 audit event로 기록함.
- 인증 전 실패는 `principal_id=None`이며 token 원문, MAC이나 전체 credential ID를 식별자로 대신 기록하지 않음.

- audit 삭제·수정 API는 제공하지 않음.
- SQLite의 single writer transaction에서 마지막 sequence/hash를 읽고 `SHA-256(domain_separator || previous_hash || canonical_event_without_hash_fields)`로 다음 hash를 계산해 monotonic sequence와 함께 삽입함.
- sequence unique constraint와 SQLite trigger로 `UPDATE`와 `DELETE`를 거부함.
- 다만 DB file 관리자까지 막는 tamper-proof storage는 아니므로 주기적으로 서명된 외부 append-only sink에 export하고 checkpoint hash를 보관함.
- local append-only capacity와 최소 여유 공간을 admission/readiness에서 감시하고 임계값에서 관리 변경을 fail closed해 disk full로 DB 전체가 손상되는 것을 막음.
- 인증 실패 중 DB 장애가 나면 bounded security log에 fallback하고 누락 여부를 health에 표시함.

## 테스트 및 검증

- 역할별 모든 action matrix를 table-driven test로 검증함.
- 신규 Action fixture가 정책에서 누락되면 compile/test가 실패해야 함.
- audit transaction 실패 시 관리 변경도 성공하면 안 됨.
- 동시 audit insert, hash chain 변조, export 실패, 중복 request ID, token·private key·raw body 노출이 없어야 함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
