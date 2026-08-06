---
tags:
  - firecrab
  - m2image
  - spec
status: 설계 완료
scope: MVP-2주
updated: 2026-08-02
---

# M2Image 웹 빌드 · 패키지 관리 · 화면 정리 — 설계

> [!summary] 한 줄 요약
> 웹에서 "이미지 빌드" 버튼으로 microVM을 부팅해 패키지를 설치/삭제하고,
> 그 상태를 새 템플릿 버전으로 스냅샷한다. 새 특권 데몬·docker 의존 없음 —
> Firecracker microVM 자체가 이미 root 격리 환경이라는 점을 재사용한다.
> Images 화면은 Store/Packer 2패널·가짜 4단계바를 단일 표 + 빌드 모달로 정리한다.

## 왜

- `M2Image-Packer` 패널은 지금 이름과 달리 **빌드가 아니라 다운로드**다.
  `FIRECRAB_IMAGE_BASE_URL`의 `.tar.zst`를 받아 구조만 검증한다
  (`image_install.rs:302 download_package_once`). "소스 확인/rootfs 확인/커널
  확인/패키지 검증" 4단계는 다운로드 로그 접두사를 빌드처럼 보이게 포장한 것뿐.
- 실제 빌드는 CLI 전용(`scripts/build-m2images.sh`)이며 docker + (Ubuntu는)
  `sudo` chroot가 필요하다. `firecrab-api`는 `NoNewPrivileges=yes` +
  `ProtectSystem=full`의 비특권 서비스라 이 경로를 그대로 못 쓴다
  (`packaging/systemd/firecrab-api.service`).
- 패키지 설치/삭제 API가 없다 — `POST /api/vms/{id}/packages/update`(업그레이드
  전용)만 있고, 프론트엔드에 배선조차 안 되어 있다(`packages.rs`).
- Images 화면(Images.tsx, 704줄)이 Store 패널 + Packer 패널 + 단계바 + 설명문
  으로 정보가 과다하다.

## 아키텍처 — microVM 빌더

**핵심 전환**: docker가 하던 일은 "호스트에 없는 배포판 패키지 매니저를 빌려
격리된 root로 쓰는 것"뿐이었다(Alpine/Rocky는 컨테이너의 `apk`/`dnf` 바이너리를
빌림, Ubuntu는 애초에 docker 없이 `sudo`+`chroot`). firecrab에는 이미 이 목적에
더 맞는 격리 수단이 있다 — **Firecracker microVM**. 게스트 안에서는 실제
root이므로 apt/apk/dnf가 별도 특권 없이 그냥 동작한다.

```
웹 "이미지 빌드"
  │
  ▼
create_vm (source alias 부팅) ── 기존 VM 생성/부팅 파이프라인 그대로 재사용
  │  네트워크 · 콘솔 전부 기존 경로
  ▼
콘솔에서 패키지 설치/삭제 ── 공유 패키지 엔진(sentinel 방식, update_packages와 동일 패턴)
  │
  ▼
"이미지로 저장" → VM stop
  → e2fsck -p (rootfs.rs::recover_before_specialization 재사용)
  → identity 파일 strip (STRIP_PATHS 변형 — 템플릿용, per-VM specialize와 구분)
  → images/rootfs/{alias}-{version}.ext4 로 확정, sha256 계산
  → register_spec (같은 alias = 새 버전/리빌드, 다른 alias = 파생 이미지)
  → 빌더 VM 삭제 (기존 delete_vm 경로)
```

### 의도적 범위 경계

- **완전히 새로운 배포판을 처음부터 부트스트랩**하는 것(예: 4번째 배포판 최초
  추가)은 CLI(`build-m2images.sh`, docker 그대로)에 남긴다. 웹 빌드는 이미
  설치된 템플릿을 소스로 하는 파생/리빌드만 다룬다.
- **커널은 갱신하지 않는다.** Firecracker 커널은 게스트 `/boot`가 아니라 호스트
  `images/kernel/`에서 온다. 게스트 안에서 커널 패키지를 올려도 다음 부팅에
  반영되지 않으므로, 웹 빌드는 rootfs(패키지)만 바꾸고 커널/initrd는 소스
  템플릿 것을 그대로 공유한다.

## 데이터 모델 변경

**`vms` 테이블에 `purpose` 컬럼 추가** (기존 `migrate_disk_gb_column` 등과 동일한
ALTER TABLE 패턴):

```sql
ALTER TABLE vms ADD COLUMN purpose TEXT NOT NULL DEFAULT 'instance'
```

- `instance`(기본) / `builder` 두 값. `list_vms` 기본 응답은 `instance`만
  포함 — 빌더 VM이 일반 MicroVM 목록에 섞이지 않는다.
- 빌더 VM은 `GET /api/images/builds`로 별도 조회.

**신규 `BuildTracker`** — `ImageInstallTracker`와 동형 구조(alias별 최신
스냅샷 + 로그). 빌드 세션 상태: `booting → ready → installing → finalizing →
succeeded | failed`.

## API 표면

### 패키지 엔진 일반화 (`handlers/packages.rs`)

```
POST /api/vms/{id}/packages
  { "action": "install" | "remove" | "update", "packages"?: string[] }
```

