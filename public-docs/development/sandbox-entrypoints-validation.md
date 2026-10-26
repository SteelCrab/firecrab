# Sandbox entrypoint validation — 2026-10-26

This record separates disposable fixtures from the real model/MicroVM run.
It does not accept the Canvas foundation gate or public Tunnel deployment.

## Fixtures and local builds

- Shared API types: 66 tests, including Canvas role limits and immutable reconnect/cancel references.
- Tunnel REST/persistence: 10 focused tests. These cover delegate denial, retained operator tombstones without a project, atomic successor claims, runtime/environment ownership, revoked connections and stale cleanup isolation.
- CLI Sandbox: 9 tests. MCP: 12 tests. CLI all-target Clippy passes with warnings denied.
- Frontend: 41 native tests, WASM release build and WASM Clippy with warnings denied.
- Chromium: 19 fixture tests pass. They cover English/Korean, 390/1280 px, context, dependencies, environment readiness, results/artifacts, preview binding and explicit successor retarget. Mobile cases distinguish completed tasks from failed tests and never mark zero tests as passed. Those cases also reject browser page errors during input/selection changes.
- Official MCP SDK 1.32.2 against a disposable REST fixture validates schema interoperability and session-owned cleanup. It is not real VM/provider acceptance.

The fixture preview contains no registration credential. Its public checks stay
unverified. Screenshots are generated under the E2E test-results directory.

## Real official-SDK session

The actual GPT-6.1-sol agent operated an official SDK stdio client. It read each
tool result before selecting the next call. The interactive client did not
choose or create resources automatically. No browser, login, OAuth request,
provider credential read or public service publication was performed.

The final run used CLI implementation `538ec3d8`, built explicitly before the
session. The client recorded 58 tools and the binary digest before connecting.

| Evidence | Value |
| --- | --- |
| CLI binary SHA-256 | `eadea03d1765a4bab629bd67fd5d591e534f4a65981d56eea141d6070bb9f2da` |
| Running QA API binary SHA-256 | `1011de873e278a8d0419f231c0b88993e1ab32cad97febe7fe70c442efd35917` |
| Network | `fcbd5635-613a-4655-ba9e-259e87276226`, `172.30.215.0/24` |
| Network operation | `4d681252-8f58-4ea9-8ca0-f1769c3bbab0` |
| VM | `81eb46c4-ad75-4ff4-9d2e-e6735aefde98`, Ubuntu 26.04 v5, 1 CPU, 512 MiB, 2 GiB |
| VM operation | `49c76682-3bb7-4cb5-a002-4e8e94025514` |
| Project | `bd8897fd-27eb-49db-a3ba-d2987cc1c3df` |
| Task | `0f4214cb-a833-4a55-8ab4-2c953394ca66` |
| Attempt | `72e68d2c-91bb-48ff-93ca-0de64e1e7a56` |
| Runtime generation | `25ba5595-a862-438c-9b0f-3808ed1dd242` |
| Guest boot ID | `c1cd0589-c446-4bb7-bfeb-ab3e7a0b216c` |
| Output nonce | `mcp-explicit-bd8897fd` |
| Retained artifact | `3c47770c-bd1c-4c8c-a50f-2537890e132a`, `egress.txt`, 374 bytes |
| Artifact SHA-256 | `96e652c0707906fa58479c1ff09e4527a95897a5bd11fdac5a731ee745bc1ebc` |

After image/network discovery, the agent explicitly requested
`internetEnabled: true` for the new network and `egressPolicy: isolated` for the
new VM. The create response and a separate GET both reported isolated before
start. Bounded `vm_wait` observed running; task-scoped guest health reported
systemd, execution supported, Docker/nspawn unsupported.

The saved verified GPT-6.1-sol connection executed `run_argv` in the new guest
as UID 1000. Its working directory contained the exact attempt ID. The command
used a 20-second wall limit and 128 MiB memory limit. Ordinary guest operations
succeeded. One TCP connection to `1.1.1.1:443` under a 3-second timeout returned
124. The overall command returned 0. Actual stdout/stderr were retained, the
artifact was exported and its size/digest checked, and `record_test` retained
the passed result linked to command event 1.

A 3-second socket control on the same QA management host connected to the same
endpoint in 7.95 ms and immediately closed without sending payload. This
supports the observed isolated-guest denial for that endpoint. It does not
prove every route, DNS case or destination policy. The original source
`probe.txt` still contained `not executed`; the nonce existed only in the guest
workspace/result. No source apply occurred.

After export, the same MCP session deleted its project, stopped/deleted its VM,
then deleted its empty network. Every deletion receipt confirmed REST absence.
Independent final GETs returned 404 for all three. The session ownership ledger
was empty. The pre-existing recovery VM remained running on its original
network; that borrowed network was the only remaining network.

Private evidence files are mode 0600 in `/private/tmp`:

- `firecrab-414-entrypoints-mcp-explicit-policy.jsonl`: full SDK schemas, requests/results, binary digest and host control.
- `firecrab-414-entrypoints-task-0f4214cb.json`: authoritative final task, real events, test and artifact metadata.
- `firecrab-414-entrypoints-explicit-egress.txt`: verified artifact bytes.
- `firecrab-414-entrypoints-explicit-final-rest.json`: final absence, borrowed preservation and ownership ledger.

## Preserved failures and remaining acceptance

An earlier run exposed the old MCP VM creation's hidden Internet policy. The
agent stopped only its new VM and corrected it through REST. That run is not
proof of the corrected creation inputs. Its traces were retained; the fresh
final run above exercised the corrected MCP contract end to end.

The earlier Ubuntu task attempted unavailable Python and returned 127. It
reported egress untested. A later shell task retained its actual result.
An inspection after task cleanup returned an explicit guest-generation
inspection error, not a fabricated empty process list. The stale native
`record_test` description was reported to the core owner; the final run proved
the server accepts actual `run_argv` events.

Public HTTPS content, WebSocket handshake/frames, public shutdown and remote
Tunnel retirement were not exercised. Saved credentials were not used to
publish a preview. These remain separate acceptance work after an explicit
target/domain authorization. Canvas here is only the shared identity,
permission, event/reconnect, cancellation and retention contract for #496.

Canvas implementation remains on hold until AI Sandbox implementation and
direct UI/MicroVM QA are complete and accepted. The earlier foundation-only
handoff is insufficient. Existing shared types do not authorize Canvas work.
Issues #414 and #496 record this delivery order.
