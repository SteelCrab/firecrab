# Shared resource plans

The shared planning API is part of [#548 stage 2](../reference/iac.md).
The first executable configuration adapter is `shell`.
`GET /api/iac/identity` advertises it separately in `resourcePlanKinds`.
The 43 ownership adapters do not imply 43 executable configuration adapters.
Native Stack expansion and the typed Terraform provider remain pending.

## Plan and review

Use existing authenticated operator access and [remote mTLS](mtls.md).
Transport authentication does not replace resource ownership.

`POST /api/iac/resource-plans` accepts a typed declaration:

```json
{
  "operationId": "11111111-1111-4111-8111-111111111111",
  "expectedEndpointId": "22222222-2222-4222-8222-222222222222",
  "owner": {
    "manager": "terraform",
    "id": "33333333-3333-4333-8333-333333333333",
    "key": "firecrab_shell.setup"
  },
  "resource": null,
  "desired": {
    "kind": "shell",
    "configuration": {
      "name": "setup",
      "description": "Guest setup script",
      "content": "echo ready\n"
    }
  }
}
```

Use `manager: stack` with an existing Stack registration UUID and a logical key
for the same server behavior. This API does not add Shells to the legacy YAML.
A null resource requests a new UUID; names never adopt another Shell.
For an existing Shell, supply `{"kind":"shell","id":"SHELL_UUID"}`.
It must already have the exact owner through explicit [adoption](iac-ownership.md).
A null `desired` requests deletion of that exact owned Shell.

Planning saves its original observation and allocates output identities.
It does not create a Shell, change ownership or execute its script.
Retrying the same operation UUID returns the original plan.
Changing that UUID's request is rejected.
Unknown fields and unsupported configuration kinds fail before mutation.

The result exposes `create`, `update`, `noop` or `delete`, changed field names,
exact ownership, a `planSha256` and the first-admission deadline.
Script bodies and description values stay out of public plan and receipt views.
Read the original result through `GET /api/iac/resource-plans/PLAN_UUID`.

## Execute and recover acknowledgements

`POST /api/iac/resource-plans/PLAN_UUID/apply` accepts only:

```json
{
  "expectedEndpointId": "22222222-2222-4222-8222-222222222222",
  "planSha256": "COPY_THE_EXACT_64_CHARACTER_PLAN_HASH"
}
```

The server rechecks ownership, the exact observed revision and configuration,
active operations, parent restrictions and references in one SQLite transaction.
A changed observation or an expired ten-minute plan requires a new plan UUID.
No request can substitute new configuration at execution time.
The configuration, ownership and completion receipt commit atomically.
An HTTP timeout does not instruct the client to create a second resource.

After a lost response, repeat the exact apply request or read
`GET /api/iac/resource-plans/PLAN_UUID/receipt`.
A completed receipt remains readable after expiry, API restart, later drift,
management transfer and deletion. Repeating it never repeats the mutation.
Plan UUIDs share the existing ownership, native-operation and Stack namespace.
An advisory plan reserves its UUID, not a resource execution lock.

## Shell behavior and current limits

Name and description updates preserve the Shell UUID and its current revision.
Changed content appends one immutable revision. Existing VM pins keep their
original revision. An unchanged configuration creates no new revision.
Deletion rejects any remaining VM pin and never deletes a VM or guest data.
Direct public revision/deletion writes recheck authority inside their transaction.
Two API processes cannot both commit plans based on the same earlier revision.

Shell readiness here means saved metadata and an available immutable revision.
Saving a Shell does not run its guest script. Guest execution, other configuration
adapters, complete Stack/Terraform workflows and every-row live acceptance are
separate unfinished requirements. Do not merge #548 until all are verified.
