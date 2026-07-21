# NAT·firewall 자동화 테스트

## 자동 테스트 (root 불필요)

```sh
cargo test -p firecrab-net-helper firewall::
```

## 확인 항목

- 소유 table 2개만 선언 — `inet firecrab`, `bridge firecrab_l2` (host `flush ruleset` 없음)
- base chain(`forward_dispatch`, `postrouting_dispatch`) policy `accept`, `fcbr0`/subnet traffic만 regular chain으로 jump
- masquerade·`ct state established,related accept`는 regular chain에만 있음
- `add table` + `flush table`로 idempotent — host 다른 table은 안 건드림
- uplink 이름에 `"`/`\`/`;` 등 섞이면 `nft` 호출 전에 거부
- remove는 `add table` 후 `delete table` — 없어도 에러 안 남
- uplink 탐지(`detect_uplink`)는 실제 rtnetlink 기본 route 조회로 검증(root 불필요)
- `ensure_firewall`가 같은 uplink면 `nft` 자체를 안 부르고 skip (실제 흐름으로 검증, root 불필요)

## 수동 확인 (root 필요, 여기선 미실행)

sudo 비밀번호가 필요해 이 세션에서는 직접 실행하지 못했다. 터미널 2개로 확인한다.

### 터미널 세션 1 — helper 실행

```sh
cargo build -p firecrab-net-helper
sudo FIRECRAB_NET_HELPER_SOCK=/tmp/firecrab-net.sock \
     FIRECRAB_NET_HELPER_ALLOWED_UID="$(id -u)" \
     ./target/debug/firecrab-net-helper
```

`[INFO] net-helper listening on /tmp/firecrab-net.sock` 로그가 나오면 이 터미널은 그대로 둔다.

### 터미널 세션 2 — 요청·결과 확인

```sh
sudo python3 docs/tests/net-helper-client.py /tmp/firecrab-net.sock ensure_firewall
```

기대: `{'version': 1, 'request_id': '...', 'result': {'Ok': None}}`

```sh
sh docs/tests/nat-firewall-automation.sh
```

`inet firecrab`, `bridge firecrab_l2` table 내용을 출력한다. `nft`가 설치돼 있어야 한다.

## 정리

터미널 세션 1에서 `Ctrl-C`로 helper를 종료하면 socket 파일이 제거된다. table은 helper가 지우지 않으므로 필요하면 직접 지운다.

```sh
sudo nft delete table inet firecrab
sudo nft delete table bridge firecrab_l2
```
