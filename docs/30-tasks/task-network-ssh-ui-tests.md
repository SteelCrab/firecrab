---
tags:
  - firecrab
  - network
status: 보류
updated: 2026-07-23
---

# Network·SSH·UI 통합 테스트

실제 KVM host에서 멀티 VM E2E 검증.

## 참고

이 task는 AWS의 특정 서비스에 대응되진 않는다 — 지금까지 만든 네트워크 기능들(IPAM, NAT, 방화벽, TAP, SSH)이 실제 VM들 사이에서 다 같이 맞물려 돌아가는지 확인하는 우리 자체 통합 테스트다. AWS로 치면 "여러 EC2 인스턴스를 실제로 띄워서 서로 통신·격리·SSH가 다 기대대로 되는지" 사람이 콘솔로 하나하나 확인하는 걸 자동화하는 것과 같다고 보면 된다.

## 작업

- harness: 공용 teardown에서 stop/delete cleanup (`Drop` 미의존, 오류 수집 후 원 결과와 함께 보고), 이전 run의 UUID namespace 잔여물만 정리
- 시나리오: 동시 2 VM 생성·시작 → rootfs/socket/PID/TAP/MAC/IP 상이 확인 → gateway·외부 IP·DNS 통신 → 등록 key SSH(host fingerprint 상이) → 한 VM stop/delete 후 다른 VM 유지 → spoofing·VLAN/IPv6·east-west·reserved subnet 차단 → agent reconnect·crash·API 재시작 후 상태 수렴
- `FIRECRAB_KVM_TEST=1` gate + `/run/lock/firecrab-kvm-test.lock`으로 동시 실행 방지, 사전 조건(KVM/binary/image/helper 권한) 검사
- 브라우저 E2E: thirtyfour(WebDriver)로 대시보드 lifecycle 조작
- CI 일반 job은 fake runtime test만, KVM job은 self-hosted runner에서 선택 실행

## 완료 기준

- 위 시나리오 전부 자동 통과, 실패 시 진단 bundle(민감 정보 제거) 수집
- crash·재시작 후 host 자원(TAP/rule/lease) 정리 확인

## 산출물

`firecrab-api/tests/kvm_network_ssh.rs`, `firecrab-frontend/tests/dashboard.rs`, `docs/network-ssh-ui-smoke.md`
