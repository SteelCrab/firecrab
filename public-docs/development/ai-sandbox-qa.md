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

Use the [combined live foundation gate](ai-sandbox-foundation-qa.md) for a single real-model/real-MicroVM run with project defaults, task overrides, denied models, disconnected-call rejection and exported attempt evidence.

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

Use the [recovery acceptance gate](ai-sandbox-recovery-qa.md) to reproduce command
timeouts, cancellation, disconnected admission, controller crash and a real guest
reboot through the existing REST lifecycle. Its faults require a dedicated API
host and explicit ownership controls.

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

## Caller and resource authorization, 2026-10-25

The follow-up [Sandbox access policy](../guides/sandbox-access.md) uses verified mTLS host identities and the trusted loopback operator.
The initial local counts below use main at `f9d4f2d3`. Later main integration is validated by the follow-up PR's exact-head CI results.
It persists project owners, admission callers and immutable project/task/attempt/runtime/source references.
Delegated hosts cannot bypass resource grants through VM keys, consoles or provider APIs.
Source filtering happens before task pagination. Provider history is confined to the attempt's original source.

- Linux API unit suite: **1,296 passed, 6 ignored**. API integration suites: **1 + 6 passed**.
- Linux CLI unit suite: **746 passed, 3 ignored**; applicable CLI integration suites passed.
- Eight REST authorization scenarios cover real mTLS sessions, forged identity headers, cross-host route denial, exact source/VM/connection rights, policy replacement/revocation, an in-flight provider response, historical-source filtering, selected apply and explicit runtime reconciliation.
- Follow-up regressions reproduce revocation during a provider-lock wait and during the model tool probe. Both block the subsequent provider request and avoid saving a revoked verification proof. Discovery and inference also recheck grants after credential refresh.
- Persistence regressions reject reference/owner mutation and duplicate attempt IDs, backfill legacy local records, retain grants/revocation after reopening, and clean derived references with owned project deletion.
- CLI/MCP fixtures verify typed access-policy routes and reject caller-role injection before HTTP.
- Linux API/CLI/types Clippy passes with warnings denied. Formatting, changelog and documentation checks pass.
- The ignored root guest contract passes in a disposable network-isolated container. This is a guest-shell contract, not a real MicroVM run.

Full suites ran as an unprivileged user with zstd, fakeroot, dnsmasq, sudo and Node.js 22/npm.
Earlier runs lacked tools or used root/read-only source mounts; those environment failures were corrected without weakening tests.
This slice uses temporary databases and local provider fixtures. It does not rerun live provider inference or the combined real-model-to-MicroVM foundation gate.

## Initial combined foundation QA attempts, 2026-10-26

The opt-in [foundation harness](ai-sandbox-foundation-qa.md) requires one live provider-to-MicroVM run.
Its regression suite passes **11 tests** without credentials. Fixtures cannot satisfy the live gate.
The suite rejects fabricated, incomplete or misattributed guest results and protects borrowed accounts and VMs.

The implementation fixes two development blockers found during setup:

- Source archives now include the host-kernel build script used at API compile time. The archive/extraction regression passes.
- ChatGPT login accepts forwarded ports only from the trusted loopback listener. Both embedded and source Vite proxies retain the selected API for callbacks. **16 Pipe AI API tests** and **4 real Vite tests** pass.

Live preparation uses main `accb9bbe` plus these branch changes. Later main integration is covered by the follow-up PR's CI.
A dedicated macOS management VM runs the Linux API/helper with schema 7 and usable nested KVM.
Ubuntu `ubuntu-26.04-v5` boots on a disposable user VM with one CPU, 512 MiB RAM and a 2 GiB disk.
The VM uses a dedicated MicroNetwork with internet disabled and isolated egress. Its pinned SSH host key matches.
The development API/helper build and deployment pass after the source-archive fix.

Live model validation was **blocked before inference** in these initial attempts. Two fresh ChatGPT OAuth flows reached the official OpenAI consent page, then returned `Operation timed out`.
Neither callback completes. The dedicated API stores zero connections and sends no inference request. Pending local login attempts are cancelled.
No local OAuth session is available to confirm retirement of a possible provider-side grant.
The user's pre-existing connection database is not changed or copied.

