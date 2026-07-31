---
tags:
  - firecrab
  - storage
status: 완료
scope: 4주차
updated: 2026-07-31
---

# VM별 rootfs 및 artifact 관리 구현

## 요약

각 VM은 서버 생성 UUID로만 식별되는 디스크 세대와, start마다 새로 만드는 runtime 디렉터리를 쓴다.
Host 절대 경로는 durable state에 넣지 않고, 설정된 vms root에서 파생한다.

## 경로 모델

```text
{vms_root}/{vm_id_simple}/
  d/{generation_simple}.ext4     # durable writable rootfs
  d/.{generation_simple}.tmp     # atomic publish temp
  r/{runtime_simple}/
    fc.json                      # Firecracker config (this start)
    fc.sock                      # API socket (this start)
    console.log                  # guest console tee
```

- `VmArtifactPaths::for_vm` / `rootfs` / `runtime` / `create_runtime` — `firecrab-api/src/artifacts.rs`
- DB: `vms.disk_generation`, `vms.last_runtime_id` (nullable UUID text)

짧은 디렉터리/파일 이름(`d`, `r`, `fc.sock`)은 nested UUID 아래에서도 AF_UNIX 소켓 경로 제한(~108바이트)을 지키기 위함.

## 동작

| 시점 | 동작 |
|---|---|
| 첫 start | generation UUID 할당 → temp copy → rename → grow → specialize; runtime 디렉터리 생성 후 config/spawn |
| stop→start | 같은 `disk_generation` 파일 재사용(inode/내용 유지); **새** `runtime_id` 디렉터리 |
| prepare 실패 | `.tmp` 및 미완성 final 제거 |
| delete | VM artifact tree 전체 삭제 (`remove_vm_artifacts`) |

## 테스트

```sh
cargo test -p firecrab-api artifacts
cargo test -p firecrab-api stop_start_reuses
cargo test -p firecrab-api failed_copy_leaves
cargo test -p firecrab-api concurrent_vms
cargo test -p firecrab-api   # full suite
```

- concurrent: 두 VM prepare → 서로 다른 path + inode
- stop/start: 같은 generation 파일·inode, 서로 다른 runtime dir
- failed copy: temp/final 없음
