# Stack ownership and planning

Register a stack on one Firecrab host and compare its YAML declaration to existing configuration.
This extends [offline stack validation](stacks.md) for [GitOps IaC #437](https://github.com/SteelCrab/firecrab/issues/437).
Planning does not create, update, stop, or delete resources.

For a separate Terraform REST API proof, see the [experimental example](../../examples/terraform/basic/README.md).
It manages network, stopped VM and saved service configuration through an external REST provider.
It does not apply YAML stacks or run Git reconciliation.

## Register and inspect

Use the usual [host selection](../reference/cli.md#host-profiles) flags for remote commands.
Keep remote APIs behind their existing trusted connection.

```sh
firecrab stack validate --file examples/stacks/basic.yaml
firecrab stack register --name demo
firecrab stack list
firecrab stack status demo
firecrab stack plan --file examples/stacks/basic.yaml --json
```

`validate` remains offline and ignores host profiles.
The other commands contact the selected host.
Registration stores a name and stable UUID in that host's existing SQLite database.
Repeating registration returns the same UUID.
It does not store the manifest or its environment values.

## Explicit ownership

An existing resource is never adopted by name.
Copy its UUID from the relevant resource list and specify the manifest's logical key:

```sh
firecrab network list --json
firecrab stack bind --stack demo --kind network --key app --id NETWORK_UUID
firecrab stack bind --stack demo --kind vm --key worker --id VM_UUID
firecrab stack bind --stack demo --kind service --key web --id SERVICE_UUID
firecrab stack status demo
firecrab stack plan --file examples/stacks/basic.yaml
```

Kinds are `network`, `vm`, and `service`.
Replace the uppercase UUID placeholders before running these commands.
Each key must match its map key in the YAML manifest.
A stack can own at most 256 resources.
Each resource UUID can belong to only one stack/key within its kind.
A conflicting claim returns `409 stack_ownership_conflict`.
Repeating an identical claim does not change the ownership revision.

Missing resources and builder or service-owned VMs cannot be claimed.
New VM claims also wait until all service deployment/stop operations have finished.
This prevents adopting a newly created replica before its deployment record lists it.
Claim the MicroService configuration instead of claiming its replica VMs.

To correct a binding, release its exact UUID first:

```sh
firecrab stack unbind --stack demo --kind vm --key worker --id VM_UUID
```

Unbinding only removes ownership metadata.
It keeps the resource, VM disk, and application data.
An incorrect UUID cannot release a newer binding.
Repeated release is harmless when the logical slot is empty.
A stale binding can be released even after its resource has disappeared.

After releasing every binding, delete empty registration metadata with its exact stack UUID:

```sh
firecrab stack delete demo --id STACK_UUID
```

This never deletes host resources or disks.
Nonempty registrations return `409 stack_not_empty`.
A stale UUID cannot delete a newer registration with the same name.

## Plan results

Plans compare declared fields to resource UUIDs in the ownership record.
They report field names, not old/new values or environment contents.
Network dependencies appear before standalone VMs and services.
Removed declarations appear last in reverse dependency order.

| Action | Meaning |
| --- | --- |
| `create` | No resource is bound and no existing resource name conflicts |
| `noop` | The owned resource's declared configuration matches |
| `update` | Editable fields differ; applying them later may require stopping the resource |
| `replace` | Immutable fields differ, a disk would shrink, or a network dependency would be replaced |
| `deploy` | Saved service configuration matches but its deployment configuration or placement differs |
| `retain` | A binding remains after its declaration was removed; pruning is disabled |
| `blocked` | Ownership, an unavailable bound resource, a conflicting name, or a busy service prevents planning |

For example, a new network produces an entry such as:

```json
{
  "kind": "network",
  "key": "app",
  "resourceId": null,
  "action": "create",
  "fields": [],
  "reason": null
}
```

The plan envelope includes `stackId`, `name`, `revision`, `blocked`, and `changes`.
`revision` identifies the observed ownership set.
It increases when a binding is added or removed.
Plans and status reads do not change it.
`blocked: false` means the ownership/configuration comparison completed; it does not establish deployment readiness.

`stack plan` exits `1` when a plan is blocked or the command fails.
It exits `0` for other plans, including plans with proposed changes.
With `--json`, a successfully computed blocked plan is still printed on stdout before the nonzero exit.
Metadata commands print JSON.

## REST

| Method | Path | Behavior |
| --- | --- | --- |
| `POST` | `/api/stacks` | Register `{ "name": "demo" }` idempotently; return `200` |
| `GET` | `/api/stacks` | List registrations and bindings |
| `GET` | `/api/stacks/{name}` | Read the ownership set and revision |
| `POST` | `/api/stacks/{name}/bindings` | Claim an exact `{ "kind": "vm", "key": "worker", "resourceId": "UUID" }` |
| `POST` | `/api/stacks/{name}/unbind` | Release the exact binding; preserve the resource |
| `POST` | `/api/stacks/{name}/plan` | Compare a validated stack JSON object to its registered ownership set; the URL and manifest names must match |
| `DELETE` | `/api/stacks/{name}?id=STACK_UUID` | Delete empty registration metadata after matching the exact UUID; return `204` |

Planning requires prior registration and returns `404` for an unknown stack.
It does not register or bind anything implicitly.
Existing JSON, origin, request-ID, and 64 KiB API body limits apply.
Offline YAML validation still permits files up to 1 MiB, including comments.

## Limits

The plan is advisory and can become stale immediately.
It cannot be passed to an apply endpoint; no stack apply endpoint exists yet.
Ownership metadata survives API restarts but does not prevent manual resource edits or deletion.
A later plan detects differences or reports a missing bound resource.

The planner reads stored configuration, not live application health.
VM lifecycle state, CPU/memory measurements, service health samples, and logs do not create configuration drift.
It does not repair failed service instances.
An omitted VM `storageRoot` preserves existing placement during comparison; new VM placement still needs host-default resolution.
Template availability and immutable content identity, registry resolution, capacity, free ports, and conflicts outside the declared configuration still require later host validation.
The planner does not authorize replacement or data deletion.

Actual apply, stale-plan protection for mutations, deployment scheduling, dashboard status, and continuous Git reconciliation remain follow-up work.
