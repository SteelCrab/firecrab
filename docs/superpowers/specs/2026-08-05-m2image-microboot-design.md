---
tags:
  - firecrab
  - m2image
  - spec
status: 설계 완료
scope: MVP
updated: 2026-08-05
---

# M2Image 부트스트랩 — MicroBoot로 최초 템플릿 의존성 제거 — 설계

> [!summary] 한 줄 요약
> 부트스트랩 빌더 VM을 띄우려면 "이미 설치된 템플릿이 최소 1개"
> 있어야 한다는 2026-08-03 설계의 경계 조건을, 배포판이 공식 배포하는
> 최소 부팅 kernel(+initrd) 아티팩트 — **MicroBoot** — 로 대체해 없앤다.
> 설치된 템플릿이 0개인 완전히 새 기기에서도 웹의 부트스트랩 버튼만으로
> alpine/ubuntu/rocky 전부 처음부터 만들 수 있게 된다.

## 왜

- 2026-08-03 설계(`2026-08-03-m2image-web-rebuild-design.md`)는 "의도적
  범위 경계"에서 이렇게 못박았다: *"부트스트랩 builder VM을 띄우려면
  이미 설치된 템플릿이 최소 1개 있어야 한다... 완전히 처음 이미지가
  하나도 없는 상태에서의 최초 1개 확보는 여전히 `build-m2images.sh`
  (CLI)의 몫이다."*
- 실제 배포 환경에서 바로 이 경계에 부딪혔다: 로컬 `images/kernel/`이
  배포판별 규격 파일명(`vmlinux-ubuntu-26.04-x86_64` 등)과 안 맞아
  `TemplateRegistry`에 등록된 템플릿이 0개였고, 그 결과 웹에서
  어떤 배포판을 부트스트랩하려 해도 `pick_builder_source`가 즉시
  503("no template is installed yet to serve as the builder VM")로
  막았다.
- CLI 스크립트(`install-ubuntu-roofs.sh` 등)를 직접 돌려 우회할 수는
  있지만, 그 스크립트는 host에서 `sudo`+`mount`+`chroot`를 직접 쓰는
  구경로다 — 애초에 부트스트랩 기능 전체가 "host 권한을 새로 노출하지
  않는다"는 원칙으로 설계됐는데, 최초 1개를 위해 그 원칙을 깨는 건
  일관성이 없다.
- 목표는 명확하다: **설치된 템플릿 0개에서 시작해도**, 웹만으로 처음부터
  끝까지 완결되어야 한다.

## 핵심 아이디어: MicroBoot

**정의**: 배포판이 공식 배포하는, rootfs 디스크나 이미 설치된 템플릿 없이
그 자체(kernel + initrd)만으로 Firecracker에서 부팅 가능한 최소
아티팩트. 기본적인 네트워크 동작과 최소한의 기본 도구만 제공하며,
부트스트랩 **대상** 배포판의 실제 패키지는 전혀 들어있지 않다.

- `/api/images`에는 절대 노출되지 않는다 — 사용자가 설치/관리하는
  대상이 아니라, 순수하게 빌더 VM을 부팅시키는 내부 재료다.
- 최종 산출물이 되는 법이 없다 — 빌더 VM 안에서 대상 배포판의 공식
  소스를 받아 별도 빈 디스크에 rootfs를 새로 만드는 동안, MicroBoot
  자신은 그 작업을 실행하는 "장소"로만 쓰이고 버려진다.
- `pick_builder_source`(이미 설치된 템플릿 중 하나를 고르던 로직)의
  자리를 대체한다.
- **`TemplateRegistry` 등록 여부는 구현 방식의 문제다** — 아래
  "`pick_builder_source` 대체" 참고. 사용자에게 노출되지 않는다는
  원칙과, 내부적으로 `TemplateRegistry`의 검증·조회 기계를 재사용하려고
  비공개 네임스페이스로 등록하는 것은 모순이 아니다.

## 배포판별 MicroBoot 소스

