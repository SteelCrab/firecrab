# CODE workspace configuration

The first CODE implementation persists workspace configuration and references to existing accounts.
It is the data and connection-contract foundation for #556. It does not provision a VM, clone a repository, launch an editor, or run an agent.
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
Branch syntax is checked locally; the existence of that exact remote branch and repository permissions require the later GitHub integration.
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
Deletion requires a positive revision and returns `204`; an absent record returns `404`.
It remains possible to remove configuration after an account is disconnected.
Deletion does not disconnect GitHub or AI AUTH accounts.
This phase owns no VM, volume, editor process, or agent process to delete.
