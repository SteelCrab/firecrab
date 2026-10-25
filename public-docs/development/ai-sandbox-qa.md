# AI Sandbox validation

The [AI Sandbox guide](../guides/ai-sandbox.md) defines the supported provider, file and guest-execution contracts.
Do not report a mocked provider or a shell running in Docker as a real MicroVM/model inference result.

## Deterministic checks

```sh
cargo fmt --all --check
cargo clippy -p firecrab-api -p firecrab-cli -p firecrab-api-types --all-targets -- -D warnings
cargo test -p firecrab-api --bin firecrab-api ai_sandbox
cargo test -p firecrab-api --bin firecrab-api pipe_ai
cargo test -p firecrab-cli --bin firecrab
npm run build --prefix firecrab-frontend
npm run typecheck:sandbox --prefix firecrab-e2e
npm run test:sandbox --prefix firecrab-e2e
python3 scripts/check-doc-links.py
```

The normally ignored `guest_shell_enforces_identity_limits_retention_export_and_cancel` test creates real dedicated Linux users.
Run it **only in a disposable root container**, with `FIRECRAB_SANDBOX_GUEST_QA=1` and an explicit `--ignored` filter.
Never enable that test on the user's API host or a shared Linux machine.
It checks unprivileged execution, no-new-privileges, clean environment, permission boundaries, separate output, nonzero exit, exit 255, timeout, output overflow, dependency retention, archive export, cancellation, and owned cleanup.
Repeat with the injected BusyBox toolbox installed at `/etc/firecrab/busybox` to validate its file/process applets. Keep util-linux `setpriv` and `flock` available; BusyBox-only UID switching is not supported.

The remaining tests use private temporary databases and native provider fixtures.
They cover catalogue versus inference permission, account-specific ChatGPT discovery, namespaced tools, Anthropic Messages, correlated tool results, stable admission identity, defaults and overrides, credential replacement/disconnect, restart without replay, source isolation, selected apply, permission/source conflicts, and untrusted input.
The stdio MCP test uses a local HTTP fixture for the same REST lifecycle.

## Live account check

The ignored `saved_chatgpt_discovers_verifies_native_tools_and_completes_persistent_conversation` test requires `FIRECRAB_LIVE_CHATGPT_DB` pointing to an existing private server database.
It sends real requests and consumes that account's plan usage.
Use the original private database so OAuth refresh-token rotation is saved correctly; make an owner-only SQLite backup first.
Do not print, export, copy into arguments, or place tokens in the guest environment.
The test discovers an account-visible model, completes a native readiness tool round trip, sends two durable conversation turns, checks context recall, and deletes its temporary conversation project.
It does not execute a MicroVM.
Anthropic live inference requires a connected official API key. Do not use Claude Code subscription tokens.

## Real MicroVM check

The ignored `real_guest_operator_transport_runs_exports_cancels_and_cleans_owned_data` test uses the actual executor without a provider.
Set `FIRECRAB_LIVE_GUEST_TARGET` to a private JSON file with `{vmId, runtimeId, ipv4, vmsDir}` for an explicitly selected disposable user VM.
Run the Linux test binary on the API host that owns that VM's existing operator keys. The target file contains no key material.
This test does not start/stop the VM or replace the resident API service. It checks command transport, file recovery/apply and cancellation separately from live model inference.

Use a dedicated user VM and an explicitly selected API-host fixture project, with source files under version control.
Confirm the selected VM's operator SSH and pinned host identity work before admitting a code task.
Record VM ID, boot identity, source path, task ID, and attempt ID without credentials.

1. Verify a real model's native tool support and save it as the project default.
2. Request a small source change and a deterministic command that writes one file, emits stdout/stderr, and exits with a chosen status.
3. Inspect actual tool activity and guest-side evidence. Confirm the host source stays unchanged before apply.
4. Inspect the before/after review and apply only one selected file.
5. Change another source file manually and confirm applying its stale result reports a conflict.
6. Stop a long command and confirm the dedicated UID has no running processes. Preserve the VM/network and recovered changes.
7. Restart the controller during an attempt. Confirm `interrupted`, no automatic replay, explicit reconciliation, and a distinct retry attempt.
8. Reboot the same VM and confirm reconciliation does not signal an old supervisor PID in the new boot.
9. Disconnect/replace the account and confirm subsequent inference requires current verification.
10. Delete only the QA project and resources created explicitly for this check. Preserve borrowed resources and user source.

## Recorded local evidence, 2026-10-25

