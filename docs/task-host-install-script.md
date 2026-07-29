---
tags:
  - firecrab
  - host
  - install
status: 미완료
scope: 4주차
updated: 2026-07-29
---

# Host 설치 — `install.sh`

> [!summary] 한 줄 요약
> 지금 firecrab을 새 머신에 올리려면 문서를 보며 손으로 여러 단계를 밟아야 한다.
> 한 번 실행으로 끝나는 설치 스크립트를 만든다.

## 왜

- 흩어져 있는 것들: [firecracker 설치](../scripts/install-firecracker.sh),
  [KVM 확인](../scripts/kvm-check.sh), rootfs 빌드 스크립트, net-helper 실행 방법,
  `/run/firecrab` 디렉터리와 소켓 그룹 권한
- 실제로 이번 주에만 소켓 그룹을 잘못 줘서 API가 helper에 붙지 못하는 일이 있었음
  (`sudo -u root -g <group>`을 빠뜨리면 재현)
- 데모·심사 환경에서 "clone → install → 실행"이 안 되면 아무것도 못 보여줌

## 작업

- `install.sh` — 의존성 확인·안내(KVM, `nft`, `dnsmasq`, `firecracker`, `e2fsprogs`)
- 서비스 계정/그룹 생성, `/etc/firecrab`, `/var/lib/firecrab`, `/run/firecrab` 생성 + 권한
- 빌드 산출물(`firecrab-api`, `firecrab-net-helper`) 배치, 이미지 디렉터리 준비
- 프론트엔드 `npm ci && npm run build` → `dist/` 배치
  ([프론트엔드 서빙](task-host-frontend-serving.md)이 이걸 서빙)
- [systemd 유닛](task-host-systemd-daemons.md) 설치·활성화 호출
- `--uninstall` — 유닛 정지·제거, 디렉터리 정리(데이터는 명시적 플래그가 있을 때만)
- 재실행 안전(idempotent) — 이미 있는 것은 건너뛰고 바뀐 것만 적용

## 완료 기준

- 깨끗한 머신에서 `./install.sh` 한 번으로 데몬 2개가 뜨고, 브라우저에서 VM 생성까지 됨
  (터미널을 3개 띄울 필요 없음)
- 두 번 실행해도 같은 결과(오류·중복 없음)
- 의존성이 없으면 **무엇이 없는지** 정확히 알려주고 중단
- `--uninstall` 후 firecrab이 만든 것만 사라지고 host의 다른 설정은 그대로

## 참고

- 패키지(.deb/.rpm)·업그레이드·롤백은 [5주차](week5-tasks.md)의
  [패키징·systemd·upgrade](task-packaging-systemd-upgrades.md) 범위 —
  이 태스크는 그 전 단계인 **소스에서의 설치**
