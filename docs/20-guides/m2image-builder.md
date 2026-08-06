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
