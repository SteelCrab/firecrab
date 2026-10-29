# Canvas and Sandbox boundary

This is a contract and design record for #414 and #496. The Sandbox foundation
handoff is **pending**. This document does not implement Canvas sessions,
interactive terminals, shared input or agent collaboration.

## Identities and authority

Canvas retains project, task, attempt, environment generation and VM runtime
references from Sandbox responses. A task's `attemptId`, `runtime.vmId` and
`runtime.runtimeId` are immutable. An Environment reference also fixes `id`,
`generation` and `role`. A reboot never silently rebinds an existing attempt.
Artifacts and Tunnel bindings retain the same task and attempt identities.

Canvas roles restrict the authenticated caller's existing grants. A share link
cannot grant a Sandbox resource right. Viewer permits observation only. Editor
permits collaboration and may request terminal input control. Agent execution
requires a separate explicit grant plus Sandbox project/resource execution
rights. Owner manages sharing but cannot exceed the caller's project/runtime
authority. Every HTTP/WebSocket operation must recheck expiry, revocation,
role, resource grants and the exact bound runtime.

Provider credentials remain in Pipe's private server storage. Canvas messages,
share links, local browser storage, task settings, guests and event replay never
carry them. An agent mention is text until an authorized explicit task request
is admitted through the existing Sandbox task endpoint.

## Existing task and result transport

| Operation | Authoritative REST contract |
| --- | --- |
| Project context/defaults | `GET /api/ai-sandbox/projects/{project}` |
| Task history/updates | `GET /api/ai-sandbox/projects/{project}/tasks` with existing paging/`since` filters |
| Idempotent task admission | `PUT /api/ai-sandbox/projects/{project}/tasks/{task}` with the same identity and identical input after uncertainty |
| Attempt/result inspection | `GET /api/ai-sandbox/tasks/{task}` |
| Explicit cancellation | `POST /api/ai-sandbox/tasks/{task}/cancel` |
| Interrupted attempt recovery | `POST /api/ai-sandbox/tasks/{task}/reconcile` |
| Reviewed source apply | `POST /api/ai-sandbox/tasks/{task}/apply` with exact selected paths |
| Retained artifact download | `GET /api/ai-sandbox/tasks/{task}/artifacts/{artifact}` |
| Environment lifecycle | `/api/ai-sandbox/projects/{project}/environments/{environment}` and its explicit actions |
| Fixed application preview | `/api/ai-sandbox/tunnels/{binding}` and its explicit actions |

Task summaries and details are projections of one durable Sandbox record.
Canvas must not create a second task engine or reinterpret a model's partial
stream as a finished result. Dependencies use task IDs in `coordination`.
`retryOf`, `retryRoot` and `retryNumber` preserve bounded explicit retries.
Tests and artifacts are retained with task/attempt references. Preview helper
health, actual public HTTPS checks and WebSocket handshake checks are separate.

## Reconnect and cancellation

On reconnect, reauthenticate, reauthorize and fetch the authoritative project
and task records. Reconcile saved IDs before dispatch. Never admit another task
because a browser disconnected. Client replay deduplicates by project/task/
attempt and event `sequence`; `updatedAt` prevents an older poll replacing a
newer result. A missing/expired permission stops observation and input.

An interrupted task needs explicit reconciliation. Cancellation is a request
until Sandbox confirms termination. A bounded client wait may end with
`reached=false`; this does not confirm cancellation. Canvas disconnect, Canvas
deletion, terminal termination, agent cancellation, Tunnel stop and VM deletion
are separate operations.

Future persistent PTY work must define create/attach/input/output/resize/
terminate, process identity, output cursor replay and replay truncation. It must
test two clients and a single current input controller. Browser reconnect must
not create another shell. Bounded guest commands do not establish any of these
PTY capabilities. No PTY/WebSocket route is promised by this contract record.

## Retention and handoff

Environment management owns VM/network cleanup. Task management owns retries.
Required results must be exported before temporary or TTL cleanup. Persistent
Workspace disks require explicit final-deletion authorization. Borrowed VMs,
shared images, unrelated Pipe connections and retained results survive Canvas
deletion and browser disconnect.

Tunnel stop retains its saved token and fixed URL. Local deletion does not
prove remote token/address retirement or public shutdown. The binding reports
operator actions and keeps public checks unverified until actual checks run.
The operator-only `GET /api/ai-sandbox/tunnels` inventory retains those warnings
after project deletion. Ordinary project grants cannot access the inventory.

Runtime restart never changes an admitted attempt. Start a new authorized task
on the current Environment/runtime, then explicitly call
`POST /api/ai-sandbox/tunnels/{old}/retarget` with
`{id: newBindingId, taskId: newTaskId, target}`. The new app must pass readiness.
The saved subdomain claim transfers atomically to the successor binding. Pipe
reuses its saved token and fixed URL. The old record becomes `superseded`; it
cannot stop, delete, publish or reconcile the successor. Inspect the new binding
after uncertainty and reuse the identical new identity, never another task or
binding ID. No registration, OAuth flow or pre-created Quick Tunnel is required.

Accept the foundation handoff only after combined real model/MicroVM execution,
permissions, timeout/cancellation, reconnect, controller crash/guest reboot
recovery, owned cleanup and CI evidence are recorded. Then validate Runtime
PTY lifecycle before Canvas shared terminals. Agent bindings follow shared
terminals. Editor/browser panels and automatic provisioning follow those gates.
