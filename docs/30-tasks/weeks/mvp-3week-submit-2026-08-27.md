---
tags:
  - firecrab
  - plan
  - mvp
  - contest
status: 진행 중
updated: 2026-08-01
---

# 제출 MVP 플랜 — 2026-08-01 → 2026-08-27

> [!summary] 한 줄
> 8/1–8/2 이틀에 브랜치 · 설치 · 대회 문구를 정리하고, **8/3부터 3주를 서비스 기능 구현에 쓴다**.
> 제출물(README · 영상 · 문서)은 버퍼 구간에서 만든다.

## 일정

| 구간 | 날짜 | 초점 |
|---|---|---|
| **준비** | 8/1(토) – 8/2(일) | 브랜치 머지 · 대회 문구 · 설치 재검증 |
| **1주** | 8/3(월) – 8/9(일) | 콘솔 셸 — 좌측 메뉴 · 터미널 UI |
| **2주** | 8/10(월) – 8/16(일) | 이미지 — 카탈로그 · 템플릿 설치 · 진행 로그 |
| **3주** | 8/17(월) – 8/22(토) | 서비스 기능 — 관측 · 초기화 스크립트 |
| **버퍼** | 8/23(일) – 8/27(목) | 동결 · 제출물 · **8/26 제출**(8/27 예비) |

> [!important] 원칙
> - 3주(8/3–8/22)는 전부 기능 구현. 문서 · 영상은 버퍼에서만 만든다
> - "된다"의 정의 = 빈 호스트에서 `install.sh` → 대시보드에서 이미지 설치 → 네트워크 생성 → VM start → 콘솔 접속
> - VRF · Jailer · 인증 · Snapshot은 제출 후
> - **화면이 완성되는 주에 스크린샷 · GIF를 그때 찍어 둔다** — 버퍼에서 몰아 찍지 않는다
> - 제출 필수 문서만 리포에 커밋한다

## 제출 MVP 한 장 (Must)

상태는 **main 기준**이다. 최근 기능 다수가 `feat/explicit-micro-networks`에 있고 아직 머지되지 않았다.

| # | 데모 문장 | main 상태 | 담당 구간 |
|---|---|---|---|
| M1 | 한 줄 설치로 API · helper · 대시보드가 뜬다 | 있음, 실호스트 재검증 필요 | 준비 |
| M2 | MicroNetwork를 만들고 VM을 그 네트워크에 붙인다 | **브랜치에만 있음** | 준비 |
| M3 | MicroStorage로 디스크 위치를 고른다 | **브랜치에만 있음** | 준비 |
| M4 | AWS 콘솔처럼 좌측 메뉴로 VM · 네트워크 · 스토리지 · 이미지를 오간다 | 없음 (모달 5개) | 1주 |
| M5 | 브라우저 터미널에서 게스트 셸을 쓴다 | 동작함, 디자인 미완 | 1주 |
| M6 | 대시보드에서 템플릿을 골라 이미지를 설치하고, 진행 로그를 복사한다 | 없음 (`install.sh`에서만) | 2주 |
| M7 | 실행 중 VM의 CPU · 메모리가 보인다 | 없음 | 3주 |
| M8 | 저장해 둔 쉘 스크립트를 골라 VM을 만들면 첫 부팅에 실행된다 | 없음 | 3주 |
| M9 | README · 설치 · 데모 시나리오 · 대회 문구 | 부분 | 준비(문구) · 버퍼 |

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
- TAP/NAT/DHCP, MicroNetwork CRUD, 시작 타임라인, xterm 콘솔(WebSocket)
- `install.sh`, systemd 유닛, doctor, MicroStorage, artifact layout
- 게스트 파일 주입 경로 — `rootfs.rs`의 `write_into_image`(debugfs, root 불필요)
- CI: install job의 create 스모크, nightly `vm-boot` alpine 1셀

## 제출에 넣지 않는 것 (Icebox / 제출 후)

| 항목 | 이유 |
|---|---|
| VRF / 네트워크별 uplink | 데모 필수 아님, 환경 의존 |
| Jailer / cgroup 풀세트 | 범위 · 검증 시간이 큼 |
| Snapshot / Backup | 7주차 설계 |
| API 인증 · RBAC | 로컬 설치 데모는 loopback으로 충분 |
| 멀티 호스트 / 스케줄러 | 단일 호스트 MVP |
| M2Image 캡처(VM → 이미지) | 이미지 설치(2주)까지가 제출 범위 |
| GitHub 리포 연동 | 초기화 스크립트의 `git clone`으로 대체 |
| lifecycle 이벤트 로그 | 시작 타임라인으로 데모는 충분 |

