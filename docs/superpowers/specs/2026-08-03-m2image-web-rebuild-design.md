---
tags:
  - firecrab
  - m2image
  - spec
status: 설계 완료
scope: MVP
updated: 2026-08-03
---

# M2Image 웹 부트스트랩 빌드(원본 배포판을 microVM으로 준비) — 설계

> [!summary] 한 줄 요약
> Alpine/Ubuntu/Rocky를 공식 업스트림 소스로부터 다시 빌드하는 것(현재
> `build-m2images.sh`, docker + sudo/chroot 필요)을 **builder microVM
> 안에서** 실행해 `{alias}.tar.zst`로 패키징하고, 이미 있는 "가져오기"
> 설치 파이프라인이 그대로 이어받게 한다. 새 특권 프로세스·docker·sudo·새
> 등록 로직 전부 필요 없다.

## 왜

- 로컬 `images/kernel/`이 최신 코드의 배포판별 전용 커널 기대치와 안 맞아
  아무 이미지도 "installed"로 안 잡히는 문제에서 출발한 요구다.
- 지금은 `./scripts/build-m2images.sh`를 셸에서 직접 돌려야 한다 —
  docker(alpine/rocky) + sudo/chroot(ubuntu)가 필요하고, `firecrab-api`는
  의도적으로 비특권(`NoNewPrivileges=yes`, docker 없음)이라 이 경로를 API
  프로세스 자체에서 못 쓴다(`packaging/systemd/firecrab-api.service`).
- 2026-08-02 설계로 만든 "이미지 빌드" 기능은 그 설계 문서 스스로 범위
  경계로 명시했듯 **이미 설치된** 템플릿을 소스로 패키지만
  커스터마이징하는 것이다 — "완전히 새로운 배포판을 처음부터
  부트스트랩"하는 건 그때 의도적으로 CLI에 남겨뒀다. 이번 설계는 그
  경계를 닫는 것이다.
- 같은 설계의 핵심 통찰 — "docker/sudo가 하던 일은 격리된 root 환경을
  빌리는 것뿐이고, Firecracker microVM 자체가 이미 그런 환경이다" — 를
  패키지 커스터마이징뿐 아니라 **배포판을 처음부터 준비하는 단계**에도
  그대로 적용한다.

## 확인된 사실 (기존 스크립트 분석)

