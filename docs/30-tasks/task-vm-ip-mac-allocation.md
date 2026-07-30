---
tags:
  - firecrab
  - network
  - ipam
status: 완료
scope: 3주차
updated: 2026-07-23
---

# VM IP·MAC 할당 (IPAM)

SQLite에서 VM별 IPv4와 MAC을 원자적으로 할당.

## AWS로 비유하면

| firecrab | AWS 대응 |
|---|---|
| 이 task 전체 | **Amazon VPC IPAM**(IP Address Manager) — AWS에 실제로 있는, IP 중복 할당을 막아주는 서비스와 같은 역할을 직접 구현 |
| `network_leases` 테이블의 IPv4 | ENI에 자동 할당되는 private IP |
| UUID 기반 MAC 생성 | ENI 생성 시 AWS가 자동으로 붙여주는 고유 MAC 주소 |
| pool 고갈 시 `PoolExhausted` | 서브넷의 사용 가능한 IP가 소진됐을 때 AWS가 `InsufficientFreeAddressesInSubnet` 에러를 내는 것과 동일 |
| lease는 stop 동안 유지, delete 후 반환 | 인스턴스 Stop은 private IP 유지, Terminate라야 IP가 서브넷 pool로 반환 |

## 작업

- `network_leases` 테이블 + partial unique index — active lease 기준 vm_id/ipv4/mac 유일, history row 보존
- `BEGIN IMMEDIATE` transaction으로 할당 직렬화: 빈 IP 탐색(gateway·network·broadcast·예약 제외) → insert → commit
  - **왜 직렬화하나**: 여러 VM이 동시에 생성될 때 두 요청이 같은 빈 IP를 동시에 집어가는 경쟁(race)을 막기 위함
- MAC: UUID hash 기반 `02:FC:xx:xx:xx:xx`, unique 충돌 시 salt 증가·제한 재시도
- pool 고갈 시 `PoolExhausted` 오류
- lease는 stop 동안 유지, delete 정리(policy·TAP·artifact) 성공 후에만 `released_at` 기록·반환

## 완료 기준

- 동시 다중 생성에도 IP/MAC 중복 없음
- stop/start 간 주소 유지, delete 완료 후에만 재사용
- 고갈·중복·rollback 단위 테스트

## 산출물

`firecrab-api/src/ipam.rs`, `firecrab-api/src/model.rs`, `firecrab-api/src/persistence.rs`
