---
tags:
  - firecrab
status: 미완료
scope: 5주차
updated: 2026-07-23
---

# 웹 운영 관리 콘솔 구현

## 브랜치 개요

- 브랜치: `feat/operations-admin-console` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: 관리자가 service health, quota, audit, maintenance, backup operation을 Rust/Wasm UI에서 안전하게 조회·실행함.

## 화면 구조

```text
Operations
├── Health: DB, KVM, runtime helper, network helper, templates
├── Capacity: VM, vCPU, RAM, disk, snapshot quota
├── Audit: actor, action, resource, request ID, outcome
├── Maintenance: current mode와 active operations
└── Backups: status, created time, size, verification
```

- secret, token hash, host filesystem path, raw environment는 응답과 UI에 포함하지 않음.

## Rust 상태 모델

```rust
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct OperationsSummary {
    pub readiness: ReadinessReport,
    pub capacity: CapacitySummary,
    pub maintenance: MaintenanceStatus,
    pub active_operation_count: u32,
    pub recent_operations: Vec<OperationSummary>,
    pub latest_backup: Option<BackupSummary>,
}
```

```rust
fn admin_actions(role: Role, state: &OperationsSummary) -> Vec<AdminAction> {
    if role != Role::Admin {
        return Vec::new();
    }

    match state.maintenance {
        MaintenanceStatus::Disabled => vec![AdminAction::EnterMaintenance],
        MaintenanceStatus::Enabled => vec![AdminAction::CreateBackup, AdminAction::ExitMaintenance],
        MaintenanceStatus::Draining => Vec::new(),
    }
}
```

## 위험 작업

- summary의 recent operation은 작은 고정 개수만 포함하고 전체 목록은 cursor pagination endpoint에서 조회함.
- maintenance 진입, backup 생성, restore 준비는 operation ID를 반환하는 비동기 명령으로 처리함.
- self-contained backup은 running VM 수와 stop/checkpoint 필요 상태를 먼저 표시하고 자동으로 불완전 backup으로 낮추지 않음.
- confirmation dialog에 대상과 영향을 표시하고 사용자가 지정 확인 문자열을 입력해야 함.
- UI timeout을 작업 실패로 간주하지 않고 operation endpoint를 다시 조회함.

- restore 자체와 package upgrade는 browser에서 즉시 실행하지 않고 offline admin 절차와 preflight 결과를 안내함.
- 운영 console이 root 명령 실행 terminal이 되어서는 안 됨.

## Audit 조회

- server-side pagination, 시간·actor·action·outcome filter를 사용함.
- raw request body는 표시하지 않고 request ID로 구조화 log와 연결함.

## 테스트 및 검증

- viewer/operator 접근 차단, maintenance 경쟁 요청, backup 실패, API restart, pagination과 대량 audit를 test함.
- 모든 admin action이 server audit event와 동일한 request ID를 가져야 함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
