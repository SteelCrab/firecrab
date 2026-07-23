# Alpine 템플릿이 항상 `no-ipv4-address`로 부팅 실패

Ubuntu·Alpine 두 템플릿을 나란히 부팅·ping 검증하던 중 발견. Ubuntu는 되는데 Alpine만 매번
`FIRECRAB_NETWORK_FAILED no-ipv4-address`로 실패했다.

## 증상

- 콘솔 로그: `* Starting dhcpcd ... [ ok ]` 직후 곧바로 `FIRECRAB_NETWORK_FAILED no-ipv4-address`
- 실제로 dnsmasq 쪽엔 이 guest의 DHCPDISCOVER조차 안 잡힘(=아직 안 보냄) — DHCP 인프라
  문제가 아니라 guest 쪽 타이밍 문제

## 원인

`scripts/firecracker-menual/install-alpine-rootfs.sh`가 만드는 `firecrab-network-ready`
OpenRC 서비스가 `depend() { need net; after dhcpcd }`로 dhcpcd 다음에 뜨도록 순서만
지정했는데, OpenRC의 `after`는 **서비스 시작 순서**만 보장하지 dhcpcd가 실제로 DHCP
트랜잭션을 끝냈다는 것까지는 보장하지 않는다. `dhcpcd`는 시작하자마자 데몬으로 fork해
버리므로 그 "start" 자체는 거의 즉시 끝나고, 실제 DHCPDISCOVER 전송은 그 이후 비동기로
일어난다. `firecrab-network-ready`의 `start()`가 이 타이밍차를 기다리지 않고 곧바로
`ip -4 addr show eth0`를 확인해 매번 "아직 없음"으로 실패 처리한 것.

(Ubuntu가 안 겪는 이유: systemd의 `network-online.target`은 `network-online.target`에
매달린 유닛이 뜨기 전에 실제 주소 설정 완료를 기다리는 동기화 지점을 제공하지만, OpenRC의
`after`는 그런 보장이 없는 순서 지정일 뿐이다.)

## 수정

`firecrab-network-ready`의 `start()`에 짧은 폴링(최대 10초, 1초 간격)을 추가 — dhcpcd가
백그라운드에서 실제 리스를 받을 시간을 벌어준다.

```sh
ipv4=""
for _ in $(seq 1 10); do
    ipv4=$(ip -4 -o addr show eth0 2>/dev/null | awk '{print $4}' | cut -d/ -f1)
    [ -n "$ipv4" ] && break
    sleep 1
done
```

이미 빌드된 이미지에는 반영 안 됨 — `scripts/firecracker-menual/install-alpine-rootfs.sh`
재실행으로 rootfs 재빌드 필요. 재빌드 후 `firecrab-api`도 재시작해야 함(템플릿 아티팩트의
inode/길이/SHA256을 기동 시점에 한 번만 검증해 메모리에 고정하므로 —
`docs/troubleshooting.md`의 "VM이 로그인 셸까지 완전히 부팅되는데도 계속 error" 항목과 동일한
이유).

## 검증

Ubuntu 2대 + Alpine 2대를 재빌드된 이미지로 새로 생성·시작 → 4대 전부 `state: "running"`,
호스트에서 4개 IP 전부 `ping` 100% 성공(패킷 손실 0%).

## 산출물

`scripts/firecracker-menual/install-alpine-rootfs.sh`(`firecrab-network-ready` 폴링 추가) +
재빌드된 `images/rootfs/alpine-rootfs-3.24.1-x86_64.ext4`.
