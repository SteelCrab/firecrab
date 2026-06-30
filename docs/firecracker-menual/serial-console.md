# MicroVM Serial Console

Firecracker MicroVM을 부팅하고 Host 터미널을 guest `ttyS0` Serial Console에 연결한다.

## 준비

Firecracker 실행 파일을 설치하고 버전을 확인한다.

```sh
./scripts/firecracker-menual/install-firecracker.sh
firecracker --version
```

커널 이미지와 Ubuntu Base rootfs를 준비한다.

```sh
./scripts/firecracker-menual/install-linux-kernel.sh
./scripts/firecracker-menual/install-ubuntu-roofs.sh
```

Firecracker 실행 파일과 KVM 접근 권한이 필요하다.

```sh
test -r /dev/kvm
test -w /dev/kvm
```

## 실행

Host 터미널에서 실행한다.

```sh
./scripts/firecracker-menual/serial-console.sh
```

스크립트는 기본 커널 이미지와 `./images/rootfs/ubuntu-rootfs.ext4`를 사용해 MicroVM을 부팅한다.
부팅 후 root shell prompt가 보이면 guest shell에 접속된 상태다.
기본 rootfs는 `ttyS0` serial getty에 root autologin을 설정하므로 `Ctrl+C` 같은 TTY interrupt가 정상 동작한다.

## 실행 흐름

1. `scripts/firecracker-menual/serial-console.sh`
   - Host 터미널에서 실행한다.

2. `scripts/firecracker-menual/run-serial-shell.sh`
   - 기본 kernel/rootfs를 정한다.

3. `scripts/firecracker-menual/boot-microvm.sh`
   - Firecracker를 실행하고 `.log`를 남긴다.

## 관련 문서

- [네트워크 기본](network-basic.md)

## 종료

guest shell에서 종료한다.

```sh
reboot -f
```

`reboot`가 없는 emergency shell에서는 procfs를 올린 뒤 kernel sysrq로 재부팅할 수 있다.

```sh
mount -t proc proc /proc
echo b > /proc/sysrq-trigger
```

## 로그

Serial Console 출력은 실행할 때마다 다음 파일에 남는다.

```text
firecracker-console.log
```
