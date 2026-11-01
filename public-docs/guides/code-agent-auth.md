# CODE agent authentication adapters

Account eligibility, provider credential checks and CLI configuration proof are
different observations. Selecting an AUTH connection does not log a CLI in.

## Fixed recipes

The server owns the recipes in
`firecrab-api/src/code_agent_auth/adapters.json`. Requests cannot choose commands,
binary paths, versions, endpoints or credentials.

| Agent | Shared account method | Exact CLI | Check |
| --- | --- | --- | --- |
| Codex | OpenAI API key | 0.166.0 | Supported |
| Codex | ChatGPT | 0.166.0 | Disabled: existing session handoff is not verified |
| Claude Code | Anthropic API key | 2.1.316 | Supported |
| OpenCode | OpenAI API key | 1.19.5 | Supported |
| OpenCode | Anthropic API key | 1.19.5 | Supported |

Other combinations and unknown CLI versions are blocked. A supported recipe
means it can be checked; it does not enable execution or assert authentication.
ChatGPT account connection remains available in Pipe AI. Its registered session
is not silently converted to a Codex cache or substituted with another account.

## Verify the saved assignment

Open a saved workspace or its **Editor status** view. **Verify agent AUTH**
requires a ready MicroVM, a compatible connected account and a supported recipe.
Preinstall the exact CLI as `/usr/local/bin/codex`, `/usr/local/bin/claude`, or
`/usr/local/bin/opencode`. The executable, resolved target and parent directories
must be root-owned and not group/world writable. This check installs no package.

The operator-only APIs are:

- `GET /api/code/agent-auth/adapters`: discover fixed recipe metadata.
- `POST /api/code/workspaces/{id}/agent-auth/verify`: send only
  `{"expectedRevision":4}` for the saved assignment.

The server first checks the selected provider credentials through Pipe AI. It
then uses the pinned CODE SSH transport and the workspace-owned VM to run the
matching CLI configuration probe. No inference or agent task is requested.

Codex uses API-key login through stdin and checks its private cache and CLI
status. Claude Code must report the selected environment-key source, first-party
provider and isolated config directory. OpenCode must report exactly one
environment credential and no saved credential. The pinned CLI contracts are
checked against published native binaries with synthetic keys.

## Credential and result lifetime

Probes run as the workspace's unprivileged user outside the repository. Each
uses a fresh private directory under `/run`, a clean environment and fixed
arguments. Secrets never enter process arguments, CODE configuration, VM env
metadata, controller journals or API responses. Output and time are bounded.
Child process groups are terminated and temporary credentials are removed on
success, rejection or handled timeout. The host CLI and existing login caches
are not used. Guest restart clears the ephemeral directory as well.

Credential mutation and runtime lifecycle gates remain held if the HTTP client
disconnects. Configuration, private account snapshots and runtime identity are
checked after the probe. Same-ID key rotation, disconnect, configuration edits
or runtime changes reject stale proof. Nonces, recipe IDs and exact versions
bind each guest receipt to its own request. CLI/provider output is not returned.

`configuration_verified` means the provider accepted the key and the pinned CLI
loaded its configuration during that disposable check. It is not a live login,
model entitlement, successful inference or execution grant. The browser labels
it as the last check and invalidates it on observed context changes. Execution
remains disabled. A future agent launch must repeat credential preparation and
its runtime checks; it cannot trust this response as authorization.

## Official contracts

- [Codex authentication](https://learn.chatgpt.com/docs/auth?surface=cli) and
  [login commands](https://learn.chatgpt.com/docs/developer-commands).
- [Claude Code authentication](https://code.claude.com/docs/en/authentication)
  and [auth status](https://code.claude.com/docs/en/cli-reference).
- [OpenCode CLI authentication](https://opencode.ai/docs/cli/).

API-key configuration status alone is insufficient proof. The provider check
and current workspace/runtime observations are required independently.
