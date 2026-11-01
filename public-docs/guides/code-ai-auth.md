# CODE AI AUTH

CODE reuses the existing [Pipe AI](pipe-ai.md) account manager and its saved
connection UUIDs. It has no second credential database or OAuth implementation.

## Connect and reuse an account

Open **AI AUTH** from CODE or **Manage AI AUTH** from workspace setup or details.
The shared account manager appears in place. Returning keeps the setup fields.
Connect with ChatGPT, an OpenAI API key, or an Anthropic API key using the existing
forms. No model or inference request is required for account connection.

Choose **Back to workspace** after connecting. Setup reloads the account metadata
so the compatible account can be selected. Saving a workspace sends only the
existing `connectionId` and selected agent, never credentials. CODE does not infer
that a CLI agent is signed in from that selection.

| Agent configuration | Eligible existing accounts |
| --- | --- |
| Codex | OpenAI API key or ChatGPT account |
| Claude Code | Anthropic API key |
| OpenCode | OpenAI or Anthropic API key |

This table is configuration eligibility. Runtime authentication adapters remain
separate implementation work. OpenAI also distinguishes ChatGPT sign-in from
[API-key authentication](https://learn.chatgpt.com/docs/auth).

## Assign or remove an account after setup

Choose **Choose AI AUTH** in workspace details to change its saved agent and
account reference. Existing accounts can be reused without another sign-in.
Only compatible configuration methods appear in the account choices. Missing
selections stay visible until an operator chooses a replacement.

**Save assignment** sends the original full configuration with only its agent
binding changed and the revision originally read. A concurrent configuration
change requires cancellation and review. It cannot be overwritten by a stale
assignment. A pending MicroVM operation can also reject the change.

If a response is lost, **Check saved assignment** reads that workspace first.
It recognizes only the intended next revision with the exact desired settings.
If the original revision remains unchanged, it retries the original request.
Newer settings are never overwritten. This draft lasts for the current view;
reloading the page reads the saved configuration and sends no assignment write.

Select **No agent (remove assignment)** to remove this workspace's binding.
The shared account stays connected for other workspaces. This operation does not
require account metadata when no agent is selected. **Manage AI AUTH** instead
manages the shared account lifecycle and retains the assignment draft on return.

## Workspace and editor states

The separate [agent authentication adapter check](code-agent-auth.md) verifies
fixed CLI recipes against the current provider account and prepared MicroVM.
Unsupported handoffs remain disabled. Its disposable result is not a live login
or an execution grant.

**Editor status** opens `#/code/{workspace-uuid}/editor`. It reads the same saved
workspace and live account observations as workspace details. Missing or
disconnected, incompatible, present but unverified, unknown and no-agent states
use the same panel. Disconnecting a shared account affects every referencing
workspace on its next read without deleting their configuration or references.

The shared manager shows ChatGPT approval progress, cancellation, success and
failure. It returns to the editor through **Back to editor**. Approval does not
automatically assign the account. A provider check that requires consent or
rejects credentials directs the operator to reconnect through AI AUTH.

This editor shell displays real account state. Web VS Code, terminal and agent
controls remain disabled until their runtime integrations are implemented and
verified. Account presence, provider approval and a ready MicroVM do not enable
execution. Late reads cannot restore a previous assignment after a saved change.

## Replace, disconnect and cancel

Replacing a key or signing in again keeps the connection UUID. Every workspace
using that UUID shares the updated credentials. Failed replacement keeps the
previous saved connection. The browser clears the submitted key from the shared
form; it does not store it in CODE configuration or browser storage.

Disconnect uses the existing Pipe AI lifecycle. It removes the saved account and
recomputes CODE connection observations. ChatGPT remote revocation is attempted;
the UI reports when it cannot confirm revocation. Removing an API-key connection
does not revoke the key at its provider.

**Cancel sign-in** cancels an unconsumed login attempt without disconnecting a
saved account. It uses `DELETE /api/pipe/ai/logins/{loginId}`. The same mutation
gate serializes cancellation with callback exchange and saving. A working
callback returns busy; the UI keeps polling so the result can be observed.
Cancellation after completed sign-in returns the completed status and retains
the account. Cancelled callback state cannot be consumed later.

ChatGPT uses the existing local API authority and reachable `127.0.0.1` callback.
The shared sign-in help explains remote-host port forwarding. The browser only
receives the authorization URL and public progress, never OAuth tokens.

## Check saved connections

Workspace details can check the saved revision through the existing
`POST /api/code/workspaces/{id}/verify` API. The provider check reuses Pipe AI's
credentials, refresh, mutation gate and database compare-and-swap behavior.
It does not restore disconnected accounts or run a CLI agent.

The view reports the last provider credential check and repository-read check
separately. Results are tied to the workspace, revision, known account observations
and current UI context. Management, replacement, disconnection and navigation
invalidate stale results. These are point-in-time observations. Agent login,
editor readiness and execution require their own verified runtime adapters.

Account metadata, mutations and login progress use `Cache-Control: no-store`.
The workspace setup flow and authenticated editor integration are tracked in
#560. No VM, editor or agent starts from the account manager or connection check.
