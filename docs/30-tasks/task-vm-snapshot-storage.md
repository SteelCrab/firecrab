---
tags:
  - firecrab
  - snapshot
status: 미완료
scope: 7주차
updated: 2026-07-23
---

# VM snapshot 저장 모델 구현

## 브랜치 개요

- 브랜치: `feat/vm-snapshot-storage` (예정)
- 커밋: 없음
- 상태: 구현 예정
- 변경 규모: 구현 전
- 목적: Firecracker snapshot state, guest memory, writable block disk 사본과 compatibility manifest를 하나의 versioned artifact로 관리함.
- Firecracker가 block device를 snapshot에 포함하지 않으므로 rootfs 사본은 필수임.
- memory와 disk는 guest secret을 포함할 수 있어 엄격하게 취급함.
- 범위: 공식 [snapshot support](https://github.com/firecracker-microvm/firecracker/blob/main/docs/snapshotting/snapshot-support.md)와 [snapshot versioning](https://github.com/firecracker-microvm/firecracker/blob/main/docs/snapshotting/versioning.md)의 file lifetime, disk, external path, CPU/kernel 호환성 제약을 기준으로 함.

## DB 모델

```sql
CREATE TABLE vm_snapshots (
    id                 TEXT PRIMARY KEY,
    vm_id              TEXT NOT NULL REFERENCES vms(id),
    status             TEXT NOT NULL CHECK (status IN ('creating', 'ready', 'failed', 'deleting')),
    firecracker_version TEXT NOT NULL,
    snapshot_data_version TEXT NOT NULL,
    architecture       TEXT NOT NULL,
    host_kernel_release TEXT NOT NULL,
    host_cpu_fingerprint TEXT NOT NULL,
    template           TEXT NOT NULL,
    template_version   TEXT NOT NULL,
    template_kernel_sha256 TEXT NOT NULL,
    template_rootfs_sha256 TEXT NOT NULL,
    artifact_generation TEXT NOT NULL UNIQUE,
    manifest_sha256    TEXT,
    manifest_mac       TEXT,
    manifest_key_version INTEGER,
    restore_policy       TEXT NOT NULL CHECK (restore_policy IN ('one_shot')),
    consumed_at          TEXT,
    consumed_operation_id TEXT,
    logical_size_bytes INTEGER CHECK (logical_size_bytes >= 0),
    allocated_size_bytes INTEGER CHECK (allocated_size_bytes >= 0),
    created_at         TEXT NOT NULL,
    last_error         TEXT,
    CHECK ((consumed_at IS NULL) = (consumed_operation_id IS NULL)),
    CHECK (consumed_at IS NULL OR status IN ('ready', 'deleting'))
);

CREATE TABLE vm_snapshot_leases (
    snapshot_id      TEXT NOT NULL REFERENCES vm_snapshots(id),
    holder_kind      TEXT NOT NULL CHECK (holder_kind IN ('restore', 'runtime', 'backup')),
    holder_id        TEXT NOT NULL,
    acquired_at      TEXT NOT NULL,
    lease_expires_at TEXT,
    PRIMARY KEY (snapshot_id, holder_kind, holder_id)
);
```

- 초기 production 범위의 `restore_policy`는 `one_shot`만 허용함.
- 성공한 checkpoint에서 원본 실행을 이어가거나 같은 memory state를 두 번 resume하면 Guest의 random state, token과 외부 side effect가 중복될 수 있기 때문임.
- snapshot 생성 성공 후 VM은 Week 4 migration에서 추가한 `checkpointed` 상태가 되고 일반 start는 거부됨.
- 관리자는 해당 snapshot을 한 번 restore하거나 snapshot을 폐기해 `stopped`로 전환한 뒤 cold start해야 함.

- SQLite는 기존 `vms.state` CHECK를 직접 확장할 수 없으므로 Week 4 migration에서 table rebuild 절차로 `checkpointed`를 추가하고 foreign key/integrity check를 수행함.
- `artifact_generation`은 UUID이며 실제 state, memory와 rootfs 경로는 설정된 snapshot root 아래의 고정 파일명으로 파생함.
- DB의 임의 path를 filesystem capability로 사용하지 않음.
- 초기 machine config는 writable rootfs 하나만 지원하며 추가 writable drive가 있으면 snapshot 요청을 거부하고 manifest schema를 먼저 확장함.

## Manifest

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub schema_version: u32,
    pub snapshot_id: Uuid,
    pub vm_id: Uuid,
    pub firecracker_version: String,
    pub snapshot_data_version: String,
    pub architecture: String,
    pub host_kernel_release: String,
    pub host_cpu_fingerprint: String,
    pub template: String,
    pub template_version: String,
    pub cpu_template_digest: Option<String>,
    pub template_kernel_sha256: String,
    pub template_rootfs_sha256: String,
    pub snapshot_rootfs_sha256: String,
    pub state_sha256: String,
    pub memory_sha256: String,
    pub runtime_paths: SnapshotRuntimePaths,
}
```

- `runtime_paths`에는 snapshot state가 참조하는 block 상대 경로, TAP 이름과 vsock UDS 이름을 기록함.
- Firecracker snapshot은 이 외부 자원을 원래와 같은 경로에서 요구하므로 restore preflight에서 검증함.
- manifest는 canonical encoding version을 고정하고 HMAC은 canonical bytes 전체에 적용해 JSON key 순서나 중복 key에 따른 검증 차이를 없앰.

## 원자적 publish

- snapshot은 `.tmp/<snapshot_uuid>`에 생성함.
- VM이 pause된 동안 state, memory와 writable rootfs 사본을 생성함.
- rootfs는 reflink가 가능하면 clone하고, 불가능하면 pause 시간과 disk quota를 확인한 뒤 full copy함.
- 모든 file과 directory를 fsync하고 checksum·manifest를 작성한 뒤 최종 directory로 rename하고 `ready`로 전환함.
- DB나 manifest의 path를 그대로 열지 않고 snapshot root directory handle에서 UUID 기반 상대 경로를 다시 계산함.
- `ready` 이전 경로는 조회·복원 API에 노출하지 않음.

- filesystem rename과 SQLite commit은 하나의 atomic transaction이 아니므로 operation phase에 artifact generation과 expected location을 먼저 기록함.
- crash 뒤 final directory만 있으면 HMAC/checksum과 ownership을 다시 검증해 DB publish를 완료하고, DB `creating` row만 있고 file이 불완전하면 정리·실패 처리함.
- 이름만 맞는 orphan directory를 자동 채택하지 않음.

- Firecracker가 생성하는 staging output은 실행 UID가 쓸 수 있는 전용 directory에 두되 publish 전에 helper가 file type/link count/owner를 다시 확인함.
- Published snapshot directory는 helper/service만 수정하고 state/memory는 `root:VM 전용 GID 0440`으로 고정해 restored Firecracker가 읽을 수 있지만 쓸 수 없게 함.
- immutable rootfs/manifest는 helper만 읽는 `0400`으로 둠.
- VM별 GID는 snapshot이 존재하는 동안 유지하고 재사용하지 않음.
- logical file length와 filesystem allocated block을 각각 기록하고 quota는 reflink COW의 최악 크기까지 포함해 예약함.
- 실제 크기가 예약을 넘으면 실패 처리함.
- checksum은 accidental corruption 검사용이고, manifest는 versioned secret의 HMAC으로 인증함.
- Local host storage가 물리적 반출 위협에 포함되면 data root를 host-managed encrypted volume에 두고, 외부 반출 bundle은 항상 별도 암호화함.

- HMAC key rotation은 기존 ready snapshot이 참조하는 key version을 보존한 상태에서 신규 snapshot부터 새 key를 사용함.
- old key를 폐기하려면 manifest를 verified offline 절차로 새 key에 re-MAC하고 DB key version을 atomic하게 갱신하거나 해당 snapshot을 먼저 삭제함.
- key가 없는데 checksum만 확인해 restore하지 않음.

## Retention

- VM별 개수와 전체 byte quota를 설정함.
- 자동 GC는 `ready`이며 consumed, lease가 없고 정책 대상인 snapshot만 `deleting`으로 선점함.
- VM을 `checkpointed`로 잠근 unconsumed snapshot은 자동 GC하지 않음.
- 생성·복원·backup 중이거나 restored VM의 memory backend로 사용 중인 snapshot에는 durable lease를 두고 삭제하지 않음.
- restore/backup lease는 operation worker lease와 함께 reconcile하고 runtime lease는 process identity가 종료될 때까지 expiry만으로 제거하지 않음.
- Firecracker는 restore 후 memory file을 MAP_PRIVATE backing으로 계속 사용하므로 VM 종료 전 변경·삭제하면 안 됨.
- `one_shot` snapshot은 restore의 irreversible resume intent transaction에서 `consumed_at`과 operation ID를 기록하며 이후 다시 claim할 수 없음.

## 삭제 계약

- `DELETE /api/vms/{vm_id}/snapshots/{snapshot_id}`는 공통 idempotency 계약의 영속 operation을 `202`로 반환함.
- in-use/backup lease가 있거나 create/restore 중이면 `409`로 거부함.
- 일반 삭제는 row를 `deleting`으로 선점하고 artifact directory 제거와 parent fsync가 끝난 뒤 DB row 및 quota를 release함.
- 파일 정리 실패를 ready/삭제 완료로 숨기지 않고 recovery가 같은 generation을 재시도함.

- 현재 VM을 잠근 unconsumed one-shot checkpoint의 삭제는 `discard` 의미를 가짐.
- reservation transaction은 exact snapshot ownership과 process 부재를 확인해 snapshot만 `deleting`으로 선점하고 VM은 `checkpointed`로 유지함.
- artifact 제거, parent fsync와 quota 반환이 끝난 최종 transaction에서 snapshot row를 제거하고 `checkpointed -> stopped`로 전환해 중간 start race를 막음.
- VM 자체 delete는 연결된 snapshot row와 in-use lease가 모두 사라진 뒤에만 허용함.
- unlink가 SSD/reflink의 물리적 secure erase를 보장한다고 문서화하지 않으며 at-rest 위협은 storage encryption과 key 폐기로 처리함.

## 테스트 및 검증

- disk full, reflink 미지원, checksum/HMAC 실패, daemon crash를 publish/delete 각 단계에 주입함.
- 부분 artifact가 `ready`로 보이지 않고 snapshot memory·rootfs가 일반 사용자에게 읽히지 않아야 함.
- in-use snapshot GC, consumed snapshot 재복원과 snapshot 보유 VM 삭제가 거부되고 discard 완료 전 일반 start가 차단되는지 확인함.

## 완료 및 후속 범위

- 현재 로컬 구현 브랜치는 없음.
- 위 설계와 테스트 기준을 충족하는 구현 및 검증이 필요함.
