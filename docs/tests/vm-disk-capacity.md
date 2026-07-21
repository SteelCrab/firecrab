# VM 디스크 용량 설정 테스트

## 자동 테스트

```sh
cargo test -p firecrab-api rootfs::
cargo test -p firecrab-api handlers::vms::tests::validates_disk_gb
cargo test -p firecrab-api-types create_vm_request
cd firecrab-frontend && npx tsc --noEmit && npm run build && npm run lint
```

## 확인 항목

- `min_disk_gb_for`: 템플릿 rootfs 실제 크기를 GiB로 올림 → 그 미만 값은 `diskGb` 필드 검증 오류
- 상한(`MAX_DISK_GB=500`) 초과도 동일하게 거부, 경계값(최소/최대)은 통과
- `rootfs::grow`: 목표 크기가 현재 파일 크기 이하면 완전히 no-op(기존 디스크 재사용 시 크기 불변)
- `rootfs::grow`가 실제 크게 만들 때는 `File::set_len` → `e2fsck -f -y` → `resize2fs` 순서로 실행,
  둘 다 stdout/stderr를 캡처(`Command::output`)해 서버 로그를 오염시키지 않음
- `grows_a_real_ext4_filesystem_to_the_requested_size`: 진짜 `mkfs.ext4`로 만든 8MiB 이미지를
  32MiB로 실제로 키워서 `dumpe2fs`로 block_count*block_size가 목표와 정확히 일치하는지 확인(가짜
  바이트가 아닌 진짜 파일시스템 검증)
- SQLite `vms` 테이블에 `disk_gb` 컬럼 추가 — 기존 DB(컬럼 없음)는 `ALTER TABLE ... DEFAULT 2`로
  마이그레이션, 새 DB는 `CREATE TABLE`에 바로 포함

## 실제 검증됨 (이번 세션에서 완료)

- `cargo test -p firecrab-api`: 63/63 green (신규 `grows_a_real_ext4_filesystem_to_the_requested_size`,
  `validates_disk_gb_against_the_template_floor_and_fixed_ceiling` 포함)
- **실제 프로덕션 DB 마이그레이션**: `data/firecrab.db`(실제 VM 4대: `1`/`12`/`123`/`avc`) 복사본에
  대해 서버를 실제로 기동 → `GET /api/vms` 응답에 4대 모두 `diskGb: 2`로 정상 로드됨을 확인(마이그레이션이
  기존 데이터를 보존)
- **실제 2GB 우분투 템플릿으로 엔드투엔드 확인**: `diskGb: 4`로 VM 생성 → start(5.6초, running 도달) →
  - 호스트에서 `rootfs.ext4` 실파일 크기 4294967296바이트(정확히 4GiB) 확인
  - `dumpe2fs -h`로 block_count(1048576) × block_size(4096) = 4294967296바이트, `Filesystem state: clean`
  - **guest 부팅 로그**(`/api/vms/{id}/log`)에서 커널이 직접 보고: `virtio_blk virtio0: [vda] 8388608
    512-byte logical blocks (4.29 GB/4.00 GiB)`, 이어서 `EXT4-fs (vda): mounted filesystem ... r/w
    with ordered data mode` — fs 에러 없이 깨끗하게 마운트. 호스트 도구 결과가 아니라 guest 커널
    스스로 인식한 크기라는 점에서 가장 강한 증거
- **실제 브라우저(headless Chrome + CDP)**로 UI 확인:
  1. 생성 폼에 `disk (GiB)` 필드 존재, 기본값 `2`
  2. `diskGb: 7`로 생성 → 테이블에 즉시 `7 GiB`로 반영(다른 기존 VM은 `2 GiB` 그대로)
  3. VM 이름 클릭 → 상세 모달의 필드 목록에 `disk: 7 GiB` 표시

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
curl -s -X POST http://127.0.0.1:3000/api/vms \
  -H "Content-Type: application/json" \
  -d '{"name":"disktest","template":"ubuntu-26.04","ram":512,"cpu":1,"diskGb":4}'
# 응답의 id로:
curl -s -X POST http://127.0.0.1:3000/api/vms/<id>/start
ls -la data/vms/<id>/rootfs.ext4   # 4294967296바이트인지 확인
dumpe2fs -h data/vms/<id>/rootfs.ext4 | grep -E "Block count|Block size"
```

`http://localhost:8080/`에서 `disk (GiB)`에 템플릿 크기(2) 미만 값을 넣고 생성 시도 → 필드 아래 빨간
검증 오류가 바로 표시되는지 확인.

## 정리

```sh
curl -s -X POST http://127.0.0.1:3000/api/vms/<id>/stop
curl -s -X DELETE http://127.0.0.1:3000/api/vms/<id>
```

세션 1의 두 터미널을 `Ctrl-C`로 종료.
