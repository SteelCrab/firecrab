# 이미지 템플릿 Alpine Linux 추가 테스트

## 자동 테스트

```sh
cargo test -p firecrab-api templates::tests
cargo test --workspace
cd firecrab-frontend && npx tsc -b && npm run build
```

## 확인 항목

- `TemplateRegistry::load_default`: alias `alpine-3.x` 추가, 기존 `ubuntu-26.04`와 alias/version 중복 없음
- Alpine과 Ubuntu 모두 같은 커널(`kernel/vmlinux-7.1.2-x86_64`) 공유 — 커널은 virtio/ext4/serial만
  요구하는 distro-agnostic 산출물이라 재빌드 불필요
- `CreateVm.tsx`의 `TEMPLATES` 배열에 `alpine-3.x` 추가
- rootfs 이미지: `images/rootfs/alpine-rootfs-3.24.1-x86_64.ext4` + `alpine-rootfs.ext4` 심볼릭 링크
- 빌드 스크립트 `scripts/firecracker-menual/install-alpine-rootfs.sh`: host root/sudo 불필요 —
  `images/rootfs/`가 root:root 소유(기존 ubuntu 스크립트가 sudo로 만든 디렉터리)라 pista 계정은 쓰기
  불가. Docker 컨테이너를 root로 띄워 `apk --root`로 staging을 구성하고 같은 컨테이너 안에서
  `mkfs.ext4 -d`까지 실행 — ubuntu 스크립트의 "sudo 재실행" 패턴 대신 "Docker root" 패턴 사용
- 최신 stable Alpine 브랜치(minirootfs)를 `latest-releases.yaml`에서 자동 resolve, sha256 검증 후
  빌드 — 커널/우분투 스크립트의 "latest 자동 resolve" 패턴과 동일
- OpenRC runlevel에서 `hwclock` 제외 — Firecracker에 RTC 디바이스가 없어 실패만 하고
  `modprobe: can't change directory to '/lib/modules'` 잡음까지 유발(이 커널은 loadable module
  지원 없이 빌드됨)

## 실제 검증됨 (이번 세션에서 완료)

- `cargo test --workspace`: 83/9/9/31 전부 green
- 프론트 `npm run build`: 타입체크 + vite build 통과
- **raw kernel+rootfs 부팅**: `scripts/firecracker-menual/boot-microvm.sh`로
  `alpine-rootfs.ext4` 부팅 → OpenRC 기동, dhcpcd/sshd 시작, "Welcome to Alpine!" +
  `firecrab login: root (automatic login)` + 쉘 프롬프트(`firecrab:~#`) 도달. 같은 스크립트로
  `ubuntu-rootfs.ext4`도 재부팅해 회귀 없음 확인(둘 다 `verify_boot_log`의 에러/성공 패턴 기준 통과)
- **실제 API+프론트 엔드투엔드**(Playwright로 브라우저 조작): `cargo run -p firecrab-api` +
  `npm run dev` 기동 → 생성 폼 template 드롭다운에서 `alpine-3.x` 선택 → 생성 → start → VM 상세
  모달의 실제 캡처된 console.log에서 Alpine 부팅(OpenRC → dhcpcd → sshd → login) 확인. 이어서
  `ubuntu-26.04`도 동일 플로우로 생성·시작해 systemd 부팅이 `Reached target graphical.target`까지
  끝까지 도달하는 것으로 회귀 없음 확인. 두 테스트 VM 모두 stop 후 delete로 정리, 띄운 서버도 종료

## 터미널 세션 1 — 이미지 빌드(최초 1회, 또는 이미지 갱신 시)

```sh
./scripts/firecracker-menual/install-alpine-rootfs.sh
```

Docker(데몬 접근 가능한 계정)가 필요하다 — host root/sudo는 필요 없음.

## 터미널 세션 2 — API + 프론트 실행 후 수동 확인

```sh
cargo run -p firecrab-api
```

다른 터미널에서:

```sh
cd firecrab-frontend
npm run dev
```

`http://localhost:8080/`(꼭 `localhost`)에서 template 드롭다운에 `alpine-3.x` 선택 → 생성 → start →
이름 클릭해 상세 모달 로그에서 Alpine 부팅 확인.

## 정리

```sh
curl -s -X POST http://127.0.0.1:3000/api/vms/<id>/stop
curl -s -X DELETE http://127.0.0.1:3000/api/vms/<id>
```

세션 1(이미지 빌드는 1회성), 세션 2의 두 서버를 `Ctrl-C`로 종료.
