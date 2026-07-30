---
tags:
  - firecrab
  - host
  - diagnostics
status: 미완료
scope: 4주차
updated: 2026-07-29
---

# Host 진단 — `firecrab doctor`

> [!summary] 한 줄 요약
> 실패 원인이 대부분 **host 설정**인데, 지금은 증상만 보고 문서를 뒤져야 한다.
> host 상태를 한 번에 점검하는 명령을 만든다.

## 왜

실제로 겪은 실패가 전부 코드가 아니라 host 문제였다.

| 증상 | 원인 |
|---|---|
| guest가 `no-ipv4-address` | UFW가 새 브리지의 DHCP를 막음 ([troubleshooting](../20-guides/troubleshooting.md)) |
| VM 아웃바운드 타임아웃 | UFW가 라우팅 기본 거부 ([bug](../50-bugs/vm-outbound-forward-blocked-by-ufw.md)) |
| API가 helper에 연결 실패 | 소켓 그룹 권한 |
| 없는 컬럼 오류 | 다른 디렉터리에서 실행해 옛 DB를 염 |

## 작업

`firecrab doctor`(또는 `install.sh --check`)가 점검·출력:

- KVM 접근(`/dev/kvm`), firecracker 바이너리·버전
- `net.ipv4.ip_forward`, `nft` 존재와 firecrab 테이블 유무
- dnsmasq 프로세스와 서빙 인터페이스, helper 소켓 존재·권한
- UFW가 켜져 있으면 firecrab 브리지에 대한 허용 규칙 유무
- 데이터 루트 경로·여유 공간, 이미지 파일 존재와 digest
- 각 항목마다 **무엇을 하면 되는지** 한 줄 조치 안내

## 완료 기준

- 위 표의 네 가지 실패를 각각 재현했을 때 doctor가 원인을 정확히 지목
- 전부 정상이면 통과 요약만 출력(잡음 없음)
- root 없이 실행 가능(권한이 필요한 항목은 "확인 불가"로 구분 표시)

## 참고

- [호스트 방화벽 연동 정책](task-micro-network-host-firewall.md)의 (a)안을 택하면 그 진단이 여기 들어감
