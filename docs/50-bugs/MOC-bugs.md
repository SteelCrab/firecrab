---
tags:
  - firecrab
  - moc
  - bug
updated: 2026-07-30
---

# 버그 기록

> [!summary] 왜 남기나
> 여기 있는 건 전부 **한 번은 원인을 못 찾아 헤맸던** 것들이다.
> 증상만 보고는 어디를 봐야 할지 알 수 없었던 경우라, 다음에 같은 증상을 만나면
> 조사를 처음부터 반복하지 않도록 증상 → 원인 → 수정을 남긴다.
> 바로 해결하려면 [트러블슈팅](../20-guides/troubleshooting.md)이 먼저다.

## 증상으로 찾기

| 증상 | 원인 | 기록 |
|---|---|---|
| 게스트가 IP를 못 받음 (`no-ipv4-address`) — 전부 | dnsmasq 설정 경로 충돌, `dhcp-hostsfile` 접두어, 호스트 UFW, 리스 재사용 | [dhcp-never-reaches-guest](dhcp-never-reaches-guest.md) |
| 게스트가 IP를 못 받음 — **Alpine만** | OpenRC `after dhcpcd`가 완료를 보장하지 않음 | [alpine-network-ready-races-dhcpcd](alpine-network-ready-races-dhcpcd.md) |
| VM에서 나가는 **새** 연결만 타임아웃 | 호스트 UFW가 라우팅(forward)을 기본 거부 | [vm-outbound-forward-blocked-by-ufw](vm-outbound-forward-blocked-by-ufw.md) |
| Alpine이 커널 패닉 — root 마운트 실패 | ext4가 모듈이라 `rootfstype=ext4` 없이는 타입을 못 정함 | [alpine-official-kernel-cant-mount-root](alpine-official-kernel-cant-mount-root.md) |
| 동시 시작 시 일부가 `starting`에서 멈춤 | 한 물리 디스크에 I/O가 몰려 병목 | [vm-startup-stuck-under-concurrent-load](vm-startup-stuck-under-concurrent-load.md) |
| 동시 시작 시 멀쩡한 VM이 SIGKILL 당함 | 콘솔 브로드캐스트의 `Lagged`를 `Closed`로 오인 | [vm-killed-mid-boot-under-concurrent-load](vm-killed-mid-boot-under-concurrent-load.md) |
| 터미널에 `;1R;80R…`이 반복 출력 | 커서 위치 응답이 에코 루프를 만듦 | [terminal-cursor-position-echo-loop](terminal-cursor-position-echo-loop.md) |

## 패턴

> [!warning] 호스트 방화벽은 세 번 물었다
> `dhcp-never-reaches-guest`(원인 3), `vm-outbound-forward-blocked-by-ufw`,
> 그리고 MicroNetwork 브리지의 DHCP 차단 — 전부 UFW다.
> firecrab의 nftables는 정상인데 UFW가 **별도로** 막는 구조라 코드를 아무리 봐도 안 나온다.
> 네트워크가 이유 없이 안 되면 `sudo ufw status verbose`부터 본다.

- **동시 부하에서만 나는 버그가 두 건**이다(디스크 I/O 병목, 콘솔 채널 오인).
  한 대로 재현이 안 되면 여러 대 동시에 돌려본다
- **배포판별로 갈리는 버그가 두 건**이다(Alpine의 dhcpcd 경합, 모듈화된 ext4).
  Ubuntu에서 되는데 Alpine에서 안 되면 init 시스템과 모듈 구성을 먼저 의심한다

## 전체 목록

```dataview
LIST
FROM "50-bugs"
WHERE file.name != this.file.name
SORT file.name ASC
```
