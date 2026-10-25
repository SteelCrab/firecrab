# Pipe AI

Pipe AI manages AI provider account connections and server-side credentials.
The Dashboard is limited to connecting accounts, showing connection status, replacing credentials, and disconnecting.
It supports OpenAI through ChatGPT sign-in or an API key, and Anthropic through an official API key.
Model selection, conversation, and task UI belong to **AI Sandbox** and are not implemented on the Pipe AI page.
Dashboard, CLI, and a local stdio MCP adapter reuse the same saved account connections.
This is the connection and entry-point slice of [issue #414](https://github.com/SteelCrab/firecrab/issues/414).
Project conversations, native tool calls, and guest execution are available in [AI Sandbox](ai-sandbox.md).

## Authentication and capabilities

| Capability | Supported behavior |
| --- | --- |
| OpenAI API key | OpenAI project API key sent as an HTTPS bearer credential |
| Anthropic API key | Workspace-scoped Claude Console API key sent as `x-api-key` over HTTPS; `anthropic-version: 2026-10-21` |
| ChatGPT login | Official open-source public-client flow with dynamic registration, PKCE S256, state, nonce, and signed OIDC identity validation |
| Login endpoint | `https://auth.openai.com/api/accounts/authorize` |
| Token endpoint | `https://auth.openai.com/api/accounts/oauth/token` |
| Signature keys | `https://auth.openai.com/.well-known/jwks.json`; RS256 only |
| API-key connection check | `GET https://api.openai.com/v1/models`; requires permission to list models; no inference request |
| Anthropic connection check | `GET https://api.anthropic.com/v1/models`; no inference request; redirects and custom endpoints are rejected |
| Storage | Owner-only SQLite on the API host; no browser storage, guest environment, or model-context credentials |

Account connection does not require a model or project ID.
The API-key check validates the credential but does not treat catalog entries as inference-verified models.
ChatGPT sign-in validates the OAuth exchange, signed identity, and granted scope without generating a response.
Account, workspace, region, plan, and provider limits still apply.

### Anthropic API key connections

Select **API key → Anthropic (Claude)**, enter a workspace-scoped key from [Claude Console](https://platform.claude.com/settings/keys), then choose **Verify and save**.
The server checks the [official Models API](https://platform.claude.com/docs/en/api/models/list) before saving. Authentication headers follow the [official API contract](https://platform.claude.com/docs/en/api/overview).
Provider changes clear the draft key. Failed replacement keeps the previous connection.
Pipe AI provides account connection, key replacement, and disconnect for Anthropic. AI Sandbox provides its separate model and execution adapter.
Saved metadata has no verified models and advertises `streaming: false`, `toolCalls: false`.
Supplying an Anthropic `model` during connection returns `400 ai_invalid_input`.
Shared model verification, selection, and response routes return `409 ai_connection_only` for Anthropic connections.
Use the separate AI Sandbox routes for Anthropic model discovery, verification, conversation, and native tool calls.
Disconnect removes the server's saved key; revoke the actual key in Claude Console if needed.
Claude Code installation, CLI login, terminal access, subscription tokens, and MicroVM execution are not part of this integration.

## Shared model adapter for AI Sandbox

The existing REST, CLI, and MCP model operations are integration primitives for AI Sandbox.
These model operations currently support OpenAI connections.
They do not expose model selection, conversation, or task controls in Pipe AI.
Use [AI Sandbox](ai-sandbox.md) for the Dashboard model picker, persistent conversation, task board, and guest commands. The older `/api/pipe/ai` model routes below remain OpenAI text-only compatibility APIs.

| Capability | Supported behavior |
| --- | --- |
| Inference endpoint | `POST https://api.openai.com/v1/responses`; custom endpoints and redirects are rejected |
| Models | User-configured Responses text models; a completed inference probe is required before selection |
| Model discovery | A verified-model list, rather than a provider catalog that could include inaccessible models |
| API-key streaming | Requests use `stream: false` |
| ChatGPT streaming | Requests use `store: false`, `stream: true`, and array input; the server requires `response.completed` |
| Shared REST/CLI/MCP streaming | Buffered JSON text responses; incremental client delivery is not exposed |
| Tool calls | Not enabled in the provider adapter; `toolCalls: false` prevents presenting it as an execution agent |

Login alone does not prove permission to use a ChatGPT plan or a selected model.
The ChatGPT token must include `chatgpt.tokens.use.direct`, and the selected model must return a complete text response before it is usable.
Model verification consumes plan usage or API balance; account-only connection does not perform it.
OpenAI API clients can still supply an optional `model` during connection to request an inference probe.
The probe does not request tools.

ChatGPT inference follows the [official request restrictions](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations).
It omits unsupported fields such as `max_output_tokens`.
API-key requests limit output to 1024 tokens.
Both paths have a 45-second provider deadline, a 1 MiB response limit, and no automatic inference retries.
An incomplete, interrupted, or failed response is not a successful probe.

## Dashboard

Open **Pipe → Pipe AI**, or `/#/pipe/ai`.
Select **Connect ChatGPT**.
The connection name starts as **ChatGPT** and can be changed.
Choose **Continue with ChatGPT** to authorize in the browser.
The page waits for sign-in before saving the connection.
If a popup is blocked, use **Open ChatGPT sign-in**.
Alternatively, select **Use API key**, select OpenAI or Anthropic, enter the key, and choose **Verify and save**.
The connection form also lets you switch between **ChatGPT** and **API key**.
**Sign-in help** explains the local callback requirements.
After connecting, the account list shows the connection name, authentication method, and connection status.
Use **Add connection** to connect another account.

The ChatGPT callback uses `http://127.0.0.1:API_PORT/auth/callback`.
Use the API through its local `127.0.0.1` or `localhost` address.
The browser must reach the same API at that loopback port.
For a remote host, forward the same port through SSH before login.
A reverse proxy with a different public port needs a matching loopback forward.
Arbitrary remote callback URLs are not accepted.
The login attempt expires after ten minutes and does not survive an API restart.

Use **Sign in again** or **Replace API key** to update a connection.
Replacing credentials keeps the old connection until authentication succeeds.
A successful account-only replacement resets model proofs and removes dependent defaults.
**Disconnect** removes credentials and dependent defaults, then blocks new calls.
There is no automatic fallback to another connection, model, or billing method.

ChatGPT access tokens refresh near expiry with the saved issued client ID.
The server replaces access and refresh tokens together.
AI operations are serialized on the host; a competing operation returns `409 ai_busy`.
On ChatGPT disconnect, the server attempts remote revocation as well as local removal.
If `remoteRevocationConfirmed` is false, disconnect Firecrab in ChatGPT settings.
The non-secret account/client mapping and stable host ID remain for later sign-in.
Use a new connection UUID when registering a different ChatGPT account or workspace.

## CLI

Use normal `--api`, `--host`, or `FIRECRAB_API` host selection.
For ChatGPT login:

```sh
AI_CONNECTION_ID=$(uuidgen)
firecrab ai login "$AI_CONNECTION_ID" --name Personal
```

Open the printed authorization URL in a browser.
The command waits for verified completion or failure.
Use `--no-wait` to return JSON with `loginId`, `authorizationUrl`, and `expiresIn`.
Inspect it with `firecrab ai login-status LOGIN_ID`.
The URL contains a one-time login state but no access or refresh token.

To connect an API key, write a private JSON file with these fields:

```json
{"name":"Personal","provider":"openai","apiKey":"YOUR_API_KEY"}
```

For an Anthropic connection, use `{"name":"Claude API","provider":"anthropic","apiKey":"YOUR_API_KEY"}` with the same `ai connect --file` command. Omit `model`.

```sh
chmod 600 /private/path/openai.json
firecrab ai connect "$AI_CONNECTION_ID" --file /private/path/openai.json
firecrab ai list
firecrab ai disconnect "$AI_CONNECTION_ID"
```

`--file -` reads connection JSON from stdin.
Never put a key in process arguments or shell history.
Disconnect JSON reports whether remote ChatGPT revocation was confirmed.

### AI Sandbox integration commands

These commands exercise the shared model API; they are not Pipe AI UI features.
Before use, verify a configured model with the saved account credentials.
Project UUIDs identify model settings; these endpoints do not create a Project or scheduled Task.
An explicit request selection does not replace the project default.

```sh
firecrab ai verify "$AI_CONNECTION_ID" YOUR_MODEL_ID
firecrab ai verify "$AI_CONNECTION_ID" ANOTHER_MODEL_ID
firecrab ai set-default PROJECT_UUID "$AI_CONNECTION_ID" YOUR_MODEL_ID
printf 'Say hello.\n' | firecrab ai respond PROJECT_UUID
printf 'Review this task.\n' | firecrab ai respond PROJECT_UUID \
  --connection "$AI_CONNECTION_ID" --model ANOTHER_MODEL_ID
firecrab ai get-default PROJECT_UUID
firecrab ai disconnect "$AI_CONNECTION_ID"
```

For compatibility, `ai login --model MODEL_ID` and an optional connection JSON `model` perform an inference probe during connection.
Pipe AI Dashboard does not send this option.

## Local MCP

Configure a trusted local MCP client to launch:

```json
{"mcpServers":{"firecrab":{"command":"firecrab","args":["--api","http://127.0.0.1:5523","mcp"]}}}
```

The adapter implements newline-delimited JSON-RPC over stdio with the `2025-11-25` MCP protocol.
It supports `initialize`, notifications, `ping`, `tools/list`, and `tools/call`.
Only protocol messages go to stdout.
Input messages are limited to 64 KiB.
Tool results contain text plus structured JSON; array/null results use an `items` envelope.
Inputs are typed and revalidated before they reach REST.
Guest environment values are removed from MCP output.

| Tools | Purpose |
| --- | --- |
| `images_list`, `networks_list`, `vms_list` | Discover existing resources |
| `network_create`, `vm_create` | Create resources using an explicit operation UUID and network ID |
| `vm_get`, `vm_start`, `vm_stop`, `vm_wait` | Inspect and control the existing VM lifecycle |
| `owned_resources`, `vm_delete`, `network_delete` | Inspect session ownership and perform requested cleanup |
| `vm_reassign_network` | Report unsupported behavior until REST provides it |
| `ai_connections_list`, `ai_model_verify` | Inspect secret-free connections and verify a configured model |
| `ai_default_get`, `ai_default_set`, `ai_respond` | Use project defaults or a request-specific selection |

Credentials and login tokens are not MCP tool inputs.
Connect accounts through Dashboard or CLI first.
The MCP client is a trusted administrator of its selected REST host, just like the CLI.
It inherits that host's validation, resource policy, and origin protections.
This slice does not introduce tenant/project authorization or a public remote MCP server.

Create tools append the operation UUID to the supplied name.
Reuse the same `operationId` after an uncertain response.
The adapter inspects REST state instead of sending another create.
A matching candidate after a lost response is reported as `outcome: uncertain`, `owned: false`.
Name matching cannot establish ownership; session cleanup cannot delete that candidate.
Review it through REST and use explicit operator cleanup if needed.
Never start another create with a new UUID to hide an uncertain outcome.

Confirmed resources can be deleted only by the MCP session that created them.
Borrowed networks and VMs cannot be deleted through these cleanup tools.
No resources are deleted automatically at EOF, timeout, cancellation, or client disconnect.
Ownership is not recovered after restarting the MCP process.
Stop a VM before deleting it, then delete its owned empty network.
`vm_wait` accepts 1–60 seconds; one REST read may add up to its 15-second request deadline.
It reports the final VM state with `reached: false` on timeout or VM error.
Running state and startup evidence do not prove application readiness or successful egress.
Authenticated guest execution and egress probes remain part of later #414 slices.

## REST and failures

The [API reference](../reference/api.md#pipe-ai) lists paths and bodies.
Provider error bodies and tokens are never returned or logged.
Errors distinguish rejected authentication, denied models, quota/rate limits, timeouts, and provider failures.
A timeout may represent an uncertain provider outcome; callers must not blindly retry billed requests.
`409 ai_model_unverified` requires verification before selection.
`409 ai_default_missing` requires a saved default or explicit override.
After a disconnect, an explicit selection returns `404 ai_connection_missing`.
An API restart preserves successful connections and project defaults.

See OpenAI's [sign-in contract](https://developers.openai.com/siwc/token-sharing-open-source/sign-in),
[token lifecycle](https://developers.openai.com/siwc/token-sharing-open-source/profiles-and-sessions),
the [models list endpoint](https://developers.openai.com/api/reference/resources/models/methods/list),
and [Responses authentication](https://developers.openai.com/api/reference/overview).

## Validation

Deterministic tests cover signed identity validation, PKCE/state, declined consent, token rotation, model probes, failed credential replacement, defaults, overrides, disconnect, persistence, and redaction.
MCP tests cover JSON-RPC framing, input limits, cleanup ownership, and inspection after uncertain creation.
Browser tests cover English/Korean desktop/mobile account management and assert that model/project/conversation/task controls and requests are absent.
The [MCP SDK validation script](../../scripts/validate-pipe-ai-mcp.mjs) launches a real SDK client against the CLI and a disposable REST fixture.
It validates schemas, VM lifecycle requests, duplicate-create prevention, secret omission, and cleanup ownership.
These tests use local provider fixtures; they do not establish live provider access.
Anthropic fixtures verify official request headers, credential-only verification, failed key replacement, redirect rejection, malformed catalog responses, private persistence, secret-free listing, disconnect, and blocked model use. Browser tests cover the API key lifecycle in English/Korean at desktop/mobile sizes. No real Anthropic API key was supplied, so live Anthropic verification remains unexecuted.
Live ChatGPT account connection was verified on 2026-10-23 with the user's consent.
The real browser flow completed account selection, login, agent registration, and plan-access consent.
The local API exchanged the authorization code, validated the signed identity, and saved access/refresh tokens in its owner-only SQLite database (`0600`).
Secret-free REST metadata and the account-management page retained the same connection after API restart and browser reload.
This validation supplied no model and performed no inference; the verified-model list remained empty.
Credentials and personal account details are excluded from this evidence.
Real inference and a real MCP-client MicroVM create/readiness/egress/cleanup trace remain operator validation requirements.

```sh
cargo test -p firecrab-api --bin firecrab-api pipe_ai --locked  # Linux
cargo test -p firecrab-cli mcp --locked
npm run test:pipe --prefix firecrab-e2e -- tests/pipe-ai.spec.ts
```

To run the SDK proof, install `@modelcontextprotocol/sdk@1` in a temporary directory.
Pass the built CLI and SDK package directory as absolute paths:

```sh
node scripts/validate-pipe-ai-mcp.mjs /absolute/path/firecrab \
  /absolute/path/node_modules/@modelcontextprotocol/sdk
```
