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
| `rocky-9` | 위 Alpine MicroBoot를 그대로 재사용 | dnf는 커널과 무관하지만 **libc와는 무관하지 않다** — Rocky Container-Base(`Rocky-9-Container-Base.latest.x86_64.tar.xz`, `dl.rockylinux.org/pub/rocky/9/images/x86_64/`)의 dnf/rpm은 glibc 바이너리라, musl 기반 Alpine 셸에서 직접 실행할 수 없다. 그래서 outer(Alpine) 셸은 그 아카이브를 받아 풀고 **그 안으로 chroot한 다음** dnf를 돌린다 — 지금 `install-rocky-rootfs.sh`/`bootstrap-rocky-in-guest.sh`가 이미 하는 구조(먼저 chroot, 그 다음 `dnf --installroot`) 그대로라 별도 Rocky 전용 커널이 필요 없다 |

### 실제 부팅 검증 (2026-08-05, 구현 착수 전 직접 확인)

원래 이 설계는 "Alpine netboot 자체가 apk까지 내장된 완전한 환경"이라고
가정했는데, 실제로 Firecracker에 `vmlinux-virt`(`extract-vmlinux`로 ELF
추출)+`initramfs-virt`만 붙여 여러 조합으로 부팅해본 결과 **원래
가정은 틀렸지만, 훨씬 깔끔한 대안을 실제로 찾아 검증했다**:

- Alpine 자신의 `/init`(Alpine Init 3.14.0-r0, 소스 직접 읽음)은
  부팅 후 root 마운트("Mounting root")를 시도하고, 실패하면 자기 안에
  `recovery_shell()`이라는 **정식 비상 셸 기능**이 있다: `$KOPT_panic`
  (커널 인자 `panic=`에서 채워짐)이 비어 있으면
  `echo "Launching initramfs emergency recovery shell." && /bin/busybox sh`
  로 떨어지고, `panic=`이 설정돼 있으면 그 셸을 건너뛰고 진짜 커널
  패닉으로 죽는다. **처음 실패한 시도들은 전부 기존 템플릿의 관행대로
  `panic=1`을 그대로 복사해 넣은 게 원인이었다** — `panic=`을 아예
  빼면(`console=ttyS0 reboot=k`만) 이 정식 복구 셸로 들어간다.
- 이 정식 복구 셸에서 확인된 것(**전부 직접 검증 완료**):
  - `/proc`, `/sys`, `/dev`가 **이미 마운트돼 있다** (Alpine 자신의
    `/init`이 root 마운트를 시도하기 전에 이미 해놓은 일이라서).
  - `PATH`가 정상적으로 설정돼 있다(`/sbin:/usr/sbin:/bin:/usr/bin`류).
  - `busybox`(v1.37.0, multi-call) 전체와 `apk`(`apk-tools 3.0.6-r0`)가
    정상 동작. `chroot`/`tar`/`wget`/`mount`/`umount`도 전부 `which`로
    확인됨.
  - **`mkfs.ext4`는 없다** — e2fsprogs가 이 initramfs엔 안 들어있다
    (`/lib/modules/<커널버전>` 자체도 사실상 비어 있어서 `ext4`
    커널 모듈도 없다 — Alpine의 실제 라이브 미디어는 이 둘을 별도
    `modloop-virt`/네트워크로 채워 넣는데, 우리는 그 경로를 안 씀).
  - `/sys/class/net`엔 `lo`만 있었다 — 다만 이건 테스트에 네트워크
    인터페이스 자체를 안 붙여서고(TAP 설정까지 재현하진 않았다),
    실제 빌더 VM은 오늘 코드가 이미 모든 부트스트랩 VM에 네트워크를
    붙여준다(`builder_micro_network_id`) — 이 부분은 실제 구현
    태스크에서 진짜 네트워크로 검증한다.
- 결론: 게스트 부트스트랩 스크립트가 콘솔로 받는 첫 명령에서
  `apk add e2fsprogs`(및 필요하면 `curl`)만 설치하면, 이후로는 기존
  스크립트가 정상 설치된 Alpine 위에서 동작하던 것과 사실상 동일한
  환경이 된다 — `/proc`·`/sys`·`/dev` 수동 마운트나 `busybox --install`
  같은 준비 단계는 **필요 없다**(이전 초안에 있었지만 이번 검증으로
  불필요함이 확인돼 삭제).
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
       kernel/initrd = MicroBoot 아티팩트, boot_args에서 panic=을
       빼서 Alpine 자신의 정식 복구 셸로 진입(디스크는 root 아님,
       자리표시자 스크래치 파일)
  │
  ▼
[신규] 콘솔로 apk add e2fsprogs 실행(mkfs.ext4 확보) — 그 외
       /proc·/sys·/dev·PATH는 이미 준비돼 있어 추가 조치 불필요
  │
  ▼
콘솔로 부트스트랩 스크립트 실행(기존과 동일한 다운로드/chroot/설치
로직, 최종 mkfs.ext4 -F -d 대상 경로만 /dev/vda로 변경)
  │
  ▼
[변경] 패키징: MicroBoot 세션은 dump_from_image 생략 — 디스크
       제너레이션 파일 자체가 이미 완성된 rootfs.ext4라 그대로 복사
  │
  ▼
(이하 2026-08-03 설계와 완전히 동일 — 변경 없음)
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

### 부팅 방식 — 디스크가 아니라 Alpine의 정식 복구 셸이 진입점

