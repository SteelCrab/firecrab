---
tags:
  - firecrab
  - plan
status: 완료
updated: 2026-07-30
---

# 2주차 Tasks - MVP Lifecycle (기능 구현 중심)

기준: `feat/api-security-input-validation@ca10d3a`. 태스크 하나 = 모듈 하나(파일 1~2개, 100~200줄), start/stop은 동기 처리, 각 태스크에 단위 테스트 포함. API 변경 시 `docs/api.md` 갱신.

| 상태 | 제목 | 작업 | 완료 기준 | 산출물 |
|---|---|---|---|---|
| ✅ | [Rust API 서버 기반](../task-rust-api-server-foundation.md) | axum/Tokio HTTP 서버, 공유 AppState | 서버 실행, router가 AppState 사용 | `firecrab-api/src/main.rs`, `firecrab-api/src/state.rs` |
| ✅ | [MicroVM 생성 API](../task-microvm-create-api.md) | `POST /api/vms` → VM 레코드 생성 | 201 + UUID + `created` | `firecrab-api/src/handlers/vms.rs` |
| ✅ | [VM 레코드 저장·복원](../task-vm-record-file-persistence.md) | vms.json 저장, 시작 시 복원 | 재시작 후 레코드 유지 | `firecrab-api/src/persistence.rs` |
| ✅ | [생성 브라우저 테스트 페이지](../task-vm-create-browser-test.md) | 정적 페이지에서 생성 API 호출 | 생성 요청·응답 표시 | `firecrab-frontend/index.html` |
| ✅ | [Rust workspace + shared contracts](../task-rust-workspace-and-shared-contracts.md) | workspace 전환, API type crate 분리 | root lockfile, 고정 toolchain | root `Cargo.toml`, `firecrab-api-types/` |
| ✅ | [Template registry](../task-template-registry.md) | alias → 불변 버전/digest 매핑 | path traversal·symlink 차단, digest 검증 | `firecrab-api/src/templates.rs` |
| ✅ | [API 보안·입력 검증](../task-api-security-and-input-validation.md) | loopback bind, CORS, body 제한, 필드 검증, 구조화 오류 | 기본 `127.0.0.1:3000`, 검증 오류 400 JSON | `firecrab-api/src/server.rs`, `firecrab-api/src/error.rs` |
| ✅ | [VM 목록 API](../task-vm-list-api.md) | `GET /api/vms` — 이름순 정렬 배열, pagination 없음 | 빈 목록 `[]` 200, 생성한 VM 포함 | `firecrab-api/src/handlers/vms.rs`, `docs/api.md` |
| ✅ | [VM 상세 API](../task-vm-detail-api.md) | `GET /api/vms/{id}` | 200 / 404 / UUID 오류 400 | `firecrab-api/src/handlers/vms.rs`, `docs/api.md` |
| ✅ | [VM 상태 모델](../task-vm-state-model.md) | `VmState`에 starting/running/stopping/stopped/error 추가 + 전이 검사 함수 | 전이 허용/거부 단위 테스트 | `firecrab-api-types/src/lib.rs` |
| ✅ | [SQLite 저장소](../task-sqlite-store.md) | `vms` 테이블 1개(WAL)로 persistence 교체, vms.json 1회 import | 재시작 후 레코드 유지, CRUD 동작 | `firecrab-api/src/persistence.rs`, `firecrab-api/src/state.rs` |
| ✅ | [rootfs 준비 모듈](../task-vm-rootfs-prepare.md) | `data/vms/{id}/rootfs.ext4`로 template 복사 (tmp→rename) | 복사 성공, 실패 시 `.tmp` 잔여물 없음 | `firecrab-api/src/rootfs.rs` |
| ✅ | [Firecracker config 생성](../task-firecracker-config.md) | cpu/ram/kernel/rootfs/boot_args → `firecracker.json` | config의 `vcpu_count`/`mem_size_mib` = 요청값 | `firecrab-api/src/firecracker.rs` |
| ✅ | [Firecracker 프로세스 모듈](../task-firecracker-process.md) | spawn + API socket readiness + SIGTERM→timeout→SIGKILL | 스폰 후 socket 응답, stop 시 프로세스 종료 | `firecrab-api/src/firecracker.rs` |
| ✅ | [VM 시작 API](../task-vm-start-api.md) | `POST /api/vms/{id}/start` 동기 — rootfs→config→spawn→readiness | created/stopped/error→running 200, 그 외 409 | `firecrab-api/src/handlers/vms.rs`, `docs/api.md` |
| ✅ | [VM 중지 API](../task-vm-stop-api.md) | `POST /api/vms/{id}/stop` 동기 | running→stopped 200, 그 외 409, 프로세스 실제 종료 | `firecrab-api/src/handlers/vms.rs`, `docs/api.md` |
| ✅ | [VM 삭제 API](../task-vm-delete-api.md) | `DELETE /api/vms/{id}` — 디렉터리 정리 + 레코드 삭제(hard) | running 409, 삭제 후 재조회 404 | `firecrab-api/src/handlers/vms.rs`, `docs/api.md` |
| ✅ | [종료 감시](../task-vm-exit-monitor.md) | `child.wait()` 감시 — 정상 stopped, 비정상 error | Guest 내부 종료 시 상태 자동 갱신 | `firecrab-api/src/firecracker.rs`, `firecrab-api/src/state.rs` |
| ✅ | [재시작 정리](../task-startup-state-cleanup.md) | 서버 시작 시 starting/running/stopping 레코드 → stopped | 재시작 후 유령 running 없음 | `firecrab-api/src/main.rs`, `firecrab-api/src/persistence.rs` |
