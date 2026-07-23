# DHCP가 항상 실패한다 (`no-ipv4-address` / `dns-unreachable`)

`docs/task-guest-network-configuration.md` 기능(IPAM 리스를 실제 guest DHCP로 적용) 실사용 중
발견. VM이 부팅은 잘 되는데 매번 `FIRECRAB_NETWORK_FAILED no-ipv4-address`로 끝났다. 겹친
버그 세 개를 하나씩 걷어내고서야 끝까지 해결됐다.

## 증상

- `reason="network readiness check failed: guest reported network failure: no-ipv4-address"`로
  거의 100% 실패
- `journalctl`에 `dnsmasq[pid]: bad hex constant at line 1 of /run/firecrab/dnsmasq-hosts.conf`
  반복 출력
- `log-dhcp`를 켜도 dnsmasq 쪽에 `DHCPDISCOVER` 로그 자체가 안 찍힘(요청을 아예 못 받음)
- 위 두 개를 고친 뒤에는 dnsmasq가 `DHCPDISCOVER`까지는 받는데(`tcpdump`로 fcbr0에서 요청 패킷
  확인) 응답이 안 나가고, 여전히 로그에 아무것도 안 찍힘

## 원인 1: base config와 hosts 파일 경로 충돌

`firecrab-net-helper/src/dhcp.rs`의 `spawn_dnsmasq`가 base config(인터페이스/레인지/
`dhcp-hostsfile=` 지시자) 경로를 `hosts_path.with_extension("conf")`로 만들었는데,
`hosts_path`(`/run/firecrab/dnsmasq-hosts.conf`)가 이미 `.conf`로 끝나 이 호출이 **아무 것도
안 바꾸는 no-op**이었다 — 결과적으로 base config와 리스 예약 파일이 **같은 경로**로 겹쳐썼다.
dnsmasq가 처음 뜰 때는 base config 내용을 읽지만, 그 직후 `sync_dhcp_leases`가 같은 경로를
리스 목록만으로 덮어써버려 `interface=`/`dhcp-range=`/`dhcp-hostsfile=` 지시자가 통째로
사라졌다.

**수정**: 별도 헬퍼 `base_config_path(hosts_path)`를 `hosts_path.with_file_name("dnsmasq.conf")`로
만들어 항상 다른 경로가 되도록 함. 회귀 테스트
`base_config_path_never_collides_with_the_hosts_file_it_describes`.

## 원인 2: `dhcp-hostsfile` 파일에 `dhcp-host=` 접두어를 붙임

`render_hosts_file`이 각 줄을 `dhcp-host=MAC,IP,HOSTNAME` 형식으로 썼는데, 이건 일반
`--conf-file`에서나 맞는 문법이다. `--dhcp-hostsfile=`로 지정된 파일은 접두어 없는
`MAC,IP,HOSTNAME` 형식을 기대한다 — 접두어가 붙으면 dnsmasq가 리터럴 텍스트 "dhcp-host"를
선행 MAC/hex 필드로 파싱하려다 "bad hex constant"로 전체 파일을 거부하고, 예약을 통째로
못 읽는다. (원인 1을 고치기 전엔 두 버그가 겹쳐 있어 이 증상이 안 보였다 — 파일이 아예
base config로 덮여 있었으니까.)

**수정**: `render_hosts_file`에서 `dhcp-host=` 접두어 제거.

## 원인 3 (진짜 근본 원인): 호스트의 UFW가 DHCP/DNS 서버 포트를 막고 있었음

위 두 개를 고친 뒤에도 여전히 실패했다. `bpftrace`/`tcpdump`로 확인해보니 guest의
DHCPDISCOVER는 `fcbr0`까지 정상 도달했는데(firecrab 자체 nftables 규칙은 전부 정상),
dnsmasq는 `log-dhcp`를 켜도 요청을 받은 흔적이 전혀 없었다. `sudo nft list ruleset`으로
전체 규칙을 보니 **firecrab이 관리하지 않는, 이 개발 머신에 이미 있던 UFW**가 원인이었다:

```
chain ufw-after-input {
    ...
    udp dport 67 counter packets 3303 bytes 1885225 jump ufw-skip-to-policy-input
    ...
}
chain ufw-skip-to-policy-input {
    counter packets 3403 bytes 1894221 drop
}
```

