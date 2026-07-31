---
tags:
  - firecrab
  - ci
  - m2
  - blog
  - guide
updated: 2026-08-01
---

# M2 CI — 모든 템플릿·호스트에서 게스트가 뜨는지

> [!summary] 한 줄
> firecrab의 제품 완료 조건은 “API가 살아 있다”가 아니라 **M2(게스트 MicroVM)가 부팅되고 네트워크에 응답한다**는 것이다.
> Nightly CI가 **게스트 템플릿 × 호스트 Linux** 조합으로 그걸 검증한다.

## 왜 나왔나

PR CI는 빠르고 얇게 간다.

- `cargo test` / clippy / 문서
- `install.sh --no-images` — 데몬·MicroNetwork·create **거부** 경로

여기에는 **성공하는 게스트 create/start가 없다.** 이미지가 없고, 이미지 빌드+부팅은 분 단위이기 때문이다.

그래서 매일 한 번(그리고 수동 실행 시) **무거운 부팅 매트릭스**를 돌린다.

| 층 | 역할 |
|---|---|
| PR | 회귀를 빨리 차단 |
| Nightly M2 boot | “이 템플릿이 이 호스트에서 실제로 뜨는가” |
| install-distro | 호스트 **패키지/deps**만 (부팅 아님) |

## 매트릭스

### 게스트 템플릿 (전부)

`firecrab-api` `default_specs()` 와 동기화한다.

| alias | install 플래그 |
|---|---|
| `alpine-3.24` | 기본 이미지 빌드 |
| `ubuntu-26.04` | `--with-ubuntu-image` |

템플릿이 추가되면 CI matrix의 `template:` 목록에도 같은 alias를 넣는다.

### 호스트 Linux

| 구분 | 호스트 | 비고 |
|---|---|---|
| **GitHub-hosted (기본 nightly)** | `ubuntu-24.04`, `ubuntu-22.04` | nested KVM 사용 가능 |
| **Self-hosted (선택)** | debian-12, fedora, arch, opensuse, aarch64 | 라벨 있는 러너 + repo variable |

> [!important] GitHub가 빌려 주는 Linux + KVM 러너는 사실상 **Ubuntu 계열**이다.
> Debian/Fedora/Arch/openSUSE **위에서** firecrab를 설치하고 M2를 띄우려면
> self-hosted 러너(또는 자체 팜)가 필요하다. deps-only 컨테이너(`install-distro`)는
> 패키지 이름만 검증하며, KVM 게스트 부팅과는 다른 층이다.

기본 nightly 조합 (항상 시도):

```text
template × host
  alpine-3.24  ×  ubuntu-24.04
  alpine-3.24  ×  ubuntu-22.04
  ubuntu-26.04 ×  ubuntu-24.04
  ubuntu-26.04 ×  ubuntu-22.04
```

셀마다:

1. KVM 확인  
2. `install.sh` (+ Ubuntu면 `--with-ubuntu-image`)  
3. `scripts/ci-m2-guest-boot.sh <template>`  
   - MicroNetwork 생성 (`mnb…`)  
   - VM create (`microNetworkId` 필수)  
   - start → running → **ping** → stop → delete  

### Self-hosted 켜기

1. 러너에 라벨 부여 예: `self-hosted`, `linux`, `x64`, `kvm`, `debian-12`  
2. 리포지토리 Variable: `ENABLE_M2_SELF_HOSTED` = `true`  
3. Nightly / workflow_dispatch 시 `vm-boot-self-hosted` job이 스케줄됨  

변수가 없으면 self-hosted job은 **아예 안 돈다** (대기열에 묶이지 않음).

aarch64 예: 라벨 `self-hosted`, `linux`, `arm64`, `kvm` — 현재 matrix는 alpine 한 줄.

## PR vs Nightly 한눈에

```text
PR / main push
  rust · docs · frontend · install(--no-images) · install-distro
  └ install: MicroNetwork + create 400 스모크 (게스트 없음)

schedule (17:00 UTC) / workflow_dispatch
  위 전부
  + vm-boot matrix (Ubuntu 호스트 × 전 템플릿)     ← M2 실부팅
  + vm-boot-self-hosted (variable on 일 때만)
```

## 로컬에서 같은 검증

```sh
# 이미지 포함 설치 후
sudo ./install.sh --with-ubuntu-image   # 또는 Alpine만: sudo ./install.sh
chmod +x scripts/ci-m2-guest-boot.sh
scripts/ci-m2-guest-boot.sh alpine-3.24
scripts/ci-m2-guest-boot.sh ubuntu-26.04
```

명시적 MicroNetwork 모델: 네트워크 없이 VM create는 400이다. 스크립트가 네트워크를 만든다.

## AWS로 비유하면

| firecrab CI | AWS |
|---|---|
| 게스트 템플릿 | AMI 종류 |
| host runner | 어떤 리전/하이퍼바이저 위에서 띄우나 |
| nightly matrix | 릴리스 전 multi-AMI launch smoke |
| PR --no-images | 콘솔 API 단위 테스트 (인스턴스 없이) |

## 자주 하는 오해

| 오해 | 사실 |
|---|---|
| install-distro가 M2 부팅이다 | 아니다. **deps만** 깐다 |
| PR에서 게스트가 뜬다 | 아니다. nightly / 수동만 |
| “모든 Linux” = GHA 무료로 끝 | Ubuntu 호스트 전 템플릿은 끝. 다른 distro는 self-hosted |
| fcbr0가 기본으로 있다 | 아니다. MicroNetwork를 만든 뒤 `mnb…` |

## 관련

- 워크플로: `.github/workflows/ci.yml` (`vm-boot`, `vm-boot-self-hosted`)  
- 스크립트: `scripts/ci-m2-guest-boot.sh`  
- 템플릿 정의: `firecrab-api/src/templates.rs` `default_specs()`  
- 설치: [install.md](install.md)  
- 네트워크 모델: 로컬 초안 `explicit-micro-network.md` (커밋 대상 아님일 수 있음)
