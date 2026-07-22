# CI/CD(GitHub Actions) 사용법

## 로컬 재현 명령 — `ci.yml`

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo llvm-cov --workspace --locked --lcov --output-path lcov.info
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items
cd firecrab-frontend && npm ci && npm run lint && npm run build
```

## 로컬 재현 명령 — `release.yml`

```sh
cargo test --workspace --release --locked

cargo install cross --locked
rustup target add x86_64-unknown-linux-musl
cross build --release --target x86_64-unknown-linux-musl -p firecrab-api -p firecrab-net-helper
```

## 확인 항목 — `ci.yml`

- 트리거: `push`(main), 모든 `pull_request` — GitHub 저장소 Actions 탭 또는 PR 하단 체크 목록에서 결과 확인
- `rust` job: fmt-check → clippy(경고만, CI 안 막음) → `cargo-llvm-cov`로 테스트+커버리지 →
  Codecov 업로드
- `docs` job: `cargo doc -D warnings`(깨진 링크·문법 오류 차단) → rustdoc 문서화율 게이트(≥75%,
  `RUSTC_BOOTSTRAP=1` 편법) — 퍼센트는 GitHub Actions 탭 → 해당 run → Summary(또는 `docs` job
  페이지 상단)에 마크다운으로 표시됨. **현재 실측 ~14%라 이 게이트는 지금 상태로는 fail** — 워크
  스페이스 전체에 doc 주석을 채우는 작업은 이번 범위 밖(문서화 완성도 목표치, 버그 아님)
- `frontend` job: oxlint → `tsc -b` + vite build
- Codecov: PR에 커버리지 diff 코멘트 자동 게시, `codecov.yml`의 project/patch 80% 게이트

## 확인 항목 — `release.yml`

- 트리거: 태그 push(`v*`), `main` 대상 PR
- `release-test` job: `cargo test --workspace --release --locked`
- `build-release` job: `x86_64`/`aarch64` × `gnu`/`musl` 4개 타겟을 `cross`로 빌드,
  `firecrab-api`+`firecrab-net-helper`를 타겟별 tar.gz로 패키징해 아티팩트 업로드(PR에서도 실행 —
  크로스 빌드 자체가 깨지는지 조기 확인용)
- `create-release` job: 태그 push일 때만 모든 타겟 아티팩트를 모아 GitHub Release 생성(`softprops/
  action-gh-release`, `generate_release_notes: true`)
- 로컬 검증 상태: `x86_64-unknown-linux-musl`은 실제 `cross build --release`로 바이너리 생성 확인.
  `aarch64-unknown-linux-gnu`는 이 세션의 로컬 docker/rustup 마운트 충돌로 검증 못 함(실제 GitHub
  Actions runner에서는 재현 안 될 환경 특이사항) — 첫 태그 push 시 Actions 탭에서 직접 확인 필요

## 공통

- 세 워크플로 어느 job이 실패해도 **머지 자체는 안 막힘** — 브랜치 보호 규칙 미설정(아래 참고)

## 사전 준비 (1회, GitHub 저장소 설정 — 직접 실행 불가)

- Settings → Secrets and variables → Actions → `CODECOV_TOKEN` 등록(Codecov 대시보드에서 발급).
  미등록이어도 CI 자체는 안 막힘(`fail_ci_if_error: false`), Codecov 업로드만 실패
- `create-release` job은 기본 `GITHUB_TOKEN`(`permissions: contents: write`)으로 동작 — 별도
  시크릿 불필요
- (선택) Settings → Branches → main에 대한 branch protection rule 추가하고 `rust`/`frontend`
  (그리고 필요하면 `release-test`/`build-release`)를 required status check로 지정해야 실패한
  PR의 머지가 실제로 막힘 — `docs`는 rustdoc 게이트가 아직 fail이라 required로 넣으면 안 됨.
  지금은 체크 표시만 뜨고 머지 자체는 가능한 상태

## 실패 시 대응

| 증상 | 조치 |
|---|---|
| fmt-check 실패 | `cargo fmt --all` 로컬 실행 후 재커밋 |
| clippy 경고 | CI는 안 막힘 — 로그 확인 후 선택적으로 수정 |
| test 실패 | 로컬 `cargo test --workspace`로 재현 |
| rustdoc 게이트 실패 | 문서화율 75% 미달 — 알려진 상태(~14%), 별도 문서화 작업 필요 |
| frontend lint/build 실패 | `npm run lint` / `npm run build` 로컬 재현 |
| release-test 실패 | 로컬 `cargo test --workspace --release --locked`로 재현 |
| build-release 실패 | 해당 타겟으로 로컬 `cross build --release --target <target> -p firecrab-api -p firecrab-net-helper` 재현 |
