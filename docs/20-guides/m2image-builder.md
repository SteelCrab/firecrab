---
tags:
  - firecrab
  - guide
  - m2image
updated: 2026-08-02
---

# M2Image-Packer — 템플릿 빌드와 패키징

`M2Image-Packer`는 Ubuntu/Alpine/Rocky Linux의 공식 입력으로 Firecracker용 kernel·rootfs를 만든 뒤,
대시보드 설치용 `.tar.zst` 패키지와 `SHA256SUMS`를 생성한다. API 서비스에서 실행하지 않고,
빌드 호스트 또는 CI에서만 실행한다.

## 한 번에 빌드하기

깨끗한 워크트리의 x86_64 Linux 빌드 호스트에서 실행한다.

```sh
./scripts/build-m2images.sh
```

이 명령은 다음 순서와 결과를 고정한다.

1. Alpine 3.24: 공식 minirootfs·`linux-virt` → `images/kernel/`, `images/rootfs/`
2. Ubuntu 26.04: 공식 Ubuntu Base·`linux-image-generic` → `images/kernel/`, `images/rootfs/`
3. Rocky Linux 9: 공식 Rocky BaseOS/AppStream·EL9 `kernel`·dracut initramfs → `images/kernel/`, `images/rootfs/`
4. `package-m2images.sh` → `dist/m2images/alpine-3.24.tar.zst`, `ubuntu-26.04.tar.zst`, `rocky-9.tar.zst`
5. `dist/m2images/SHA256SUMS` 생성 및 `sha256sum -c` 검증

Ubuntu 빌드는 임시 chroot를 쓰므로 중간에 `sudo` 암호를 요청한다. Alpine과 Rocky 빌드는 Docker를
사용한다. 세 배포판 모두 호스트에 등록된 로컬 이미지나 `firecrab-api` 프로세스에서 빌드하지
않는다.

개별 템플릿만 다시 만들 때는 다음처럼 범위를 좁힐 수 있다.

```sh
./scripts/build-m2images.sh --alias alpine-3.24
./scripts/build-m2images.sh --alias ubuntu-26.04
./scripts/build-m2images.sh --alias rocky-9
```

## 결과 확인과 게시 입력

성공한 빌드는 아래처럼 다시 확인할 수 있다.

```sh
sha256sum -c dist/m2images/SHA256SUMS
tar --list --zstd --file dist/m2images/alpine-3.24.tar.zst
tar --list --zstd --file dist/m2images/ubuntu-26.04.tar.zst
tar --list --zstd --file dist/m2images/rocky-9.tar.zst
```

`dist/m2images/` 전체와 `SHA256SUMS`가 M2Image 레지스트리의 게시 입력이다. 패키지 내부에는
`kernel/`·`rootfs/` 상대 경로만 들어가며, API는 `FIRECRAB_IMAGE_BASE_URL`의
`{alias}.tar.zst`를 **M2Image-Packer의 패키지 설치**에서 내려받아 구조를 확인하고 로컬에
준비한다. 이어서 **M2Image-Store의 이미지 가져오기**가 준비된 로컬 패키지를 풀어 아티팩트를
검증·등록한다. 이미지를 삭제해도
준비된 패키지는 남으므로 다시 설치할 때 재다운로드하지 않는다. 업스트림 Ubuntu/Alpine URL을
Rocky URL을 install URL로 직접 설정하면 안 된다.

다음 단계인 고정 HTTP/GitHub Release 게시과 빈 호스트 설치 절차는
[M2Image 레지스트리 태스크](../30-tasks/task-m2image-registry.md)를 따른다.

> [!note]
> 설치된 템플릿에 패키지를 얹어 파생 이미지를 굽던 웹 빌드
> (Images 화면의 "빌드" · "+ 새 이미지 빌드" 버튼과
> `POST /api/images/{alias}/build` 계열 엔드포인트)는 제거되었다.
> 웹에서 이미지를 만드는 경로는 아래 **배포판 부트스트랩** 하나다.

## 웹에서 배포판 부트스트랩

`build-m2images.sh`가 하던 일(공식 소스로부터 배포판을 처음부터 준비)을
docker나 sudo 없이 웹에서 트리거할 수 있다 — builder microVM 안에서
공식 base를 내려받아 chroot로 들어가 그 배포판 자신의 패키지 매니저로
패키지·커널을 설치하고, `mkfs.ext4 -d`로 완성된 rootfs를 만든다.

1. Images 화면에서 "배포판 부트스트랩" 아래 원하는 alias 클릭
2. 이미 설치된 임의의 템플릿으로 builder VM이 뜨고, 콘솔에서 부트스트랩
   스크립트가 실행됨. 세션 로그는 단계 단위로 갱신된다 — VM 부팅 완료,
   실행 중 진행 표시(1분 간격), 스크립트 종료 시 출력 전체. 게스트 콘솔
   출력이 그대로 흐르는 실시간 스트림은 아니다.
3. 완료되면 `images/.packages/{alias}.tar.zst`가 생기고, 해당 행에
   "로컬 패키지 설치" 버튼이 나타난다. 이 버튼은 원격 다운로드 없이
   방금 만든 로컬 패키지를 그대로 설치한다 — `FIRECRAB_IMAGE_BASE_URL`이
   설정되지 않은 호스트에서도 동작한다(기존 "가져오기" 버튼은 그 환경
   변수가 있을 때만 나타나므로 부트스트랩 결과에는 쓰이지 않는다).

부트스트랩은 builder VM을 중지한 뒤에야 디스크에서 결과물을 꺼낸다 —
실행 중인 게스트가 아직 쓰기 중인 ext4를 읽으면 잘린 rootfs가 정상
패키지처럼 게시될 수 있기 때문이다. 성공/실패/취소 어느 경로든 builder
VM은 정리된다.

동시에 하나의 부트스트랩만 진행할 수 있다. `rocky-9` 부트스트랩은
`dnf`가 필요해 builder VM 자체가 이미 `rocky-9`여야 한다 — 나머지
alpine-3.24/ubuntu-26.04는 이미 설치된 아무 템플릿에서나 부트스트랩
가능하다. 완전히 새로운 배포판(현재 3개 외)을 추가하는 것은 여전히 이
문서 위쪽의 CLI 경로를 쓴다.
