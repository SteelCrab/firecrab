---
tags:
  - firecrab
  - overview
updated: 2026-07-30
---

# 용어집

> [!summary] 무엇을 담나
> 문서와 코드에 자주 나오지만 **일반적인 뜻과 다르게 쓰거나**, 이 프로젝트가
> 직접 만든 말만 모았다. AWS 대응은 [AWS 대응표](aws-mapping.md)에 있다.

## 리소스 이름

| 용어 | 뜻 |
|---|---|
| **M2 / MicroMachine** | firecrab의 컴퓨팅 단위. 문서·코드에서는 대개 그냥 `VM`으로 쓴다 |
| **MicroNetwork** | 사용자가 만드는 가상 네트워크. bridge 하나 + subnet 하나 + 그 위의 DHCP/NAT/방화벽을 묶은 것 |
| **MicroStorage** | VM에 붙이는 영구 스토리지(4주차 범위) |
| **M2Image** | 실행 중인 VM에서 떠낸 인스턴스 이미지(4주차 범위) |
| **기본 네트워크** | MicroNetwork를 고르지 않은 VM이 붙는 내장 네트워크. bridge `fcbr0`, `172.30.0.0/24` |

## 네트워크

| 용어 | 뜻 |
|---|---|
| **lease** | 한 VM에 배정된 IP+MAC 한 쌍. 생성 시 할당되고 **stop해도 유지**되며 delete에서만 반납된다 |
| **IPAM** | lease를 겹치지 않게 나눠주는 부분. SQLite 트랜잭션으로 동시 생성에도 중복이 안 나게 한다 |
| **TAP** | VM 하나에 하나씩 만드는 가상 NIC. 이름은 `fct` + vm-id 해시 |
| **bridge** | TAP들이 붙는 가상 스위치. 기본 네트워크는 `fcbr0`, MicroNetwork는 `mnb` + id 해시 |
| **uplink** | 호스트가 실제로 외부로 나가는 인터페이스. IPv4 기본 경로에서 자동 판별 |
| **egress policy** | VM 단위 외부 통신 posture. `internet`(허용) 또는 `isolated`(차단) |
| **east-west** | VM ↔ VM 통신. 기본적으로 차단된다 |
| **anti-spoofing** | 자기 lease의 IP/MAC이 아닌 출발지로는 못 보내게 막는 규칙 |

## 실행·부팅

| 용어 | 뜻 |
|---|---|
| **golden image** | 배포용으로 미리 만들어 둔 rootfs 템플릿. SSH 호스트키·machine-id 등 인스턴스별 값을 미리 지워둔다 |
| **template alias / version** | `ubuntu-26.04` 같은 별칭과, 그게 생성 시점에 고정된 실제 버전 |
| **sentinel** | 게스트가 serial console에 찍는 약속된 문자열. `FIRECRAB_NETWORK_READY` / `FIRECRAB_NETWORK_FAILED` |
| **startup step** | 시작 과정의 이름 붙은 단계(디스크 준비 → 설정 생성 → 프로세스 시작 → 네트워크 확인) |
| **exit monitor** | Firecracker 프로세스를 지켜보다 게스트가 스스로 꺼지거나 죽으면 상태·네트워크를 정리하는 태스크 |

> [!note] guest agent가 없다
> 게스트 안에 에이전트를 넣지 않았다. 그래서 "네트워크가 됐는지"를 물어볼 상대가 없고,
> 대신 게스트가 serial console에 **sentinel**을 찍게 해서 그걸 읽는다.
> 패키지 업데이트도 같은 방식이다.

## 특권 경계

| 용어 | 뜻 |
|---|---|
| **net-helper** | 네트워크 변경만 대신하는 특권 프로세스. API는 여기에 정해진 요청만 보낼 수 있다 |
| **신뢰 경계** | API가 보낸 값을 helper가 **다시 검증하는** 지점. API가 이미 검사했어도 helper는 자기 기준으로 또 본다 |
| **결정론적 이름** | 인터페이스 이름을 UUID 해시에서 유도하는 방식. 이름이 문자열로 넘어갈 일이 없어진다 |

## 상태값

| `status`(문서) | 뜻 |
|---|---|
| `미완료` | 계획됐고 아직 안 함 |
| `진행 중` | 일부 완료 |
| `완료` | 완료 |
| `보류` | 어느 주차에도 안 걸림 — 범위 밖으로 미룬 것 |

| `state`(VM) | 뜻 |
|---|---|
| `created` | 레코드만 있고 아직 한 번도 안 띄움 |
| `starting` | 시작 파이프라인 진행 중 |
| `running` | 프로세스가 떠 있고 게스트가 부팅됨 |
| `stopping` / `stopped` | 종료 요청 / 종료 확인됨 |
| `error` | 예기치 않게 종료됐거나 시작에 실패 |
