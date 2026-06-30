# Firecrab 문서

Firecracker MicroVM을 준비하고 부팅한 뒤 네트워크와 SSH 접속까지 확인하는 문서 모음이다.

## 처음 실행 순서

1. KVM 호스트 환경 확인

```sh
./scripts/kvm-check.sh
```

2. Firecracker 설치

```sh
./scripts/firecracker-menual/install-firecracker.sh
firecracker --version
```

3. 커널 이미지 준비

```sh
./scripts/firecracker-menual/install-linux-kernel.sh
```

4. Ubuntu rootfs 준비

```sh
./scripts/firecracker-menual/install-ubuntu-roofs.sh
```

5. Host TAP 장치 준비

```sh
sudo ip tuntap add dev tap0 mode tap user "$(id -un)"
sudo ip addr replace 172.16.20.1/24 dev tap0
sudo ip link set tap0 up
```

6. Host NAT 설정

```sh
# Host에서 인터넷으로 나가는 기본 네트워크 인터페이스 이름을 찾는다.
HOST_IFACE=$(ip route show default | awk '{print $5; exit}')

# Guest 패킷을 Host가 외부로 전달할 수 있게 IPv4 forwarding을 켠다.
sudo sysctl -w net.ipv4.ip_forward=1

# Guest 대역에서 외부로 나가는 패킷을 Host 주소로 NAT 처리한다.
sudo iptables -t nat -C POSTROUTING -s 172.16.20.0/24 -o "$HOST_IFACE" -j MASQUERADE 2>/dev/null || \
  sudo iptables -t nat -A POSTROUTING -s 172.16.20.0/24 -o "$HOST_IFACE" -j MASQUERADE

# Guest에서 외부망으로 나가는 forwarding을 허용한다.
sudo iptables -C FORWARD -i tap0 -o "$HOST_IFACE" -j ACCEPT 2>/dev/null || \
  sudo iptables -A FORWARD -i tap0 -o "$HOST_IFACE" -j ACCEPT

# 외부망에서 돌아오는 응답 패킷을 Guest로 전달한다.
sudo iptables -C FORWARD -i "$HOST_IFACE" -o tap0 -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT 2>/dev/null || \
  sudo iptables -A FORWARD -i "$HOST_IFACE" -o tap0 -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT
```

7. Serial Console로 MicroVM 부팅

```sh
./scripts/firecracker-menual/serial-console.sh
```

8. Guest DNS 설정

```sh
printf 'nameserver 1.1.1.1\n' > /etc/resolv.conf
```

9. Guest 네트워크 확인

```sh
ip addr show eth0
ip route
ping 172.16.20.1
ping 1.1.1.1
ping google.com
```

10. Host에서 SSH 접속

```sh
ssh-keygen -f "$HOME/.ssh/known_hosts" -R '172.16.20.2'
ssh root@172.16.20.2
```

## 문서

- [이미지 준비](image-setup.md): 커널 이미지와 Ubuntu rootfs 생성
- [Serial Console](serial-console.md): MicroVM 부팅과 콘솔 접속
- [네트워크 기본](network-basic.md): TAP, NAT, DNS, apt 문제 확인
- [SSH 접속](ssh-access.md): host key 초기화와 SSH 접속
- [boot-microvm](boot-microvm.md): Firecracker 실행 스크립트 동작
- [실행 로그 캡처](run-log-capture.md): 이전 실행 로그 분석 기록
