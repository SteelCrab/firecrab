---
tags:
  - firecrab
  - storage
  - blog
  - guide
updated: 2026-07-31
---

# MicroStorage - 물리적 디스크 고르게 쓰기

> [!summary] 한 줄
> MicroNetwork가 “어느 네트워크에 붙일지”라면, MicroStorage는 **어느 물리 디스크에 VM 디스크를 둘지**다.
> AWS로 치면 EBS 풀/볼륨 위치에 가깝고, 파티션을 새로 쪼개 주는 서비스는 아니다.

## 왜 나왔나

동시에 여러 VM을 켜면 디스크 일이 한 장치에 몰린다.

1. 템플릿 rootfs → VM 전용 파일 복사
2. `diskGb`까지 늘리기 (`set_len` + `resize2fs`)

한 대는 수 GB 순차 쓰기다. 같은 물리 디스크(`data/vms/` 한곳)에 여러 대가 붙으면:

- `iostat` `%util` ≈ 100%
- `w_await` 수백 ms
- 대시보드 “디스크 준비”에 오래 멈춘 것처럼 보임

> [!important] 2026-07-21 실측
> NVMe 하드웨어 문제는 아니었다. **한 큐에 I/O가 몰린 것**이다.
> 소프트웨어 버그(템플릿 재해싱·타임아웃 orphan)도 겹쳤고, 그 수정 기록은
> [vm-startup-stuck-under-concurrent-load](../50-bugs/vm-startup-stuck-under-concurrent-load.md)에 있다.
> 디스크 경로를 나누는 쪽은 이 글의 주제다.

네트워크를 MicroNetwork로 나눈 것처럼, **디스크 경로도 나눌 수 있어야** 한다.

---

## 세 겹으로 정리하면

| 층 | 하는 일 | 상태 |
|---|---|---|
| 물리 디스크 선택 | VM 생성 시 어느 root에 둘지 | 완료 |
| MicroStorage 서비스 | 풀 등록·수동 재할당·마운트 탐색 | 완료 |
| disk generation / artifact | start마다 깨지지 않는 rootfs·runtime 레이아웃 | 완료 |

아래는 그 순서대로 무엇·왜·어떻게만 짚는다.

---

## 1. VM 생성 시 물리 디스크 선택

### 한 줄

등록된 **storage root id**만 고른다. path 문자열을 클라이언트가 마음대로 넣지 않는다.

### root가 오는 곳

| 종류 | 출처 | 비고 |
|---|---|---|
| `default` | cwd 기준 `data/` | 예전 레이아웃 그대로 |
| env | `FIRECRAB_STORAGE_ROOTS=id=path:id2=path2` | 배포·고정 경로 |
| MicroStorage | DB 이름+절대경로 | API/UI 등록 |

통합 목록:

```http
GET /api/storage
```

```json
[
  { "id": "default", "name": "default", "path": "data",
    "availableGib": 120, "totalGib": 500, "kind": "default" },
  { "id": "a1b2…", "name": "nvme1", "path": "/mnt/disk2",
    "availableGib": 800, "totalGib": 1000, "kind": "micro_storage" }
]
```

### 생성 시

```json
POST /api/vms
{
  "name": "worker-1",
  "template": "alpine-3.24",
  "cpu": 1,
  "ram": 512,
  "diskGb": 2,
  "storageRoot": "<default | env-id | micro-storage-uuid>"
}
```

- 미지정 → 목록 첫 root (`default` 또는 env 첫 id)
- 없는 id / 여유 공간 < `diskGb` → **복사 전에** `400` (`storageRoot`)
- 실제 파일 위치: `{root path}/vms/{vm-id}/…` (아래 generation 절)

> [!note] 호환
> 풀을 안 쓰면 예전과 같이 `data/vms/{id}/`다. 기존 스크립트는 깨지지 않는다.

스펙 메모: [task-vm-physical-disk-selection](../30-tasks/task-vm-physical-disk-selection.md)

---

## 2. MicroStorage 서비스

### 한 줄

호스트에 **이미 마운트된** 디렉터리에 이름을 붙인 영구 풀이다. CRUD와 VM에 수동으로 붙이는 기능이 있다.

### API

| 메서드 | 경로 | 역할 |
|---|---|---|
| `GET` | `/api/micro-storages` | 풀 목록 + 여유 공간 |
| `POST` | `/api/micro-storages` | `{ "name", "path" }` 등록 (절대 경로) |
| `GET` | `/api/micro-storages/{id}` | 상세 + 이 풀을 쓰는 VM |
| `DELETE` | `/api/micro-storages/{id}` | 삭제 (VM 있으면 **409**) |
| `PUT` | `/api/vms/{id}/storage` | 수동 재할당 `{ "storageRoot" }` |

### 재할당 조건

- VM 상태: `created` / `stopped` / `error`
- **rootfs가 이미 있으면 409** — 수 GB를 조용히 옮기지 않는다
- UI: 헤더 **MicroStorage** 모달 · 생성 폼 · VM 상세 편집

### 파티션 — “탐색만”

```http
GET /api/storage/devices
```

