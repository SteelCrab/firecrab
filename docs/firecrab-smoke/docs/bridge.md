# 공용 bridge (fcbr0) smoke

`ensure_bridge` 실동작 확인. root 필요.

framing만 확인하려면 [net-helper.md](net-helper.md) 먼저.

## 확인 항목

- 생성: `fcbr0` UP, `172.30.0.1/24`, alias `firecrab:bridge:v1`
- idempotent 반복 실행 → 주소 중복 없음
- 비-Firecrab 동명 interface → `not_owned` 실패, 미변경
- IPv6 비활성
- API 시작 시 재검증·주소 복원
- 재부팅 후 복구, 기존 host bridge/route 보존

## 준비

```sh
cargo build -p firecrab-net-helper
sudo FIRECRAB_NET_HELPER_SOCK=/tmp/firecrab-net.sock \
     FIRECRAB_NET_HELPER_ALLOWED_UID="$(id -u)" \
     ./target/debug/firecrab-net-helper
```

## 1. 생성

```sh
python3 docs/firecrab-smoke/script/net-helper.py /tmp/firecrab-net.sock ensure_bridge

ip -d link show fcbr0                              # UP, alias firecrab:bridge:v1
ip addr show fcbr0                                  # 172.30.0.1/24
cat /proc/sys/net/ipv6/conf/fcbr0/disable_ipv6      # 1
ip -6 addr show dev fcbr0                           # 출력 없음
```

## 2. idempotent 재실행

```sh
python3 docs/firecrab-smoke/script/net-helper.py /tmp/firecrab-net.sock ensure_bridge
python3 docs/firecrab-smoke/script/net-helper.py /tmp/firecrab-net.sock ensure_bridge
```

`ip addr show fcbr0` — 주소 중복 없어야 함.

## 3. 주소 누락 복원

```sh
sudo ip addr del 172.30.0.1/24 dev fcbr0
python3 docs/firecrab-smoke/script/net-helper.py /tmp/firecrab-net.sock ensure_bridge
ip addr show fcbr0   # 복원 확인
```

## 4. 소유권 충돌

```sh
sudo ip link del fcbr0
sudo ip link add fcbr0 type dummy

python3 docs/firecrab-smoke/script/net-helper.py /tmp/firecrab-net.sock ensure_bridge
# 기대: result.Err.code == not_owned

ip -d link show fcbr0   # dummy 그대로, 미변경 확인
sudo ip link del fcbr0  # 정리
```

## 5. API 재검증

```sh
FIRECRAB_NET_HELPER_SOCK=/tmp/firecrab-net.sock cargo run -p firecrab-api
```

helper 없이 실행 시 종료 로그: `failed to verify the Firecrab bridge through network helper`

## 6. 재부팅 복구

재부팅 → helper/API 재시작 → 1번과 동일 구성 복구, 기존 host bridge·route 보존.

IPv4 forwarding은 helper가 관리하지 않는 전역 설정이다.

```sh
sudo sysctl -w net.ipv4.ip_forward=1
echo 'net.ipv4.ip_forward = 1' | sudo tee /etc/sysctl.d/99-firecrab.conf
```

## 정리

```sh
sudo ip link del fcbr0
```