| alias | MicroBoot 소스 | 이유 |
|---|---|---|
| `alpine-3.24` | Alpine 공식 `netboot/vmlinuz-virt` + `netboot/initramfs-virt` (`dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/netboot/`) | `apk`가 실제로 동작하는 최소 환경(아래 "실제 부팅 검증" 참고). 자기 자신을 자기 자신으로 부트스트랩 |
| `ubuntu-26.04` | 위 Alpine MicroBoot를 그대로 재사용 | outer(빌더) 환경엔 애초에 `apt`가 필요 없다 — `ubuntu-base-*.tar.gz` 자체에 apt가 내장돼 있고, outer는 `tar`/`mount`/`chroot`만 있으면 된다(기존 스크립트 분석과 동일한 결론) |
| `rocky-9` | Rocky 공식 Container-Base(`Rocky-9-Container-Base.latest.x86_64.tar.xz`, `dl.rockylinux.org/pub/rocky/9/images/x86_64/`) 안의 단일 레이어를 루트로, Ubuntu 템플릿의 `linux-image-generic` 커널(virtio_blk/ext4 builtin, initrd 불필요)로 부팅 | outer 환경에 실제 동작하는 `dnf`/`rpm`이 필요(`dnf --installroot` 방식) — 아래 "실제 부팅 검증"에서 확인. 커널은 Rocky 전용일 필요 없음(dnf는 커널과 무관) — 이미 이 저장소에 있는 커널을 재사용해 별도 커널 조달이 필요 없다 |

### 실제 부팅 검증 (2026-08-05, 구현 착수 전 직접 확인)

원래 이 설계는 "Alpine netboot 자체가 apk까지 내장된 완전한 환경"이라고
가정했는데, 실제로 Firecracker에 `vmlinux-virt`(`extract-vmlinux`로 ELF
추출)+`initramfs-virt`만 붙여 부팅해본 결과 **그 가정은 틀렸다**:

- Alpine 자신의 `/init`(Alpine Init 3.14.0-r0)은 부팅 후 "Mounting boot
  media"를 시도하고, CD/네트워크 저장소 등 인식 가능한 매체를 못 찾으면
  **커널 패닉으로 죽는다** — 디스크를 안 붙여도, 빈 디스크를 붙여도
  동일하게 실패했다(직접 재현 확인). `netboot`이라는 이름 그대로,
  원래는 `alpine_repo=`/`ip=dhcp` 같은 커널 인자로 네트워크 너머의
  진짜 시스템(스쿼시FS 등)을 받아오는 걸 전제로 만들어진 파일이지,
  독립적으로 셸까지 뜨는 라이브 환경이 아니다.
- `boot_args`에 `rdinit=/bin/sh`를 주면 Alpine의 `/init`을 완전히
  건너뛰고 콘솔(ttyS0)로 진짜 인터랙티브 셸에 도달한다 — **직접 검증
  완료**(콘솔에 명령을 흘려보내고 echo가 돌아오는 것까지 확인). `apk
  --version`도 정상 동작(`apk-tools 3.0.6-r0`)해서, 도구 자체는
  initramfs 안에 실제로 들어있음도 확인했다.
- 다만 `rdinit=/bin/sh`는 Alpine `/init`이 원래 해주던 일(busybox
  심볼릭 링크 설치로 `ls`/`mount`/`mkdir` 등을 PATH에 넣어주는 것,
  `/proc`·`/sys`·`/dev` 마운트)을 전혀 안 해준다 — `mount`/`ls`조차
  "not found"였다. 그래서 게스트 부트스트랩 스크립트 맨 앞에
  **환경을 직접 갖추는 준비 단계**(`busybox --install`로 심볼릭 링크
  생성, `/proc`·`/sys`·`/dev` 마운트, `PATH` 설정)를 새로 넣어야
  한다 — 아래 "게스트 스크립트에 필요한 준비 단계" 참고.
- Rocky Container-Base는 평범한 tar가 아니라 **OCI 이미지 레이아웃**
  (`blobs/sha256/...`+`index.json`)이었다 — `docker pull`의 원본이니
  당연한 것이었는데 최초 서술에 이 사실이 빠져 있었다. 다행히 레이어가
  1개뿐이라(`tar+gzip`) 그 블롭 하나만 그대로 풀면 끝이다. 풀어서
  확인한 결과 `/usr/bin/dnf`(`dnf-3`로의 심볼릭 링크), `/usr/bin/rpm`이
  실제 실행 가능한 바이너리로 존재한다 — **직접 검증 완료**.

## 아키텍처 — 무엇이 바뀌는가

