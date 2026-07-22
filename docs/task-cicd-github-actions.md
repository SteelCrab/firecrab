# CI/CD 구현 (GitHub Actions)

PR/push마다 fmt·clippy·test+coverage·rustdoc(Rust)와 lint·typecheck·build(frontend)를
자동 검증하고(`ci.yml`), 태그 push 시 멀티 아키텍처 릴리스 바이너리를 빌드·배포한다(`release.yml`).

## 작업 — `ci.yml`

- `.github/workflows/ci.yml` 신규, 3개 job
- `rust` job: `rust-toolchain.toml`이 채널(1.94.1)+`clippy`/`rustfmt`/`llvm-tools` 컴포넌트를
  고정하므로 별도 버전 지정 없이 `rustup show`로 설치 → `cargo fmt --all -- --check` →
  `cargo clippy --workspace --all-targets`(경고만, `-D warnings` 아님) →
  `taiki-e/install-action@cargo-llvm-cov`로 `cargo-llvm-cov` 설치 →
  `cargo llvm-cov --workspace --locked --lcov` → `codecov/codecov-action@v5`로 업로드
- `docs` job: `RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps
  --document-private-items` — 깨진 intra-doc 링크·잘못된 rustdoc 문법을 CI에서 잡음. 이어서
  `RUSTC_BOOTSTRAP=1`로 nightly 전용 `--show-coverage` 언락 → 문서화율 계산 → **75% 미만이면 fail**,
  GitHub Actions Job Summary에 퍼센트 표시(`$GITHUB_STEP_SUMMARY`)
- `codecov.yml` 신규: project/patch 커버리지 80% 미만이면 PR 상태 체크 fail(현재 실측 82.38%로 통과)
- `frontend` job: Node 22 + `npm ci` → `npm run lint`(oxlint) → `npm run build`(tsc -b + vite build)
- `Cargo.lock`/`package-lock.json` 해시로 `actions/cache`, `actions/setup-node`의 npm 캐시 사용
- 트리거: `push`(main), `pull_request`(전체)

## 작업 — `release.yml`

- 트리거: 태그 push(`v*`) + `main` 대상 PR(크로스 빌드 자체가 깨지는지 PR 단계에서 미리 확인)
- `release-test` job: `cargo test --workspace --release --locked` — release 프로파일 전용 버그
  (overflow check 비활성화, 다른 코드젠) 사전 확인
- `build-release` job: `dtolnay/rust-toolchain` + `cross`로 4개 타겟 매트릭스 빌드
  (`x86_64`/`aarch64` × `gnu`/`musl`) — Firecracker 자체가 x86_64/aarch64만 지원해서 armv7·riscv64는
  제외(일반 Rust 릴리스 템플릿과 다르게 이 프로젝트가 실제로 돌아갈 아키텍처로 좁힘).
  `firecrab-api`/`firecrab-net-helper` 두 바이너리를 타겟별로 tar.gz 패키징 후 아티팩트 업로드
- `create-release` job: 태그 push일 때만(`startsWith(github.ref, 'refs/tags/v')`) 모든 아티팩트를
  모아 `softprops/action-gh-release`로 GitHub Release 생성
- 로컬 검증: `x86_64-unknown-linux-musl`은 실제 `cross build --release`로 바이너리 생성까지 확인.
  `aarch64-unknown-linux-gnu`는 이 세션 샌드박스의 docker/rustup 마운트 충돌로 로컬 검증만 실패
  (실제 GitHub Actions runner에는 해당 마운트가 없어 재현되지 않을 환경 특이사항 — 코드 자체의
  cross 호환성은 musl 빌드 성공으로 확인됨)

## 필요한 사전 설정 (GitHub 저장소 Settings, 직접 실행 불가)

- Settings → Secrets and variables → Actions에 `CODECOV_TOKEN` 등록 필요(Codecov 대시보드에서
  저장소 연결 후 발급). 없으면 `fail_ci_if_error: false`라 CI 자체는 안 막히지만 커버리지 업로드는
  실패함
- `create-release` job은 저장소 기본 `GITHUB_TOKEN`으로 동작(`permissions: contents: write`) —
  별도 시크릿 불필요

## clippy를 `-D warnings`로 안 만든 이유

`firecrab-api`에 `ipam.rs`/`network_policy.rs`/`persistence.rs`의 dead_code 경고가 이미 존재 —
TAP 자동화(`task-vm-tap-automation.md`)·Guest DHCP(`task-guest-network-configuration.md`)가
붙기 전까지는 정상적으로 미사용 상태인 선행 구현. 지금 `-D warnings`로 막으면 버그가 아니라
계획된 미완성 상태 때문에 CI가 막힘 — 해당 태스크들이 들어가 실제로 호출되기 시작하면 재검토.

## 범위 밖

- systemd 서비스 설치·업그레이드 자동화는 별도 태스크(`task-packaging-systemd-upgrades.md`) —
  `release.yml`은 바이너리 tar.gz를 GitHub Release에 올리는 것까지만, 배포 대상 호스트에 설치하는
  절차는 다루지 않음

## 완료 기준

- push/PR에서 `ci.yml`의 `rust`(fmt/clippy/test+coverage 업로드)·`frontend`(lint/build)는 현재
  코드 기준 green
- `docs` job의 `cargo doc -D warnings`는 green. **rustdoc 문서화율 게이트(75%)는 현재 실측
  ~14%라 fail** — 워크스페이스에 doc 주석을 채우는 건 이 태스크 범위 밖(별도로 시도했던
  `feat/rustdoc-coverage` 브랜치는 병합하지 않기로 하고 main 기준으로 되돌림). 문서화 완성도
  목표치로 설정된 것이라 버그 아님 — 나중에 별도로 채울 것
- `release.yml`의 `release-test`(release 프로파일 테스트) green. `build-release`는
  `x86_64-unknown-linux-musl` 로컬 실빌드로 검증, 나머지 3개 타겟은 GitHub Actions의 클린
  환경에서 실제 트리거 시 확인 필요
- 로컬에서 동일 명령 실행 결과와 CI 결과가 일치(같은 pinned 툴체인 사용)

## 산출물

`.github/workflows/ci.yml`, `.github/workflows/release.yml`, `codecov.yml`,
`rust-toolchain.toml`(llvm-tools 컴포넌트 추가)