| 하는 것 | 하지 않는 것 |
|---|---|
| 마운트된 파티션/FS **목록** | `fdisk` / 파티션 테이블 생성 |
| path를 풀로 **등록** | `mkfs` · 포맷 · 마운트 자체 |
| 여유 공간 표시 | 게스트 안 파티션 나누기 |

`/proc/mounts`(+ 가능하면 `lsblk`)로 실디스크 마운트만 고른다. UI에서 행을 고르면 path가 채워진다.

> [!important] 신뢰 경계
> API는 보통 비특권이다. 파티션 조작은 root·파괴적 작업이다.
> 클라이언트가 임의 path를 넘기면 host 전역 쓰기가 되므로 **등록된 id만** 받는다.

### 실무 흐름

```text
1. (호스트) 두 번째 디스크 → /mnt/disk2 마운트
2. 대시보드 MicroStorage → name: disk2, path: /mnt/disk2
   또는 devices 표에서 선택
3. VM 생성 시 저장 위치 = disk2
4. 여러 대를 default / disk2 에 나눠 동시 start
5. iostat 으로 장치 util 이 갈라지는지 확인
```

env만 쓸 때:

```sh
export FIRECRAB_STORAGE_ROOTS="local=data:fast=/mnt/disk2"
```

---

## 3. disk generation · artifact ledger

### 한 줄

**디스크 내용**과 **이번 start의 런타임 파일**을 나눈다. stop→start 해도 rootfs는 그대로 두고, config/socket만 새로 만든다.

### 경로 모델

호스트 절대 경로는 durable state에 넣지 않는다. 설정된 vms root에서만 파생한다.

```text
{vms_root}/{vm_id}/
  d/{generation}.ext4     # durable writable rootfs
  d/.{generation}.tmp     # atomic publish 임시
  r/{runtime}/
    fc.json               # 이번 start Firecracker config
    fc.sock               # API socket
    console.log           # guest console
```

- 짧은 이름(`d`, `r`, `fc.sock`) — nested UUID 아래 AF_UNIX 경로 길이(~108B)를 지키기 위함
- DB: `vms.disk_generation`, `vms.last_runtime_id` (UUID text)
- 구현: `firecrab-api/src/artifacts.rs` — `VmArtifactPaths`

> [!note] 이름
> 주차 메모의 `disks/{gen}.ext4` 와 같은 뜻이다. 실제 트리는 `d/{gen}.ext4` + `r/{runtime}/`다.

### 시점별 동작

| 시점 | 동작 |
|---|---|
| 첫 start | generation UUID 할당 → temp copy → rename → grow → specialize · runtime dir 생성 |
| stop→start | **같은** generation 파일 재사용(inode/내용 유지) · **새** runtime_id dir |
| prepare 실패 | `.tmp` · 미완성 final 제거 |
| delete | VM artifact tree 전체 삭제 |

UUID generation은 이후 **snapshot lineage** 확장용이다.  
풀 multi-gen retention UI는 후속이다.

스펙 메모: [task-vm-rootfs-and-artifacts](../30-tasks/task-vm-rootfs-and-artifacts.md)

---

## 설계 원칙 (MicroNetwork와 같은 결)

| 원칙 | 내용 |
|---|---|
| 기본값은 그대로 | 미지정 시 `data/vms/{id}/` |
| 생성 시 path 자유 입력 없음 | 등록 id만 |
| 등록은 운영자 행위 | MicroStorage CRUD 또는 env |
| 용량은 생성·할당 시점 확인 | `statvfs` · 복사 전 거부 |
| 파티션 “생성” 안 함 | 마운트만 발견·등록 |
| durable vs ephemeral | generation rootfs 유지 · runtime은 start마다 |

---

## API 한눈에

| 용도 | API |
|---|---|
| 선택 목록 (생성·할당 폼) | `GET /api/storage` |
| 마운트 파티션 탐색 | `GET /api/storage/devices` |
| 풀 관리 | `GET/POST /api/micro-storages`, `GET/DELETE …/{id}` |
| 생성 시 지정 | `POST /api/vms` · `storageRoot` |
| 수동 재할당 | `PUT /api/vms/{id}/storage` |

더 짧은 표·curl: [api.md — 저장 위치 · MicroStorage](api.md)

---

## 다음에 올 수 있는 것 (아직 아님)

- 이미 있는 rootfs를 다른 풀로 **복사/이전** (지금은 디스크 있으면 재할당 거부)
- 풀별 쿼터·예약
- multi-gen retention UI (snapshot lineage)
- 게스트 내부 디스크 파티션 (별 영역)

---

## 정리

동시에 여러 MicroVM을 돌리는 호스트에서, 병목은 “코드”만이 아니라 **한 디스크**일 수 있다.

- **풀**로 물리 위치를 나누고
- **생성·재할당**으로 어디에 앉힐지 고르고
- **generation / runtime**으로 영구 디스크와 한 번의 start를 갈라 둔다

그게 MicroStorage 층이 지금 하는 일이다.

### 같이 보면

- 4주차 계획: [week4-tasks — MicroStorage](../30-tasks/weeks/week4-tasks.md)
- AWS 대응: [aws-mapping](../10-overview/aws-mapping.md) (EBS ↔ MicroStorage)
- 용어: [glossary](../10-overview/glossary.md)
