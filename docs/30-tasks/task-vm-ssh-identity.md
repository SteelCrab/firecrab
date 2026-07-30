---
tags:
  - firecrab
  - vm
status: 보류
updated: 2026-07-23
---

# VM SSH identity·접근 정책

VM별 host key 분리, private key는 API를 통과하지 않음.

## AWS로 비유하면

| firecrab | AWS 대응 |
|---|---|
| 각 VM 첫 boot에 host key·machine-id 재생성 | AWS AMI가 클론될 때마다 **cloud-init**이 SSH host key·machine-id를 매번 새로 생성하는 것과 정확히 동일 |
| 공개키만 등록, private key 입력 자체를 거부 | EC2 **키 페어(Key Pair)** 모델 — launch 시 public key만 등록하고 private key는 로컬에만 있음, AWS는 애초에 private key를 받지도 저장하지도 않음 |
| `PUT /api/vms/{id}/ssh-policy`로 사용자+key만 지정 | 인스턴스 launch 시 Key Pair 하나 선택하는 것과 동일한 단순함 |
| host fingerprint를 VM별로 검증 | SSH 최초 접속 시 `known_hosts`에 뜨는 host key fingerprint 확인 절차와 동일 |

## 작업

- template 마지막 단계에서 `/etc/ssh/ssh_host_*`, machine-id, random-seed 제거 → 각 VM 첫 boot에 `ssh-keygen -A`·`systemd-machine-id-setup` 재생성
  - **왜 지우나**: 안 지우면 템플릿에서 복제된 모든 VM이 같은 host key/machine-id를 그대로 쓰게 되어, VM 하나의 키가 유출되면 전체가 동시에 위험해짐(cloud-init이 AMI마다 이걸 재생성하는 이유와 동일)
- 공개키 registry: algorithm allowlist·decoded 크기·canonical encoding·fingerprint 검증, private material 입력 거부, revoke 지원 (초기엔 root 소유 config/admin CLI 등록)
- `PUT /api/vms/{id}/ssh-policy`: created/stopped에서 검증된 user + active public key ID만 idempotent 저장 (기본값: password 인증·root login 비활성)
- running VM의 revoke는 별도 reprovision operation으로 Guest key 파일 갱신 — DB만 revoke 금지, 완료 전 접속 정보 stale 표시
- 권한: VM artifact 디렉터리 0700, `.ssh` 0700, `authorized_keys` 0600, symlink 미추종
- host fingerprint: agent가 등록한 public host key를 Rust SSH parser로 검증·계산, SSH handshake 제시 키와 정확 비교

## 완료 기준

- VM별 host fingerprint 상이
- active key의 허용 사용자만 접속, malformed/private key 등록·revoked key 사용 차단
- API 응답·DB·로그에 private key 부재

## 산출물

`firecrab-api/src/rootfs.rs`, `firecrab-api/src/ssh.rs`, Guest SSH 설정, `docs/vm-ssh-identity-smoke.md`
