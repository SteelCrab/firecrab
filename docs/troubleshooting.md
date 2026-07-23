# 트러블슈팅

증상별로 찾아보는 문제 해결 모음. 아래 표에서 증상을 찾아 해당 섹션으로 이동한다. 기능별 상세
검증 절차는 `docs/tests/`, 개별 버그의 전체 원인·수정 기록은 `docs/bugs/`에 있다.

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

- **원인·수정**: [bugs/vm-startup-stuck-under-concurrent-load.md](bugs/vm-startup-stuck-under-concurrent-load.md) —
  템플릿 재해싱 중복 + 타임아웃된 요청의 future가 drop되며 VM이 고아 상태가 되는 버그, 둘 다 수정됨

### 호스트 디스크가 꽉 차서 VM 생성/시작이 느리거나 실패한다

- **원인**: VM 하나당 디스크(`data/vms/<id>/rootfs.ext4`)가 기본 2GiB — 테스트/재현용으로 VM을
  많이 만들면 금방 쌓인다. ext4는 여유 공간이 임계치 이하로 떨어지면 쓰기 성능이 급격히 나빠진다
- **해결**: `df -h`로 확인하고, 안 쓰는 VM은 `DELETE /api/vms/{id}`로 정리(디스크 파일도 같이 삭제됨)

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
- **원인 3~6**: [bugs/dhcp-never-reaches-guest.md](bugs/dhcp-never-reaches-guest.md) —
  dnsmasq base config/hosts 파일 경로 충돌, `dhcp-hostsfile`에 잘못된 `dhcp-host=` 접두어,
  **호스트 UFW가 67/53 포트를 막고 있던 것**(코드 문제 아님, 새 개발 머신마다 수동으로
  `sudo ufw allow in on fcbr0 to any port 67 proto udp` 등 해줘야 함), IP를 빠르게 재사용할 때
  dnsmasq의 예전 리스와 충돌(`dhcp_release`로 강제 해제하도록 수정, `dnsmasq-utils` 설치 필요).
  넷 다 수정/조치됨 — VM 5대 연속 생성·삭제로 재현 검증 완료

### Alpine 템플릿만 매번 `no-ipv4-address`(Ubuntu는 정상)

- **원인·수정**: [bugs/alpine-network-ready-races-dhcpcd.md](bugs/alpine-network-ready-races-dhcpcd.md) —
  OpenRC의 `after dhcpcd`는 시작 순서만 보장하지 dhcpcd가 실제로 IP를 받았다는 보장이 아님(dhcpcd가
  즉시 데몬으로 fork). `firecrab-network-ready` 서비스에 짧은 폴링 추가로 수정, 수정됨

### VM을 여러 대 동시에 시작하면 부팅 극초반에 일부가 원인 불명으로 죽는다

- **원인·수정**: [bugs/vm-killed-mid-boot-under-concurrent-load.md](bugs/vm-killed-mid-boot-under-concurrent-load.md) —
  콘솔 브로드캐스트 채널이 컨슈머 지연으로 `Lagged`를 반환하는 걸 `Closed`(진짜 종료)와 구분 못 해
  멀쩡히 부팅 중인 VM을 죽였던 버그, 수정됨. bpftrace로 SIGKILL 발신자가 `firecrab-api` 자신임을
  특정한 과정도 기록해뒀다(비슷한 미스터리 킬을 또 만나면 참고)

### VM 내부에서 새 목적지로 나가는 연결이 타임아웃(예: `apt update`는 되는데 `apk update`는 안 됨)

- **원인·수정**: [bugs/vm-outbound-forward-blocked-by-ufw.md](bugs/vm-outbound-forward-blocked-by-ufw.md) —
  `dhcp-never-reaches-guest.md` 원인 3과 같은 클래스: 호스트 UFW가 라우팅(forward)을 기본
  거부(`라우팅 된: deny`)하는데 새 아웃바운드 연결을 허용하는 규칙이 없었음(established/related와
  ping만 예외). `inet firecrab` 테이블 자체는 정상이라 코드 문제가 아니었음(코드 문제 아님, 새
  개발 머신마다 수동으로 `sudo ufw route allow in on fcbr0 out on <업링크>` 해줘야 함)

## 터미널

### 터미널 버튼을 눌러도 "연결 끊김"만 뜨고 안 붙는다

- **원인**: 백엔드에 `/ws` 콘솔 라우트(`firecrab-api/src/console.rs`, `handlers/console.rs`)가
  없는 브랜치 — `feat/microvm-terminal`에서 구현, 이후 브랜치에 병합됨
- **해결**: 현재 브랜치에 병합됐는지 확인

### 터미널 프롬프트에 `;1R;80R;1R;80R...` 같은 게 반복 출력된다

- **원인·수정**: [bugs/terminal-cursor-position-echo-loop.md](bugs/terminal-cursor-position-echo-loop.md) —
  xterm.js의 커서 위치 응답이 guest tty에 echo되며 생기는 루프, 수정됨

## 기능별 상세 디버깅

| 기능 | 문서 |
|---|---|
| MicroVM 터미널 | [tests/microvm-terminal.md](tests/microvm-terminal.md) |
| VM 시작 단계별 진행 상황 | [tests/vm-startup-progress.md](tests/vm-startup-progress.md) |
| VM 상세 모달 | [tests/vm-detail-modal.md](tests/vm-detail-modal.md) |
| VM 디스크 용량 설정 | [tests/vm-disk-capacity.md](tests/vm-disk-capacity.md) |
| VM 리소스(CPU/RAM/DISK) 수정 | [tests/vm-resource-update.md](tests/vm-resource-update.md) |
| 프론트엔드 React 이전 | [tests/frontend-react-migration.md](tests/frontend-react-migration.md) |
