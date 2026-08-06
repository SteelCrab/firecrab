---
tags:
  - firecrab
  - guide
updated: 2026-08-05
---

# 트러블슈팅

증상별로 찾아보는 문제 해결 모음. 아래 표에서 증상을 찾아 해당 섹션으로 이동한다. 기능별 상세
검증 절차는 `docs/tests/`, 개별 버그의 전체 원인·수정 기록은 `docs/bugs/`에 있다.

> [!tip] 먼저 host 진단
> 실제로 겪은 실패 상당수는 코드가 아니라 **host 설정**(UFW, helper 소켓, 잘못된 cwd/DB)이다.
> 문서를 뒤지기 전에:
>
> ```sh
> ./install.sh --doctor          # 또는 ./scripts/firecrab-doctor.sh
> # 설치 후: firecrab-doctor
> ```
>
> root 불필요. 권한이 필요한 항목은 `[SKIP]`, 문제만 `[FAIL]` + 조치 한 줄.

## 한눈에 보기

| 증상 | 분류 |
|---|---|
| "API 연결 안 됨 — 15s 간격 재시도" | [대시보드/API](#대시보드api) |
| `localhost:8080`에서 안 뜨거나 403 | [대시보드/API](#대시보드api) |
| VM 상세 모달 "불러오는 중…" 계속 | [대시보드/API](#대시보드api) |
| VM 여러 대 동시 시작 시 일부가 starting에서 멈춤 | [VM 생성·시작](#vm-생성시작) |
| 디스크 공간 부족으로 생성/시작이 느리거나 실패 | [VM 생성·시작](#vm-생성시작) |
| "network helper is unavailable" 로 start가 500 | [네트워크](#네트워크) |
| VM은 로그인 셸까지 완전히 부팅되는데 계속 `error` | [네트워크](#네트워크) |
| `FIRECRAB_NETWORK_FAILED no-ipv4-address` | [네트워크](#네트워크) |
| Alpine 템플릿만 매번 `no-ipv4-address`(Ubuntu는 정상) | [네트워크](#네트워크) |
| VM을 여러 대 동시에 시작하면 부팅 극초반에 죽음 | [네트워크](#네트워크) |
| VM 내부에서 새 목적지로 나가는 연결(apt/apk update 등)이 타임아웃 | [네트워크](#네트워크) |
| Alpine이 공식 커널로 바꾸니 부팅 중 커널 패닉("No such file or directory") | [VM 생성·시작](#vm-생성시작) |
| 부트스트랩 빌더 VM이 `Error`로 바로 죽음(`Bad magic number in super-block`) | [이미지 부트스트랩](#이미지-부트스트랩microboot) |
| 부트스트랩 시작이 `File not found by ext2_lookup`로 실패 | [이미지 부트스트랩](#이미지-부트스트랩microboot) |
| 부트스트랩 로그가 heartbeat만 반복하다 30분 타임아웃 | [이미지 부트스트랩](#이미지-부트스트랩microboot) |
| `Unable to lock database` / `curl: not found` | [이미지 부트스트랩](#이미지-부트스트랩microboot) |
| 게스트 스크립트에서 패키지가 전부 `no such package`(DNS 실패) | [이미지 부트스트랩](#이미지-부트스트랩microboot) |
| `mkfs.ext4 -d`가 `No such process`로 실패 | [이미지 부트스트랩](#이미지-부트스트랩microboot) |
| `oom-killer`가 apt-get/dnf를 죽임, `exited with code 137` | [이미지 부트스트랩](#이미지-부트스트랩microboot) |
| `At least NNNMB more space needed on the / filesystem` | [이미지 부트스트랩](#이미지-부트스트랩microboot) |
| 부트스트랩한 이미지로 만든 VM이 dracut emergency shell에 빠짐 | [이미지 부트스트랩](#이미지-부트스트랩microboot) |
| rocky만 1초 만에 `cp: can't stat '/etc/pki/rpm-gpg'` | [이미지 부트스트랩](#이미지-부트스트랩microboot) |
| microboot.rs를 고쳤는데 재시작해도 그대로 | [이미지 부트스트랩](#이미지-부트스트랩microboot) |
| 터미널 "연결 끊김"만 뜨고 안 붙음 | [터미널](#터미널) |
| 터미널 프롬프트에 `;1R;80R;1R;80R...` 반복 | [터미널](#터미널) |

## 대시보드/API

### "API 연결 안 됨 — 15s 간격 재시도"

- **원인**: `firecrab-api`가 죽었거나, 3번 연속 폴링 실패로 15초 간격 재시도 모드로 전환됨
  (`App.tsx`의 `SLOW_POLL_AFTER`/`SLOW_POLL_MILLIS`)
- **해결**: `firecrab-api`가 살아있는지 확인 — 서버가 다시 뜨면 자동으로 3초 폴링으로 복귀

### `localhost:8080`에서 안 뜨거나 API 요청이 403

- **원인**: Origin 불일치로 CORS가 거부함
- **해결**: 꼭 `localhost`로 접속(`127.0.0.1` 아님)

### VM 상세 모달이 계속 "불러오는 중…"

- **원인**: `firecrab-api` 미기동, 또는 VM id가 더 이상 존재하지 않음(삭제됨)
- **해결**: `firecrab-api` 재시작 여부·VM id 존재 여부 확인

## VM 생성·시작

### VM을 여러 대 동시에 시작하면 일부가 "starting"(특히 "디스크 준비")에서 멈춘 채 안 넘어간다

- **원인·수정**: [bugs/vm-startup-stuck-under-concurrent-load.md](../50-bugs/vm-startup-stuck-under-concurrent-load.md) —
  템플릿 재해싱 중복 + 타임아웃된 요청의 future가 drop되며 VM이 고아 상태가 되는 버그, 둘 다 수정됨

### Alpine이 공식 커널로 바꾸니 부팅 중 커널 패닉("No such file or directory")

- **원인·수정**: [bugs/alpine-official-kernel-cant-mount-root.md](../50-bugs/alpine-official-kernel-cant-mount-root.md) —
  Alpine 공식 `linux-virt` 커널은 ext4가 모듈이라, `rootfstype=` 없이 mount를 호출하면 모듈이
  아직 안 실린 상태라 타입을 인식 못 해 실패한다("No such file or directory"는 이 상황을
  오해하기 쉽게 표현한 에러). `boot_args`에 `rootfstype=ext4` 추가로 수정, 수정됨

### 모든 VM이 시작하자마자 `Error` — 개발 모드로 직접 실행할 때

- **원인**: `/dev/kvm` 접근 권한. udev의 `uaccess` 태그(`70-uaccess.rules`)는 **좌석에 연결된
  그래픽 세션 사용자에게만** ACL을 동적으로 부여한다 — SSH·헤드리스·백그라운드 세션에는 안 붙는다.
  `install.sh`의 `ensure_account()`가 kvm 그룹 가입을 처리하지만, 그건 **정식 설치 경로 전용**이고
  `cargo run -p firecrab-api` 개발 모드는 이 처리를 거치지 않는다
- **확인**: `ls -l /dev/kvm` → 그룹이 `kvm`이 아니거나, `id -nG`에 `kvm`이 없으면 이 케이스

```sh
sudo chgrp kvm /dev/kvm && sudo chmod 660 /dev/kvm   # 일시적
sudo usermod -aG kvm "$USER"                          # 영구(재로그인 필요)
```

### 특정 MicroNetwork의 VM만 DHCP를 못 받는다

- **원인**: 그 네트워크의 브리지가 죽어 있다. 붙은 TAP이 하나도 없으면 브리지는 `NO-CARRIER`/`DOWN`
  상태로 남는다 — 코드 문제가 아니라 해당 네트워크의 상태 문제이므로, 다른 정상 네트워크로 바꾸면
  같은 VM이 그대로 뜬다
- **확인**: `ip -br link show type bridge` → 대상 `mnb<hex>`가 `DOWN`이면 이 케이스. UFW가 원인인
  경우와 헷갈리기 쉬우니 [네트워크](#네트워크)의 UFW 항목도 같이 확인

### 이미지 설치가 권한 오류로 실패한다(`images/rootfs`)

- **원인**: `images/rootfs`가 `root:root` 소유로 남아 있으면 비특권 프로세스가 tar를 풀 수 없다.
  과거에 sudo로 스크립트를 돌렸다면 이 상태가 된다
- **해결**: `sudo chown "$USER:$USER" images/rootfs`

### 호스트 디스크가 꽉 차서 VM 생성/시작이 느리거나 실패한다

- **원인**: VM 하나당 디스크(`data/vms/<id>/rootfs.ext4`)가 기본 2GiB — 테스트/재현용으로 VM을
  많이 만들면 금방 쌓인다. ext4는 여유 공간이 임계치 이하로 떨어지면 쓰기 성능이 급격히 나빠진다
- **해결**: `df -h`로 확인하고, 안 쓰는 VM은 `DELETE /api/vms/{id}`로 정리(디스크 파일도 같이 삭제됨)

## 이미지 부트스트랩(MicroBoot)

웹에서 "{alias} 부트스트랩"을 누르면 빌더 VM이 **MicroBoot**(Alpine 공식 netboot 커널+initrd)로
떠서 게스트 스크립트를 돌린다. 설계는
[specs/2026-08-05-m2image-microboot-design.md](../superpowers/specs/2026-08-05-m2image-microboot-design.md).

> [!important] MicroBoot 게스트의 3가지 전제
> 아래 증상 대부분은 이 세 가지에서 파생된다. 새 증상을 만나면 여기부터 의심한다.
>
> 1. **`/`가 ramfs다** — 크기 정보가 없어 `df`에 아예 안 나오고, statvfs가 총량·여유 모두 0을
>    반환한다. 디스크가 아니라 RAM이며, 용량 상한은 VM RAM이다
> 2. **정상 설치된 배포판이 아니다** — Alpine의 비상 복구 셸이라 apk DB도, curl도, 네트워크
>    설정도 없다. busybox 애플릿은 GNU 도구와 옵션이 다르다
> 3. **`/dev/vda`는 root가 아니다** — 스크립트 마지막에 결과물을 통째로 덮어쓸 출력 장치일 뿐,
>    중간 작업 공간이 아니다

### 부트스트랩 로그 읽는 법

- API의 `log` 필드는 콘솔 출력의 **꼬리만** 담는다 — 초반 실패는 여기 안 남는다
- 전체 원본은 host의 `data/vms/<id에서 - 제거>/r/<runtime_id>/console.log`
- 실패하면 빌더 VM이 삭제되며 이 파일도 같이 사라진다. 초반 구간을 봐야 하면 **시작 직후**
  `tail -F`로 미리 잡아둔다

```sh
vmid=$(curl -s -X POST localhost:8080/api/images/rocky-9/bootstrap \
  | grep -o '"vmId":"[^"]*"' | cut -d'"' -f4 | tr -d '-')
tail -F "$(find data/vms/$vmid -name console.log)"
```

### 빌더 VM이 바로 `Error` — `Bad magic number in super-block`

- **원인**: MicroBoot의 자리표시자 rootfs가 실제 ext4가 아님. 모든 VM은 부팅 전에
  `rootfs::prepare_rootfs`의 `grow()`가 `e2fsck -f -y` → `resize2fs`를 돌리므로, 내용은 아무래도
  좋지만 **구조는 진짜 ext4여야 한다**
- **해결**: `microboot.rs`의 `register_blocking()`이 `mkfs.ext4`로 생성하도록 수정, 수정됨

### 부트스트랩 시작이 `File not found by ext2_lookup`로 실패

- **원인**: 매 VM 시작마다 `rootfs::specialize_guest`가 `/etc/hostname`을 `debugfs`로 쓰는데,
  `debugfs`의 `write`는 부모 디렉터리를 만들지 않는다. 갓 `mkfs.ext4`한 이미지엔 `/etc`가 없다
- **해결**: 자리표시자 생성 직후 `debugfs -w -R "mkdir /etc"` 추가, 수정됨

### 로그가 heartbeat만 반복하다 30분 타임아웃(콘솔은 조용함)

- **원인**: `eth0`은 존재하지만(virtio_net은 자동 로드) **administratively DOWN** 상태다. 설치된
  템플릿은 네트워크 관리자가 DHCP 과정에서 링크를 올려주지만 `udhcpc`는 스스로 올리지 않아,
  down 링크에서 패킷을 한 개도 안 보내고 조용히 영원히 대기한다
- **확인**: host에서 해당 TAP을 `sudo tcpdump -i <tap> -n` — 10분간 0 패킷이면 이 케이스
- **해결**: 3개 게스트 스크립트 모두 `udhcpc` 앞에 `ip link set eth0 up` 추가, 수정됨

### `Unable to lock database: No such file or directory` / `curl: not found`

- **원인**: 복구 셸은 정상 설치된 Alpine이 아니라 `/lib/apk/db` 자체가 없다. 그리고 busybox는
  `wget`만 제공하고 `curl`은 없는데 스크립트는 `curl`을 쓴다
- **해결**: `apk add --no-cache --initdb --repository <url> e2fsprogs curl` — `--initdb`로 이 root에
  DB를 새로 만들고 `curl`도 같이 설치, 수정됨

### 대상 chroot 안에서 패키지가 전부 `no such package`(DNS transient error)

- **원인**: `chroot`는 `/proc`·`/sys`·`/dev` bind 마운트와 달리 `/etc/resolv.conf`를 공유하지
  않는다. 대상 rootfs의 (사용 불가능한) resolv.conf가 그대로 쓰여 모든 조회가 실패한다
- **해결**: resolv.conf 쓰기를 chroot 블록 **앞**으로 이동(최종 값은 동일, 타이밍만 이동), 수정됨

### `mkfs.ext4 -d`가 `No such process`로 실패

- **원인**: busybox의 `umount`에는 `-R` 옵션이 **아예 없다** — `umount -R ... 2>/dev/null || true`
  전체가 조용히 no-op이 되어 `/proc`이 staging 아래 마운트된 채 남는다. 그 상태로 `mkfs.ext4 -d`가
  디렉터리를 훑으며 `/proc`의 프로세스별 임시 파일을 복사하려다 실패한다
- **해결**: `umount X || umount -l X || true` 2단 폴백으로 교체, 수정됨

### `oom-killer`가 apt-get/dnf를 죽임 — `bootstrap script exited with code 137`

- **원인**: 위 전제 1 — 다운로드·전개·chroot 설치가 전부 게스트 RAM에서 일어난다. 빌더 RAM
  1024MiB로는 ubuntu가 못 버틴다(의존성 하나인 `linux-firmware-nvidia-graphics`만 109MB)
- **확인**: 콘솔 로그에 `invoked oom-killer` + `Out of memory: Killed process ... (apt-get)`
- **해결**: 빌더 VM RAM을 8192MiB로 상향(`handlers/bootstrap.rs`). Firecracker는 게스트 메모리를
  lazy 할당하므로 실제로 만진 만큼만 든다, 수정됨

### `At least NNNMB more space needed on the / filesystem`(rocky)

- **원인**: 위 전제 1의 함정 — **실제 메모리 부족이 아니다.** `/`가 ramfs라 statvfs가 여유를 0으로
  보고하는데, rpm은 모든 트랜잭션 전에 그 statvfs를 본다. 그래서 "부족량"은 그냥 설치 총량이고,
  **RAM을 아무리 늘려도 숫자가 한 글자도 안 변한다**(4GiB·8GiB 모두 350MB로 동일, 같은 시점
  `free`는 7.4GiB 유휴). apk·apt는 이런 사전 검사를 안 해서 rocky만 걸린다
- **해결**: 작업 영역에 진짜 tmpfs를 마운트(`mount -t tmpfs tmpfs "$work"`) — tmpfs는 크기를
  정직하게 보고하고, `size=` 없이 두면 VM RAM의 절반으로 자동 결정되어 빌더 RAM 설정을 그대로
  따라간다, 수정됨
- **교훈**: 같은 숫자가 조건을 바꿔도 안 변하면 "자원이 모자라다"가 아니라 "**측정이 틀렸다**"를
  먼저 의심한다

### 부트스트랩은 성공했는데 그 이미지로 만든 VM이 dracut emergency shell에 빠진다(rocky)

- **증상**: `Failed to start File System Check on /dev/vda` → `Dependency failed for /sysroot` →
  `Entering emergency mode`. 부트스트랩·패키징·설치는 전부 성공한 뒤라 원인이 안 보인다
- **원인**: rootfs.ext4를 만드는 `mkfs.ext4`는 **바깥(MicroBoot) 셸의 것** — Alpine 3.24의
  e2fsprogs 1.47.x라 `orphan_file` 피처를 기본으로 켠다. 그런데 그 파일시스템을 매 부팅마다
  검사하는 건 방금 만든 rootfs 안의 e2fsck이고, Rocky 9는 e2fsprogs **1.46.5(2021)** 라 그 피처를
  모른다. Alpine·Ubuntu는 자기 initramfs에도 1.47.x가 들어가서 이 문제가 안 생긴다
- **확인**: host에서 rootfs 안의 e2fsck를 꺼내 직접 돌려보면 확정된다

```sh
debugfs -c -R "dump /usr/sbin/e2fsck /tmp/e2fsck-rocky" images/rootfs/rocky-rootfs-9-x86_64.ext4
chmod +x /tmp/e2fsck-rocky && /tmp/e2fsck-rocky -fn images/rootfs/rocky-rootfs-9-x86_64.ext4
# → has unsupported feature(s): orphan_file / Get a newer version of e2fsck!
```

- **해결**: rocky의 rootfs 생성만 `mkfs.ext4 -F -O '^orphan_file' ...`로 변경, 수정됨
- **교훈**: 게스트 이미지를 **다른 배포판 도구로** 만들 때는 만든 쪽이 아니라 **쓰는 쪽**의 도구
  버전이 기준이다

### rocky만 1초 만에 `cp: can't stat '/etc/pki/rpm-gpg'`

- **원인**: 스크립트가 RPM 서명 키를 **바깥 셸**의 `/etc/pki/rpm-gpg`에서 복사했다. 예전엔 빌더가
  설치된 rocky 템플릿이라 거기 있었지만, 지금 바깥 셸은 Alpine이라 그 경로가 없다
- **해결**: 같은 키를 담고 있는 Container-Base 추출본(`$container_root/etc/pki/rpm-gpg`)에서
  가져오도록 이동 + 없으면 명확히 실패하는 체크 추가, 수정됨

### `microboot.rs`를 고쳤는데 재시작해도 증상이 그대로다

- **원인**: `TemplateRegistry::register_spec`이 등록을 `images/.templates.json`에 영속화하고 다음
  기동 때 그대로 재생한다. `ensure_registered`는 `resolve_alias`가 이미 있으면 즉시 반환하므로,
  **깨진 등록이 그대로 살아남아** 고친 코드가 실행되지 않는다
- **해결**: 새 프로세스를 **띄우기 전에** 아래를 지운다(띄운 뒤에 지우면 이미 캐시된 경로를 참조해
  `No such file or directory`가 난다)

```sh
rm -f images/.templates.json images/.microboot/placeholder.ext4
```

### 게스트 스크립트를 고쳤는데 반영이 안 된다

- **원인**: 3개 스크립트는 `include_str!`로 바이너리에 박힌다 — 파일만 고치고 재빌드를 안 하면
  옛 내용이 그대로 실행된다
- **확인**: `strings target/debug/firecrab-api | grep '<새로 넣은 문구>'`

## 네트워크

### "network helper is unavailable at /run/firecrab/net-helper.sock"

- **원인**: `firecrab-net-helper`가 안 떠 있거나, `firecrab-api`가 보는 기본 소켓 경로
  (`/run/firecrab/net-helper.sock`)와 다른 경로(`docs/net-helper.md`의 개발용 `/tmp/firecrab-net.sock`
  예시 등)로 기동됨
- **해결**: `./scripts/dev-net-helper.sh`로 기동. `sudo -u root -g pista` 둘 다 필요 —
  `-g pista`만 쓰면 root가 아니라 호출한 사용자로 실행되어 `/run/firecrab` 바인드가 permission
  denied로 실패한다(`-u root` 없이 `-g`만 지정하면 sudo는 대상 사용자를 root가 아니라 **호출한
  사용자**로 취급)

### VM이 로그인 셸까지 완전히 부팅되는데도 계속 `error`로 끝난다

- **원인**: rootfs 템플릿 이미지가 `firecrab-network-ready.service`를 추가한 빌드 스크립트보다
  오래된 채로 재빌드가 안 됨 — guest가 네트워크 준비 신호(`FIRECRAB_NETWORK_READY`)를 영영 콘솔에
  출력하지 않아 `wait_for_network_ready`가 실패한다. 게다가 `firecrab-api`는 템플릿 파일의
  inode/길이/SHA256을 **기동 시점에 한 번만** 검증해 메모리에 고정하므로(`TemplateRegistry::
  load_default`, `firecrab-api/src/main.rs`), 이미지를 재빌드해도 `firecrab-api`를 재시작하지
  않으면 "template artifact changed" 로 계속 실패한다
- **확인**: `debugfs -c -R "ls -l /etc/systemd/system/multi-user.target.wants" <rootfs.ext4>` 로
  `firecrab-network-ready.service` 심볼릭 링크가 있는지 직접 확인 가능
- **해결**: `scripts/firecracker-menual/install-{ubuntu,alpine}-rootfs.sh` 재실행 →
  `firecrab-api` 재시작(새 이미지 인식용)

### `FIRECRAB_NETWORK_FAILED no-ipv4-address`

VM이 부팅과 `firecrab-network-ready.service` 실행까지는 성공하지만 guest가 DHCP로 IP를 못 받는 경우.
원인이 두 가지 겹쳐 있었다(둘 다 수정됨):

- **원인 1 — bridge forward delay**: 새로 붙은 TAP 포트가 커널 기본 forward delay(단계당 15초,
  최대 ~30초) 때문에 한동안 forwarding 상태가 안 됨 — `stp_state=0`(STP 자체를 꺼도) 이 지연은
  별개로 적용된다. guest는 부팅 후 몇 초 안에 DHCPDISCOVER를 보내므로 그 안에 대부분 못 받는다.
  `firecrab-net-helper/src/bridge.rs`의 `ensure_bridge`에 `forward_delay(0)` 추가로 수정 — VM
  start마다 매번 호출되는 idempotent 함수라 기존 브리지에도 바로 적용된다
- **원인 2 — dnsmasq 고아 프로세스 미재사용**: DHCP를 서빙하는 `dnsmasq` child의 참조
  (`DhcpActor.child`)가 net-helper 프로세스 메모리에만 있다. net-helper가 재시작되면(개발 중
  흔함) 이 참조가 사라지고, 이미 떠 있는(재시작 전 net-helper가 띄운) 고아 dnsmasq를 재사용하지
  않은 채 새 dnsmasq를 spawn 시도 → 포트 충돌로 새 프로세스가 죽거나 무시됨 → 이후 신규 VM의
  lease가 실제로 서빙 중인(원래) dnsmasq에는 절대 반영되지 않는다. `firecrab-net-helper/src/
  dhcp.rs`에 `dnsmasq.pid` 파일을 확인해 살아있는 기존 프로세스면 그걸 재사용(SIGHUP)하도록 수정
- **원인 3~6**: [bugs/dhcp-never-reaches-guest.md](../50-bugs/dhcp-never-reaches-guest.md) —
  dnsmasq base config/hosts 파일 경로 충돌, `dhcp-hostsfile`에 잘못된 `dhcp-host=` 접두어,
  **호스트 UFW가 67/53 포트를 막고 있던 것**(코드 문제 아님, 새 개발 머신마다 수동으로
  `sudo ufw allow in on fcbr0 to any port 67 proto udp` 등 해줘야 함), IP를 빠르게 재사용할 때
  dnsmasq의 예전 리스와 충돌(`dhcp_release`로 강제 해제하도록 수정, `dnsmasq-utils` 설치 필요).
  넷 다 수정/조치됨 — VM 5대 연속 생성·삭제로 재현 검증 완료

### Alpine 템플릿만 매번 `no-ipv4-address`(Ubuntu는 정상)

- **원인·수정**: [bugs/alpine-network-ready-races-dhcpcd.md](../50-bugs/alpine-network-ready-races-dhcpcd.md) —
  OpenRC의 `after dhcpcd`는 시작 순서만 보장하지 dhcpcd가 실제로 IP를 받았다는 보장이 아님(dhcpcd가
  즉시 데몬으로 fork). `firecrab-network-ready` 서비스에 짧은 폴링 추가로 수정, 수정됨

### VM을 여러 대 동시에 시작하면 부팅 극초반에 일부가 원인 불명으로 죽는다

- **원인·수정**: [bugs/vm-killed-mid-boot-under-concurrent-load.md](../50-bugs/vm-killed-mid-boot-under-concurrent-load.md) —
  콘솔 브로드캐스트 채널이 컨슈머 지연으로 `Lagged`를 반환하는 걸 `Closed`(진짜 종료)와 구분 못 해
  멀쩡히 부팅 중인 VM을 죽였던 버그, 수정됨. bpftrace로 SIGKILL 발신자가 `firecrab-api` 자신임을
  특정한 과정도 기록해뒀다(비슷한 미스터리 킬을 또 만나면 참고)

### VM 내부에서 새 목적지로 나가는 연결이 타임아웃(예: `apt update`는 되는데 `apk update`는 안 됨)

- **원인·수정**: [bugs/vm-outbound-forward-blocked-by-ufw.md](../50-bugs/vm-outbound-forward-blocked-by-ufw.md) —
  `dhcp-never-reaches-guest.md` 원인 3과 같은 클래스: 호스트 UFW가 라우팅(forward)을 기본
  거부(`라우팅 된: deny`)하는데 새 아웃바운드 연결을 허용하는 규칙이 없었음(established/related와
  ping만 예외). `inet firecrab` 테이블 자체는 정상이라 코드 문제가 아니었음(코드 문제 아님, 새
  개발 머신마다 수동으로 `sudo ufw route allow in on fcbr0 out on <업링크>` 해줘야 함)

### MicroNetwork에 넣은 VM만 `no-ipv4-address`로 실패(기본 네트워크 VM은 정상)

- **원인**: 위 두 UFW 항목과 같은 클래스 — UFW 허용 규칙이 `fcbr0` **인터페이스 이름에 묶여**
  있어서(`67/udp on fcbr0`), MicroNetwork마다 새로 생기는 `mnb<hex>` 브리지에는 적용되지 않는다.
  guest의 DHCPDISCOVER가 host INPUT에서 UFW에 drop된다. 코드 문제 아님
- **확인**: `sudo ufw status verbose`에 그 브리지 이름이 안 보이면 이 케이스다. dnsmasq 자체는
  정상이라 `sudo ss -lunp | grep :67`에는 멀쩡히 떠 있고, `/run/firecrab/dnsmasq.conf`에도
  `interface=mnb<hex>`가 들어 있다(2026-07-29 실제로 이 증상으로 한 번 헤맴)
- **조치**: MicroNetwork를 만들 때마다 그 브리지에 대해 한 번씩

```sh
BR=$(ip -br link show type bridge | grep mnb | cut -d' ' -f1)   # 대상 브리지
sudo ufw allow in on "$BR" to any port 67 proto udp
sudo ufw allow in on "$BR" to any port 53
sudo ufw route allow in on "$BR" out on <업링크>                  # 외부 통신까지 필요하면
```

- firecrab은 자기 소유가 아닌 firewall(UFW)을 건드리지 않는 것이 원칙이라
  (`task-host-network-privileges.md`) 자동화하지 않았다. 운영 배포 시에는 UFW를 끄고
  프로젝트의 nftables만 쓰거나, 브리지 접두어(`mnb`) 단위 규칙을 미리 넣어두는 쪽이 낫다

## 터미널

### 터미널 버튼을 눌러도 "연결 끊김"만 뜨고 안 붙는다

- **원인**: 백엔드에 `/ws` 콘솔 라우트(`firecrab-api/src/console.rs`, `handlers/console.rs`)가
  없는 브랜치 — `feat/microvm-terminal`에서 구현, 이후 브랜치에 병합됨
- **해결**: 현재 브랜치에 병합됐는지 확인

### 터미널 프롬프트에 `;1R;80R;1R;80R...` 같은 게 반복 출력된다

- **원인·수정**: [bugs/terminal-cursor-position-echo-loop.md](../50-bugs/terminal-cursor-position-echo-loop.md) —
  xterm.js의 커서 위치 응답이 guest tty에 echo되며 생기는 루프, 수정됨

## 기능별 상세 디버깅

| 기능 | 문서 |
|---|---|
| MicroVM 터미널 | [tests/microvm-terminal.md](../40-tests/microvm-terminal.md) |
| VM 시작 단계별 진행 상황 | [tests/vm-startup-progress.md](../40-tests/vm-startup-progress.md) |
| VM 상세 모달 | [tests/vm-detail-modal.md](../40-tests/vm-detail-modal.md) |
| VM 디스크 용량 설정 | [tests/vm-disk-capacity.md](../40-tests/vm-disk-capacity.md) |
| VM 리소스(CPU/RAM/DISK) 수정 | [tests/vm-resource-update.md](../40-tests/vm-resource-update.md) |
| 프론트엔드 React 이전 | [tests/frontend-react-migration.md](../40-tests/frontend-react-migration.md) |
