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
| `alpine-3.24` | Alpine 공식 `netboot/vmlinuz-virt` + `netboot/initramfs-virt` (`dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/netboot/`) | `apk`까지 내장된 완전한 최소 환경. 자기 자신을 자기 자신으로 부트스트랩 |
| `ubuntu-26.04` | 위 Alpine MicroBoot를 그대로 재사용 | outer(빌더) 환경엔 애초에 `apt`가 필요 없다 — `ubuntu-base-*.tar.gz` 자체에 apt가 내장돼 있고, outer는 `tar`/`mount`/`chroot`만 있으면 된다(기존 스크립트 분석과 동일한 결론) |
| `rocky-9` | Rocky 공식 Container-Base 타르볼(`Rocky-9-Container-Base.latest.x86_64.tar.xz`, `dl.rockylinux.org/pub/rocky/9/images/x86_64/`) + 부팅 가능한 임의 커널 | outer 환경에 실제 동작하는 `dnf`/`rpm`이 필요(`dnf --installroot` 방식) — Container-Base는 `docker pull rockylinux:9`의 원본이라 dnf가 실제로 동작함을 이미 확인. 커널은 Rocky 전용일 필요 없음(dnf는 커널과 무관) |

Alpine netboot 아카이브에는 `modloop-virt`(커널 모듈 로그백 이미지)도
포함돼 있다 — virt 커널이 일부 드라이버를 모듈로 유지하기 때문이다.
MicroBoot 부팅에 `modloop-virt`가 실제로 필요한지(초기 부팅에 필요한
virtio_blk/virtio_net이 이미 커널에 내장인지, 모듈인지)는 구현 플랜에서
직접 부팅 검증으로 확정한다.

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
       kernel/initrd = MicroBoot 아티팩트, disk = 결과물을 담을
       새 빈 디스크(기존처럼 소스 템플릿의 디스크를 쓰는 게 아님)
  │
  ▼
(이하 2026-08-03 설계와 완전히 동일 — 변경 없음)
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

### 빈 디스크 생성

MicroBoot는 자기 자신의 rootfs 디스크가 없다 — 위 방식으로 등록하더라도
그 "템플릿"의 rootfs 아티팩트는 (기존 배포판 rootfs처럼 이미 채워진
파일이 아니라) 빈 ext4 이미지여야 한다. 결과물을 담을 그 빈 디스크
자체를 MicroBoot 등록 시점에 한 번 만들어두거나, 빌더 VM 생성 때마다
새로 만든다 — 크기 산정은 기존 `bootstrap_disk_gb` 로직을 그대로 쓰되,
이제 "소스 템플릿의 디스크 크기"라는 하한 기준 자체가 사라지므로
(MicroBoot엔 원래 disk 내용이 없음) 대상 alias 기준 크기만 남는다.

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
- 빈 디스크 생성 경로: 기존 `create_vm`류 테스트 패턴 재사용.
- 3개 배포판 각각 "설치된 템플릿 0개" 상태에서 웹 부트스트랩 →
  가져오기 → VM 생성까지 end-to-end 성공은 수동 검증(실제 네트워크
  필요, 2026-08-03 설계와 동일한 한계).

## 완료 기준 (MVP)

- [ ] alias별 MicroBoot 소스 고정 매핑 + 다운로드/캐시/체크섬 검증
- [ ] `pick_builder_source`를 MicroBoot 기반으로 교체(더 이상 설치된
      템플릿을 요구하지 않음)
- [ ] 빌더 VM용 빈 디스크 생성 경로 추가
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
