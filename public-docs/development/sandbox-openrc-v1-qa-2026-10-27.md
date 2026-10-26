# Sandbox OpenRC V1 operational evidence — 2026-10-27

## Scope and current verdict

This record covers the actual OpenRC literal task and cleanup repair for #414.
The repair source is `52e1f252921feee99be19f46fdd15350988df57e`.
Before the repair, the actual task retained its command result and artifact,
but cleanup failed. A later API-only application and one reconciliation of the
same task confirm cleanup acceptance. The task becomes `cancelled`.
The original failure is not reclassified as a passing Result.

## Actual task and failure cause

| Item | Actual record |
| --- | --- |
| Task | `c8fc16f2-ef45-5aa0-b2c6-dea031cc36c7` |
| Attempt | `e2f2ac7d-08f1-4ded-907f-b10e657d6f92` |
| Environment / generation | `4b9b2c97-bff6-4623-aa30-df7e9812c141` / `584299e1-aaf3-451b-aa0a-596575d7c02d` |
| VM / runtime | `5739d926-bb90-40be-b3ef-68471659050a` / `6bcba8b4-4c7d-4b84-a01b-c2710c86f520` |
| Actual boot | `fd89bc9f-4101-4553-967e-d1fe8f98c671` |
| Command | Native `run_argv` 1 time, `run_command` 0 times, intentional exit `7` |
| Result | Separate stdout/stderr, retained final result and export, artifact 882 bytes |
| Final state before repair | `interrupted`, `sandbox_guest_cleanup_required` |

`record_test` records `passed=false` from the actual exit 7.
The intentional exit does not make this a passing test.
The retained task has 5 events.

Read-only diagnosis confirms 6 ownership records.
Each is a root:root regular file with mode 0600 and link count 1.
The attempt, boot and group-creation acknowledgments all match.
At diagnosis, the account UID and primary GID are each 1000.
The group contains exactly the owned username
`ae2f2ac7d08f14ded907fb10e657d6f`.

Alpine BusyBox `adduser -G` also adds the user to the primary group's member list.
The previous `group_registration` accepts only an empty member list.
Cleanup therefore rejects this valid owned group with exit 72.
The diagnosis does not attribute the failure to the command or provider.

## Ownership boundaries of the repair

The repair distinguishes an empty member list from exactly one owned username.
The single self-member requires a durable group-creation acknowledgment and a
protected UID record. The actual passwd name, UID and primary GID must each
match. The UID and GID are not required to be numerically equal.

Group checks before user creation and after user deletion still require an
empty member list. Foreign, multiple, duplicate or malformed members remain
rejected, as do duplicate group rows and changed GIDs.
Foreign users with the same primary GID and invalid UID records remain rejected.
A pre-existing group without a creation acknowledgment must not be deleted.
Existing attempt, boot and process checks and the global account lock remain.

## Regression results and limits

The builds and tests in this section run on local Linux.
Post-repair actual MicroVM acceptance is recorded separately in the next section.

| Check | Record | Evidence scope |
| --- | --- | --- |
| Exact 52e1 API build | Pass | Rust 1.97.1 Linux ARM64 build |
| Shell syntax for every execution mode and operation | 1 passed | Shell syntax |
| `ai_sandbox` filter | 89 passed, 5 ignored | Local API regression |
| Privileged guest filter | 4 passed, 1 failed | No full-pass claim |
| Process fixture with BusyBox applets hidden | 15 checks passed | Local Linux processes |
| Process fixture with BusyBox applets visible | 16 checks passed | Local Linux processes |
| API test Clippy, `--tests -- -D warnings` | Pass | After reinstalling the official Rust 1.97.1 component |

The privileged failure is
`guest_transport_recovers_interrupted_output_before_owned_cleanup`.
The image lacks `/usr/sbin/sshd`, so spawn at `guest_tests.rs:697` fails with
`Os { code: 2, kind: NotFound, message: "No such file or directory" }`.
The original `fixture.log` is preserved. This run is not recorded as 5 passes.

The initial build controller records exit 1 because `cargo-clippy` is missing.
The API build and shell check results are confirmed separately in that log.
After reinstalling the official 1.97.1 Clippy component, `clippy.log` confirms
a pass. The initial `handlers::ai_sandbox::tests` filter runs 0 tests.
That filter does not count as validation. The initial receipt is not overwritten.

The separate process fixture checks the actual visible BusyBox applet path.
Its positive case confirms the real group member list is `[owned username]`.
It then checks the nonroot command's exit 7, separate streams, export and cleanup.
Negative cases reject member, account-row and UID-record tampering while
preserving existing account bytes. The hidden-applet fallback checks remain.
Both runs are local Linux process checks, not actual provider or MicroVM proof.

