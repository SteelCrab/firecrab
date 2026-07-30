---
tags:
  - firecrab
  - operations
status: 미완료
scope: 5주차
updated: 2026-07-23
---

# 리소스 quota 및 admission control 구현

## 브랜치 개요

- 브랜치: `feat/resource-quotas-and-admission` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: principal과 host 전체의 VM 수, vCPU, RAM, disk, snapshot byte, 동시 operation을 DB transaction으로 예약함.

## 모델

```rust
#[derive(Debug, Clone)]
pub struct ResourceRequest {
    pub vm_count: u32,
    pub vcpu: u32,
    pub memory_mib: u64,
    pub disk_logical_bytes: u64,
    pub disk_physical_reserve_bytes: u64,
    pub snapshot_logical_bytes: u64,
    pub snapshot_physical_reserve_bytes: u64,
    pub operations: u32,
}

#[derive(Debug, Clone)]
pub struct QuotaLimit {
    pub principal_id: Uuid,
    pub maximum: ResourceRequest,
}
```

## 원자적 예약

```rust
pub async fn reserve_in_operation(
    &self,
    transaction: &mut ImmediateTransaction,
    principal: Uuid,
    operation: Uuid,
    request: &ResourceRequest,
) -> Result<QuotaReservation, AdmissionError> {
    reserve_if_within_limits(
        transaction,
        principal,
        operation,
        request,
    ).await
}
```

- 현재 사용량을 읽고 나중에 증가시키는 두 단계 로직은 동시 요청에서 초과 할당됨.
- conditional update와 reservation row를 같은 immediate transaction에서 생성함.
- lifecycle 상태 전이, queued operation, event와 quota reservation도 하나의 transaction에서 commit해 operation만 있거나 quota만 잡힌 crash window를 없앰.

- reservation은 `(operation_id, resource_kind)` unique ledger row로 저장하고 `reserved`, `committed`, `released` 상태를 조건부 전이함.
- 숫자 counter만 감소시키는 방식은 retry에서 이중 반환될 수 있으므로 사용하지 않음.

## Admission 조건

- principal quota와 host global quota
- 실제 free disk의 안전 임계값
- KVM·helper readiness
- VM별 cgroup overhead를 포함한 host memory 예산
- 진행 중인 lifecycle/snapshot operation 수

- reflink나 sparse file의 현재 allocated block만 quota로 세면 Guest write 이후 COW가 발생할 때 host disk를 초과할 수 있음.
- logical size와 현재 physical usage를 모두 관측하되 admission은 writable disk와 snapshot이 최대로 COW될 upper bound를 예약하거나 filesystem project quota로 강제함.
- filesystem free-space check는 다른 process와 경쟁하는 advisory check이므로 reservation을 대체하지 않음.

- snapshot restore는 immutable snapshot을 유지한 채 새 writable rootfs generation을 만들므로 둘의 동시 COW upper bound와 memory backend 보존 공간을 추가 예약함.
- previous active generation을 rollback 보관하는 기간도 quota에 포함하고 retention 삭제가 끝난 뒤에만 반환함.

- 예약은 artifact 생성 전에 수행하고 실제 resource 소유가 확정되면 `committed`로 전환함.
- 실패·취소는 reservation을 반환하고 VM disk는 delete cleanup, snapshot bytes는 GC가 실제 파일 제거와 directory fsync를 완료한 뒤에만 idempotent하게 `released`로 전환함.
- active operation count는 terminal operation commit과 함께 반환함.

## 테스트 및 검증

- quota 경계에 동시 요청을 보내도 한도를 넘지 않아야 함.
- reflink COW와 sparse file 확장, operation commit 직후 crash, artifact 삭제 실패를 주입함.
- process crash 후 reservation reconciliation이 DB ledger, filesystem project quota, artifact와 process 실상에 맞게 사용량을 복구해야 함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