UFW가 포트 67(DHCP 서버) 수신 트래픽을 명시적으로 drop하도록 설정되어 있었다(랩탑/워크스테이션이
의도치 않게 DHCP 서버로 오작동하는 걸 막기 위한 흔한 기본값으로 보임). firecrab의 자체
nftables 테이블(`firecrab_l2` 등)은 완전히 별개의 테이블이라 이 UFW 규칙과 무관하게 둘 다
평가되고, 어느 한쪽이라도 drop하면 패킷은 죽는다. DHCPOFFER/ACK가 나간 뒤에도 guest가 DNS
서버(같은 dnsmasq, 172.30.0.1:53)에 붙는 단계가 있는데 그것도 같은 이유로 막혀
`dns-unreachable`로 실패했다.

**수정(호스트 설정, 코드 아님)**:
```sh
sudo ufw allow in on fcbr0 to any port 67 proto udp
sudo ufw allow in on fcbr0 to any port 53 proto udp
sudo ufw allow in on fcbr0 to any port 53 proto tcp
```
`fcbr0` 인터페이스로 들어오는 트래픽만 허용해 다른 인터페이스는 영향받지 않는다. 이 프로젝트가
관리하는 어떤 스크립트도 이 규칙을 만들지 않으므로, **새 개발 머신에 처음 셋업할 때 수동으로
한 번 해줘야 한다** — `scripts/dev-net-helper.sh` 실행 안내에 이 UFW 명령도 같이 문서화해둘
필요가 있음(아직 스크립트화는 안 함).

## 원인 4: IP 재사용 시 dnsmasq의 예전 리스와 충돌

원인 1~3을 고친 뒤에도 VM을 빠르게 연속 생성/삭제하면(IPAM이 해제된 IP를 즉시 재사용) 가끔
`no-ipv4-address`가 재현됐다. dnsmasq 로그: `not using configured address 172.30.0.2 because
it is leased to <이전 MAC>` → `DHCPDISCOVER ... no address available`.

firecrab의 IPAM(SQLite)은 VM 삭제 시 IP를 논리적으로 즉시 재사용 가능하게 풀지만, dnsmasq는
**자기 자신의 별도 리스 DB**(`/run/firecrab/dnsmasq.leases`, 디스크에 파일로 남아 dnsmasq
프로세스가 재시작돼도 유지됨)에 "이 IP는 아직 이 MAC이 쓰고 있다(리스 만료 전까지, 최대
1시간)"를 기억한다. `dhcp-hostsfile` 정적 예약을 SIGHUP으로 리로드해도 이미 활성화된 리스는
무효화되지 않는다 — 새 MAC에 그 IP를 안 준다.

**수정**: `sync_dhcp_leases`가 매번 dnsmasq의 리스 DB(`/run/firecrab/dnsmasq.leases`, 명시적으로
`dhcp-leasefile=`로 지정)를 직접 읽어, 지금 적용하려는 새 예약 목록과 (ip, mac)이 안 맞는
항목을 찾아 `dnsmasq-utils`의 `dhcp_release <bridge> <ip> <mac>`으로 즉시 강제 해제한다.
프로세스 메모리가 아니라 디스크의 실제 리스 DB를 직접 대조하므로 net-helper 자체가 재시작돼도
안전하다. `dnsmasq-utils` 패키지 설치 필요(`dhcp_release` 바이너리).

## 곁다리 변경: `bind-interfaces` → `bind-dynamic`

원인 3을 찾기 전, 유니캐스트 DHCPOFFER가 안 나가는 것 아닌가 의심해서 `bind-interfaces`
대신 `bind-dynamic`으로 바꿨다. 실제 근본 원인은 아니었던 것으로 보이지만(그때도 여전히
UFW가 막고 있었음), dnsmasq 공식 문서가 인터페이스 여러 개인 호스트에서 `bind-dynamic`을
권장하므로 되돌리지 않고 유지함. `log-dhcp`도 향후 디버깅을 위해 켜둔 채로 남김.

## 검증

`final-verify` 등 여러 VM을 만들어 `state: "running"`까지 정상 도달 확인, dnsmasq 로그에
`DHCPDISCOVER → DHCPOFFER → DHCPREQUEST → DHCPACK` 전체 시퀀스 확인. 원인 4 수정 후 VM 5대를
빠르게 연속 생성·시작·정지·삭제(같은 IP 풀을 즉시 재사용)해도 5대 전부 `running` 도달 확인.

## 산출물

`firecrab-net-helper/src/dhcp.rs`(`base_config_path` 분리, `render_hosts_file` 접두어 제거,
`bind-dynamic`/`log-dhcp`, `dhcp-leasefile` 명시 + `release_stale_leases`, 회귀 테스트 6개) —
호스트 UFW 설정(원인 3)과 `dnsmasq-utils` 패키지 설치(원인 4)는 코드 밖의 별도 조치.
