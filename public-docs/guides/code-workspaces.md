# CODE workspace configuration

CODE persists workspace configuration and references to existing accounts.
An explicit verification API checks repository reads and provider credentials.
Configuration and verification requests allocate no VM or agent resources.
Use the separate [CODE runtime API](code-runtime.md) to provision a MicroVM and clone the repository.
The local CODE UI mockup remains separate from these configuration APIs.

## Access and storage

All routes require the existing verified controller operator identity: either the loopback operator listener or the configured controller's mTLS identity.
A worker certificate, delegated Sandbox grant, or caller-supplied identity header does not grant CODE access.
The existing REST origin, request-size, and concurrency controls still apply.
CODE responses use `Cache-Control: no-store` so account observations are not cached.

Configuration is stored in the private control database's `code_workspaces` table.
Each record contains a stable UUID, a unique case-insensitive ASCII name, a revision, timestamps, and a configuration document.
CODE does not keep another credential store. It does not create a second Sandbox project or task state machine.
Future runtime integration must use the existing project, runtime, and execution interfaces.

## Create and update

Choose a non-nil UUID, then send `PUT /api/code/workspaces/{id}`:

```json
{
  "expectedRevision": 0,
  "spec": {
    "name": "web-app",
    "github": {
      "account": "octocat",
      "owner": "team",
      "repository": "web-app",
      "branch": "feature/login"
    },
    "runtime": "node_js",
    "resources": { "cpu": 2, "ram": 4096, "diskGb": 16 },
    "agent": {
      "agent": "codex",
      "connectionId": "11111111-1111-4111-8111-111111111111"
    }
  }
}
```

The GitHub login must match the currently saved `github` Pipe AUTH account, ignoring login case.
The repository owner may differ from that login. An organization repository is a normal example.
The reference does not accept a URL, embedded credential, revision-expression syntax, local path, or command option.
Branch syntax is checked locally; use the explicit verification API below to check the exact remote branch and repository read access.
An existing GitHub account connection proves neither repository access nor clone permission.

Supported runtime configuration values are `node_js`, `python`, and `rust`.
CPU is 1–32 vCPUs.
RAM is a power of two from 128 through 32768 MiB.
Disk is 1–500 GiB.
These are configuration bounds. Actual image minimums, network/storage placement, and host capacity must be checked before later provisioning.

`agent: null` disables AI for the workspace.
Otherwise the reference must select an existing, provider-eligible AI AUTH connection:

| Requested agent | Eligible existing authentication metadata |
| --- | --- |
| `codex` | OpenAI API key or ChatGPT account |
| `claude_code` | Anthropic API key |
| `opencode` | OpenAI or Anthropic API key |

This table only rejects incompatible account references.
No runtime authentication adapter is implemented or verified by this phase.
In particular, ChatGPT account authentication is not enabled for OpenCode by this contract.
Future adapters must verify their own credential mechanism before enabling execution.

Successful writes return `200` and start at revision 1.
To replace the configuration, read its current revision and supply that value as `expectedRevision`.
Revision checks, case-insensitive name uniqueness, and account metadata checks run in the same SQLite write transaction.
Concurrent writes from the same revision cannot both succeed.
Creation time is preserved. Update time cannot move backwards when the system clock does.

Unknown request fields are rejected, including credentials, VM IDs, workspace paths, and caller-supplied readiness states.
The API never accepts a password, provider key, or OAuth token through a CODE request.

## Observe connections

`GET /api/code/workspaces` lists records by name.
`GET /api/code/workspaces/{id}` returns one record.
Both compute current account observations from one database snapshot:

```json
"connections": {
  "github": "present_unverified",
  "ai": "present_unverified"
}
```

| Observation | Meaning |
| --- | --- |
| `present_unverified` | Account metadata exists and matches; repository permission or agent authentication remains unverified |
| `missing` | The account was disconnected or does not exist |
| `identity_changed` | The single GitHub account connection now belongs to a different login |
| `incompatible` | The referenced AI account now has an ineligible provider or authentication method |
| `not_required` | This workspace disables its agent |

Observations are not saved as readiness flags.
Disconnecting or replacing an account immediately affects the next read without deleting workspace configuration.
Repair the account or select another eligible reference before saving another configuration revision.
A later execution request must recheck the current bindings; a previously returned observation is not authorization to run an agent.

## Conflicts and deletion

Stale writes or deletes return `409 code_workspace_revision_conflict`.
If a create/update response is lost, read the chosen UUID before retrying; do not generate another UUID.
A duplicate name returns `409 code_workspace_name_taken`.
Invalid syntax, missing account references, or incompatible authentication returns `400 validation_failed` with field names.

