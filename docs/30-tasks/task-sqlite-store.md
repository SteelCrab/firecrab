---
tags:
  - firecrab
  - storage
status: 완료
scope: 2주차
updated: 2026-07-23
---

# SQLite 저장소

vms.json 저장을 SQLite로 교체. 테이블 1개, 최소 스키마.

## 작업

- rusqlite(WAL mode), DB 경로 `data/firecrab.db`
- `vms` 테이블: 현재 `VmRecord` 필드 그대로 (id, name, state, template, template_version, sha256 3종, cpu, ram)
- `persistence.rs`를 load/insert/update/delete 함수로 교체
- 기존 `data/vms.json` 있으면 시작 시 1회 import

## 완료 기준

- 재시작 후 레코드 유지
- CRUD 동작 단위 테스트
- vms.json import 후 목록 일치

## 산출물

`firecrab-api/src/persistence.rs`, `firecrab-api/src/state.rs`