## Actual API-only application and cleanup acceptance

The operator applies the exact 52e1 API candidate on the same QA manager.
The stage is
`/var/lib/firecrab/qa-artifacts/414-api-only/df746190-5761-40b4-a528-04fc8fc7679a`.
The API executable SHA-256 is
`e9e72f4c5c964737efe7388ada99debbb655978f7d89d0c24a811f09758555de`.
The new API PID is 199899. Helper PID 138610 remains unchanged.
All 13 VM references and 8 actual guest boots are preserved.
DNS and helper/CLI identities also remain unchanged.

The first reconciliation tool stops during preflight with HTTP 404 from an
incorrect process-query route. This is before intent creation, with 0 reconcile
POSTs. The original `root-reconcile-literal` failure record is preserved.
The correct scoped route is
`/api/ai-sandbox/projects/{project}/tasks/{task}/guest/processes`.
It returns the task process list `[]`.

The separate `root-reconcile-literal-v2` sends exactly 1 reconcile POST.
The same task becomes `cancelled`.
Its original events, result, test record and 882-byte artifact remain unchanged.
Provider calls and command replays are both 0.
Read-only guest evidence confirms the same boot and absence of the original
PID 15063 / start-ticks 388644 tuple.
Account, group and control-path cleanup proof also passes.
This accepts owned-resource cleanup. It does not change the original exit 7 or
`passed=false`.

An additional memory check is blocked by automatic approval review for
insufficient authorization before process creation. Its remote execution count
is 0. The additional 4 OpenRC checks and 2 separate TTL environments remain
pending user approval. No account login or consent is automated.
Completion of the full OpenRC set or TTL validation is not claimed.

The operator retains the following remote receipt SHA-256 values.
Original failure records are not overwritten.

| Evidence | SHA-256 |
| --- | --- |
| API-only `deployment.json` | `402f61bc1589c503803de1380062253a2c432ec7a5d9b90c1ec70b39d929e690` |
| API-only `result/verification.json` | `906c8791b65f7ca0874170edd6a1871893fe149b4d96abf228c4f7b6ba3c0dd9` |
| `root-reconcile-literal-v2` receipt | `cf6588cccb7d86e0b0c2e8dbdda98084036ccf5dd99bfecc4b80a43a68615c3d` |
| `final-verdict.json` | `830a979accd9adb0d27ffff0756469495ce22cf405584699e8f8398129453084` |
| Read-only guest proof | `ffd5343958868de34f870b1437bcf929c92a2701e9e4c93cb06e776304a02f4c` |

## Frozen evidence

Build and regression records are retained at
`/private/tmp/firecrab-414-api-cleanup-52e1f252`.
Actual task diagnosis records are retained at this local path:
`/Users/pista/firecrab-projects/worktree/feat-414-sandbox-core/target/guest-helper-qa/openrc-cleanup-diagnosis-c8fc16f2`.
The following SHA-256 values are calculated directly from those local files.

| File | SHA-256 |
| --- | --- |
| `source.tar` | `46e517cf07f438b3270f6142350dd8cd5ee4c24ae8d297e5873f4f468b26d0b8` |
| `build.log` | `d1d906a13a70b5540a9ed72b9ebfbf576dc07c1f84883f5ac4adb23ba1f90328` |
| `fixture.log` | `ef8a4771ee579242c94e0284cb967f9dabfdd5022d5bea05e5b938215da41a76` |
| `process-fixture.log` | `5ae90ffea6aac8fe9210b6af8751dfd553742915ad46da701a30288345c99e64` |
| `clippy.log` | `9b15e8c6741d47b12414443123add19569be953286ce05db13fd60aa1c2c32d9` |
| `task.json` | `dec5b9c717e2fd210ba8b5bcbd530307545f1ffccd7365169782aaf61ed2ee85` |
| `literal-artifact.json` (882 bytes) | `75935e99e9a0428ef3238a3b037edd19ab392ba5ca66ae63ea8d8184ef45d41d` |
| `openrc-cleanup-control-readonly-c8fc16f2-result.json` | `c19191cda1e513e21097fc1881805d560afe6fefe0e131fa7241eb2c3f30da6c` |

The unstripped 52e1 API build is 151,610,080 bytes with SHA-256
`473a8004add72e95b61f4db76740e07fc60b83599d95dcc18e6f91deb8ec827f`.
The separate stripped candidate is 72,718,824 bytes with SHA-256
`e9e72f4c5c964737efe7388ada99debbb655978f7d89d0c24a811f09758555de`.
The transformation receipt records unchanged loaded sections, program headers
and entry point. Build identities and actual application/cleanup acceptance
receipts are preserved as separate evidence.