Delete metadata with `DELETE /api/code/workspaces/{id}?expectedRevision=1`, using the current revision.
When a runtime is bound, stop and detach it first; bound configuration deletion returns `409 code_runtime_bound`.
Deletion requires a positive revision and returns `204`; an absent record returns `404`.
It remains possible to remove configuration after an account is disconnected.
Deletion does not disconnect GitHub or AI AUTH accounts.
The configuration APIs own no VM, volume, editor process, or agent process to delete.

## Verify repository and AUTH credentials

Send `POST /api/code/workspaces/{id}/verify` with the current revision:

```json
{ "expectedRevision": 1 }
```

The same verified operator identity and origin controls apply.
The request accepts no credentials, URLs, model names, provider overrides, or readiness flags.
Verification does not change configuration revisions or save a readiness flag.
The response uses `Cache-Control: no-store`.

```json
{
  "id": "22222222-2222-4222-8222-222222222222",
  "revision": 1,
  "checkedAtMs": 1793332800000,
  "github": {
    "status": "verified",
    "commitSha": "1234567890abcdef1234567890abcdef12345678"
  },
  "ai": {
    "status": "credentials_verified",
    "method": "openai_api_key"
  }
}
```

The GitHub check uses only the saved Pipe AUTH GitHub token and fixed `https://api.github.com` endpoints.
It verifies the authenticated login, repository identity, and exact `refs/heads/{branch}` commit reference.
Organization owners may differ from the authenticated login.
Path segments are encoded separately, including branches with slashes.
Redirects are rejected, so moved repositories require correcting the saved selection.
For private repositories, the token must grant repository content reads; a GHCR package connection alone does not establish that permission.
GitHub documents the exact-reference contract and required contents permission in [Get a reference](https://docs.github.com/en/rest/git/refs?apiVersion=2026-03-10#get-a-reference).
Public repository readability does not prove private-repository privileges or push access.
No Git clone, GitHub write, or repository script is executed.

The AI check first validates the agent/provider combination and the saved authentication metadata.
It then uses the existing provider clients with fixed endpoints:

| Selected authentication | Probe | Success method |
| --- | --- | --- |
| OpenAI API key | OpenAI model catalogue (`object`/`data` response) | `openai_api_key` |
| ChatGPT plan account | ChatGPT account model catalogue (`models` response) | `chatgpt_plan` |
| Anthropic API key | Anthropic Models API | `anthropic_api_key` |

ChatGPT uses the public `/v1/models` endpoint with the selected account's OAuth bearer token, as documented in [Models and inference](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference).
The session must retain `resource.invoke` and `chatgpt.tokens.use.direct` consent.
Expiring tokens reuse the existing Pipe AI refresh flow.
Fresh credentials replace only the exact saved account read before refresh; they cannot recreate a disconnected account or overwrite a replacement.
Scope and expiry are checked again after refresh.
Refresh tokens stay in the existing private AUTH store.

Agent-disabled workspaces return `ai: {"status":"not_required"}`.
Failed checks return `status: "failed"` with a stable `reason`:

| Reason | Recovery |
| --- | --- |
| `missing` | Reconnect or select an existing account |
| `identity_changed` | Select the intended GitHub account again |
| `incompatible` | Select an eligible provider/authentication combination |
| `authentication_rejected` | Reconnect the expired, revoked, or invalid credential |
| `consent_required` | Reconnect ChatGPT and grant plan-usage consent |
| `access_denied` | Check repository permission and organization SSO approval |
| `not_found` | Check the repository and exact branch; private resources may be hidden |
| `rate_limited` | Retry after provider limits recover |
| `redirected` | Correct the moved repository selection |
| `invalid_response` | Retry or inspect provider availability; invalid or oversized data was rejected |
| `unavailable` | Retry when the provider becomes available |
| `timed_out` | Retry the bounded provider check |

Provider bodies, credential values, and redirect destinations never appear in the report.
GitHub responses are capped at 128 KiB with a six-second request timeout and a 20-second overall GitHub deadline.
The AI check has a 25-second overall deadline, including refresh.
The existing AI mutation gate serializes this operation with local AI replacement and disconnect; a busy gate returns `409 ai_busy`.

Independent repository and AI check failures are reported under HTTP `200` so the setup can show both results.
A stale initial revision returns `409 code_workspace_revision_conflict` before contacting providers.
The API compares the configuration and exact credential documents again after network requests.
Changed or deleted configuration/accounts return `409 code_workspace_bindings_changed`, including same-ID credential rotations from another database connection.
Read the current configuration and retry.

The report is a point-in-time observation, not a reusable authorization grant.
GET configuration responses retain their metadata-only `present_unverified` observations.
Provider credential acceptance does not verify model inference, CLI installation, CLI login, or an agent authentication adapter.
Later clone and agent execution must recheck current credentials and perform their own operation-specific checks.