- Linux Sandbox: **22 passed**, including the disposable-root shell contract, bounded file ownership/modes, private-database exclusion, indexed task summaries, atomic projection/checkpoint rollback, disconnected HTTP admission, early cancellation, native provider protocols and source conflicts.
- The final Linux run installed Debian BusyBox 1.35 at `/etc/firecrab/busybox`. Its file/process applets passed with native util-linux privilege/lock helpers and a capability-selected GNU tar. BusyBox builds without `--exclude` are not used for export. BusyBox-only UID switching is not supported.
- Existing Pipe AI regression: **15 passed**, including completed SSE-item reconstruction and late-failure rejection.
- Linux API/CLI/types and macOS CLI/types Clippy passed with warnings denied. macOS CLI: **488 passed, 2 ignored**. Earlier Linux MCP/HTTP-client checks: 5/18 passed; the final macOS CLI run also included their REST fixtures.
- Browser Sandbox: **9 passed**; English/Korean, 1280/390px, persistent conversation, code board, review, conflict, selected apply, cancel/retry/reconcile, fetch errors, keyboard focus, and uncertain project-save identity. A dedicated test port avoided other running dashboards.
- Existing command-palette/dashboard-frame tests: 21 passed after moving Sandbox into the final AI navigation group. Final frontend build, targeted lint, Sandbox TypeScript check, formatting and documentation checks passed. The existing bundle-size advisory remains.
- **Live ChatGPT REST check passed** with `gpt-6.1-sol`: account model discovery, native readiness tool round trip, two persisted conversation turns and marker recall. Its temporary project was deleted. OAuth refresh rotation stayed in the original private database. No MicroVM ran in this provider check.
- **Real Alpine MicroVM transport check passed** using the production guest executor: pinned operator SSH, unprivileged/no-new-privileges execution, clean environment, separate stdout/stderr, exit 7/255, export, unchanged source before explicit apply, selected source apply, cancellation and owned cleanup. This check used no model or provider credential.
- The real guest used official Alpine 3.24.1 and required util-linux/tar prerequisites prepared only in that disposable VM. Its ID was `57b6319a-1517-42d3-ba1d-3a0679483185`, runtime `c92a6101-8ad6-4edf-bff7-e9531416c684`, attempt `e5d653eb-ba14-459f-b8e2-92e224d4cd6c`.
- The temporary VM, network, newly installed QA image/package, guest test binary and task-owned Docker build cache were removed after validation. Existing workloads and saved connected-account data were preserved.

The initial live REST probe exposed ChatGPT terminal events with empty `response.output`; complete items arrived in `response.output_item.done` events. The parser now reconstructs only complete items and still requires terminal completion. BusyBox validation also exposed missing UID applets and tar exclusion options; prerequisites are now explicit and export chooses a compatible tar before staging.
Host disk exhaustion interrupted earlier runs. With explicit user approval, shared Cargo incremental cache was cleared and Docker Desktop/local microManager were restarted before the successful final checks. No source or connected credentials were deleted.
An earlier root-container CLI run lacked Node.js; a restricted macOS run could not bind fixture servers. The normal macOS run above passed. No tests were weakened to hide those environment failures.

Anthropic live inference remains untested because no API key was supplied. It is covered by deterministic native Messages/tool-result fixtures. A single live-model-to-real-MicroVM end-to-end scenario, controller-crash/guest-reboot recovery on a real VM, and incremental browser token streaming are not claimed by these results.
Screenshots under `assets/dashboard/ai-sandbox/` use labelled fixture account/model data; they are UI evidence only.

## Rust/WASM integration, 2026-10-25

PR #490 ports the UI from main at `ec06ac64` and replaces its React slice with Rust-owned state, rendering and delegated events.
It integrates subsequent main changes through `75c7cbd0`, including Git sources, mTLS, development dashboard port/proxy and wasm-bindgen cache fixes, RabbitMQ queues, console reconnect handling, Deploy/rollback, linked-account Secret import and MicroBlockStorage.
The same conversation, project dialog, task board, diff/activity review, selected apply, icons and final AI sidebar group are preserved.
The browser binding remains thin; the Sandbox application logic is in `firecrab-frontend/src/sandbox.rs` and its modules.
The old React components, navigation files and browser API helper are removed.

- Frontend Rust unit tests: **31 passed** after MicroBlockStorage integration, including authoritative REST summary compatibility, stale-poll protection, verified default/override gating and escaped review output.
- WASM Clippy with warnings denied and production build: passed. Sandbox browser types, formatting, changelog and documentation links: passed.
- Production frontend browser/compression contracts: **48 passed** after MicroBlockStorage integration.
- The existing 9 Sandbox browser scenarios pass on WASM. A tenth scenario verifies draft text, focus and caret preservation when an active task changes state; **10 passed** in total.
- Sandbox is included in the shared `test:dashboard` CI suite. The browser fixture now follows the VM response schema and asserts `data-engine=wasm`.
- Updated screenshots use the WASM UI and retain the fixture-data labels. Original live-provider/MicroVM evidence above is separate from this frontend port.
- REST project guards cover invalid/system/private source paths, credential rejection, ineligible or duplicate VM bindings, active-work mutation/deletion, and owned-only deletion that preserves source files and borrowed VMs. The Linux Sandbox suite now passes **24 tests**, including the isolated root guest contract.

Run `cargo test --manifest-path firecrab-frontend/Cargo.toml --locked` and `npm run test:sandbox --prefix firecrab-e2e` for the port-specific checks.
Latest-main integration passes **69 Sandbox/Pipe AI/navigation/Deploy browser scenarios**, plus **4 Queue scenarios**. The current head's CI and the unchanged 80% patch-coverage gate are checked before completion.
The subsequent linked-account Secret merge also passes **20 Sandbox/Secret browser scenarios**.
The MicroBlockStorage merge passes **62 Sandbox/Volume/navigation/VM-detail/schedule browser scenarios**.

Native CI also runs the ignored root-only guest contract through `scripts/test-ai-sandbox-guest.sh` in a disposable, network-isolated container. Only its test executable and coverage directory are mounted. Dedicated users and task files stay in the container, and its LLVM profile is included in the native coverage report. CLI/MCP regression tests check wire identities, configured model selection, recovery, explicit review paths, bounded stdin and credential rejection.
