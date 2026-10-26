# Sandbox control from Dashboard, CLI and MCP

All three entry points use the same `/api/ai-sandbox` REST lifecycle. Provider
tokens stay in Pipe's private server storage. Source paths refer to the API
host. Register an account in Pipe before choosing a model or project.

## Projects and task results

Project settings retain a Git ref, source-relative selected `SKILL.md` paths,
source-relative reference files, instructions and a verified model default.
Choose a borrowed MicroVM or a managed Environment. Environment tasks select
an explicit role. Task options retain dependencies, assigned agent and a bounded
retry policy. Retrying creates another task/attempt; reconnect never reruns work.

Conversation and task board use the same task records. Review shows real tool
output, tests, file changes, retained artifacts and preview bindings. Apply only
selected reviewed files. Downloaded CLI/MCP artifacts are checked against their
attempt owner, byte size and SHA-256. CLI downloads create a new file rather
than replacing an existing file. MCP artifact reads are limited to 64 KiB.

```sh
firecrab sandbox project PROJECT_ID
firecrab sandbox run PROJECT_ID TASK_ID < task.json
firecrab sandbox wait TASK_ID --timeout-seconds 30
firecrab sandbox artifact TASK_ID ARTIFACT_ID ./exported-result.bin
firecrab sandbox guest PROJECT_ID TASK_ID health
firecrab sandbox guest PROJECT_ID TASK_ID processes
```

Waits accept 1–60 seconds, include their HTTP polling time and return the last
authoritative state. `reached: false` or `interrupted` does not confirm termination.
Signals return `sent: true, terminationConfirmed: false`. Inspect processes or
use task cancellation/reconciliation to confirm termination.

## Environments

The Dashboard environment form selects discovered immutable image pins and
declares roles, CPU/memory/disk limits, services, readiness dependencies,
execution modes, allowed TCP connections and retention. Agent, network and
application readiness are separate. Undeclared connections and Internet egress
stay blocked. Export required results before temporary cleanup. Retained
Workspace disks require explicit final-deletion authorization.

```sh
firecrab sandbox environment images PROJECT_ID
firecrab sandbox environment create PROJECT_ID ENVIRONMENT_ID < environment.json
firecrab sandbox environment inspect PROJECT_ID ENVIRONMENT_ID
firecrab sandbox environment reconcile PROJECT_ID ENVIRONMENT_ID
firecrab sandbox environment export PROJECT_ID ENVIRONMENT_ID
firecrab sandbox environment stop PROJECT_ID ENVIRONMENT_ID
firecrab sandbox environment resume PROJECT_ID ENVIRONMENT_ID
```

Creation identities reserve resources before side effects. After an uncertain
response, inspect/reconcile the same ID. Do not create a replacement resource
with another ID. Deletion uses the Environment's saved resource ledger and
preserves borrowed VMs, shared images and unrelated Pipe connections.

## Fixed previews and restart

Register a connection token once in **Pipe → Firecrab Tunnel**. Then select its
saved address in a task's **Create app preview** form. Choose a task VM port,
declared Environment service or saved public-eligible Nginx hostname. Nginx
upstreams must belong to the admitted task runtime. A pre-created Quick Tunnel
is not required. Publish waits for local application readiness.

```sh
firecrab sandbox tunnel addresses
firecrab sandbox tunnel save PROJECT_ID BINDING_ID < preview.json
firecrab sandbox tunnel inspect BINDING_ID
firecrab sandbox tunnel stop BINDING_ID
firecrab sandbox tunnel publish BINDING_ID
firecrab sandbox tunnel inventory
```

Stop preserves the saved token and fixed URL. API restart does not stop Pipe's
independent address updater. Reconcile inspects actual helper state without
repeating publication. A concurrent Pipe edit or shared connector blocks stale
cleanup. Expired/revoked tokens and cross-host claims require explicit operator
repair; there is no silent connection fallback.

VM reboot changes its runtime generation. The original task/attempt remains
immutable. Start a new authorized task on the current runtime. In its preview
form, select **Reconnect an existing fixed URL**. The explicit retarget request
is `POST /api/ai-sandbox/tunnels/{old}/retarget` with
`{id: newBindingId, taskId: newTaskId, target}`. Readiness must pass before the
claim moves. The old binding becomes `superseded` and cannot control the new
binding. SQLite transfers the address claim atomically. The same Pipe token and
fixed URL are reused. Reuse the same new binding ID after uncertainty.

An uncertain handoff reserves both VM addresses, including when the first helper
inspection fails. Missing `handoffConfirmedAt` on a superseded predecessor means
that its reservation still needs confirmation. Reconcile the successor, or stop
its inspected owned connector, before deleting either VM or retargeting again.
An updated helper address alone is insufficient: its local connector target must
match, or the previous connector must be confirmed inactive. Graceful ingress with
unknown or draining workers retains the reservation. Task, attempt and runtime references never
change during this repair.

While a preview claims an address, use its Sandbox publish/stop/retarget/delete
actions. Raw Pipe start/restart/edit/delete requests for its connector or address
return `409 sandbox_preview_claimed`, including after a Sandbox stop. Unrelated
individual connectors and addresses remain editable. Full Pipe manifests, legacy
Quick Connect, global Pipe controls and token registration can affect a claimed
mapping indirectly. Delete preview bindings before using those global actions.
Status inspection remains available.

`helperConnected` is separate from verified public HTTPS, WebSocket handshake,
public shutdown and remote retirement. An actual HTTPS check requires the exact
expected bounded response-body SHA-256. A WebSocket check validates its protocol
upgrade; application frames still require separate tests. Local deletion does
not prove public shutdown or remote token/address retirement. Complete required
operator action in the connection dashboard. Operator-only `tunnel inventory`
retains these warnings after project deletion. Ordinary project grants cannot
access that inventory or mutate Tunnel connections.

## Local stdio MCP

Run `firecrab --api http://127.0.0.1:PORT mcp` from an official MCP client.
Use `tools/list` for typed input/result schemas. Image/network discovery precedes
VM creation. `vm_create` requires an explicit `microNetworkId`. `vm_wait` proves
only lifecycle state; inspect agent/network readiness and record an actual guest
egress probe separately.

MCP `network_create.internetEnabled` defaults to `false` and
`vm_create.egressPolicy` defaults to `isolated`. Set either explicitly for each
recipe. Internet-capable networks do not override an isolated VM policy. These
MCP defaults are narrower than the existing general REST VM defaults. The VM
response reports the effective `egressPolicy`; inspect it before start.

`network_create` and `vm_create` retain a session-local `operationId`. Reuse it
with identical input after uncertainty. A name-matched candidate does not prove
ownership and cannot be deleted by session cleanup. `owned_resources` retains
creation identities. Delete receipts remove ownership only after REST GET 404.
Pending deletion keeps `deleted: null` and `owned: true`. EOF and disconnect do
not delete resources. Existing-VM network reassignment remains unsupported until
its REST capability exists.

`scripts/sandbox-mcp-session.mjs` is an interactive official SDK client for QA.
Supply the binary, installed `@modelcontextprotocol/sdk` directory, approved
loopback API and a new private trace file. It starts read-only by default. Send
one `{name, arguments}` JSON line at a time, inspect its actual result, then
choose the next call. It does not provision resources automatically. The full
SDK tool list and I/O are retained in the 0600 trace. Use
`mutations-authorized` only after the operator approves the exact QA runtime.

Fixture tests and SDK/fixture compatibility do not complete real provider,
MicroVM, public HTTPS, WebSocket or shutdown acceptance. Canvas/PTY integration
remains governed by the separate foundation handoff and #496.
