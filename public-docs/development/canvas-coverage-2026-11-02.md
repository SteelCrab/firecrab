# Canvas transport coverage — 2026-11-02

Issue: [#550](https://github.com/SteelCrab/firecrab/issues/550). Delivery:
[Draft PR #562](https://github.com/SteelCrab/firecrab/pull/562), targeting
`release/v0.5.0`. Keep the PR unmerged until all issue requirements and acceptance
checks are complete.

## Problem and change

The exact preceding head `05d8a95b8b787b0221929b43c6cd67195afbc67a` passed
Actions CI and Release Build, but its Codecov patch check reported **68.13%**:
6,460 hits and 3,021 misses across 9,481 changed executable lines. The configured
patch target is **80%**. Many missing paths required real SSH and native process
transport rather than database-only fixtures.

The coverage job now runs a separate, opt-in disposable Linux container with:

- A real loopback SSH server, generated operator/host keys and pinned host checking.
- The real descriptor-based file, PTY and bounded-command implementations.
- Actual versioned HTTP file writes, stale-save denial, comparison, restoration and
  operation retries. A simulated lost acknowledgement leaves the controller record
  pending after the native write; reconciliation observes its real receipt without
  overwriting a later external edit.
- Real PTY output, controlled input, lease release/stale-control denial, resize and
  confirmed asynchronous native UID termination. Service observers run as in the app.
- Frozen synthetic skill-package installation through native writes, exact revision
  capture, read-only retained resources after removal and rejection of new admissions.
- Retained-folder agent file tools and a real bounded command whose receipt prevents
  a second execution. No provider/model is invoked.
- The actual SHA-256-pinned OpenVSCode package, immutable bundle publication, private
  editor start, HTTP workbench/static-file proxy, file gateway, conditional stop,
  reopen and stale-instance stop denial.

The container has no network, published ports or host service mounts. The native
**test build** substitutes its owned directory for mount identity checks. Production
helper checks remain intact. This verifies the component transport on a real Linux
kernel; it does not establish ext4 mounting, a MicroVM reboot or production service
user acceptance. The test checks its explicit opt-in and container/root identity
before changing any fixed guest paths.

The existing actual official-source download/hash test also runs explicitly in CI,
outside that network-isolated container. It verifies public source bytes without
executing downloaded package code.

## Local evidence

- Dedicated Linux aarch64 SSH/native/OpenVSCode integration: **1 passed, 0 failed**.
- Actual official-source QA: **1 passed, 0 failed**. OpenAI `brainstorming` at
  `0722921d5542fc593105c27bd52630babd8b8c2a`: **10 files**. Anthropic
  `brand-guidelines` at `ca45fc496a3436251f191354091d557da31c15b5`: **3 files**.
- The passing executions produced real LLVM profiles. Comparing their LCOV line
  hits with the preceding Codecov per-line comparison covered **1,731** previously
  missed patch lines. Unchanged source lines were matched across the small module
  registration insertion. The old-patch estimate is **86.39%**; this is a local
  estimate, not a claimed new Codecov server result.
- The preparation attempts exposed missing service observers and an immediate-stop
  assumption in the new fixture. The fixture now waits for actual observation and
  native exit. Failed preparation profiles were retained separately and excluded
  from the passing-run local measurement.
- Rust type checking passes. Hosted CI and the new Codecov result must be checked
  on the pushed commit. The 80% targets and production coverage scope are unchanged.

Reproduction on a compatible Linux host:

```sh
source <(cargo llvm-cov show-env --sh)
cargo test -p firecrab-api --bin firecrab-api --locked --no-run
bash scripts/test-project-workspace-transport.sh "$API_TEST_BINARY" "$CARGO_LLVM_COV_TARGET_DIR"
cargo test -p firecrab-api --bin firecrab-api --locked official_catalogs_and_small_packages_match_their_pinned_git_blobs -- --ignored --nocapture
cargo llvm-cov report --lcov --output-path lcov.info
```

`API_TEST_BINARY` must name the just-built instrumented test executable, as resolved
by the CI job's Cargo JSON output. The wrapper creates and cleans only its own
container, image and temporary directory. It accepts a pre-staged official editor
archive for offline reuse and verifies its pinned digest before use.

The existing live-provider, native VM lifecycle, matched backup/restore, cross-host
and multi-user issue gates remain open. This coverage change does not authorize
merging PR #562.
