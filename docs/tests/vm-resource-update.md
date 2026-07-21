# 구축된 VM CPU/MEM/DISK 수정 테스트

## 자동 테스트

```sh
cargo test -p firecrab-api handlers::vms::tests::update
cargo test -p firecrab-api handlers::vms::tests::validates_update
cargo test -p firecrab-api rootfs::tests::growing_an_already_existing_disk
cargo test -p firecrab-api-types update_vm_resources_request
cargo test -p firecrab-api-types only_inactive_states_allow_resource_edits
cd firecrab-frontend && npx tsc --noEmit && npm run build && npm run lint
```

## 확인 항목

- `PUT /api/vms/{id}`: `created`/`stopped`/`error`에서만 허용, `running`/`starting`/`stopping`은 `409`
- cpu 1–32, ram 128–32768 MiB는 생성 때와 동일한 범위 검증
- disk는 현재 값 미만으로 줄이면 `400`(`diskGb` 필드 오류, 메시지에 현재 값 포함), 상한(500GiB) 초과도 `400`
- DB 저장 실패 시 메모리상의 cpu/ram/disk도 이전 값으로 롤백(`start_vm`의 롤백 패턴과 동일)
- `rootfs::prepare_rootfs`가 **기존** 디스크를 재사용하는 경로에서도 목표 크기로 확장 — 최초 생성
  시에만 확장하던 이전 동작에서 변경. 확장 실패 시 기존 파일은 보존(삭제 안 함 — 새로 만드는
  경우에만 실패한 파일을 정리)

## 실제 검증됨 (이번 세션에서 완료)

- `cargo test --workspace`: 69/9/8/27 전부 green(신규 5개 handler 테스트 + rootfs 재사용 확장
  테스트 + 타입 테스트 2개 포함)
- **실제 2GB 우분투 템플릿으로 엔드투엔드 확인**: `cpu:1,ram:512,diskGb:2`로 생성 → `PUT`으로
  `cpu:2,ram:1024,diskGb:6`으로 수정(아직 `created` 상태) → start(4초, running 도달) →
  - `firecracker.json`에 `vcpu_count: 2, mem_size_mib: 1024` 정확히 반영
  - 실제 `rootfs.ext4` 파일 6442450944바이트(정확히 6GiB), `dumpe2fs`로 동일 크기 확인
  - **guest 부팅 로그**: 커널이 "CPU topo: Allowing 2 present CPUs", "smp: Brought up 1 node, 2
    CPUs", "Memory: 982340K/1048184K available"(~1GiB), `virtio_blk ... 6.00 GiB`,
    `EXT4-fs (vda): mounted ...` 클린 마운트까지 전부 guest 커널이 직접 보고 — 호스트 도구 결과가
    아니라 guest 스스로 인식한 값
  - `running` 상태에서 `PUT` 시도 → `409` 확인
  - `stopped`로 전환 후 현재(6GiB)보다 작은 값(4GiB)으로 `PUT` 시도 → `400`, 메시지에 "must be at
    least the current size (6 GiB)" 포함 확인
- **실제 브라우저(headless Chrome + CDP)**로 UI 확인:
  1. 상세 모달에서 `created`/`stopped`/`error` 상태일 때만 "수정" 버튼 노출
  2. "수정" 클릭 → cpu/ram/disk가 입력 필드로 바뀌고 "저장"/"취소" 버튼 등장
  3. 값 변경 후 "저장" → 모달과 테이블 양쪽에 새 값(cpu 3, ram 768 MiB, disk 5 GiB)이 즉시 반영

## 터미널 세션 1 — API + 프론트 실행

```sh
cargo run -p firecrab-api
```

다른 터미널에서:

```sh
cd firecrab-frontend
npm run dev
```

## 터미널 세션 2 — 수동 확인

```sh
ID=$(curl -s -X POST http://127.0.0.1:3000/api/vms -H "Content-Type: application/json" \
  -d '{"name":"resize-demo","template":"ubuntu-26.04","ram":512,"cpu":1,"diskGb":2}' | python3 -c 'import json,sys;print(json.load(sys.stdin)["id"])')

curl -s -X PUT "http://127.0.0.1:3000/api/vms/$ID" -H "Content-Type: application/json" \
  -d '{"cpu":2,"ram":1024,"diskGb":6}'

curl -s -X POST "http://127.0.0.1:3000/api/vms/$ID/start"
cat "data/vms/$ID/firecracker.json"
dumpe2fs -h "data/vms/$ID/rootfs.ext4" | grep -E "Block count|Block size"
```

`http://localhost:8080/`에서 `resize-demo` 이름 클릭 → "수정" 버튼으로 값 바꾸고 저장 → 테이블에
즉시 반영되는지 확인. `running`으로 시작한 뒤에는 "수정" 버튼 자체가 안 보이는지 확인.

## 정리

```sh
curl -s -X POST http://127.0.0.1:3000/api/vms/<id>/stop
curl -s -X DELETE http://127.0.0.1:3000/api/vms/<id>
```

세션 1의 두 터미널을 `Ctrl-C`로 종료.
