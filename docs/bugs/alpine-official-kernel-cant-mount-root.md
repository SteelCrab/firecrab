# Alpine의 공식 linux-virt 커널로 바꾸니 root 마운트가 실패한다

`task-distro-standard-kernels.md`(자체 빌드 vanilla 커널 대신 각 배포판의 공식 커널을 쓰도록
전환) 작업 중 발견. Ubuntu는 문제없이 바로 부팅됐는데, Alpine만 매번 커널 패닉으로 끝났다.

## 증상

```
virtio_blk virtio0: 1/0/0 default/read/poll queues
virtio_blk virtio0: [vda] 4194304 512-byte logical blocks (2.15 GB/2.00 GiB)
mount: mounting /dev/vda on /sysroot failed: No such file or directory
Kernel panic - not syncing: Attempted to kill init! exitcode=0x00000000
```

`/dev/vda`는 실제로 존재하고(`virtio_blk`가 정상적으로 디스크를 인식함), `/sysroot`도 존재하는데
`mount`는 계속 "No such file or directory"로 실패했다.

## 잘못 짚었던 원인: virtio-mmio 이중 등록

처음엔 콘솔에 찍힌 이 메시지 때문에 ACPI 기반/커맨드라인 기반 virtio-mmio 장치 등록이 충돌하는
줄 알았다:

```
virtio-mmio virtio-mmio.0: error -EBUSY: can't request region for resource [mem 0xc0001000-0xc0001fff]
virtio-mmio virtio-mmio.1: error -EBUSY: can't request region for resource [mem 0xc0002000-0xc0002fff]
```

`boot_args`에 `acpi=off`를 넣고 firecracker를 직접(API 안 거치고) 실행해서 확인해보니 — network
interface 없이 diskonly로 붙였을 때(mmio 장치가 1개뿐일 때)는 EBUSY가 아예 안 뜨는데도 **root
마운트는 똑같이 실패**했다. 즉 EBUSY는 disk+network 두 장치를 동시에 붙일 때 나타나는 별개
증상이고(원인 미상, 실제 부팅에는 영향 없음 — disk 자체는 정상 attach됨), 진짜 원인이 아니었다.

## 진짜 원인: `ext4` 커널 모듈이 로드되기 전에 `mount`가 먼저 시도됨

`panic=1`을 빼고 수동으로 firecracker를 띄워 Alpine의 initramfs 복구 셸(emergency shell)에
직접 들어가 확인했다:

```sh
~ # ls -la /dev/        # /dev/vda 존재 확인됨
~ # mkdir -p /sysroot   # 이미 존재
~ # mount /dev/vda /sysroot
mount: mounting /dev/vda on /sysroot failed: No such file or directory   # 여전히 실패

~ # modprobe ext4
~ # mount -t ext4 /dev/vda /sysroot
EXT4-fs (vda): mounted filesystem ... r/w with ordered data mode.        # 성공!
```

Alpine 공식 커널(`linux-virt`)은 virtio_blk와 마찬가지로 **ext4도 모듈**(`=m`)로 빌드돼 있다.
mkinitfs가 생성한 `/init` 스크립트는 root 마운트 시 `rootfstype`(커널 커맨드라인의
`rootfstype=` 값)이 없으면 `mount`에 `-t` 없이 호출하는데, 이 상태에서 `mount`는 파일시스템
타입을 자동 인식하지 못한다 — ext4 모듈이 아직 로드 안 됐으니 커널이 "이런 타입 압니다"라고
답할 수 없고, 결과적으로 실제 원인과 무관한 "No such file or directory"라는 오해하기 쉬운
에러로 실패한다. `-t ext4`로 명시하면 커널의 온디맨드 모듈 로딩(`request_module`)이 자동으로
ext4.ko를 불러와 정상 동작한다.

자체 빌드 vanilla 커널(`install-linux-kernel.sh`)은 이 문제가 아예 없었다 — virtio_blk/ext4를
전부 커널에 빌트인(`=y`)으로 고정해뒀기 때문에 모듈 로딩 자체가 필요 없었다.

## 수정

`firecrab-api/src/templates.rs`의 Alpine `boot_args`에 `rootfstype=ext4` 추가:

```
console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rootfstype=ext4 rw
```

## 검증

수정 전: `rootfstype=ext4` 없이 100% 재현(3회 연속 동일 패닉). 수정 후: 실제 API로 Alpine VM
생성·시작 → `running` 도달, 실제 IP로 ping 0% 손실 확인. Ubuntu는 애초에 root=ext4 지원이
빌트인이라 이 문제 자체가 없었음(그대로 무변경).

## 산출물

`firecrab-api/src/templates.rs`(boot_args 한 줄) — 코드 변경은 이게 전부, 나머지는 진단
과정(수동 firecracker 실행, initramfs 복구 셸 직접 조작)이었음.
