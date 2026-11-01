# Project editor immutable bundles — 2026-11-01

Related: #550, Draft PR #562. This is a prerequisite for safe editor lifecycle changes. It does not complete editor-session cleanup or integrated acceptance. Keep the PR unmerged.

## Behavior

- New editor records retain their exact code-bundle revision. Records without that field keep the legacy package path.
- An already running editor is reused without changing its code. A later explicit reopen after termination uses the current bundle with the same private editor identity and retained user data.
- Bundle identity covers the pinned official archive hash and length-delimited manifest/extension bytes. Rust and Node share a fixed digest regression vector.
- Each bundle is published by an atomic directory rename under the protected project metadata directory. Official files use hard links; shared inodes are never modified. The custom extension has separate files for each revision.
- Completed bundles are validated before reuse. A mismatched revision, changed extension, unexpected owner/mode, unsafe symlink, or invalid path fails explicitly.
- Failed publication removes only its own unpublished stage. Older interrupted stages and completed bundles remain retained. No automatic garbage collection is added.
- Root SSH transport accepts only pinned legacy paths or a canonical 64-character lowercase bundle digest. Neither caller text nor project files can supply a shell path.

## Conditional process termination

The editor metadata exposes a hash-based execution identity without exposing its native PTY UUID or token. The stop endpoint requires that exact identity. A retry for an old execution cannot stop its replacement, even when the same file-opening request ID is reused. Completed stopped metadata stays queryable without VM execution rights; admitting a new stop requires the existing project/VM execution policy.

Stop intent is saved before guest I/O. An owned worker continues after HTTP disconnection, and maintenance resumes persisted `stopping` records after controller interruption. The native process proof is checked before sending the fixed termination operation. An unknown outcome remains `stopping`; an established scope that is unexpectedly missing is not claimed to be terminated.

Maintenance re-reads the current editor under the project lock. It cannot overwrite a replacement attempt or revive a completed stop from an earlier snapshot. Proxy streams reject an ended/replaced execution. Private draft routes remain available under verified caller/project rights while the process or VM is stopped. No browser iframe is destroyed by this backend change.

HTTP regressions cover private ownership, repeated completed stops, stale stop rejection after replacement, read-only admission denial, forged request fields, stopped-runtime private draft save/retry/read, operator isolation, and grant revocation. A DB regression covers stale maintenance observations and newer file-opening metadata.

## Validation

The Node suite uses isolated filesystem fixtures, not a real OpenVSCode workload. It checks two coexisting revisions and an unchanged legacy bridge, idempotent reuse, interrupted-stage retention, digest/path/ownership/symlink rejection, failed-publication recovery, stdin bootstrap execution, and the shared Rust/Node digest. One fixture copies the actual Node executable and runs both published revision paths.

Commands:

```sh
node --test firecrab-frontend/tests/editor-bundle.test.mjs
CARGO_INCREMENTAL=0 cargo test -p firecrab-api project_workspace --locked
CARGO_INCREMENTAL=0 cargo test -p firecrab-api editor_ --locked
CARGO_INCREMENTAL=0 cargo test -p firecrab-api bundle_digest --locked
CARGO_INCREMENTAL=0 cargo clippy -p firecrab-api -p firecrab-api-types --all-targets --locked -- -D warnings
cargo fmt --all -- --check
python3 scripts/check-doc-links.py
```

Local results: all 9 Node filesystem tests, all 57 project-workspace API tests, the editor transport path regression, API/types clippy, formatting, and documentation checks pass. The copied Homebrew Node fixture sets its macOS library lookup path; it does not change product runtime configuration.

The frontend test command includes the new Node suite. Linux CI also runs it as root against temporary filesystem fixtures. That CI step is not live editor or VM acceptance.

## Live qualification limit

The existing Linux QA environment returned `/bin/bash: Input/output error` before its read-only inventory command could run. This attempt did not validate the new bundle in a workload VM. No installed-service restart, VM restart, data deletion, or environment reset was requested or performed.

Live native stop/restart, real editor upgrade/reopen, and browser flush/detach acceptance remain pending. Provider-backed folder agents, Sandbox project-DB migration, official skill installation, automated matched backups, production service-user qualification, and genuine cross-host restoration remain open in #550.