---

# 준비 — 브랜치와 정합성 (8/1 – 8/2, 2일)

> [!summary] 목표
> 브랜치를 main에 올리고, 심사자가 보게 될 설치 경로와 대회 문구를 사실과 맞춘다. 이틀 안에 끝낸다.

| 상태 | 제목 | 작업 | 완료 기준 |
|---|---|---|---|
| ✅ | micro_storages 테스트 보강 | 핸들러 커버리지 61% → 95.9% (테스트 11개 추가, 남은 미커버는 DB 실패 분기) | 브랜치 patch 91.2% |
| ⬜ | 명시 MicroNetwork 브랜치 머지 | 9커밋 · Rust +2584 리뷰 | PR 초록, main에 명시 네트워크 · MicroStorage · doctor |
| ⬜ | M2 매트릭스 1회 실행 | 머지 전 `workflow_dispatch`로 브랜치 지정 실행 | **ubuntu-26.04 셀 통과**가 기준, alpine은 보조 |
| 🟨 | 대회 문구 정합성 | `오픈소스개발자대회.md` 문구를 `Ubuntu·Alpine 템플릿`으로 수정 (293/300자) | 문서 수정 완료, **접수 사이트 수정 반영은 직접 확인 필요** |
| 🟨 | 설치 경로 재검증 | 빈 VM에서 `install.sh` 1회 | 테스트 수 갱신 완료(216/20/16/56, 합계 308). **유닛 2개 active · 네트워크 생성 후 VM start는 일회용 VM에서 직접 실행 필요** — [host-install](../../40-tests/host-install.md) |

**완료 정의:** main에서 설치 → 네트워크 → **ubuntu** VM start → running 1회 성공

---

# 1주 — 콘솔 셸 (8/3 – 8/9)

> [!summary] 주간 목표
> 모달 5개로 흩어진 화면을 **AWS 콘솔형 셸**로 바꾸고, 브라우저 터미널을 쓸 만하게 만든다.
> 지금 구조: `App.tsx` 하나 + 모달 5개, 라우터 없음, xterm은 이미 있음.

| 상태 | 제목 | 작업 | 완료 기준 | 산출물 |
|---|---|---|---|---|
| ✅ | 앱 셸 레이아웃 | 헤더 + 좌측 내비 + 콘텐츠 영역, 반응형 축소(≤1000px 아이콘 레일 · ≤700px 가로 줄) | 해시 라우팅(`#/vms`)으로 새로고침 · 뒤로가기 유지 확인 | `App.tsx`, `components/Shell.tsx`, `navigation.ts` |
| ✅ | 좌측 메뉴 · 화면 전환 | 모달 → 페이지 승격: 네트워크 · 스토리지 · 호스트. 헤더 버튼 제거, 이미지만 자리표시자(2주) | 메뉴로 5개 전환, 헤더 버튼 0개, 화면 이탈 시 해당 페이지 폴링 정지 확인 | `MicroNetworks.tsx`, `MicroStorages.tsx`, `HostInfo.tsx` (모두 `*Modal` → 페이지로 개명) |
| ✅ | 터미널 웹 UI | xterm 화면 디자인 — 전체화면 토글, 폰트 · 색, 접속 상태 표시, 끊김 시 재연결 | 콘솔에서 로그인 · 명령 실행이 불편하지 않음 | `Console.tsx` |
| ⬜ | 로그 복사 유틸 | 클립보드 복사 버튼 (콘솔 출력 · 시작 타임라인) | 버튼 한 번으로 전체 텍스트 복사 | `components/` 공용 훅 — 2주에서 재사용 |

**완료 정의**

- [x] 좌측 메뉴만으로 5개 화면 이동, 모달 의존 제거 (VM 콘솔 · VM 상세만 모달로 남김 — 목록 위에 겹쳐야 하는 화면)
- [x] 콘솔에서 게스트 셸 작업이 실제로 가능(폭 · 스크롤 · 재연결)
- [ ] 기존 lifecycle 조작(생성 · 시작 · 정지 · 삭제) 회귀 없음
- [ ] 좌측 메뉴 · 터미널 화면 캡처 확보 (README GIF 소재)

---

# 2주 — 이미지 (8/10 – 8/16)

> [!summary] 주간 목표
> 지금은 `install.sh`만 이미지를 넣을 수 있다. **대시보드에서 템플릿을 골라 설치**하고 그 과정을 보게 한다.

