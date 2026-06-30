# 실행 로그 캡처

이 문서는 Firecracker MicroVM 실행 후 생성된 Serial Console 로그를 기준으로 부팅 로그, 실행 로그, 에러 로그를 확인한 결과입니다.

## 캡처 대상

```text
firecracker-console.log
firecracker-config.json
```

캡처 시점의 설정 파일은 다음 이미지를 사용했습니다.

```text
kernel_image_path: <repo>/images/kernel/vmlinux-7.1.2-x86_64
rootfs path:       <repo>/images/rootfs/ubuntu-rootfs.ext4
```

## 실행 로그

Firecracker 프로세스가 실행되고 API socket을 연 뒤, JSON 설정으로 MicroVM을 시작했습니다.

```text
2026-06-30T03:37:24.620677576 [anonymous-instance:main] Running Firecracker v1.16.0
2026-06-30T03:37:24.620780281 [anonymous-instance:main] Listening on API socket ("/tmp/firecracker.socket").
2026-06-30T03:37:24.620899257 [anonymous-instance:fc_api] API server started.
2026-06-30T03:37:24.633568612 [anonymous-instance:main] Successfully started microvm that was configured from one single json
```

Firecracker 프로세스 자체는 정상 종료했습니다.

```text
2026-06-30T03:37:26.348608511 [anonymous-instance:main] Firecracker exiting successfully. exit_code=0
```

## 부팅 로그

Serial Console을 통해 게스트 커널 로그가 출력되었습니다.

```text
[    0.000000] Linux version 7.1.2 (builder@host) (gcc (Ubuntu 15.2.0-4ubuntu4) 15.2.0, GNU ld (GNU Binutils for Ubuntu) 2.45) #1 SMP PREEMPT_DYNAMIC Tue Jun 30 01:40:39 KST 2026
[    0.000000] Command line: console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw pci=off root=/dev/vda rw virtio_mmio.device=4K@0xc0001000:5
[    0.000000] Hypervisor detected: KVM
[    0.017164] printk: legacy console [ttyS0] enabled
[    0.102678] 00:00: ttyS0 at I/O 0x3f8 (irq = 27, base_baud = 115200) is a 16550A
```

따라서 Serial Console 로그 캡처는 가능한 상태입니다.

## 에러 로그

이번 캡처에서는 게스트가 userspace까지 정상 진입하지 못했습니다. root block device를 열지 못해 커널 패닉이 발생했습니다.

```text
[    0.620710] /dev/root: Can't open blockdev
[    0.620856] VFS: Cannot open root device "/dev/vda" or unknown-block(0,0): error -6
[    0.621923] Kernel panic - not syncing: VFS: Unable to mount root fs on unknown-block(0,0)
```

Firecracker 쪽에는 MMIO/IO 접근 실패 로그도 함께 기록되었습니다.

```text
2026-06-30T03:37:24.645211925 [anonymous-instance:fc_vcpu 0] vcpu: IO write @ 0xcf8:0x4 failed: bus_error: MissingAddressRange
2026-06-30T03:37:24.645224420 [anonymous-instance:fc_vcpu 0] vcpu: IO read @ 0xcfc:0x2 failed: bus_error: MissingAddressRange
```

## 판정

Serial Console 로그 확인은 가능합니다.

다만 이 캡처의 MicroVM은 정상 부팅 완료 상태가 아닙니다. Firecracker 프로세스는 `exit_code=0`으로 종료했지만, 게스트 커널은 rootfs mount 실패로 `Kernel panic`에 도달했습니다.

이후 실행은 `scripts/firecracker-menual/boot-microvm.sh`에서 커널 패닉과 rootfs mount 실패를 성공으로 오판하지 않도록 검증 로직을 보강했습니다.

현재 `artifacts/` 아래의 이전 커널 빌드는 사용하지 않습니다. root block device 인식에 필요한 `CONFIG_VIRTIO_MMIO`가 포함된 커널은 `images/kernel/vmlinux-<kernel-version>-<arch>` 버전 파일로 빌드합니다.
