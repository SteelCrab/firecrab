# CODE Web VS Code and terminal

CODE opens an authenticated browser editor and terminal for the retained MicroVM
working folder, `/home/fc-code/project`. They run as the existing `fc-code` user.
Preparing or opening them does not run the assigned AI agent or transfer its credentials.

## Open the workspace

1. Create and start the [CODE runtime](code-runtime.md). Wait for **MicroVM ready**.
2. Open **Editor status**, then click **Prepare editor and terminal**.
3. Wait for preparation to finish. Choose **Open Web VS Code** or **Open terminal**.

The editor and terminal open in separate tabs. The editor also includes its own
integrated terminal. Files saved in either view belong to the same guest folder.
VS Code may ask you to trust a folder before running its tasks or integrated terminal.
That choice does not authenticate an AI agent.

Preparation installs the pinned official [code-server 4.144.0 release](https://github.com/coder/code-server/releases/tag/v4.144.0).
The guest verifies its compiled SHA-256 before bounded extraction into a root-owned
release directory. Both Linux ARM64 and AMD64 are supported. Use Ubuntu or another
supported glibc image with Python 3 and CA certificates. Alpine/musl images are not
supported by this standalone release. See the upstream [installation requirements](https://coder.com/docs/code-server/install).
A failed preparation retains the VM and working files; retry after resolving the cause.

A guest-local lock serializes preparation. The editor cannot execute until its process
identity is atomically recorded. A completed installation and a live guest editor
process are reused when reopening.
No repository hook or package script runs during preparation.
No VM, network, public port forwarding or storage root is created by these controls.

## Authentication and isolation

Opening starts with the controller operator's existing access. It issues a
30-second, single-use handoff for one workspace and one editor session.
The editor site exchanges it for an HttpOnly, host-only session cookie.
The browser stores no editor credential in local storage.

Each preparation session has its own browser origin. On loopback, it uses a
workspace/session hostname under `code.localhost` at the dashboard's existing port.
The development proxy and installed API both support this address.
The editor origin cannot call controller REST/WebSocket routes or embed the dashboard.
Editor content is never served under the dashboard origin.

The guest editor listens only on `127.0.0.1:17777`. The controller forwards through
its existing operator SSH key and pinned guest host key, with a private Unix socket
on the host. The guest receives neither the browser cookie nor controller headers.
Port-proxy routes in code-server are disabled. The standalone terminal uses a real
PTY with bounded binary input and resize frames; its shell drops supplementary groups
and runs under the saved workspace user's UID/GID.

Sessions last at most one hour. A changed workspace revision, runtime operation,
VM instance or ownership invalidates access. Stop/restart ends browser connections.
An unavailable editor service also closes its transport. Return to CODE and prepare
again to reopen. API restarts discard browser sessions while retaining guest files
and the installed editor; reopening checks and reuses the retained guest process.

## Remote dashboards

Loopback setup needs no additional domain or public guest port.
For a remote production dashboard, set `FIRECRAB_CODE_EDITOR_DOMAIN` to a dedicated
editor suffix, for example `code.example.com`.
Configure wildcard DNS and HTTPS for `*.code.example.com` at the existing controller
reverse proxy. Preserve the original Host and support WebSocket upgrades.
The generated workspace/session names occupy one wildcard label.

The suffix must be separate from the dashboard hostname. Do not route other apps
under it: requests in this namespace belong exclusively to the CODE gateway.
Remote editor cookies use Secure. Preparation is rejected when the default loopback
origin cannot be used and no remote editor suffix is configured.
This setting does not create DNS records, certificates or proxy infrastructure.

## API

These metadata and preparation routes require the controller operator:

| Method | Path | Result |
| --- | --- | --- |
| GET | `/api/code/workspaces/{id}/editor` | Current session metadata, or `null` when none is valid |
| POST | `/api/code/workspaces/{id}/editor` | `202` with a preparing session; duplicate requests reuse it |
| GET | `/api/code/workspaces/{id}/editor/open` | Single-use redirect to the authenticated editor site |
| GET | `/api/code/workspaces/{id}/editor/open?terminal=true` | Same session, standalone workspace terminal |

Prepare with the current saved revision and observed runtime identities:

```json
{
  "expectedRevision": 1,
  "vmId": "22222222-2222-4222-8222-222222222222",
  "runtimeId": "33333333-3333-4333-8333-333333333333",
  "operationId": "44444444-4444-4444-8444-444444444444"
}
```

Unknown fields, including commands, paths and credentials, are rejected.
A current, running, ready CODE binding is required. Metadata contains workspace,
revision, VM, runtime and operation IDs, pinned version and `preparing`, `ready` or
`failed` phase. It contains no handoff ticket or cookie.
Read the current metadata after an interrupted request; another prepare reuses the
existing valid job. A failed job permits explicit preparation again.
Editor readiness and [AI AUTH proof](code-agent-auth.md) remain separate checks.