**중요한 정정**: 기존 모든 템플릿은 `root=/dev/vda` 방식으로, 템플릿의
rootfs 아티팩트가 그대로 커널이 마운트하는 `/`가 된다. MicroBoot는 그
방식을 안 쓴다 — `boot_args`에서 `panic=`을 **빼서**(`console=ttyS0
reboot=k`만 유지) Alpine의 `/init`이 root 마운트에 실패했을 때 자기
안의 정식 `recovery_shell()`로 떨어지게 한다(위 "실제 부팅 검증" 참고).
붙어 있는 디스크(`/dev/vda`)는 이 과정에서 root로 마운트되지 않고 그냥
존재만 한다. `create_vm`/`FirecrackerConfig`(Rust) 자체는 안 바뀐다 —
`is_root_device` 플래그는 Firecracker 쪽 장치 나열 순서 문제일 뿐,
커널이 실제로 뭘 root로 삼는지는 전적으로 `boot_args`가 결정한다.

### 스크래치 디스크 (구 "빈 디스크") — 마운트 없이, 블록 디바이스로 직접

디스크가 root가 아니게 됐으니, 그 역할은 순수히 **최종 결과물을 담아
host가 나중에 꺼낼 수 있게 하는 저장소**다. 다운로드·chroot 설치 같은
중간 작업 자체는 recovery 셸의 root(RAM 기반 tmpfs)에서 그냥 진행해도
되고, 마지막에 이 저장소 원본이 필요할 뿐이다.

이 코드베이스는 이미 "마운트 없이 ext4 다루기" 철학을 곳곳에 쓴다
(`rootfs.rs`의 `debugfs` 기반 조작, `dump_from_image` 등, 2026-08-03
설계가 명시한 원칙). 여기서도 그대로 적용된다: 기존 스크립트가 이미
하던 마지막 단계 `mkfs.ext4 -F -d <staging> <출력파일>`을, **출력
경로만 일반 파일이 아니라 `/dev/vda`(블록 디바이스)로 바꾸면 끝이다**
— `mkfs.ext4`는 대상이 파일이든 블록 디바이스든 구분하지 않는다. 별도
포맷+마운트 단계 자체가 필요 없다.

MicroBoot로 등록하는 "템플릿"의 rootfs 아티팩트는 (기존 배포판
rootfs처럼 이미 채워진 파일이 아니라) 실제로는 아무 내용이나 상관없는
**크기만 맞춘 자리표시자 파일**이다 — 어차피 게스트가 `mkfs.ext4 -F`로
통째로 덮어쓴다(`-F`가 기존 파일시스템 흔적을 무시하고 강제 생성).
크기 산정은 기존 `bootstrap_disk_gb` 로직을 그대로 쓰되, 이제 "소스
템플릿의 디스크 크기"라는 하한 기준 자체가 사라지므로(MicroBoot엔
원래 disk 내용이 없음) 대상 alias 기준 크기만 남는다.

### 게스트 스크립트에 필요한 준비 단계 — apk로 e2fsprogs 설치뿐

위 "실제 부팅 검증"에서 확인했듯, Alpine의 정식 복구 셸은
`/proc`·`/sys`·`/dev` 마운트와 `PATH` 설정이 이미 돼 있다 —
수동으로 갖출 게 없다. 딱 하나 빠진 것: **e2fsprogs(`mkfs.ext4`)가
이 initramfs엔 없다.** 3개 `bootstrap-{alpine,ubuntu,rocky}-in-guest.sh`
스크립트 맨 앞에 필요한 준비는 이것 하나뿐이다:

```sh
apk add --no-cache e2fsprogs
```

(네트워크가 필요하므로, 빌더 VM에 이미 붙는 네트워크가 이 시점에
살아있는지는 실제 구현 태스크에서 진짜 부팅으로 검증한다 — 위 "실제
부팅 검증"의 마지막 항목 참고.)

기존 스크립트의 핵심 로직(다운로드/chroot/설치)은 안 바뀐다. 최종
이미지 생성 한 줄(`mkfs.ext4 -F -d <staging> <경로>`)의 `<경로>`만
`/dev/vda`로 바뀐다.

**패키징 단계의 중요한 분기**: 기존 흐름은 게스트 디스크 **안의 특정
파일 하나**를 `dump_from_image`(debugfs)로 꺼낸다 — 디스크 자체는
게스트의 전체 OS이고, 그 안 어딘가에 결과물 파일이 있기 때문이다.
MicroBoot 흐름은 다르다: `mkfs.ext4 -F -d <staging> /dev/vda`는
디스크 **전체를 통째로** 목표 rootfs로 덮어쓰므로, host 쪽의
디스크-제너레이션 파일(`artifact_paths.rootfs(disk_generation)`)
자체가 이미 완성된 `rootfs.ext4`다 — `dump_from_image` 호출이 아예
필요 없고, 그 파일을 그대로 복사/rename해서 패키징하면 된다.
`package_bootstrap`이 이 세션의 소스가 MicroBoot였는지 판단해 이
경로로 분기해야 한다 — 정확한 판단 방법(세션에 플래그를 두는 등)은
구현 플랜에서 정한다.

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
- [ ] 빌더 VM용 자리표시자 스크래치 디스크 생성 경로 추가(`panic=`
      없는 `boot_args`로 root 아닌 Alpine 복구 셸 진입)
- [ ] 3개 게스트 스크립트 맨 앞에 `apk add --no-cache e2fsprogs` 추가,
      최종 `mkfs.ext4 -F -d <staging> <경로>`의 경로를 `/dev/vda`로 변경
- [ ] `package_bootstrap`이 MicroBoot 세션에서 `dump_from_image`를
      생략하고 디스크 제너레이션 파일을 그대로 패키징하도록 분기
- [ ] 설치된 템플릿 0개인 상태에서 `alpine-3.24` 웹 부트스트랩 성공
      (수동 검증, 실제 네트워크 붙은 빌더 VM으로)
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
