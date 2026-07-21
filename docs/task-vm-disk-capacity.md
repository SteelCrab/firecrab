# VM 생성 시 디스크 용량 설정

지금은 VM 디스크가 rootfs 템플릿(2GB ext4)을 그대로 복사한 고정 크기다. 생성 폼에서 디스크 용량을
직접 지정할 수 있게 하고, 지정한 크기만큼 파일시스템을 확장한다.

## 작업

- `CreateVmRequest`/`VmResponse`/`VmRecord`에 `disk_gb: u16`(GiB 단위) 추가
- 검증: 템플릿 rootfs 실제 크기(현재 2GB, `VerifiedArtifact`에 `length()` 추가해 조회) 미만으로는
  줄일 수 없음(ext4 shrink 미지원) — 최소값은 템플릿 크기를 올림한 GiB, 최대값은 임의 상한(예: 500)
- `rootfs.rs`의 `prepare_rootfs`: 템플릿 복사 후 목표 크기가 더 크면 `File::set_len`으로 파일 확장 →
  `e2fsck -f -y` → `resize2fs`로 파일시스템 확장(둘 다 host의 `e2fsprogs` 사용, 이미 설치돼 있음).
  기존 디스크 재사용 경로(재시작)는 그대로 — 최초 생성 시에만 적용
- 프론트: 생성 폼에 디스크(GiB) 입력 필드 추가(cpu/ram과 동일한 검증 에러 표시 패턴), VM 상세
  모달에 디스크 용량 표시

## 완료 기준

- 생성 폼에서 지정한 용량으로 VM 디스크가 만들어지고, guest 안에서 `df -h`로 확인한 실제 사용 가능
  용량이 지정값과 일치함
- 템플릿 크기 미만 값은 검증 오류로 거부됨
- 기존 VM(디스크 이미 존재)은 재시작해도 크기가 바뀌지 않음

## 산출물

`firecrab-api-types/src/lib.rs`, `firecrab-api/src/model.rs`, `firecrab-api/src/templates.rs`,
`firecrab-api/src/rootfs.rs`, `firecrab-api/src/handlers/vms.rs`,
`firecrab-frontend/src/components/`(생성 폼, `VmDetailModal.tsx`)
