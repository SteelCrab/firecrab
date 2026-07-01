# 1주차 결과 정리

## 완료

- Firecracker 실행 환경 기준 정리함
- 설치, KVM 확인, kernel/rootfs 생성 스크립트 준비함
- Serial Console 부팅 흐름 정리함
- TAP, NAT, DNS, SSH 접속 문서화함
- 생성 파일 제외용 `.gitignore` 정리함

## 성공 내용

- Firecracker 설치와 버전 확인 절차 추가함
- `scripts/firecracker-menual` 기준으로 경로 최신화함
- repo 루트 기준으로 `images/`, `build/`, 로그 경로 정리함
- rootfs에 부팅, 네트워크, SSH 필수 패키지 포함함
- Guest `eth0`, gateway, DNS 자동 설정 반영함
- Host `tap0`, NAT, forwarding 절차 문서화함
- shell 문법 검사 통과함

## 실패 내용
- 초기 linux-kernel 빌드 스크립트에 빌드 실패 시 오류 메시지 출력과 종료 코드 누락했음
- 초기 rootfs에 `reboot`, 네트워크 도구, serial getty 설정 부족했음
- 수동 네트워크 설정이 reboot 후 사라져 `eth0` DOWN 발생함
- NAT, DNS 설정 누락으로 외부 통신과 이름 해석 실패했음
- 스크립트 이동 후 문서 경로 불일치 있었음
- `serial-console.sh`에 중복 경로 문제 있었음

## 산출물

- `docs/week1-result.md`
- `docs/firecracker-menual/README.md`
- `docs/firecracker-menual/image-setup.md`
- `docs/firecracker-menual/serial-console.md`
- `docs/firecracker-menual/network-basic.md`
- `docs/firecracker-menual/boot-microvm.md`
- `docs/firecracker-menual/ssh-access.md`
- `scripts/firecracker-menual/install-firecracker.sh`
- `scripts/firecracker-menual/install-linux-kernel.sh`
- `scripts/firecracker-menual/install-ubuntu-roofs.sh`
- `scripts/firecracker-menual/serial-console.sh`
- `scripts/firecracker-menual/run-serial-shell.sh`
- `scripts/firecracker-menual/boot-microvm.sh`
- `.gitignore`