- 기존 `POST /api/vms/{id}/packages/update`를 대체 (같은 sentinel 대기 로직
  재사용, `PackageManager`에 배포판별 install/remove 명령만 추가).
- `install`/`remove`는 `packages` 1개 이상 필수. 각 패키지명은
  `^[a-zA-Z0-9][a-zA-Z0-9._+-]*$` + 항목 수 상한(예: 32개) 검증 — 게스트
  콘솔 입력에 그대로 들어가는 값이므로 셸 메타문자를 거른다.
- 응답/폴링 모델은 기존 `packageUpdate`(`PackageUpdateStatus`) 그대로 재사용,
  실행 중인 action 종류만 로그에 표기.

### 빌드 세션 (신규 `handlers/builds.rs`)

```
POST   /api/images/{alias}/build              { "newAlias"?: string }
GET    /api/images/builds                      (list, purpose=builder 필터)
GET    /api/images/builds/{buildId}
POST   /api/images/builds/{buildId}/packages   { action, packages }
POST   /api/images/builds/{buildId}/finalize
DELETE /api/images/builds/{buildId}            (취소: stop+삭제, 등록 안 함)
```

- `newAlias` 생략 → 소스와 같은 alias로 새 버전 등록(베이스 리빌드).
  지정 → 새 alias로 파생 템플릿 등록. 새 alias는 기존 `known_specs`/설치된
  alias와 충돌 시 409.
- `finalize`는 최소 1회 이상 패키지 액션이 성공했을 때만 허용(아무 변경 없는
  템플릿 재등록 방지).
- 빌더 VM에는 기존 MicroNetwork 중 "외부 통신 허용"이 강제 적용(패키지 다운로드
  필요).

## 프론트엔드

### Images 화면 — 단일 표 + 빌드 모달

Store 패널 + Packer 패널 + 4단계 가짜 파이프라인 + 설명 문단을 표 하나로
축소:

```
이미지                 크기      상태          작업
────────────────────────────────────────────────────
alpine-3.24            180 MiB   설치됨        [빌드] [삭제]
ubuntu-26.04           1.2 GiB   설치됨        [빌드] [삭제]
rocky-9                —         패키지 준비됨  [가져오기]
my-nginx-base          1.1 GiB   설치됨        [빌드] [삭제]
                                               [+ 새 이미지 빌드]
```

- `packageUrl`이 있는 미설치 행만 "가져오기"(기존 package→install 흐름 유지,
  로그는 클릭 시 펼침).
- "빌드" / "+ 새 이미지 빌드" → 모달:
  1. (신규 빌드 시) 소스 템플릿 선택
  2. 부팅 로그 스트림
  3. 패키지 설치/삭제 입력 2개(각 텍스트박스 + 버튼, 실시간 로그 공유)
  4. 하단 토글: "같은 이미지 갱신" / "새 이미지로 저장"(alias 입력)
  5. 확정 → finalize, 실패/취소 → DELETE로 빌더 VM 정리

### VM 상세 모달

기존 "패키지 업데이트" 단일 버튼을 설치/삭제/업데이트 3개 액션 + 공용 로그
영역으로 교체. 같은 `/packages` API 재사용.

## 테스트 전략

- Rust: `handlers/packages.rs`에 install/remove 경로 단위 테스트(패키지명
  검증 실패, 배포판별 명령 매핑) — 기존 update 테스트 패턴 재사용.
- Rust: `handlers/builds.rs` — 빌드 세션 상태 전이(booting→ready→…),
  finalize 전 미완료 패키지 액션 시 거부, 새 alias 충돌 409, 취소 시 VM
  정리 확인.
- Rust: `rootfs.rs` 템플릿 finalize 헬퍼(identity strip) 단위 테스트.
- 수동 검증: 웹에서 alpine-3.24 기반으로 `curl` 설치 → 새 alias로 저장 →
  해당 alias로 VM 생성 → `curl --version` 확인.

## 완료 기준 (MVP)

- 웹에서 설치된 템플릿 중 하나를 선택해 패키지를 설치/삭제하고, 결과를 새
  alias 또는 같은 alias의 새 버전으로 저장할 수 있다.
- 실행 중인 VM 상세 화면에서 패키지 설치/삭제/업데이트가 각각 동작한다.
- Images 화면이 단일 표 + 빌드 모달로 정리되고, 기존 Store/Packer 2패널·
  가짜 단계바·설명 문단이 제거된다.
- `firecrab-api`는 여전히 비특권 프로세스로 동작한다(새 root 데몬·docker
  의존 없음).

## 참고

- 기존 다운로드 흐름: [task-m2image-builder](../../30-tasks/task-m2image-builder.md) ·
  [task-m2image-registry](../../30-tasks/task-m2image-registry.md)
- 패키지 엔진 원본: `firecrab-api/src/handlers/packages.rs`
- 템플릿 등록: `firecrab-api/src/templates.rs` (`register_spec`, `known_specs`)
- rootfs 도구: `firecrab-api/src/rootfs.rs` (`prepare_rootfs`,
  `specialize_guest`, `recover_before_specialization`)
- VM 생성/부팅 파이프라인: `firecrab-api/src/handlers/vms.rs`