> [!warning] 먼저 정할 것 — 설치 방식
> `firecrab-api`는 비특권 계정 + `NoNewPrivileges=yes` + `ProtectSystem=full`이라 **docker 빌드를 실행할 수 없다**.
> - **(권장) 사전 빌드 이미지 다운로드** — 릴리스에 올린 이미지를 API가 받아 digest 검증. 레지스트리에 digest가 이미 있어 검증 코드 재사용
> - 기본 템플릿이 ubuntu(1.2GB, chroot 빌드)라 호스트에서 빌드하는 방식은 설치 시간이 감당되지 않는다 — 다운로드가 사실상 유일한 선택
> - (대안) root 이미지 데몬 추가 — 새 유닛 · 권한 설계가 필요해 3주 일정에 위험

| 상태 | 제목 | 작업 | 완료 기준 | 산출물 |
|---|---|---|---|---|
| ⬜ | [카탈로그 API](../task-m2image-catalog-api.md) | `GET /api/images` — alias · 버전 · digest · 설치 여부 | 생성 폼의 하드코딩 `TEMPLATES` 제거 | `handlers/images.rs`, `CreateVm.tsx:14` |
| ⬜ | 이미지 설치 API | `POST /api/images/{alias}/install` — 비동기 작업 + 상태 조회 | 미설치 템플릿이 설치 후 생성 가능 상태가 됨 | `handlers/images.rs`, `templates.rs` |
| ⬜ | 설치 진행 로그 | 단계별 로그 조회(내려받기 · 검증 · 배치) | 화면에서 진행이 보이고 **복사 버튼**으로 전체 복사 | 1주 복사 유틸 재사용 |
| ⬜ | 이미지 화면 | 좌측 메뉴 "이미지" — 목록 · 설치 버튼 · 용량 표시 | 이미지 0개 상태에서도 무엇을 해야 할지 보임 | `components/Images.tsx` |
| ⬜ | 기본 템플릿 = Ubuntu | 대시보드 · `install.sh` 기본을 ubuntu로 전환(현재 기본은 alpine, ubuntu는 `--with-ubuntu-image`) | 아무 옵션 없이 설치했을 때 ubuntu로 VM이 뜸 | `install.sh`, `templates.rs` |
| ⬜ | Ubuntu 이미지 배포 (`v0.1.0-rc`) | 1.2GB를 릴리스 아티팩트로 게시(압축 + digest). 다운로드 대상이 있어야 설치 API를 검증할 수 있음 | 대시보드 설치 버튼으로 ubuntu 이미지 확보 | `release.yml`, 프리릴리스 태그 |

**완료 정의**

- [ ] `install.sh --no-images`로 깐 호스트에서 대시보드만으로 이미지 설치 → VM 생성 → running
- [ ] 대시보드 템플릿 목록 = 서버 레지스트리
- [ ] 설치 로그 복사 버튼 동작
- [ ] 이미지 설치 진행 화면 녹화 확보 (README GIF 소재)

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

| 리스크 | 완화 |
|---|---|
| 이미지 설치 권한 — API가 docker를 못 씀 | 사전 빌드 이미지 다운로드 방식으로 우회(2주 첫날 확정) |
| Ubuntu 이미지 1.2GB — 빌드 시간 · 배포 용량 | 호스트 빌드 대신 **압축 아티팩트 다운로드**. alpine은 CI · 빠른 검증용으로만 유지 |
| 좌측 메뉴 리팩터가 기존 lifecycle 화면을 깨뜨림 | 모달 제거 전 화면별 수동 회귀 1회 |
| `micro_storages` 커버리지로 patch 80% 미달 | 준비 구간 첫 태스크로 선처리 |
| 대회 문구가 실제 기능보다 넓음 | 준비 구간에 수정 |
| 버퍼 5일에 문서 7건이 몰림 | 캡처는 주차별로 미리 확보. 우선순위는 GIF → 비교 섹션 → 릴리스 → 시나리오 → 영문 |
| 범위 팽창 | Icebox 표 고정 — 제출 후 backlog |

---

# 관련 문서

- 설치: [install](../../20-guides/install.md) · 테스트: [host-install](../../40-tests/host-install.md)
- API: [api](../../20-guides/api.md) · 대시보드: [web](../../20-guides/web.md)
- CI 부팅 매트릭스: [m2-ci-boot-matrix](../../20-guides/m2-ci-boot-matrix.md)
- MicroStorage: [micro-storage](../../20-guides/micro-storage.md)
- 백로그: [week4-tasks](week4-tasks.md) · [week5-tasks](week5-tasks.md)
- 대회 초안: 저장소 루트 `오픈소스개발자대회.md`
