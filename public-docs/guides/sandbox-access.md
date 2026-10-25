# Sandbox access

Sandbox authorizes the caller for every project, model, task, cancellation, reconciliation, and apply operation.
Provider inference verification remains a separate check.

## Caller identity

The trusted loopback plaintext listener represents the local controller operator.
On [mTLS](mtls.md), the caller is the host UUID from the verified certificate.
The configured controller certificate's own UUID represents the controller operator.
HTTP headers, task input, project instructions, and model tool arguments cannot select a caller or operator role.

The operator can manage all Sandbox resources and delegate exact rights to another host UUID.
Other hosts have no Sandbox permissions until granted explicitly.
An existing CA host without a delegation policy retains its previous access to other controller APIs; CA issuance still grants controller trust.
Install a delegation policy **before giving a restricted client its certificate**.
Once a policy exists, that host can use only Sandbox API routes and public static assets.
VM keys, consoles, account APIs, and other controller routes are blocked to prevent bypassing its grants.
Revocation retains this restriction, including after restart.

## Grant resources and operations

| Resource | Rights | Use |
| --- | --- | --- |
| Project UUID | `read`, `write`, `execute`, `apply` | Inspect; configure/delete; admit/cancel/reconcile; apply changes |
| Connection UUID | `read`, `execute` | Read model catalogue/proofs; discover, verify, and infer |
| VM UUID | `read`, `execute` | Identify the selected resource; execute/recover owned guest work |
| Source directory | `read`, `write` | Snapshot/read source context; apply reviewed changes |

Rights are explicit and do not imply other rights.
Source grants match the exact existing canonical API-host directory, without descendant or path-alias inheritance.
Task inspection also requires read access to its original source. Lists filter captured sources before pagination, including after a project changes source directories.
Applying a result requires project `read`/`apply` and original-source `read`/`write`, even if the project configuration later changes.
Cancellation/reconciliation do not require provider access; they still require the project and captured VM permissions.
Only project-authorized results are listed. Credentials are never returned.

`GET /api/ai-sandbox/access/HOST_UUID` reads a policy.
`PUT` atomically replaces `{grants:[{resource,permissions}]}` for that host.
`DELETE` stores an empty policy and revokes all its Sandbox rights.
These endpoints require the controller operator.
Missing transport identity returns `401`; denied permissions return `403`.

```json
{
  "grants": [
    {"resource":{"kind":"project","id":"11111111-1111-4111-8111-111111111111"},"permissions":["read","write","execute","apply"]},
    {"resource":{"kind":"connection","id":"22222222-2222-4222-8222-222222222222"},"permissions":["read","execute"]},
    {"resource":{"kind":"vm","id":"33333333-3333-4333-8333-333333333333"},"permissions":["execute"]},
    {"resource":{"kind":"source","path":"/srv/project"},"permissions":["read","write"]}
  ]
}
```

Use actual resource UUIDs and a canonical source directory.
A project UUID can be reserved before its first save; the source, connection, and VM still need their own grants.
The server records the creator as `owner`; changing configuration does not transfer ownership or grant privileges.

```sh
firecrab sandbox set-access HOST_UUID < access.json
firecrab sandbox get-access HOST_UUID
firecrab sandbox revoke-access HOST_UUID
```

The local stdio MCP equivalents are `sandbox_access_set`, `sandbox_access_get`, and `sandbox_access_revoke`.
They use the same REST policy and the client's existing API host/mTLS profile.
The fixed model tool set cannot manage grants.

## Attempts, runtime references, and revocation

Admission persists project/task/attempt identity, the authenticated caller, and the selected `{vmId,runtimeId}` before dispatch.
Task detail and summaries return the same original runtime reference.
Checkpointing cannot change project, attempt, caller, or original runtime identity. Attempt IDs are unique.
A VM restart before queued execution fails that attempt instead of redirecting it to the new runtime.
Explicit reconciliation may recover the same VM after reboot; the original admission reference remains unchanged.

Current grants are checked before provider calls, file tools, and guest staging/commands.
All rights for one action are checked against one policy snapshot. Provider history stays within the attempt's captured source directory.
Revocation prevents subsequent calls/actions, including tool requests received from an already-running provider turn.
An in-flight provider request may still consume usage; grant revocation is not provider-side cancellation.
Controller safety cleanup can stop and recover the recorded attempt's owned guest data after revocation.
It cannot start new model commands or delete the borrowed VM.

Existing records migrate as local-operator records; existing guest bindings backfill original runtime references.
An older attempt that never captured a runtime keeps an unknown reference and is not resumed automatically.
Migration does not grant remote hosts access. Back up the private database and retained workspaces together.

This policy covers the Sandbox controller API. It does not provide guest filesystem/network isolation, managed Environments, or a completed real-model-to-MicroVM acceptance run.
