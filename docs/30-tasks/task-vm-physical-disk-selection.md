---
tags:
  - firecrab
  - storage
status: 미완료
scope: 4주차
updated: 2026-07-23
---

# VM 생성 시 물리 디스크(저장 위치) 선택 기능

VM 여러 대를 동시에 시작하면 전부 같은 물리 디스크(`data/vms/`가 마운트된 장치) 하나에 2GB씩
동시에 쓰기 때문에 그 디스크 하나가 병목이 된다(`docs/bugs/vm-startup-stuck-under-concurrent-load.md`
수정 이후에도 남아있는 문제 — iostat로 `%util` 거의 100%, `w_await` 수백ms 확인됨, NVMe 하드웨어
자체는 문제 없음). 호스트에 디스크가 여러 개 있다면 VM을 서로 다른 물리 디스크에 분산 배치해
동시 시작 시 하나의 디스크로 I/O가 몰리는 걸 줄인다.

## 작업

- 서버 설정(예: `FIRECRAB_STORAGE_ROOTS` 환경 변수, 콜론 구분 경로 목록)으로 관리자가 사용 가능한
  저장 위치(물리 디스크별 마운트 경로) 목록을 등록. 사용자가 임의 경로를 입력하게 하지 않음
  (path traversal/host 파일 접근 방지 — 기존 `templates.rs`의 `open_beneath`/RESOLVE_BENEATH 패턴
  참고)
- `CreateVmRequest`에 `storagePath`(또는 별칭, 예: `"disk-a"`) 선택 필드 추가 — 미지정 시 기존
  기본 위치 사용(하위 호환)
- `VmRecord`/`rootfs::prepare_rootfs`가 VM별로 선택된 저장 위치 아래에 디스크를 생성하도록 수정
  (`data/vms/{id}/` 대신 `{선택된 root}/vms/{id}/`)
- 저장 위치 선택 시 실제 여유 공간(`statvfs`)을 확인해 지정한 `diskGb`가 안 들어가면 생성 거부
- 프론트: 생성 폼에 "저장 위치" select 추가(등록된 저장 위치 목록을 `GET /api/storage`류 API로 조회)

## 완료 기준

- 물리 디스크 2개 이상 등록된 상태에서 VM 여러 대를 각각 다른 저장 위치로 동시에 시작하면 한쪽
  디스크에만 I/O가 몰리지 않고(`iostat`로 확인), 전체 완료 시간이 단일 디스크 대비 단축됨
- 여유 공간이 부족한 저장 위치를 선택하면 생성 시점에 검증 오류로 거부(디스크 복사 중간에 실패하지
  않음)
- 저장 위치를 지정하지 않은 기존 흐름(단일 디스크)은 그대로 동작

## 산출물

`firecrab-api/src/state.rs`(저장 위치 설정), `firecrab-api/src/rootfs.rs`,
`firecrab-api-types/src/lib.rs`, `firecrab-api/src/handlers/vms.rs`,
`firecrab-frontend/src/components/CreateVm.tsx`
