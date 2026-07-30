---
tags:
  - firecrab
  - moc
  - test
updated: 2026-07-30
---

# 테스트 절차

> [!summary] 이 폴더의 규칙
> 한 파일 = 한 기능의 검증 절차. 구성은 늘 같다 —
> **자동 테스트**(root 불필요, `cargo test` 명령과 개수) →
> **확인 항목**(그 테스트가 실제로 덮는 것) →
> **수동 확인**(root/KVM 필요, 번호 붙인 터미널 세션) → **완료 기준 대조**.

> [!important] 개수까지 적는 이유
> `cargo test … # 19` 처럼 개수를 적어두면, 테스트가 사라졌을 때 문서가 조용히
> 거짓말하는 대신 눈에 띈다. 기능을 바꿨으면 이 숫자도 같이 고친다.

## 목록

| 테스트 | 대상 태스크 |
|---|---|
| [MicroNetwork](micro-network.md) | [task-micro-network](../30-tasks/task-micro-network.md) |
| [VM IP·MAC 할당(IPAM)](vm-ip-mac-allocation.md) | [task-vm-ip-mac-allocation](../30-tasks/task-vm-ip-mac-allocation.md) |
| [NAT·firewall 자동화](nat-firewall-automation.md) | [task-nat-firewall-automation](../30-tasks/task-nat-firewall-automation.md) |
| [VM network 격리·anti-spoofing](vm-network-isolation.md) | [task-vm-network-isolation](../30-tasks/task-vm-network-isolation.md) |
| [Host 설치](host-install.md) | [task-host-install-script](../30-tasks/task-host-install-script.md) |
| [MicroVM 부팅 + Terminal UI](microvm-terminal.md) | [task-microvm-terminal](../30-tasks/task-microvm-terminal.md) |
| [VM 시작 단계별 진행 상황](vm-startup-progress.md) | [task-vm-startup-progress](../30-tasks/task-vm-startup-progress.md) |
| [VM 상세 모달](vm-detail-modal.md) | [task-vm-startup-progress](../30-tasks/task-vm-startup-progress.md) |
| [VM 디스크 용량 설정](vm-disk-capacity.md) | [task-vm-disk-capacity](../30-tasks/task-vm-disk-capacity.md) |
| [VM CPU/MEM/DISK 수정](vm-resource-update.md) | [task-vm-resource-update](../30-tasks/task-vm-resource-update.md) |
| [Alpine 템플릿](alpine-linux-template.md) | [task-alpine-linux-template](../30-tasks/task-alpine-linux-template.md) |
| [React 프론트엔드 전환](frontend-react-migration.md) | [task-vm-dashboard](../30-tasks/task-vm-dashboard.md) |
| [CI/CD(GitHub Actions)](cicd-github-actions.md) | [task-cicd-github-actions](../30-tasks/task-cicd-github-actions.md) |
| [Guest 네트워크 스모크](guest-network-smoke.md) | [task-guest-network-configuration](../30-tasks/task-guest-network-configuration.md) |
| [VM별 TAP 자동화 스모크](vm-tap-automation-smoke.md) | [task-vm-tap-automation](../30-tasks/task-vm-tap-automation.md) |

## 보조 도구

| 파일 | 용도 |
|---|---|
| `net-helper-client.py` | net-helper에 요청을 직접 보내는 최소 클라이언트. API 없이 helper만 확인할 때 |
| `nat-firewall-automation.sh` | NAT·firewall 규칙 확인 절차 |
| `vm-ip-mac-allocation.py` | IPAM 동시 할당 확인 |

## 전체 실행

```sh
cargo test --workspace
```

기능별로 좁혀 돌리는 명령은 각 문서의 "자동 테스트" 절에 있다.
