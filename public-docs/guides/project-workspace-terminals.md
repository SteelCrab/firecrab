# Project workspace terminals

Related: #550, #496. The backend uses separate real PTYs. These are independent
from the VM's root serial console. The canvas/editor UI and integrated browser
acceptance are tracked separately in the implementation plan.

## Lifecycle and storage

Each terminal has a caller-selected session UUID, an immutable creation request,
a project binding generation and a VM runtime identity. A guest supervisor owns
its PTY. Closing a browser or restarting the API does not launch another shell.
The API records session metadata and bounded output in the private project SQLite
database. The supervisor also retains synced output on the project volume.

After a VM reboot or runtime replacement, an old terminal becomes interrupted.
Its recorded output remains available. Creating another terminal requires a new
session UUID. No previous command or process memory is replayed. An uncertain
launch remains pending while that runtime may still complete it; recovery only
observes it. It does not dispatch another launch. A confirmed new runtime ends
that uncertainty.

A project permits eight active terminals and retains at most 32 terminal
histories. Each supervisor retains up to 8 MiB of output. Controller replay keeps
about 8 MiB per terminal, plus at most one bounded chunk. Replay cursors are
absolute byte positions. A client whose cursor was evicted receives an explicit
truncation indication and the earliest retained cursor. History is not silently
deleted to make room for more sessions.

## Permissions and input control

Project read access permits listing sessions and replaying their output, including
when the VM is stopped. Creating a terminal, claiming control, typing, resizing
and stopping require project read/write/execute and VM read/execute access.
Every input message checks current grants and the current runtime binding.

Only one lease controls a terminal's input. Claiming control returns a private
token with a client UUID, generation and 30-second expiry. Session listings never
return that token. Renewal requires the same proof. Taking over another active
controller requires an explicit `takeOver: true` and the current generation.
Old tokens cannot type, renew or release a newer lease. A new API owner invalidates
leases from the previous API owner.

The guest launches each shell with a distinct non-root UID, a shared project
group, cleared supplementary groups and `no_new_privs`. The retained UID ledger
prevents reuse for another session. Cancellation uses pidfds and verified UID
membership, including detached and non-dumpable descendants. The metadata
directory is protected by a bind mount before enabling group writes on project
files. The guest protocol is root-only and uses pinned SSH. It exposes no host
shell or provider credentials.

One service holds a project ownership lock across requests. A second live
controller receives `workspace_busy`. The OS releases the lock when the owner
exits. Each operation also retains its SQLite ownership epoch and operation lock.

## API

All paths below start with
`/api/ai-sandbox/projects/{project}/workspace/terminals`.

| Method | Path | Result |
| --- | --- | --- |
| GET | `/` | Authorized terminal metadata |
| PUT | `/{session}` | Admit one stable creation request |
| GET | `/{session}` | Current observed status and replay cursors |
| GET | `/{session}/output?cursor=0` | Bounded base64 replay page |
| POST | `/{session}/control` | Claim or explicitly take over input |
| PUT | `/{session}/control` | Renew the current lease |
| DELETE | `/{session}/control` | Release only the current lease |
| POST | `/{session}/input` | Input, resize or renew message |
| POST | `/{session}/stop` | Request termination with current control proof |
| GET | `/{session}/socket?cursor=0` | WebSocket replay and controls |

Example creation body:

```json
{"label":"Build","workingDirectory":"web","rows":24,"columns":80}
```

Working directories are existing relative directories under `/workspace/shared`.
An empty directory selects the project root. Absolute paths, parent traversal,
symlinks and reserved metadata paths are rejected. Initial dimensions are part
of the immutable creation request. Use resizing for later dimension changes.

Claim input with `clientId`, `expectedGeneration` and optional `takeOver`. Send
the returned `clientId`, `generation` and `token` as the `control` object:

```json
{"type":"input","control":{"clientId":"CLIENT_UUID","generation":1,"token":"TOKEN_UUID"},"contentBase64":"bHMK"}
```

The WebSocket negotiates `firecrab.terminal.v1`. It sends `state`, `output` and
`control_error` JSON messages. It accepts `input`, `resize` and `renew` messages.
Tokens stay in message bodies; they are not URL parameters. A control error or
transport timeout does not authorize replaying previous input.

## Verification

`scripts/test-workspace-terminals.py` runs actual PTYs in a temporary Linux root
fixture. It checks independence, reconnect identity, resize, Ctrl+C, termination,
supervisor crash recovery, private metadata, non-root privileges, retained output
and UID reuse. Its test build skips the production ext4 identity and bind-mount
checks. Those checks require a real managed VM and are not implied by this fixture.

Run root fixtures in a disposable Linux container or an isolated QA host. The
fixture does not create system accounts or change host networking. The production
helper is built statically with `scripts/build-guest-console.sh`.
