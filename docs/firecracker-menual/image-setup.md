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
```

커널 이미지는 `vmlinux-<kernel-version>-<arch>` 형식의 버전 파일로 저장합니다.
`images/rootfs/ubuntu-rootfs.ext4`는 버전이 포함된 Ubuntu rootfs 이미지로 향하는 심볼릭 링크입니다.

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
