---
tags:
  - firecrab
  - plan
  - mvp
  - contest
status: 진행 중
updated: 2026-08-02
---

# 제출 MVP 플랜 — 2026-08-01 → 2026-08-27

> [!summary] 한 줄
> 8/1–8/2에 **준비 + 1주 셸 + 2주 이미지 코어**까지 앞당겨 구현했다.
> 남은 축은 **브랜치 머지 → 이미지 아티팩트·기본 템플릿 → 3주 관측·초기화 스크립트 → 버퍼 제출물**.

## 현재 스냅샷 (2026-08-02)

| 축 | 위치 | 상태 |
|---|---|---|
| 명시 MicroNetwork · MicroStorage · doctor | **main** | ✅ PR #44 머지됨 |
| 앱 셸 · 좌측 내비 · 해시 라우팅 | **main** | ✅ 이미지 메뉴만 자리표시자 |
| 콘솔 전용 페이지 · 테마 · 재연결 | `feat/m2image-install` 스택 | 🟨 브랜치 완료 · 미머지 |
| 로그 복사 · 다운로드 | 同上 | 🟨 브랜치 완료 · 미머지 |
| `GET/POST/DELETE` 이미지 · 대시보드 설치 | 同上 | 🟨 브랜치 완료 · 미머지 |
| Ubuntu 기본 템플릿 | — | ⬜ 미착수 |
| 이미지 패키지 · archive 설치 · BASE_URL 배선 | 브랜치 코드 + 로컬 `dist/m2images` | 🟨 **gh release 게시** 남음 |
| **M2Image-builder** (업스트림 → Firecracker 패키지 굽기) | `build-m2images.sh` · runbook · 로컬 산출 검증 | ✅ [task](../task-m2image-builder.md) |
| **M2Image 레지스트리** (구운 패키지 게시·다운로드) | BASE_URL flat 파일 수준 | ⬜ [task](../task-m2image-registry.md) |
| 관측 · 초기화 스크립트 | — | ⬜ 3주 |
| 제출물 (README · 영상 · 영문) | — | ⬜ 버퍼 |

**열린 브랜치 스택** (main `ae010d1` 위, 머지 전):

```
main
 └─ feat/console-terminal-page   콘솔 전용 페이지 · 상세 필드 · StrictMode 폴링
     └─ feat/log-copy-download   로그 복사/저장 (콘솔 · VM 상세)
         └─ feat/m2image-catalog-api   GET /api/images · CreateVm 상수 제거
             └─ feat/m2image-install ★현재   설치/삭제 API · Images 페이지 · 커버리지
```

> [!tip] 당장 할 일 (8/2 밤 – 8/3)
> 1. `feat/m2image-install` 스택 main 머지 (또는 PR 단위)
> 2. 빈 호스트에서 `install.sh` → 네트워크 → VM start 1회
> 3. M2Image **builder → 레지스트리 게시 → BASE_URL 설치** 경로 확정 (2주 잔여의 병목)

## 일정

| 구간 | 날짜 | 초점 | 진행 |
|---|---|---|---|
| **준비** | 8/1(토) – 8/2(일) | 브랜치 머지 · 대회 문구 · 설치 재검증 | 🟨 머지·실호스트 왕복 남음 |
| **1주** | 8/3(월) – 8/9(일) | 콘솔 셸 — 좌측 메뉴 · 터미널 UI | 🟨 구현 끝 · main 머지·회귀·캡처 |
| **2주** | 8/10(월) – 8/16(일) | 이미지 — 카탈로그 · 설치 · 진행 로그 | 🟨 코어 구현 끝 · 아티팩트·ubuntu 기본 |
| **3주** | 8/17(월) – 8/22(토) | 서비스 기능 — 관측 · 초기화 스크립트 | ⬜ |
| **버퍼** | 8/23(일) – 8/27(목) | 동결 · 제출물 · **8/26 제출**(8/27 예비) | ⬜ |