- **Alpine**: docker의 역할은 `apk --root <staging>`을 root 권한으로
  돌리는 것뿐이다. mount도 chroot도 안 쓴다
  (`install-alpine-rootfs.sh` 주석: "apk --root installs straight into a
  staging directory without a mount or chroot").
- **Ubuntu**: `debootstrap`도 안 쓴다. Canonical이 미리 만든 "ubuntu-base"
  tar를 받아 풀고, `chroot`+`mount`(/proc,/dev,/sys,/run)로 그 안에 들어가
  `apt-get install`을 돌린다. 이미 호스트 sudo로(docker 없이) 하고
  있다는 점에서 알 수 있듯, 순수 `chroot`+`mount`만으로 충분하다.
- **Rocky**: docker 컨테이너 안에서 Ubuntu와 같은 방식(`mount_chroot_fs`
  로 proc/sys/dev/run 마운트 후 chroot, `dnf --installroot`류 설치)을
  쓴다.
- **커널 추출**: `extract-vmlinux`는 순수 POSIX 셸 스크립트다(`file`,
  `gunzip`/`unxz`/`bunzip2` 등만 사용) — 어디서 실행해도 동일하게
  동작한다.
- **최종 이미지 생성**: 세 스크립트 모두 최종적으로
  `truncate -s <size> image.ext4 && mkfs.ext4 -F -d <staging> image.ext4`
  패턴을 쓴다 — **loop mount조차 필요 없다.** e2fsprogs가 디렉터리
  내용으로 직접 ext4 이미지를 만드는 기능을 제공하기 때문이다. 이 저장소
  전체가 이미 이 "mount 없이 처리" 철학을 씀(`rootfs.rs`의 debugfs 기반
  조작도 같은 이유).
- 세 가지 모두 **일반 사용자 권한으로 되는 작업**과 **root가 필요한
  작업**(mknod, chroot, mount)이 섞여 있지만, docker/sudo는 순전히 "root
  권한이 있는 임시 환경을 빌리는" 용도였을 뿐이다. microVM 게스트 안에서는
  이미 진짜 root이므로 이 모든 단계가 그대로 된다.

## 아키텍처 — 부트스트랩도 builder microVM으로

**핵심 결정**: 새 프로세스나 권한 상승을 전혀 추가하지 않는다. 기존
"이미지 빌드"(2026-08-02) 기능이 이미 갖춘 메커니즘 — builder VM
부팅(`start_build`), 콘솔 명령 실행, VM 정지 후 디스크 확정 — 을 그대로
재사용하되, 콘솔에서 실행하는 내용과 마지막에 하는 일만 다르다.

```
웹 "ubuntu-26.04 부트스트랩 빌드"
  │
  ▼
builder microVM 부팅
  (기존 start_build 그대로 — 소스는 "이미 설치된 임의의 템플릿" 중 선택,
   BuildModal의 기존 소스-선택 드롭다운 재사용. 이 VM은 목표 배포판을
   만들 작업 공간일 뿐, 최종 산출물과 무관하다.)
  │
  ▼
콘솔에서 부트스트랩 스크립트 실행
  (install-{alpine,ubuntu,rocky}-rootfs.sh의 핵심 로직에서
   docker run/호스트 sudo 재실행 부분만 제거한 버전 — 게스트 안에서는
   이미 root라 그 wrapper가 필요 없다.)
  1. 공식 base(minirootfs / ubuntu-base tar / Rocky BaseOS) 다운로드
  2. (ubuntu/rocky) chroot + mount(/proc,/dev,/sys,/run) 진입 후
     패키지 매니저로 커널 포함 패키지 설치 / (alpine) apk --root로 설치
  3. extract-vmlinux로 vmlinux 추출
  4. truncate + mkfs.ext4 -d <staging> 로 최종 rootfs.ext4를
     게스트 자신의 디스크 위에 파일로 생성 (loop mount 불필요)
  │
  ▼
VM 정지 → 게스트 디스크에서 결과물 파일들(rootfs.ext4, vmlinux, initrd)을
  호스트로 뽑아냄
  (신규: rootfs.rs의 기존 debugfs 기반 write_into_image류 헬퍼를
   확장한 "파일 하나를 통째로 dump"하는 헬퍼 — 이미 있는 마운트 없는
   ext4 조작 방식의 연장선. 지금 finalize_and_register가 하는 "디스크
   전체를 그대로 복사"와는 다르다 — 이번엔 게스트 디스크 안의 파일
   하나를 뽑아내는 것)
  │
  ▼
package-m2images.sh와 동일한 레이아웃(kernel/... rootfs/...)으로
  ubuntu-26.04.tar.zst + 체크섬 패키징
  │
  ▼
images/.packages/ubuntu-26.04.tar.zst 에 저장
  (image_install::staged_package_path()가 가리키는 바로 그 경로)
  │
  ▼
빌더 VM 삭제 (기존 delete_vm 경로)
  │
  ▼
기존 "가져오기" 설치 파이프라인이 그대로 인식 → 검증 → 템플릿 등록
  (image_install.rs, 이미 구현·테스트됨 — 새 코드 불필요.
   staged_package_exists()가 이제 true를 반환하므로 프론트엔드의
   기존 "가져오기" 버튼이 그대로 동작한다.)
```

### 의도적 범위 경계

- 대상은 **기존 3개 배포판**(alpine-3.24, ubuntu-26.04, rocky-9)뿐이다.
  `default_specs()`에 없는 완전히 새로운 배포판 추가는 범위 밖이다 —
  하드코딩된 커널 선택/`boot_args` 튜닝이 필요해 별도 작업이다.
- 부트스트랩 builder VM을 띄우려면 **이미 설치된 템플릿이 최소 1개
  있어야 한다**(어떤 alias든 무관 — 작업 공간일 뿐이다). 완전히 처음
  이미지가 하나도 없는 상태에서의 최초 1개 확보는 여전히
  `build-m2images.sh`(CLI)의 몫이다 — 이건 2026-08-02 설계가 이미
  못박은 경계이고 이번 기능도 동일하게 상속한다.
- 동시에 하나의 부트스트랩 빌드만 허용한다(기존 "이미지 빌드"와 마찬가지
  이유 — builder VM 자원/디스크 공간).
- `build-m2images.sh`/`package-m2images.sh` 자체는 그대로 유지한다 —
  CI(`docs/40-tests/cicd-github-actions.md`)가 이미 이 스크립트를 쓴다.
  웹 경로는 추가 경로일 뿐 CLI를 대체하지 않는다.

## 신규 스크립트

`scripts/firecracker-menual/`에 게스트 안에서 실행할 3개 스크립트를
새로 추가한다(기존 `install-{alpine,ubuntu,rocky}-rootfs.sh`의 핵심
로직을 이식하되 docker/호스트-sudo wrapper 제거):

- `bootstrap-alpine-in-guest.sh`
- `bootstrap-ubuntu-in-guest.sh`
- `bootstrap-rocky-in-guest.sh`

콘솔로 긴 스크립트 전체를 실행하는 방식은 기존
`run_package_action`/`run_finalize`가 짧은 명령 한 줄을 콘솔에 쓰는
것과 다르다 — 스크립트 본문을 base64로 인코딩해 콘솔에 흘려보낸 뒤
게스트에서 디코드·실행하고, 완료 시 기존과 동일한 sentinel(`:$?`)로
종료 코드를 알린다.

## 데이터 흐름 / 컴포넌트

### `rootfs.rs` 확장

기존 `write_into_image`/`remove_from_image`(작은 파일 쓰기/삭제, 마운트
없이 debugfs로)에 대응하는 `dump_from_image` 류 헬퍼를 추가한다 — 게스트
디스크 안의 파일 하나(수백MB~2GB급 rootfs.ext4 포함)를 호스트 파일로
통째로 꺼낸다. `debugfs -R "dump ..."` 기반, 큰 파일이라도 파일시스템
자체가 다루는 문제라 크기 제약은 디스크 용량뿐이다.

### `handlers/builds.rs` 확장 (또는 신규 bootstrap 전용 핸들러)

기존 빌드 세션과 **다른 종류의 세션**으로 다룬다 — 목적지가 "템플릿으로
직접 등록"이 아니라 "tar.zst로 패키징 후 가져오기 파이프라인에 전달"이기
때문이다. 정확한 라우트/타입 분리(기존 `BuildStatus`/`finalize_build`를
분기할지, 별도 `BootstrapStatus`/`bootstrap_build`를 새로 둘지)는 구현
플랜에서 결정한다 — 이 설계에서 고정할 것은 "부트스트랩 빌드의 종료
지점은 `images/.packages/{alias}.tar.zst` 생성이며, 그 이후는 전부 기존
가져오기 경로"라는 점뿐이다.

### 패키징

새 헬퍼(Rust, 또는 `package-m2images.sh`의 핵심 tar/zstd/체크섬 로직을
그대로 셸아웃)가 추출된 rootfs.ext4 + vmlinux + initrd를
`kernel/...`·`rootfs/...` 레이아웃으로 묶어 `{alias}.tar.zst`를 만들고
`images/.packages/{alias}.tar.zst`에 놓는다.

## 프론트엔드

`Images.tsx` 표에 "부트스트랩 빌드" 버튼을 추가한다(설치 여부와 무관하게
3개 alias 모두에 노출 — 재부트스트랩도 허용). 클릭 시 소스로 쓸 builder
VM의 템플릿은 이미 설치된 것 중 아무거나 선택(기존 `BuildModal`의 소스
드롭다운과 동일한 UI 재사용). 진행 상황은 기존 `BuildModal`/`가져오기`
로그 패널과 같은 스타일로 폴링해 보여준다. 완료되면 기존 "가져오기"
버튼이 자동으로 활성화된다(패키지가 이제 로컬에 존재하므로).

## 에러 처리

- 부트스트랩 스크립트 실패(다운로드 미러 장애, 패키지 설치 실패 등) →
  스크립트의 기존 `[FAIL] ...` 형태 메시지가 콘솔 로그에 그대로 노출.
- 게스트 디스크 공간 부족(목표 rootfs 크기 + 임시 다운로드분을 감당 못함)
  → builder VM 디스크 크기 산정 시 기존 `builder_disk_gb`보다 넉넉한
  여유(대상 rootfs 크기 + 다운로드 아카이브 크기)를 반영해야 한다 —
  구현 플랜에서 구체적 수치 결정.
- 겹치는 부트스트랩 요청 → 409(기존 "이미지 빌드" 세션 직렬화와 동일
  패턴).
- 패키징 실패(디스크 dump 실패, tar/zstd 실패 등) → builder VM은
  삭제하되 세션은 Failed로 남기고 로그에 사유 기록(기존
  `finalize_build`의 실패 처리 패턴 재사용).

## 테스트 전략

- 부트스트랩 스크립트 자체(다운로드+chroot+mkfs)는 실제 네트워크 +
  수 분 단위 시간이 필요해 자동화 테스트 대상이 아니다 —
  `build-m2images.sh`가 지금도 마찬가지다.
- `rootfs.rs`의 신규 `dump_from_image` 류 헬퍼는 기존 ext4 테스트
  픽스처로 단위 테스트(작은 파일을 이미지에 심고 dump해서 내용이
  일치하는지 확인).
- 패키징 로직(tar.zst 레이아웃, 체크섬)은 실제 파일 몇 개로 단위
  테스트 가능.
- 세션 상태 전이(시작→진행중→완료/실패, 직렬화 가드)는 기존
  `handlers::builds`의 콘솔-subscriber-대기 fixture 패턴을 재사용해
  가짜 콘솔 응답으로 테스트.
- 3개 배포판 각각의 전체 부트스트랩 성공은 수동 검증(실제 인터넷 필요).

## 완료 기준 (MVP)

- [ ] `bootstrap-{alpine,ubuntu,rocky}-in-guest.sh` 3개 스크립트(기존
      스크립트에서 docker/호스트-sudo wrapper 제거, 핵심 로직 유지)
- [ ] `rootfs.rs`에 게스트 디스크에서 파일을 dump하는 헬퍼
- [ ] 부트스트랩 빌드 세션(시작/진행/완료) + tar.zst 패키징 +
      `images/.packages/{alias}.tar.zst` 저장
- [ ] `Images.tsx`에 "부트스트랩 빌드" 버튼 + 로그 패널, 완료 후 기존
      "가져오기" 버튼이 자동으로 활성화됨을 확인
- [ ] 3개 배포판 각각 웹에서 부트스트랩 → 가져오기 → VM 생성까지
      end-to-end 성공 확인(수동 검증)

## 참고

- 2026-08-02 설계: `docs/superpowers/specs/2026-08-02-m2image-web-builder-design.md`
  (이미 설치된 템플릿 커스터마이징 — 이번 기능이 재사용하는 builder VM
  메커니즘의 원출처)
- `scripts/build-m2images.sh`,
  `scripts/firecracker-menual/install-{alpine,ubuntu,rocky}-rootfs.sh`,
  `scripts/package-m2images.sh`: 이번 기능이 로직을 이식/재사용하는 기존
  스크립트
- `firecrab-api/src/image_install.rs`: 이번 기능의 산출물을 그대로
  이어받는 기존 가져오기 설치 파이프라인
