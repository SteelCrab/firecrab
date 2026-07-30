---
tags:
  - firecrab
  - overview
updated: 2026-07-30
---

# AWS 대응표

> [!summary] 왜 이 표가 있나
> firecrab은 AWS의 개념 구조를 의도적으로 따라간다.
> "이게 AWS의 무엇에 해당하는가"를 알면 나머지 문서가 훨씬 빨리 읽힌다.
> 각 태스크 문서에도 자기 부분의 대응표가 있고, 여기는 그걸 모은 곳이다.

## 리소스

| AWS | firecrab | 설명 |
|---|---|---|
| EC2 | **M2 (MicroMachine)** | MicroVM 기반 컴퓨팅 |
| VPC | **MicroNetwork** | 가상 네트워크 |
| EBS | **MicroStorage** | 영구 스토리지 |
| AMI | **M2Image** | 인스턴스 이미지 |
| Snapshot | **Snapshot** | 스토리지 백업 |
| RDS | **MicroDB** | 관리형 데이터베이스 |

## 네트워크

| firecrab | AWS | 어디에 |
|---|---|---|
| MicroNetwork | VPC + Subnet(통합) | [task-micro-network](../30-tasks/task-micro-network.md) |
| bridge + gateway 주소 | Subnet의 암묵적 라우터 | [task-shared-bridge-network](../30-tasks/task-shared-bridge-network.md) |
| IPAM lease(IP+MAC) | ENI에 붙는 private IP | [task-vm-ip-mac-allocation](../30-tasks/task-vm-ip-mac-allocation.md) |
| TAP 디바이스 | ENI | [task-vm-tap-automation](../30-tasks/task-vm-tap-automation.md) |
| TAP alias `firecrab:<vm-id>` | ENI attachment 메타데이터 | 〃 |
| VM별 nftables 정책 | Security Group | [task-vm-network-isolation](../30-tasks/task-vm-network-isolation.md) |
| dnsmasq 고정 예약 | VPC 내장 DHCP + DHCP Option Set | [task-guest-network-configuration](../30-tasks/task-guest-network-configuration.md) |
| postrouting masquerade | NAT Gateway | [task-nat-firewall-automation](../30-tasks/task-nat-firewall-automation.md) |
| MicroNetwork 인터넷 on/off | Internet Gateway attach/detach | [task-micro-network](../30-tasks/task-micro-network.md) |
| MicroNetwork 간 통신 차단 | VPC 간 기본 격리(peering 없음) | 〃 |
| 리스가 남은 네트워크 삭제 거부 | ENI가 남은 Subnet 삭제 거부 | 〃 |

## 수명주기

| firecrab | AWS |
|---|---|
| `create` → `start` → `stop` → `delete` | RunInstances → Start → Stop → Terminate |
| `stop`: TAP 삭제, 리스 유지 | Stop: private IP 유지, 연결만 해제 |
| `start`: 같은 리스로 TAP 재생성 | Start: 같은 private IP로 재연결 |
| `delete`: 리스 반납 | Terminate: ENI 삭제, IP 반납 |
| 게스트 자체 종료 감시 | 인스턴스 상태 확인(status check) |
| serial console `FIRECRAB_NETWORK_READY` | reachability check |

## 이미지·템플릿

| firecrab | AWS |
|---|---|
| 템플릿 레지스트리(alias → 고정 버전) | AMI ID + alias |
| 템플릿 아티팩트 SHA256 검증 | AMI 무결성 |
| golden image에서 SSH 호스트키·machine-id 제거 | AMI 클론 시 cloud-init 재생성 |
| 결정론적 hostname `fc-<hex>` | 기본 프라이빗 DNS 이름(`ip-172-30-x-x`) |

## 대응이 **없는** 것

| 개념 | 왜 없나 |
|---|---|
| AZ(가용 영역) | 단일 호스트라 장애 도메인이 하나뿐. MicroNetwork가 VPC와 Subnet을 겸하는 이유 |
| VPC Peering | MicroNetwork 간 연결은 아직 범위 밖 |
| IAM | 인증·권한은 미착수([task-api-authentication](../30-tasks/task-api-authentication.md)) |
| 리전 | 단일 호스트 |