기존 부트스트랩 흐름(스크립트 실행, 결과물 추출·패키징,
`images/.packages/{alias}.tar.zst` 저장, 이후 "가져오기"/"로컬 패키지
설치" 파이프라인)은 전부 그대로다. 바뀌는 지점은 **빌더 VM을 무엇으로
부팅하는가** 하나뿐이다.

```
웹에서 "{alias} 부트스트랩" 클릭
  │
  ▼
[대체됨] pick_builder_source(이미 설치된 템플릿 중 선택)
       → MicroBoot 소스 결정(위 표, alias 기준 고정 매핑)
  │
  ▼
[신규] MicroBoot 아티팩트 확보
       최초 1회 다운로드 후 로컬 캐시(체크섬 검증, 기존 템플릿
       아티팩트 검증과 동일한 방식 재사용)
  │
  ▼
[변경] 빌더 VM = MicroBoot로 직접 부팅
       kernel/initrd = MicroBoot 아티팩트, boot_args의 rdinit=으로
       initrd 자신을 root로 부팅(디스크 아님). disk = 스크래치
       공간(기존처럼 소스 템플릿의 disk를 root로 쓰는 게 아님)
  │
  ▼
[신규] 콘솔로 준비 단계 실행: busybox --install, /proc·/sys·/dev
       마운트, 스크래치 디스크 mkfs.ext4 + 마운트
  │
  ▼
(이하 2026-08-03 설계와 완전히 동일 — 변경 없음, 작업 경로만
 스크래치 디스크의 마운트 지점 아래로 조정)
콘솔로 부트스트랩 스크립트 실행 → 결과물 추출·패키징 →
images/.packages/{alias}.tar.zst 저장 → 빌더 VM 삭제 →
"로컬 패키지 설치" → TemplateRegistry 등록 → 사용자가 VM 생성 가능
```

## 의도적 범위 경계

- 이 설계는 2026-08-03 설계가 명시적으로 남겨둔 경계("이미 설치된
  템플릿 최소 1개 필요") 하나만 닫는다. 그 외 모든 구조(스크립트,
  패키징 레이아웃, 세션 상태 전이, 에러 처리, 가져오기 파이프라인)는
  전혀 건드리지 않는다.
- 대상은 기존 3개 배포판(alpine-3.24, ubuntu-26.04, rocky-9)뿐이다 —
  2026-08-03 설계와 동일한 경계를 그대로 상속한다.
- MicroBoot 자체를 사용자에게 노출하거나 설치 가능한 대상으로 만들지
  않는다 — 순수 내부 구현 디테일로 유지한다.
- CLI 스크립트(`install-{alpine,ubuntu,rocky}-rootfs.sh`)는 그대로
  유지한다 — CI가 이미 이 스크립트를 쓰고, 이번 설계로 대체되는 게
  아니라 웹 경로가 CLI에 대한 의존성 없이도 독립적으로 완결되는 것뿐이다.

## 데이터 흐름 / 컴포넌트

### MicroBoot 확보/캐시

새 모듈(가칭 `microboot.rs` 또는 `bootstrap.rs` 확장)이 alias →
MicroBoot 소스(URL 목록 + 체크섬)를 고정 매핑으로 갖고, 로컬 캐시
디렉터리(예: `images/.microboot/`)에 없으면 다운로드 후 체크섬 검증,
있으면 그대로 재사용한다. 기존 `TemplateRegistry`의 아티팩트 검증
로직(`verify_artifact`)과 같은 검증 철학을 재사용한다. `TemplateRegistry`
에 등록할지(권장) 말지는 바로 아래 "`pick_builder_source` 대체" 참고 —
등록하더라도 비공개 네임스페이스라 `/api/images`엔 절대 노출 안 된다.

### `pick_builder_source` 대체 — `create_vm`과의 충돌

**중요한 제약**: `start_bootstrap`은 지금 공개 `create_vm` 핸들러를
그대로 호출하고(`handlers/bootstrap.rs:148`), `create_vm`은 항상
`state.templates.resolve_alias(&req.template)`(`handlers/vms.rs:151`)로
`TemplateRegistry`에서 템플릿을 조회해 디스크를 그 템플릿의 아티팩트
기준으로 생성한다. 즉 **`TemplateRegistry`에 전혀 없는 아티팩트로는
지금 구조의 `create_vm`을 그대로 쓸 수 없다.**

두 가지 방법이 있고, 구현 플랜에서 하나를 고른다:

- **(권장) 내부 전용 네임스페이스로 등록**: MicroBoot를 `TemplateRegistry`에
  alias 없이(`(name, version)`만 존재, `list_aliases()`/`/api/images`엔
  안 잡힘) 등록해, 기존 `create_vm`/디스크 생성/아티팩트 검증 로직을
  전부 그대로 재사용한다. `pick_builder_source`는 "이미 설치된 템플릿
  중 선택"에서 "alias별 고정 MicroBoot `(name, version)` 반환"으로만
  바뀌고, 그 결과를 여전히 `create_vm`의 `template` 필드에 넘긴다 — 이
  코드 자체는 거의 안 바뀐다.
- **대안**: `create_vm`을 우회하는 별도 내부 VM 프로비저닝 경로를 새로
  만든다 — `TemplateRegistry`를 아예 안 건드리지만, 기존 `create_vm`이
  이미 하는 아티팩트 검증·디스크 생성 로직을 사실상 중복 구현해야 한다.

이 코드베이스 전반의 재사용 원칙(2026-08-03 설계도 같은 이유로 기존
`start_build`/`create_vm`을 그대로 재사용했다)에 따라 **내부 전용
네임스페이스 등록 쪽을 권장**한다.

### 부팅 방식 — 디스크가 아니라 initrd가 root

**중요한 정정**: 기존 모든 템플릿은 `root=/dev/vda` 방식으로, 템플릿의
rootfs 아티팩트가 그대로 커널이 마운트하는 `/`가 된다. MicroBoot는 그
방식을 안 쓴다 — `boot_args`에 `rdinit=/bin/sh`(정확한 값은 구현 시
확정)를 둬서 커널이 **initrd 자신의 내용을 `/`로 부팅**하게 하고, 붙어
있는 디스크(`/dev/vda`)는 커널이 루트로 마운트하지 않는다. `create_vm`/
`FirecrackerConfig`(Rust) 자체는 안 바뀐다 — 어차피 `is_root_device`
플래그는 Firecracker 쪽 장치 나열 순서 문제일 뿐, 커널이 실제로 뭘
루트로 삼는지는 전적으로 `boot_args`가 결정한다.

### 스크래치 디스크 (구 "빈 디스크")

디스크가 root가 아니게 됐으니, 그 역할은 순수히 **게스트 부트스트랩
스크립트가 직접 포맷해서 쓰는 스크래치 공간**이다 — 다운로드한 원본
아카이브를 풀고, 대상 배포판을 chroot로 설치하고, 최종
`rootfs.ext4`(host가 나중에 dump할 파일)를 만드는 전부가 이 디스크
위에서 일어나야 한다(그래야 VM이 죽은 뒤에도 host가 그 파일을 꺼낼 수
있다 — 게스트의 tmpfs/RAM 내용은 VM 종료와 함께 사라진다).

MicroBoot로 등록하는 "템플릿"의 rootfs 아티팩트는 그래서 (기존
배포판 rootfs처럼 이미 채워진 파일이 아니라) **빈 ext4 이미지**다.
그 빈 디스크 자체를 MicroBoot 등록 시점에 한 번 만들어두거나, 빌더 VM
생성 때마다 새로 만든다 — 크기 산정은 기존 `bootstrap_disk_gb` 로직을
그대로 쓰되, 이제 "소스 템플릿의 디스크 크기"라는 하한 기준 자체가
사라지므로(MicroBoot엔 원래 disk 내용이 없음) 대상 alias 기준 크기만
남는다.

### 게스트 스크립트에 필요한 준비 단계

위 "실제 부팅 검증"에서 확인했듯, `rdinit=/bin/sh`로 도달한 셸은
Alpine의 `/init`이 원래 해주던 환경 구성(busybox 심볼릭 링크,
`/proc`·`/sys`·`/dev` 마운트)이 전혀 안 돼 있다. 3개
`bootstrap-{alpine,ubuntu,rocky}-in-guest.sh` 스크립트 맨 앞에, 지금
없는 준비 단계를 새로 추가해야 한다:

1. `busybox --install -s <PATH상의 디렉터리>`로 `ls`/`mount`/`mkdir`
   등 기본 명령을 PATH에 설치
2. `/proc`, `/sys`, `/dev`를 각각 마운트
3. 스크래치 디스크(`/dev/vda`)를 `mkfs.ext4`로 포맷하고 작업
   디렉터리(예: `/mnt/work`)에 마운트 — 이후 기존 스크립트 로직
   (원본 다운로드, chroot 설치, 최종 `mkfs.ext4 -d`)은 전부 이
   마운트 지점 아래에서 실행되도록 경로만 조정한다

기존 스크립트의 핵심 로직(다운로드/chroot/설치/최종 이미지 생성)
자체는 안 바뀐다 — 맨 앞에 이 준비 단계가 추가되고, 작업 경로가
`/root/fc-bootstrap`(기존, 이미 마운트된 실제 OS 디스크 위)에서
`/mnt/work/fc-bootstrap`(스크래치 디스크 위)로 바뀌는 것뿐이다.

## 에러 처리

- MicroBoot 다운로드 실패(네트워크, 미러 다운) → 부트스트랩 시작 자체가
  실패, 명확한 에러 메시지로 즉시 반환(기존 소스 미설치 503 에러와 같은
  자리, 메시지만 "MicroBoot 다운로드 실패"류로 교체).
- 캐시된 MicroBoot 파일 손상/체크섬 불일치 → 재다운로드 또는 명확한
  실패(기존 `TemplateRegistry`의 검증 실패 처리와 동일한 엄격도).
- Rocky Container-Base 타르볼 안에 dnf가 실제로 없거나 동작하지 않는
  경우(향후 Rocky가 이미지 구성을 바꾸는 등) → 부트스트랩 스크립트
  시작부에서 `require_command dnf`류 조기 체크로 원인을 명확히 드러낸다
  (30분 타임아웃까지 기다리다 실패하는 것보다 훨씬 빠르게).

## 테스트 전략

- MicroBoot 다운로드/캐싱 로직: 단위 테스트(가짜 URL, 로컬 파일시스템,
  체크섬 불일치 케이스 포함).
- `pick_builder_source` 대체 로직: alias → 고정 매핑이 맞는지 단위
  테스트.
- 스크래치 디스크 생성 경로: 기존 `create_vm`류 테스트 패턴 재사용.
- 3개 배포판 각각 "설치된 템플릿 0개" 상태에서 웹 부트스트랩 →
  가져오기 → VM 생성까지 end-to-end 성공은 수동 검증(실제 네트워크
  필요, 2026-08-03 설계와 동일한 한계).

## 완료 기준 (MVP)

- [ ] alias별 MicroBoot 소스 고정 매핑 + 다운로드/캐시/체크섬 검증
      (Rocky는 OCI 레이어 추출 포함)
- [ ] `pick_builder_source`를 MicroBoot 기반으로 교체(더 이상 설치된
      템플릿을 요구하지 않음)
- [ ] 빌더 VM용 스크래치 디스크 생성 경로 추가(root 아님, `rdinit=`으로
      initrd가 root)
- [ ] 3개 게스트 스크립트에 준비 단계 추가(busybox --install,
      /proc·/sys·/dev 마운트, 스크래치 디스크 mkfs+마운트) + 작업
      경로를 스크래치 마운트 지점 아래로 조정
- [ ] 설치된 템플릿 0개인 상태에서 `alpine-3.24` 웹 부트스트랩 성공
      (수동 검증)
- [ ] 동일 상태에서 `ubuntu-26.04`(Alpine MicroBoot 재사용) 성공
      (수동 검증)
- [ ] 동일 상태에서 `rocky-9`(Container-Base MicroBoot) 성공(수동 검증)

## 참고

- 2026-08-03 설계: `docs/superpowers/specs/2026-08-03-m2image-web-rebuild-design.md`
  — 이번 설계가 닫는 경계 조건을 원래 명시한 문서. 부트스트랩의 나머지
  전체 구조(스크립트, 패키징, 세션 상태)는 이 문서를 그대로 따른다.
- Alpine netboot: `https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/netboot/`
- Rocky Container-Base: `https://dl.rockylinux.org/pub/rocky/9/images/x86_64/Rocky-9-Container-Base.latest.x86_64.tar.xz`
- `firecrab-api/src/handlers/bootstrap.rs`의 `pick_builder_source`:
  이번 설계가 대체하는 기존 함수