> [!important] 원칙
> - 3주(8/3–8/22)는 전부 기능 구현. 문서 · 영상은 버퍼에서만 만든다
> - "된다"의 정의 = 빈 호스트에서 `install.sh` → 대시보드에서 이미지 설치 → 네트워크 생성 → VM start → 콘솔 접속
> - VRF · Jailer · 인증 · Snapshot은 제출 후
> - **화면이 완성되는 주에 스크린샷 · GIF를 그때 찍어 둔다** — 버퍼에서 몰아 찍지 않는다
> - 제출 필수 문서만 리포에 커밋한다
> - **앞당긴 구현은 머지·실호스트 검증이 끝나기 전 "완료"로 치지 않는다**

## 제출 MVP 한 장 (Must)

상태 기준: **main** = 머지됨 · **브랜치** = `feat/m2image-install` 스택에만 있음.

| # | 데모 문장 | 상태 | 담당 구간 |
|---|---|---|---|
| M1 | 한 줄 설치로 API · helper · 대시보드가 뜬다 | main 있음, 실호스트 재검증 필요 | 준비 |
| M2 | MicroNetwork를 만들고 VM을 그 네트워크에 붙인다 | ✅ main (PR #44) | 준비 |
| M3 | MicroStorage로 디스크 위치를 고른다 | ✅ main (PR #44) | 준비 |
| M4 | AWS 콘솔처럼 좌측 메뉴로 VM · 네트워크 · 스토리지 · 이미지를 오간다 | ✅ 셸·내비 main / 이미지 화면은 브랜치 | 1주 · 2주 |
| M5 | 브라우저 터미널에서 게스트 셸을 쓴다 | 🟨 전용 페이지·테마·재연결은 브랜치 (main은 모달 콘솔) | 1주 |
| M6 | 대시보드에서 템플릿을 골라 이미지를 설치하고, 진행 로그를 복사한다 | 🟨 API·UI·archive 설치·builder 산출 재현 완료. **레지스트리 게시·빈 호스트 E2E** 남음 | 2주 |
| M6b | builder로 ubuntu/alpine Firecracker 패키지를 다시 구울 수 있다 | ✅ `build-m2images.sh`로 두 패키지·SHA256SUMS 실제 생성·검증 | 2주 |
| M6c | 레지스트리(패키지 호스트)에서 패키지를 받아 빈 호스트에 설치한다 | ⬜ BASE_URL 배선만 · 공식 게시 없음 | 2주 |
| M7 | 실행 중 VM의 CPU · 메모리가 보인다 | 없음 | 3주 |
| M8 | 저장해 둔 쉘 스크립트를 골라 VM을 만들면 첫 부팅에 실행된다 | 없음 | 3주 |
| M9 | README · 설치 · 데모 시나리오 · 대회 문구 | 문구 문서 수정됨 · 접수 사이트·README는 버퍼 | 준비(문구) · 버퍼 |

### AWS 대응 (제출 소개용)

| AWS | firecrab | 데모에서 보여줄 것 |
|---|---|---|
| EC2 | **M2** | create / start / stop / delete |
| VPC/Subnet | **MicroNetwork** | 네트워크 생성 후 VM 소속 |
| EBS 위치 | **MicroStorage** | 저장 root 선택 |
| AMI | **M2Image** | 대시보드에서 이미지 설치 후 그 템플릿으로 생성 |
| EC2 user-data | **초기화 스크립트** | 생성 시 넣은 스크립트가 첫 부팅에 실행 |
| 시작 템플릿(user-data 저장) | **쉘 스크립트 목록** | 저장해 둔 스크립트를 골라 재사용 |
| CloudWatch(기본) | **VM 관측** | 실행 중 VM의 CPU · 메모리 |
| AWS 콘솔 | **대시보드** | 좌측 메뉴 · 브라우저 터미널 |

## 이미 있는 것 (다시 만들지 말 것)

- lifecycle API, SQLite, rootfs, Firecracker spawn, 템플릿 레지스트리(alias → digest 검증)
- TAP/NAT/DHCP, **명시** MicroNetwork CRUD, MicroStorage, 시작 타임라인
- xterm 콘솔(WebSocket) — main: 모달 / 브랜치: 전용 페이지 + 테마 + 재연결 + 로그 복사
- `install.sh`, systemd 유닛, doctor, artifact layout
- 게스트 파일 주입 경로 — `rootfs.rs`의 `write_into_image`(debugfs, root 불필요)
- CI: install job의 create 스모크, nightly `vm-boot` alpine 1셀
- **(브랜치)** `GET /api/images` · `POST/GET …/package` · `POST/GET …/install` · `DELETE /api/images/{alias}`
- **(브랜치)** `Images.tsx` 목록·설치·진행 로그·cascade 삭제 · `CreateVm` 서버 카탈로그 연동
- **(브랜치)** 설치 방식 = `FIRECRAB_IMAGE_BASE_URL`에서 패키지 다운로드·구조 검증 → 로컬 패키지 해제·아티팩트 검증·핫 등록 (docker 빌드 아님)
- 이미지 **빌드 스크립트** (`install-alpine-rootfs.sh` · `install-ubuntu-roofs.sh`) · `package-m2images.sh` — **장기 실행 builder 서비스는 아직 없음**

## 전제 — 왜 builder · 레지스트리가 필요한가

| 출처 | Firecracker용 완성 이미지 |
|---|---|
| Firecracker 프로젝트 | 게스트 이미지 공식 배포 안 함 |
| Ubuntu / Alpine / Rocky Linux | cloud·base·패키지 소스만 있음 (vmlinux+ext4 세트 아님) |
| Firecrab | 직접 구운 템플릿 (`ubuntu-26.04`, `alpine-3.24`, `rocky-9`) + 패키지 |

```text
[ M2Image-builder ]  업스트림 소스 → kernel/rootfs → {alias}.tar.zst
         ↓ 게시
[ M2Image 레지스트리 ]  catalog · SHA256 · 패키지 URL
         ↓ FIRECRAB_IMAGE_BASE_URL
[ firecrab-api ]  POST /api/images/{alias}/package → 로컬 패키지 준비
                  POST /api/images/{alias}/install → 로컬 등록
```

- **MVP builder** = 스크립트(+CI)로 재현 가능하게 굽기 — 풀 HTTP builder 서비스는 제출 후
- **MVP 레지스트리** = 패키지 호스트 + digest + `packageUrl` — 서명·멀티아키 인덱스는 제출 후

## 제출에 넣지 않는 것 (Icebox / 제출 후)

| 항목 | 이유 |
|---|---|
| VRF / 네트워크별 uplink | 데모 필수 아님, 환경 의존 |
| Jailer / cgroup 풀세트 | 범위 · 검증 시간이 큼 |
| Snapshot / Backup | 7주차 설계 |
| API 인증 · RBAC | 로컬 설치 데모는 loopback으로 충분 |
| 멀티 호스트 / 스케줄러 | 단일 호스트 MVP |
| M2Image 캡처(VM → 이미지) | 이미지 설치(2주)까지가 제출 범위 |
| **장기 실행 M2Image-builder 데몬/job API** | MVP는 스크립트·CI 굽기로 충분 — [task-m2image-builder](../task-m2image-builder.md) 제출 후 절 |
| 레지스트리 서명 · 인증 pull · 멀티아키 인덱스 | MVP는 flat 패키지 호스트 — [task-m2image-registry](../task-m2image-registry.md) 제출 후 절 |
| GitHub 리포 연동 | 초기화 스크립트의 `git clone`으로 대체 |
| lifecycle 이벤트 로그 | 시작 타임라인으로 데모는 충분 |

---

# 준비 — 브랜치와 정합성 (8/1 – 8/2, 2일)

> [!summary] 목표
> 브랜치를 main에 올리고, 심사자가 보게 될 설치 경로와 대회 문구를 사실과 맞춘다.

| 상태 | 제목 | 작업 | 완료 기준 |
|---|---|---|---|
| ✅ | micro_storages 테스트 보강 | 핸들러 커버리지 61% → 95.9% (테스트 11개 추가) | 브랜치 patch 91.2% → main 반영 |
| ✅ | 명시 MicroNetwork 브랜치 머지 | PR #44 (`d5cdc88`) | main에 명시 네트워크 · MicroStorage · doctor |
| ⬜ | M2 매트릭스 1회 실행 | 머지 전/후 `workflow_dispatch`로 브랜치 지정 실행 | **ubuntu-26.04 셀 통과**가 기준, alpine은 보조 |
| 🟨 | 대회 문구 정합성 | `오픈소스개발자대회.md` — Ubuntu·Alpine 템플릿 (293/300자) | 문서 수정 완료, **접수 사이트 반영은 직접 확인 필요** |
| 🟨 | 설치 경로 재검증 | 빈 VM에서 `install.sh` 1회 | 테스트 수 기록됨(216/20/16/56, 합계 308). **유닛 2개 active · 네트워크 생성 후 VM start는 일회용 VM에서 직접 실행 필요** — [host-install](../../40-tests/host-install.md) |
| 🟨 | 1주·2주 스택 머지 | `feat/m2image-install` → main (콘솔·로그·이미지) | main에서 `#/images` 설치 · 콘솔 전용 페이지 동작 |

**완료 정의:** main에서 설치 → 네트워크 → **ubuntu** VM start → running 1회 성공
*(ubuntu 이미지가 아직 `install.sh` 기본이 아니므로, 당장은 alpine 왕복으로 설치 경로를 잠그고 ubuntu는 2주 잔여에서 닫는다.)*

---

# 1주 — 콘솔 셸 (8/3 – 8/9)

> [!summary] 주간 목표
> 모달 5개로 흩어진 화면을 **AWS 콘솔형 셸**로 바꾸고, 브라우저 터미널을 쓸 만하게 만든다.
> **구현은 준비 구간에 끝남.** 이 주는 머지 · lifecycle 회귀 · 캡처에 쓴다.

| 상태 | 제목 | 작업 | 완료 기준 | 산출물 |
|---|---|---|---|---|
| ✅ main | 앱 셸 레이아웃 | 헤더 + 좌측 내비 + 콘텐츠, 반응형(≤1000px 레일 · ≤700px 가로 줄) | 해시 라우팅(`#/vms`) 새로고침 · 뒤로가기 | `App.tsx`, `Shell.tsx`, `navigation.ts` |
| ✅ main | 좌측 메뉴 · 화면 전환 | 네트워크 · 스토리지 · 호스트 페이지 승격. 이미지 자리표시자 | 메뉴 5개, 헤더 버튼 0, 이탈 시 폴링 정지 | `MicroNetworks.tsx`, `MicroStorages.tsx`, `HostInfo.tsx` |
| 🟨 브랜치 | 터미널 웹 UI | 전용 페이지(`#/console/<id>`), 전체화면, 폰트 · 테마, 접속 상태, 끊김 재연결 | 콘솔에서 로그인 · 명령이 불편하지 않음 | `Console.tsx`, `navigation.ts` |
| 🟨 브랜치 | 로그 복사 유틸 | 클립보드 복사 + 파일 다운로드 (콘솔 · 시작 타임라인 · **이미지 설치 로그**) | 버튼 한 번으로 전체 텍스트 복사/저장 | `LogExportActions.tsx`, `lib/textExport.ts`, `lib/formatVmLog.ts` |

**완료 정의**

- [x] 좌측 메뉴만으로 5개 화면 이동, 모달 의존 제거 (VM 상세는 모달 유지 · 콘솔은 브랜치에서 전용 페이지)
- [x] 콘솔에서 게스트 셸 작업이 실제로 가능(폭 · 스크롤 · 재연결) — 브랜치
- [ ] **main 머지** 후 기존 lifecycle 조작(생성 · 시작 · 정지 · 삭제) 회귀 없음
- [ ] 좌측 메뉴 · 터미널 화면 캡처 확보 (README GIF 소재)

---

# 2주 — 이미지 (8/10 – 8/16)

> [!summary] 주간 목표
> 대시보드에서 템플릿을 골라 설치하고 그 과정을 보게 한다.
> **카탈로그 · 설치 API · 이미지 화면은 브랜치에서 완료.**
> 남은 핵심은 **builder로 굽기 → 레지스트리에 게시 → BASE_URL로 설치** E2E.

> [!note] 설치 방식 — 확정
> `firecrab-api`는 비특권 + `NoNewPrivileges` + `ProtectSystem=full`이라 docker 빌드를 실행할 수 없다.
> → **사전 빌드 이미지 다운로드** (`FIRECRAB_IMAGE_BASE_URL`). 미설정 시 패키지 설치 API **503**.
> 패키지 다운로드·구조 검증 → 로컬 패키지 해제·아티팩트 검증 → `register_spec` 핫 등록.
> (대안이었던 root 이미지 데몬·호스트 즉석 빌드는 채택하지 않음.)

| 상태 | 제목 | 작업 | 완료 기준 | 산출물 |
|---|---|---|---|---|
| 🟨 브랜치 | [카탈로그 API](../task-m2image-catalog-api.md) | `GET /api/images` — alias · 버전 · digest · 설치 여부 · min_disk · `packageUrl` | `CreateVm` 하드코딩 `TEMPLATES` 제거 | `handlers/images.rs`, `CreateVm.tsx` |
| 🟨 브랜치 | 이미지 설치 API | `POST/GET /api/images/{alias}/package` + `POST/GET /api/images/{alias}/install` — 각 비동기 상태 조회 · `DELETE` + VM cascade | 패키지 준비 뒤 미설치 템플릿을 등록해 생성 가능 | `handlers/images.rs`, `image_install.rs`, `templates.rs` |
| 🟨 브랜치 | 설치 진행 로그 | 단계별 로그 조회 + **복사/다운로드** | 화면에서 진행이 보이고 전체 복사 | `Images.tsx` + `LogExportActions` |
| 🟨 브랜치 | 이미지 화면 | 좌측 메뉴 "이미지" — 목록 · 설치 · package 링크 · 삭제 | 이미지 0개여도 할 일이 보임 | `components/Images.tsx` |
| ✅ | [M2Image-builder (MVP)](../task-m2image-builder.md) | 업스트림 Ubuntu/Alpine/Rocky Linux → Firecracker kernel/rootfs 굽기 · `package-m2images` 연동 · runbook | `build-m2images.sh`로 alpine·ubuntu·rocky `.tar.zst` 재생성·SHA256 검증 | 빌드 스크립트 · runbook |
| ⬜ | [M2Image 레지스트리 (MVP)](../task-m2image-registry.md) | 패키지 호스트 레이아웃 · `SHA256SUMS`/`catalog.json` · 게시 절차 · BASE_URL 설치 E2E | 빈 호스트가 BASE_URL만으로 대시보드 설치 성공 | 게시 스크립트 · [install.md](../../20-guides/install.md) |
| 🟨 코드 | 패키징 · archive 설치 경로 | `{alias}.tar.zst` → package API 준비 → install API 등록. **레지스트리 게시와 연결 필요** | 로컬 `dist/m2images` HTTP로 두 단계 설치 1회 | `scripts/package-m2images.sh`, `image_install.rs` |
| 🟨 코드 | `FIRECRAB_IMAGE_BASE_URL` 배선 | `api.env` 시드 · doctor · 가이드 (기본 URL 하드코딩 없음) | 문서대로 URL 설정 후 대시보드 설치 가능 | `install.sh`, doctor, [install.md](../../20-guides/install.md) |
| ⬜ | 레지스트리 게시 (릴리스) | builder 산출을 `gh release` 또는 고정 호스트에 alpine/ubuntu + checksums | 공개(또는 데모) BASE_URL에서 설치 1회 | GitHub Releases / 객체 스토리지 |

**완료 정의**

- [x] builder runbook으로 alpine · ubuntu 패키지를 다시 구울 수 있음
- [ ] 레지스트리(또는 릴리스)에 패키지가 게시되어 `FIRECRAB_IMAGE_BASE_URL` 로 가리킬 수 있음
- [ ] `install.sh --no-images`로 깐 호스트에서 대시보드만으로 이미지 설치 → VM 생성 → running
- [ ] 대시보드 템플릿 목록 = 서버 카탈로그 (브랜치에서 충족 · main 머지 후 확정)
- [ ] 설치 로그 복사 버튼 동작 (브랜치에서 충족)
- [ ] 이미지 설치 진행 화면 녹화 확보 (README GIF 소재)
- [ ] 빈 호스트에서도 builder→레지스트리→설치 경로가 문서만으로 재현 가능

---

# 3주 — 서비스 기능 (8/17 – 8/22)

> [!summary] 주간 목표
> 실행 중 VM이 **보이게**(관측) 하고, 생성 시 **원하는 상태로 뜨게**(초기화 스크립트) 한다.

| 상태 | 제목 | 작업 | 완료 기준 | 산출물 |
|---|---|---|---|---|
| ⬜ | [VM 관측(최소)](../task-observability-dashboard.md) | Firecracker metrics에서 CPU · 메모리 수집 | 목록/상세에서 실행 중 VM의 사용량 표시 | `firecracker.rs` + UI |
| ⬜ | 초기화 스크립트 주입 | `CreateVmRequest.initScript` → `write_into_image`로 게스트에 주입 | 생성 시 넣은 스크립트가 첫 부팅에 실행됨 | `rootfs.rs`, `handlers/vms.rs`, `CreateVm.tsx` |
| ⬜ | 초기화 스크립트 실행 훅 | Ubuntu: systemd unit + wants 링크 / Alpine: `/etc/local.d` + runlevel 링크 | **템플릿 이미지 재빌드 없이** 두 템플릿에서 실행 | `rootfs.rs` (debugfs `write` · `symlink`) |
| ⬜ | 쉘 스크립트 목록 저장 | `GET/POST/DELETE /api/init-scripts` — 이름 + 본문을 SQLite에 저장 | 저장한 스크립트를 생성 폼에서 골라 재사용 | `persistence.rs` 테이블 1개 + `handlers` + `CreateVm.tsx` |
| ⬜ | 실행 결과 확인 | 스크립트 출력이 콘솔 로그에 남는지 | 상세 콘솔에서 실행 흔적 확인 | 기존 console/log API |
| ⬜ | [health · readiness](../task-service-health-readiness.md) | `/health/live`, `/health/ready` (DB · helper · 이미지) | ready=false면 대시보드에 경고 | `handlers/health.rs` |
| ⬜ | `demo-mvp.sh` 왕복 | 네트워크 → 이미지 → create → start → stop → delete | 한 명령으로 왕복 통과 | `scripts/demo-mvp.sh` |

**완료 정의**

- [ ] 대시보드에서 실행 중 VM의 CPU · 메모리가 보임
- [ ] 저장해 둔 스크립트(예: `git clone` 한 줄)를 골라 만든 VM이 **ubuntu**에서 실행함 (alpine은 가능하면)
- [ ] 관측 · 초기화 스크립트 화면 캡처 확보 (README GIF 소재)
- [ ] 기능 동결 — 이후로는 버그픽스만

---

# 버퍼 — 제출물 · 동결 · 제출 (8/23 – 8/27)

> [!warning] 전제
> 스크린샷 · GIF는 **각 기능이 끝나는 주에 그때 찍어 둔다**. 버퍼에서 몰아 찍으면 5일 안에 들어오지 않는다.
> 버퍼는 촬영이 아니라 **편집 · 배치 · 검증**의 시간이다.

## 제출물 태스크

| 상태 | 제목 | 작업 | 완료 기준 | 산출물 |
|---|---|---|---|---|
| ⬜ | README 상단 데모 GIF | 주차별로 찍어 둔 캡처를 GIF로 편집해 최상단 배치 | 첫 화면에서 무엇을 하는 도구인지 보임 | `README.md`, `assets/` (용량 주의) |
| ⬜ | 비교 섹션 — libvirt · Proxmox | 무엇이 다른지 표 + 언제 firecrab이 나은지 1문단 | 3–5행, 과장 없이 사실 기반 | `README.md` |
| ⬜ | 사용 시나리오 | 개발 샌드박스 · CI runner · 격리 실험 환경 3종, 각각 흐름과 얻는 것 | 시나리오마다 명령/화면 흐름 1개 | `README.md` 또는 `docs/20-guides/use-cases.md` |
| ⬜ | 영어 문서 보강 | `docs/`는 한국어 vault 유지, 진입 경로만 영문화 — install · api 가이드 영문판 | 해외 유저가 영어만으로 클론 → 설치 → VM 생성 | `docs/en/install.md`, `docs/en/api.md` |
| ⬜ | v0.1.0 릴리스 | 태그 + 바이너리 + **이미지 아티팩트** + 설치 가이드 | Releases 페이지만 보고 설치 가능 | `release.yml`, GitHub Releases |
| ⬜ | 데모 시나리오 문서 | 분 단위 스크립트(설치 → 이미지 → 네트워크 → VM → 콘솔) | 다른 사람이 따라 할 수 있음 | `docs/20-guides/demo-mvp.md` |
| ⬜ | 기여 · 보안 고지 | CONTRIBUTING 짧은 절, loopback 기본 주의 1절 | 오픈소스 제출 요건 충족 | `CONTRIBUTING.md`, README |

## 날짜

| 날짜 | 할 일 |
|---|---|
| 8/23 | 기능 동결. GIF 편집 + README 상단 배치. 비교 섹션 · 사용 시나리오 |
| 8/24 | v0.1.0 태그 · 릴리스 게시. 영어 문서(install · api). 3–5분 영상 편집 |
| 8/25 | 빈 환경 재설치 리허설 2회(**릴리스 바이너리로**). CONTRIBUTING · 보안 고지. 링크 · 오타 |
| 8/26 | **제출** |
| 8/27 | 예비일 (제출 실패 · 보완 요청 대응) |

### 제출 직전 체크리스트

- [ ] `git status` 깨끗, 릴리스 커밋 해시 기록
- [ ] `sudo ./install.sh` 성공
- [ ] 대시보드에서 이미지 설치 → MicroNetwork 생성 → VM create/start → running → 콘솔 접속
- [ ] `firecrab-doctor` FAIL 없음(또는 문서화된 예외)
- [ ] 스크린샷 · 영상 준비
- [ ] 대회 제출 폼 + 저장소 URL + 라이선스(Apache-2.0)

---

# 리스크

| 리스크 | 완화 | 상태 |
|---|---|---|
| 이미지 설치 권한 — API가 docker를 못 씀 | 사전 빌드 다운로드(`FIRECRAB_IMAGE_BASE_URL`)로 우회 · builder는 API 밖 | ✅ 방식 확정 · 코드 브랜치 |
| Ubuntu 이미지 1.2GB — 빌드 시간 · 배포 용량 | **builder → 레지스트리 아티팩트** 다운로드. alpine은 CI·빠른 검증용 | ⬜ 레지스트리 게시 남음 — **현재 최대 병목** |
| 업스트림에 Firecracker용 공식 이미지 없음 | M2Image-builder로 직접 굽기 + 레지스트리 배포 | 🟨 builder 완료 · 레지스트리 게시 남음 |
| 좌측 메뉴 리팩터가 lifecycle을 깨뜨림 | 머지 직후 화면별 수동 회귀 1회 | 🟨 셸은 main, 콘솔·이미지 머지 시 재확인 |
| `micro_storages` 커버리지로 patch 80% 미달 | 준비 구간에서 선처리 | ✅ |
| 대회 문구가 실제 기능보다 넓음 | 문서 수정 완료, 접수 사이트 확인 | 🟨 |
| 버퍼 5일에 문서 7건이 몰림 | 캡처는 주차별로 미리 확보. 우선순위 GIF → 비교 → 릴리스 → 시나리오 → 영문 | ⬜ 1주·2주 캡처 아직 없음 |
| 범위 팽창 | Icebox 표 고정 — 제출 후 backlog | — |
| 브랜치 스택이 길수록 main과 괴리 | 8/2–8/3에 `feat/m2image-install` 우선 머지 | 🟨 |

---

# 관련 문서

- 설치: [install](../../20-guides/install.md) · 테스트: [host-install](../../40-tests/host-install.md)
- API: [api](../../20-guides/api.md) · 대시보드: [web](../../20-guides/web.md)
- CI 부팅 매트릭스: [m2-ci-boot-matrix](../../20-guides/m2-ci-boot-matrix.md)
- MicroStorage: [micro-storage](../../20-guides/micro-storage.md)
- 명시 MicroNetwork: `docs/20-guides/explicit-micro-network.md` *(별도 관리 초안)*
- 백로그: [week4-tasks](week4-tasks.md) · [week5-tasks](week5-tasks.md)
- 대회 초안: 저장소 루트 `오픈소스개발자대회.md`
- 이미지 카탈로그: [task-m2image-catalog-api](../task-m2image-catalog-api.md)
- M2Image-builder: [task-m2image-builder](../task-m2image-builder.md)
- M2Image 레지스트리: [task-m2image-registry](../task-m2image-registry.md)
