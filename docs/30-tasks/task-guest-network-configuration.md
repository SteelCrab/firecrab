---
tags:
  - firecrab
  - network
status: 완료
scope: 3주차
updated: 2026-07-23
---

# Guest 네트워크 설정

이미 DB에 할당된 IP·MAC(IPAM lease)을, VM 안(guest)의 `eth0`가 실제로 받아서 쓰게 만드는 작업. 지금까지는 "IP를 예약"만 해놨고, guest 부팅 시 그 IP를 스스로 가져가게 하는 장치가 없는 상태.

## AWS로 비유하면

| firecrab (이 task) | AWS 대응 |
|---|---|
| host에 dnsmasq(DHCP 서버), MAC별 고정 IP 예약 | VPC에 내장된 DHCP 서버 |
| guest가 부팅 시 DHCP로 그 IP를 받음 | EC2 인스턴스가 부팅 시 DHCP로 ENI의 private IP를 받음 |
| DHCP가 gateway·DNS도 같이 알려줌 | VPC **DHCP Option Set** (게이트웨이=VPC 라우터, DNS=AmazonProvidedDNS) |
| template에서 SSH host key·machine-id·random seed 제거 | AMI가 클론될 때마다 cloud-init이 이걸 매번 새로 생성 |
| guest agent가 "네트워크 됐다"고 알려줘야 완료 처리 | EC2 status check의 reachability check |
| hostname = `fc-<uuid 일부>` | EC2 기본 프라이빗 DNS 이름(`ip-172-30-x-x`류 자동 생성) |

즉 이 task는 AWS가 VPC 안에서 인스턴스마다 자동으로 해주는 "DHCP + 인스턴스별 고유화(cloud-init)"를, firecrab에서 직접 만드는 것.

## 작업

- **DHCP 서버**: host bridge 전용 dnsmasq. MAC 주소별로 고정 IP를 예약해두는 방식(=DB의 lease를 그대로 반영) — guest가 DHCP로 물어보면 항상 같은 IP를 받음
  - **왜 이 방식인가**: guest의 rootfs(디스크 이미지) 안에 직접 IP 설정 파일을 써넣는 방법도 있지만, 그러려면 모든 VM 디스크에 쓰기 권한이 필요하고 잘못됐을 때 되돌리기도 어려움. DHCP는 host 쪽에서만 관리되니 권한 범위가 훨씬 좁고, 잘못돼도 host 파일 하나만 롤백하면 됨
- **dnsmasq용 설정 파일 갱신 절차**: 지금 활성 상태인 lease 전체를 읽어와서 → 파일로 씀 → fsync(디스크에 확실히 반영) → 문법 검사 → (심볼릭 링크 교체 방식으로) 한 번에 교체 → dnsmasq reload. 이 중 하나라도 실패하면 이전 버전 파일로 되돌림
- **동시 갱신 충돌 방지**: DB 버전(revision) 번호를 같이 확인해서, 오래된 lease 목록이 최신 것을 덮어쓰지 않게 함. lease 개수·파일 크기에도 상한을 둠(무한정 커지는 것 방지)
- **dnsmasq 격리**: 전용 시스템 계정 + 전용 systemd 유닛으로 실행하고, firecrab bridge(`fcbr0`)에만 bind — 호스트의 다른 네트워크 인터페이스에는 절대 DHCP/DNS를 열지 않음
- **Firecracker 설정 연동**: `guest_mac`은 lease의 MAC, `host_dev_name`은 그 VM의 TAP 이름 (이미 구현된 TAP 자동화와 그대로 연결)
- **guest hostname**: `fc-<uuid 일부>` 형식으로 자동 생성 — 사용자가 입력한 VM 이름을 그대로 쓰지 않음(문자 이스케이프·중복 문제 방지)
- **golden 이미지 준비**: 템플릿 이미지에서 `eth0` DHCP를 기본 활성화해두고, SSH host key·machine-id·이전 DHCP lease 파일·random seed는 미리 지워둠 — 안 지우면 모든 VM이 같은 값을 복제해서 쓰게 되어 보안 문제(같은 SSH host key 등) 발생
- **네트워크 준비 완료 판정**: host가 DHCP lease를 내줬다는 것만으로 "됐다"고 판단하지 않음 — guest 안의 agent가 실제로 `eth0`/gateway/DNS까지 확인해서 보고해야 완료 처리

## 완료 기준

- DB의 lease(IP/MAC) = Firecracker 설정의 MAC = guest `eth0`의 실제 주소, 셋 다 일치
- VM을 재부팅해도 같은 IP·gateway·DNS 유지, 외부에서 hostname으로 조회 가능
- DNS가 안 되면 "네트워크 준비" 단계에서 바로 실패 처리(조용히 넘어가지 않음)

## 산출물

`firecrab-api/src/dhcp.rs`, `firecrab-net-helper/src/dhcp.rs`, guest template 설정, `docs/guest-network-smoke.md`