At the end of these initial attempts, the combined execution, default/override, denied-model and post-disconnect checks were **unverified**.
The successful follow-up below replaces that blocked status. Canvas handoff still requires the separate recovery checks.
Private preflight, authentication-attempt and cleanup records retain the disposable resource IDs and observed failures.
The owned workload VM, MicroNetwork, installed Ubuntu image/package and temporary management VM are removed after evidence export.

## Verified combined model and MicroVM execution, 2026-10-26

The combined live run passes all nine harness checks on the real Linux API/helper and Ubuntu `ubuntu-26.04-v5` MicroVM.
Runtime code is `c0480b4c`; the harness includes the source-visibility and HTTP-response fixes described below.
The run finishes at **05:12 KST on 2026-10-26**. Both tasks reach `result` and retain one real `run_command` event each.

The earlier first-registration consent requests timed out at OpenAI.
Reauthorization with the existing issued client registration completes a new OAuth exchange on a new QA host.
Only the non-secret client/validated-subject mapping is initialized in the private QA database. The original tokens and database are not copied or changed.
The QA host receives its own tokens through the normal callback, PKCE and signed-identity checks.
This validates returning-account authorization; first-time registration remains dependent on the provider's registration service.

Two failures in the QA harness are fixed before the complete passing run:

- The installed API uses `PrivateTmp=yes` and `ProtectHome=yes`. A source created in the command shell's `/tmp` is invisible to the API. The fixture now uses a private API-user-owned directory under `/srv/firecrab-qa`, with the service protections retained. The API validates source access before any model probe.
- Successful task responses contain `error: null`; retained task failures can contain an error string. The HTTP client now checks status before interpreting API-error objects. An already-admitted command is inspected and exported after the original parser failure, without resubmitting it.

The harness regression suite now passes **14 tests**.

| Selection | Model | Task | Attempt | Guest nonce returned by the model |
| --- | --- | --- | --- | --- |
| Project default | `gpt-6.1-sol` | `ab6fd85c-da0a-48e6-baef-4144260b6d28` | `4d8ef9e5-f4a5-4339-a817-844dccbe5429` | `58f6fa4f5a8607a0aaa3b3ad8bfcbb35` |
| Task override | `gpt-6-luna` | `4589abec-9186-456a-b975-8055f002c884` | `a4e2c9c6-249f-4795-95a5-3faa81b490db` | `3fb559c09d428fd64b85b7f40102559d` |

The VM is `8355498f-fd60-47b7-898c-296f59fbdcce` and both tasks retain runtime `e9633e7f-4c1b-4111-9686-e125913a68a1`.
Project `d1189ca2-0fed-474e-b2b9-c11b6041e42b` keeps `gpt-6.1-sol` as its default after the override.
Each exact command proves an unprivileged UID, no-new-privileges and an empty provider-key/SSH-agent environment, then generates 128 bits from `/dev/urandom`.
Its nonce appears in stdout, the model's final response and the exported `guest-result.txt` diff. Stderr remains separate and the deliberate exit status is **7**.
The host source and Git worktree remain unchanged; changes are not applied.

The invalid configured model is rejected by verification and admission with HTTP **400**, without a saved proof or task.
After local disconnect, catalogue discovery, verification and task admission are blocked; the rejected task remains absent.
The disconnect receipt reports `deleted: true` and **`remoteRevocationConfirmed: true`** for the QA session.

The owner-only full report retains all command inputs, outputs, model responses, changes and references before project/source cleanup.
Its SHA-256 is `fe8c7b6c67627c28cb385c92169b1b6d557477420be58110b8441a8ba28c1c06`.
The executed harness SHA-256 is `e3c22b6f8e5e7d991d229786ac3c681e6f70c1ac1edc3fc1147abef636b29fba`.
An earlier completed task (`77a088c7-969e-4cc5-9bb3-5cab01145484`, nonce `321e0b93d7e51ec14756ea5dafe98e5d`) is also retained separately with its actual UI capture after parser-error reconciliation.

Host storage exhaustion occurs during report export after the passing run. The complete JSON is preserved before stopping and purging only the disposable management VM and its owned workload/network/image data.
Physical host free space returns to **18 GiB**. The runbook now requires host-level headroom for sparse disks and staging, separately from guest free space.
Timeout/cancellation, controller-crash/guest-reboot reconciliation and the Canvas handoff remain separate unchecked acceptance steps.
