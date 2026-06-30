# MicroVM 네트워크 기본

Serial Console로 MicroVM에 접속한 뒤 네트워크를 확인하는 기준 문서다.

## 선행

1. `scripts/serial-console.sh`
   - `ubuntu-rootfs.ext4`로 부팅한다.
   - Host에 `tap0`가 있으면 MicroVM 네트워크 장치로 자동 연결한다.

2. `scripts/boot-microvm.sh`
   - `tap0`가 연결된 실행에서는 `firecracker-config.json`에 `network-interfaces` 항목을 만든다.

3. `ubuntu-rootfs.ext4`
   - 부팅, 직렬 콘솔, 네트워크 확인에 필요한 기본 패키지를 포함한다.



## 연결 구조

```text
Host tap0 (172.16.20.1/24)
  -> Firecracker network-interfaces
    -> MicroVM eth0 (172.16.20.2/24)
```

연결 방식은 다음 순서다.

1. Host에서 TAP 장치 `tap0`를 만든다.
2. Host에서 IPv4 forwarding과 NAT masquerade를 설정한다.
3. `scripts/serial-console.sh`를 실행한다.
4. `scripts/boot-microvm.sh`가 `/sys/class/net/tap0`를 감지해 `firecracker-config.json`의 `network-interfaces`에 자동으로 추가한다.
5. Guest에서 `systemd-networkd`가 `eth0` IP 주소와 기본 route를 자동으로 설정한다.

## 필요한 작업

1. Host TAP 장치 추가

```sh
# 현재 사용자 소유의 TAP 장치 tap0를 만든다.
sudo ip tuntap add dev tap0 mode tap user "$(id -un)"

# Host 쪽 tap0에 게이트웨이 IP를 설정한다.
sudo ip addr replace 172.16.20.1/24 dev tap0

# tap0 링크를 활성화한다.
sudo ip link set tap0 up
```

이미 `tap0`가 있으면 첫 번째 명령은 생략하고 `ip addr replace`부터 실행한다.

2. Host NAT 설정

```sh
# Host의 기본 인터넷 연결 인터페이스 이름을 찾는다.
HOST_IFACE=$(ip route show default | awk '{print $5; exit}')

# Host에서 IPv4 패킷 전달을 켠다.
sudo sysctl -w net.ipv4.ip_forward=1

# Guest 대역에서 나가는 패킷을 Host 외부 인터페이스 주소로 NAT 처리한다.
sudo iptables -t nat -C POSTROUTING -s 172.16.20.0/24 -o "$HOST_IFACE" -j MASQUERADE 2>/dev/null || \
  sudo iptables -t nat -A POSTROUTING -s 172.16.20.0/24 -o "$HOST_IFACE" -j MASQUERADE

# Guest에서 외부망으로 나가는 forwarding을 허용한다.
sudo iptables -C FORWARD -i tap0 -o "$HOST_IFACE" -j ACCEPT 2>/dev/null || \
  sudo iptables -A FORWARD -i tap0 -o "$HOST_IFACE" -j ACCEPT

# 외부망에서 돌아오는 응답 패킷을 Guest로 전달한다.
sudo iptables -C FORWARD -i "$HOST_IFACE" -o tap0 -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT 2>/dev/null || \
  sudo iptables -A FORWARD -i "$HOST_IFACE" -o tap0 -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT
```

이 설정은 재부팅 후 사라질 수 있는 런타임 설정이다.

3. Serial Console 실행

```sh
# MicroVM을 serial console로 부팅한다.
./scripts/serial-console.sh
```

4. Guest eth0 자동 설정 확인

```sh
# Guest eth0에 172.16.20.2/24가 설정됐는지 확인한다.
ip addr show eth0

# Guest 기본 route가 Host tap0를 향하는지 확인한다.
ip route

# systemd-networkd 상태를 확인한다.
systemctl status systemd-networkd
```

## 확인

```sh
# Guest eth0 주소와 링크 상태를 확인한다.
ip addr show eth0

# Guest에서 Host tap0까지 통신되는지 확인한다.
ping 172.16.20.1

# Guest에서 NAT를 통해 외부 IP까지 통신되는지 확인한다.
ping 1.1.1.1
```

DNS까지 확인하려면 guest에서 resolver를 지정한 뒤 도메인으로 테스트한다.

```sh
# Guest DNS resolver를 Cloudflare DNS로 설정한다.
printf 'nameserver 1.1.1.1\n' > /etc/resolv.conf

# Guest에서 DNS 이름 해석과 외부 통신을 함께 확인한다.
ping google.com
```

## Guest 패키지 설치

