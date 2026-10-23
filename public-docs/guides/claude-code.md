# Claude Code account connection

Pipe AI can open the official Claude Code CLI's login terminal on the API host.
It provides login, current authentication status, and disconnect only.
Model selection, conversation, task execution, and a MicroVM execution adapter belong in AI Sandbox and are not implemented by this connection.

## Ownership and installation

Install the unmodified [official Claude Code CLI](https://code.claude.com/docs/en/setup) on the Linux machine running `firecrab-api`.
Use a CLI version with `claude auth login`, `claude auth status` JSON, and `claude auth logout`.
The official-CLI smoke checks used version `2.1.307` on macOS and Linux; deterministic Linux REST/PTY tests use a disposable CLI fixture.
An installed desktop CLI is not automatically available inside a remote API host, container, or MicroVM.

The API resolves `claude` through its service PATH by default.
Set `FIRECRAB_CLAUDE_CODE_BINARY` to an absolute path when the service cannot find it.
The operator installs and updates the published CLI; Firecrab does not download, modify, or impersonate it.
Do not configure an untrusted executable.

For each connection, the API sets `CLAUDE_CONFIG_DIR` to a separate owner-only directory next to its database:

```text
firecrab.db
firecrab.claude-code/
  CONNECTION_UUID/     # 0700, owned by the API service user
```

The CLI owns its credential files, system credential-store integration, login flow, and token refresh.
Firecrab never opens those credential files, extracts tokens, exchanges authorization codes, or copies credentials into SQLite.
SQLite holds the connection ID, display name, provider `anthropic`, authentication `claude_code`, and endpoint `claude-code://api-host`.
Its model list is empty and its inference/tool capabilities are false.
Service-level provider credentials and inherited authentication overrides are omitted from the CLI environment, so they cannot silently substitute a shared account for this connection.
The CLI runs as the API service user in the connection's directory.

The future AI Sandbox adapter must execute the official CLI with this same user and configuration directory.
It must not copy credentials to a guest or call Anthropic endpoints with extracted subscription tokens.
MicroVM-local CLI authentication and execution are deferred until a scoped guest execution channel is available.

## Dashboard

1. Open **Pipe → AI → Add connection**, or **Connect Claude Code** on the empty screen.
2. Select **Claude Code**, enter a connection name, and choose **Open login terminal**.
3. The embedded terminal runs only `claude auth login`, without a shell.
4. Open the URL printed by the CLI in your browser. If the CLI asks for a code, paste it into this terminal.
5. After the CLI exits, Pipe AI runs `claude auth status` and displays the actual result.

The terminal supports keyboard input, paste, selection copying, and resize.
Its output is transient: there is no server transcript, replay buffer, log export, or browser-storage persistence.
Closing the terminal or leaving the page cancels the login process group.
A login can run for at most ten minutes; up to eight login attachments may be pending/active.
Canceling a login leaves its connection metadata available for another login or disconnect.

**Check status** runs `claude auth status` without inference.
Only the validated `loggedIn` boolean is mapped to the public state; account details, stdout diagnostics, and stderr are not returned.
The states are `connected`, `signed_out`, `unavailable`, and `signing_in`.
Invalid JSON, oversized output, inconsistent exit status, and command timeout return a sanitized error instead of claiming a connection.

**Disconnect** cancels a pending/active login, runs `claude auth logout`, and verifies `signed_out` before removing metadata.
Failed logout preserves the connection for recovery.
Other connections and the user's normal CLI profile are unaffected.
Firecrab does not claim remote token revocation; its disconnect response has `remoteRevocationConfirmed: null` for this adapter.
The CLI's own logout behavior determines provider-side retirement.

## REST, CLI and MCP

All REST routes retain the host's origin policy, body limits and request deadlines.

| Method | Path | Contract |
| --- | --- | --- |
| POST | `/api/pipe/ai/connections/{id}/claude-code/login` | Accept only `{name}`; return `{connection, ticket, expiresIn: 60}` |
| GET | `/api/pipe/ai/connections/{id}/claude-code/status` | Return `{status}` from the CLI; never returns raw command output |
| DELETE | `/api/pipe/ai/connections/{id}` | Official CLI logout, signed-out verification, then metadata removal |
| WS | `/ws/pipe/ai/connections/{id}/claude-code/login` | Attach to the prepared login using the protocol below |

The WebSocket inherits origin enforcement and requires a first text frame `{ticket: UUID}` within five seconds.
The attachment is connection-bound, single-use, and expires after 60 seconds.
The ticket is not a provider credential and is never put in a URL or saved to the database.
Subsequent binary frames carry terminal input (at most 4096 bytes per frame).
A text frame `{type: "resize", cols, rows}` updates the terminal size within server limits.
Server binary frames carry CLI terminal output; the final text event is `{type: "exit", success: boolean}`.
Always check the status endpoint after exit: process success alone does not establish login state or inference permission.
Arbitrary commands, executable paths, environment variables, credential fields, and model IDs are rejected.

```sh
firecrab --api http://127.0.0.1:5523 ai claude-code login CONNECTION_UUID --name "My Claude Code"
firecrab ai claude-code status CONNECTION_UUID
firecrab ai claude-code disconnect CONNECTION_UUID
```

The CLI attaches its own terminal to the API host's login PTY; `Ctrl+]` cancels the attachment.
Use the same selected REST host for all three commands.
The stdio MCP tool `ai_claude_code_status` accepts `{id}` and returns the same secret-free status.
Login stays an interactive user action through Dashboard or CLI; MCP does not receive login URLs, authorization codes, or attachment tickets.
Existing shared model-verification/response APIs reject Claude Code connections with `409 ai_cli_only`.
An OpenAI connection and a Claude Code connection cannot replace one another under the same ID.

## Policy and validation

Each user authenticates to the published CLI with their own account and pays Anthropic directly.
Hosting Claude Code in a product is subject to Anthropic's Commercial Terms and the conditions in [Legal and compliance](https://code.claude.com/docs/en/legal-and-compliance).
The official [authentication](https://code.claude.com/docs/en/authentication) and [CLI reference](https://code.claude.com/docs/en/cli-reference) describe the native login flow.
This integration does not guarantee account availability, usage limits, or inference permission.

Linux tests use real HTTP, WebSocket and PTY I/O with a disposable CLI fixture.
They cover login, reauthentication failure, status redaction, isolated profiles, restart, failed logout, active-login cancellation, single-use/expired tickets, missing CLI, invalid/oversized output, origin enforcement, and rejection of credentials/commands/model requests.
Browser tests cover account-only controls and terminal login/cancellation at desktop/mobile sizes in English and Korean.
The installed official CLI's command contract and signed-out status are smoke-checked separately.
On 2026-10-23, the official Linux CLI also ran through the real API, WebSocket and PTY: its native OAuth login URL appeared, closing the terminal canceled the login, and official logout plus metadata removal succeeded.
That check did not complete account authentication or send inference; the existing ChatGPT connection remained present after API restart and smoke cleanup.
An actual account login was attempted later on the same date. The Claude website rejected browser verification after Google account selection; the user reported the same error when retrying manually in a regular browser.
After canceling the pending terminal, the real CLI-backed REST status was `signed_out`, while the existing ChatGPT connection remained connected. The underlying browser or network cause is undetermined. This failed attempt does not satisfy live-account acceptance.
A live Claude account login, real model response, and MicroVM-local CLI execution have not been validated.

```sh
cargo test -p firecrab-api --bin firecrab-api pipe_ai --locked  # Linux
cargo test -p firecrab-cli vm_console --locked
npm run test:pipe --prefix firecrab-e2e -- tests/pipe-ai.spec.ts
```
