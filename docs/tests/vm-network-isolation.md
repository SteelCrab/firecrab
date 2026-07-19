# VM network 격리·anti-spoofing 테스트

## 자동 테스트 (root 불필요)

```sh
cargo test -p firecrab-net-helper firewall::
cargo test -p firecrab-api network_policy::
```

렌더링된 nft ruleset 문자열의 구조·순서를 검증한다. 실제 nft 적용과 트래픽 차단 동작은 root가 필요해 아래 "수동 확인"으로 분리한다(이 세션 미실행).

## 확인 항목 (렌더링)

L2 (`bridge firecrab_l2`, per-VM):
- source MAC / ARP sender MAC·IP / IPv4 source가 lease와 불일치하면 drop
- DHCP 예외 2건만 accept — discover(`src 0.0.0.0`, 68→67, MAC 일치), ARP probe(sender IP `0.0.0.0`, MAC 일치)
- `ether type != { ip, arp } drop` — IPv6/VLAN 등 차단
- `iifname "fct*" oifname "fct*" drop` — VM 간 east-west 차단 (prefix `fct`라 `fcbr0`와 안 겹침)

L3 (`inet firecrab`):
- egress/ingress는 leased IP 기준 verdict map(`ip saddr`/`ip daddr`)으로 per-VM dispatch — routed 패킷의 iifname이 bridge라 IP로 key
- egress `internet`=accept, `isolated`=빈 chain→trailing `drop`
- 예약 대역(`127.0.0.0/8`, `169.254.0.0/16`) destination drop
- 두 dispatch chain 모두 default `drop`
- host SSH 허용은 요청 시에만 `tcp dport 22` (forwarded inbound 한정 — host 자기 output hook은 이 scope에서 미필터)

per-VM 독립성:
- 모든 per-VM 객체는 vm_id로 namespacing, add+flush로 원자 교체 → 한 VM 교체가 다른 VM에 무영향
- 제거는 map element를 chain보다 먼저 delete (still-referenced chain 거부 회피)
- unknown egress ID는 `nft` 닿기 전 `InvalidRequest`로 거부 (CIDR 문자열 포함)

API:
- `EgressPolicy` ID round-trip, unknown/CIDR 거부, 기본값 `internet`

## 수동 확인 (root 필요, 여기선 미실행)

sudo 비밀번호가 필요해 이 세션에서는 nft 적용·트래픽 검증을 못 했다. `nft -c`(check)조차 cache init에 CAP_NET_ADMIN이 필요하다.

### 터미널 세션 1 — helper 실행

```sh
cargo build -p firecrab-net-helper
sudo FIRECRAB_NET_HELPER_SOCK=/tmp/firecrab-net.sock \
     FIRECRAB_NET_HELPER_ALLOWED_UID="$(id -u)" \
     ./target/debug/firecrab-net-helper
```

### 터미널 세션 2 — 정책 적용·확인

```sh
# bridge/firewall 먼저 준비
sudo python3 docs/tests/net-helper-client.py /tmp/firecrab-net.sock ensure_bridge
sudo python3 docs/tests/net-helper-client.py /tmp/firecrab-net.sock ensure_firewall

# per-VM 정책은 apply_vm_policy가 vm_id/ipv4/mac/egress를 요구하므로,
# TAP 자동화(task-vm-tap-automation)와 lease 연동 후 통합 테스트에서 실 트래픽 검증
sudo nft list table bridge firecrab_l2
sudo nft list table inet firecrab
```

트래픽 검증(별도 KVM 환경, task-network-ssh-ui-tests):
- VM에서 source IP/MAC 위조 → 차단
- VM끼리 ping/SSH → 차단
- VM→host 관리 IP → 차단
- VM→외부 IP/DNS → 통과, 다른 VM에 무영향

## 정리

세션 1에서 `Ctrl-C`. table은 helper가 안 지우므로 필요 시:

```sh
sudo nft delete table inet firecrab
sudo nft delete table bridge firecrab_l2
```
