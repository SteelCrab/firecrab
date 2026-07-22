<p align="center">
  <a href="https://www.rust-lang.org"><img alt="Rust" src="https://img.shields.io/badge/rust-1.94%2B-orange?logo=rust&logoColor=white"></a>
  <a href="https://codecov.io/gh/SteelCrab/firecrab"><img alt="Codecov" src="https://codecov.io/gh/SteelCrab/firecrab/branch/main/graph/badge.svg"></a>
  <a href="https://www.linux.org"><img alt="Linux" src="https://img.shields.io/badge/platform-linux-blue?logo=linux&logoColor=white"></a>
  <a href="./LICENSE"><img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue"></a>
</p>

<h1 align="center">firecrab</h1>

<p align="center"><a href="https://firecracker-microvm.github.io/">AWS Firecracker</a> 기반의 사설 microVM 클라우드</p>

<p align="center">
  <a href="./README.md">English</a> ·
  <a href="./README.zh.md">中文</a> ·
  <a href="./README.ja.md">日本語</a>
</p>

## 소개

firecrab은 자체 Linux 호스트 위에 [AWS Firecracker](https://firecracker-microvm.github.io/)
microVM 기반의 사설 클라우드를 구축하는 경량 컨트롤 플레인입니다. Firecracker는 AWS Lambda·
Fargate를 구동하는 바로 그 KVM 기반 VMM(Virtual Machine Monitor)으로, 일반 VM보다 훨씬 빠르게
(수백 ms 단위) 부팅하면서도 하드웨어 가상화 수준의 격리를 그대로 제공합니다. firecrab은 이
Firecracker를 AWS 종속 없이 직접 호스팅하여, 온프레미스 서버에서도 동일한 이점(빠른 기동, 강한
격리, 낮은 오버헤드)을 누릴 수 있게 합니다.

기존 KVM·VMware 기반의 무거운 레거시 VM을 더 가볍고 빠른 microVM으로 옮기는 마이그레이션 경로를
겨냥해 설계되었습니다. 웹 대시보드나 REST API로 VM을 생성·시작·중지·삭제할 수 있고, VM마다
브라우저에서 바로 시리얼 콘솔에 접속해 부팅 로그부터 셸까지 실시간으로 확인할 수 있습니다.

Rust로 작성된 API 서버(`firecrab-api`)가 SQLite에 VM 상태를 저장하고 Firecracker 프로세스를 직접
관리하며, 커널·rootfs 템플릿은 해시로 무결성을 검증한 뒤 서빙합니다. 브리지·TAP·방화벽처럼 root
권한이 필요한 호스트 네트워크 작업은 별도의 권한 분리된 helper 프로세스(`firecrab-net-helper`)가
Unix 소켓을 통해서만 처리하며, API 서버 자체는 비특권 프로세스로 동작합니다.

## 주요 기능

- VM 전체 생명주기를 다루는 REST API + React 대시보드
- 복수 부팅 템플릿(Ubuntu, Alpine)
- WebSocket 기반 실시간 시리얼 콘솔
- SQLite 상태 저장, 권한 분리된 helper 프로세스를 통한 호스트 네트워크 격리

## 빠른 시작

### 터미널 세션 1 — API 서버

- `firecrab-api`: Rust REST API 서버(`http://localhost:3000`)
- VM 상태 저장, Firecracker 프로세스 관리

```sh
cargo run -p firecrab-api
```

### 터미널 세션 2 — 프론트엔드

- `firecrab-frontend`: React 대시보드 개발 서버(`http://localhost:8080`)
- VM 생성·조회 UI, 콘솔 접속 제공

```sh
cd firecrab-frontend
npm run dev
```

`http://localhost:8080/`에 접속하세요. 전체 사용법은 [docs/web.md](docs/web.md)를 참고하세요.

## 라이선스

[Apache License, Version 2.0](./LICENSE)에 따라 배포됩니다.
