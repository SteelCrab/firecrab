# MicroVM 이미지 준비

이 문서는 Firecracker MicroVM 부팅에 필요한 이미지 파일의 위치와 준비 방법을 정리합니다.

## 산출물

MicroVM 부팅에 사용할 파일은 `images/` 아래에 둡니다.

```text
images/
  kernel/
    vmlinux-<kernel-version>-<arch>
  rootfs/
    ubuntu-rootfs.ext4
    ubuntu-rootfs-26.04-amd64.ext4
    alpine-rootfs.ext4
    alpine-rootfs-<alpine-version>-<arch>.ext4
```

커널 이미지는 `vmlinux-<kernel-version>-<arch>` 형식의 버전 파일로 저장합니다.
`images/rootfs/ubuntu-rootfs.ext4`, `images/rootfs/alpine-rootfs.ext4`는 각각 버전이 포함된 rootfs
이미지로 향하는 심볼릭 링크입니다. 두 템플릿 모두 같은 커널 파일을 공유합니다(커널은 virtio/ext4/serial
지원만 있으면 되는 distro-agnostic 산출물이라 템플릿마다 새로 빌드할 필요가 없습니다).

## 커널 준비

현재 호스트 아키텍처에 맞는 최신 안정 버전 Linux 커널 이미지를 빌드합니다.

```sh
./scripts/firecracker-menual/install-linux-kernel.sh
```

스크립트는 다음 파일을 생성합니다.

```text
images/kernel/vmlinux-<kernel-version>-<arch>
```

버전이 포함된 커널 이미지가 실제 부팅 파일입니다.
Firecracker에 넘기는 커널 이미지는 Linux 소스 디렉터리가 아니라 실행 가능한 `vmlinux` 파일이어야 합니다.

## RootFS 준비

기본 부팅 가능한 Ubuntu rootfs 이미지를 생성합니다.

```sh
./scripts/firecracker-menual/install-ubuntu-roofs.sh
```

rootfs에는 정상 직렬 콘솔 부팅에 필요한 `systemd`, `systemd-sysv`, `udev`, `kmod`, `util-linux`와 기본 네트워크 도구가 포함됩니다.

스크립트는 다음 파일을 생성합니다.

```text
images/rootfs/ubuntu-rootfs.ext4
images/rootfs/ubuntu-rootfs-<ubuntu-series>-<arch>.ext4
```

버전이 포함된 rootfs 이미지가 실제 파일이고, `ubuntu-rootfs.ext4`는 편의용 심볼릭 링크입니다.
이미 rootfs 이미지가 있으면 스크립트가 다시 생성합니다.
Ubuntu rootfs 스크립트는 현재 호스트 아키텍처에 맞는 아카이브가 있는 최신 Ubuntu Base 릴리스를 선택합니다.

두 번째 템플릿으로 Alpine Linux rootfs 이미지를 생성합니다.

```sh
./scripts/firecracker-menual/install-alpine-rootfs.sh
```

Ubuntu 스크립트와 달리 host root/sudo가 필요 없습니다 — `images/rootfs/`가 이전 실행의 sudo로 인해
root 소유라 일반 계정은 쓸 수 없는데, 이 스크립트는 Docker 컨테이너를 root로 띄워 `apk --root`로
staging을 구성하고 같은 컨테이너 안에서 ext4 이미지까지 만들기 때문입니다(Docker 데몬 접근 권한만
있으면 됩니다). 최신 stable Alpine 브랜치를 자동으로 찾아 minirootfs 아카이브를 sha256 검증 후
내려받습니다.

스크립트는 다음 파일을 생성합니다.

```text
images/rootfs/alpine-rootfs.ext4
images/rootfs/alpine-rootfs-<alpine-version>-<arch>.ext4
```

## MicroVM 부팅

준비한 커널 이미지와 rootfs 이미지를 사용해 MicroVM을 부팅합니다.

```sh
./scripts/firecracker-menual/boot-microvm.sh ./images/kernel/vmlinux-<kernel-version>-<arch> ./images/rootfs/ubuntu-rootfs.ext4
```

실행 스크립트는 다음 파일을 생성합니다.

```text
firecracker-config.json
firecracker-console.log
```

## 부팅 로그 확인

콘솔 로그를 확인합니다.

```sh
tail -n 80 firecracker-console.log
```

다음 메시지가 출력되면 MicroVM 부팅이 성공한 상태입니다.

```text
[INFO] MicroVM boot completed successfully.
```
