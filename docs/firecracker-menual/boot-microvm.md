# boot-microvm

`scripts/boot-microvm.sh`는 커널 이미지와 rootfs 이미지를 받아 Firecracker MicroVM을 부팅하는 실행 스크립트다.

## 사용

```sh
./scripts/boot-microvm.sh \
  ./images/kernel/vmlinux-<kernel-version>-<arch> \
  ./images/rootfs/ubuntu-rootfs.ext4
```

## 핵심 기능

- 입력 파일 검증: kernel image, rootfs image
- KVM 접근 확인: `/dev/kvm`
- Firecracker 설정 생성: `firecracker-config.json`
- 콘솔 로그 저장: `firecracker-console.log`
- 부팅 로그 검증: kernel panic, rootfs mount 실패, userspace 진입 여부

## 기본 설정

스크립트는 실행할 때마다 `firecracker-config.json`과 `firecracker-console.log`를 다시 만든다.
MicroVM은 vCPU 1개, 메모리 512MiB, writable rootfs로 실행한다.

## 기본 장치

```text
kernel image -> MicroVM kernel
rootfs image -> /dev/vda
serial console -> firecracker-console.log
```

Host에 `tap0`가 있으면 실행 시 자동으로 MicroVM 네트워크 장치에 연결한다.

```sh
sudo ip tuntap add dev tap0 mode tap user "$(id -un)"
sudo ip addr replace 172.16.20.1/24 dev tap0
sudo ip link set tap0 up

./scripts/boot-microvm.sh \
  ./images/kernel/vmlinux-<kernel-version>-<arch> \
  ./images/rootfs/ubuntu-rootfs.ext4
```