rootfs 생성 과정에서 apt index는 정리되므로, guest에서 패키지를 설치하기 전에 `apt update`를 먼저 실행한다.

```sh
# apt 패키지 목록을 다시 받는다.
apt update

# htop 패키지를 설치한다.
apt install -y htop
```

`Unable to locate package htop`이 나오면 `apt update`가 성공했는지 먼저 확인한다.
`apt update`에서 DNS 오류가 나면 `/etc/resolv.conf` 설정을 다시 확인하고, 연결 오류가 나면 Host NAT 설정을 다시 확인한다.

## SSH 접속

SSH 접속 방법은 [MicroVM SSH 접속](ssh-access.md)을 따른다.

## DNS 오류 진단

`Temporary failure resolving 'archive.ubuntu.com'`이 나오면 guest에서 DNS와 외부 IP 연결을 분리해서 확인한다.

```sh
# Guest 기본 route가 Host tap0를 향하는지 확인한다.
ip route

# Guest resolver 설정을 확인한다.
cat /etc/resolv.conf

# 외부 IP까지 NAT가 되는지 확인한다.
ping 1.1.1.1

# DNS 이름 해석이 되는지 확인한다.
getent hosts archive.ubuntu.com
```

`ping 1.1.1.1`은 성공하지만 `getent hosts archive.ubuntu.com`이 실패하면 guest resolver를 다시 쓴다.

```sh
# Guest DNS resolver를 다시 설정한다.
printf 'nameserver 1.1.1.1\n' > /etc/resolv.conf

# apt 패키지 목록을 다시 받는다.
apt update
```

`ping 1.1.1.1`도 실패하면 Host에서 NAT 설정을 다시 적용한다.

```sh
# Host의 기본 인터넷 연결 인터페이스 이름을 다시 찾는다.
HOST_IFACE=$(ip route show default | awk '{print $5; exit}')

# IPv4 forwarding을 다시 켠다.
sudo sysctl -w net.ipv4.ip_forward=1

# NAT masquerade 규칙을 다시 보장한다.
sudo iptables -t nat -C POSTROUTING -s 172.16.20.0/24 -o "$HOST_IFACE" -j MASQUERADE 2>/dev/null || \
  sudo iptables -t nat -A POSTROUTING -s 172.16.20.0/24 -o "$HOST_IFACE" -j MASQUERADE

# Guest에서 외부망으로 나가는 forwarding 규칙을 다시 보장한다.
sudo iptables -C FORWARD -i tap0 -o "$HOST_IFACE" -j ACCEPT 2>/dev/null || \
  sudo iptables -A FORWARD -i tap0 -o "$HOST_IFACE" -j ACCEPT

# 외부망 응답이 Guest로 돌아오는 forwarding 규칙을 다시 보장한다.
sudo iptables -C FORWARD -i "$HOST_IFACE" -o tap0 -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT 2>/dev/null || \
  sudo iptables -A FORWARD -i "$HOST_IFACE" -o tap0 -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT
```

## Reboot 후 eth0가 DOWN일 때

최신 rootfs는 `systemd-networkd`로 `eth0`를 자동 설정한다.
기존 rootfs를 쓰고 있다면 rootfs를 다시 만든 뒤 MicroVM을 다시 부팅한다.

```sh
# Host에서 rootfs를 다시 만든다.
./scripts/install-ubuntu-roofs.sh

# Host에서 MicroVM을 다시 부팅한다.
./scripts/serial-console.sh
```

실행 중인 guest에서 임시로 적용하려면 다음 파일과 서비스를 직접 설정한다.

```sh
# Guest에 systemd-networkd용 eth0 정적 설정을 만든다.
mkdir -p /etc/systemd/network

# Guest eth0 주소, gateway, DNS를 기록한다.
cat > /etc/systemd/network/10-eth0.network <<'EOF'
[Match]
Name=eth0

[Network]
Address=172.16.20.2/24
Gateway=172.16.20.1
DNS=1.1.1.1
EOF

# systemd-networkd를 지금 켜고 다음 부팅에도 켜지게 한다.
systemctl enable --now systemd-networkd

# eth0와 route가 올라왔는지 확인한다.
ip addr show eth0
ip route
```

`ip`, `ping`, `reboot`, `/sbin/init` 중 하나가 없으면 rootfs를 다시 생성한다.

```sh
# 부팅 가능한 Ubuntu rootfs 이미지를 다시 만든다.
./scripts/install-ubuntu-roofs.sh
```

`firecracker-config.json`은 실행 때마다 다시 생성되므로 직접 수정하지 않는다.
