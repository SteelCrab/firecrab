# Native YAML stack apply

Apply a validated declaration to one registered Firecrab host.
Start with [ownership and planning](stack-planning.md) and the [stack schema](stacks.md).
Use normal CLI host selection for every remote command.
Replace the example's template alias and placeholder image digest with real host inputs before applying it.

## Review and apply

```sh
firecrab stack validate --file stack.yaml
firecrab stack register --name demo
firecrab stack plan --file stack.yaml --json > plan.json
firecrab stack apply --file stack.yaml --expected-plan plan.json --json
firecrab stack status demo
```

Registration is explicit. Apply does not register a name or adopt an existing resource by name.
Existing resources require exact UUID bindings before planning.
Without `--expected-plan`, the CLI requests a fresh plan immediately before applying.
With `--expected-plan`, the server rejects changes to the declaration, ownership, selected template content or observed host configuration with `409 stale_stack_plan`.
Plan `inputs.templates` contains exact installed version, kernel, rootfs, optional initrd and boot-argument hashes.
`inputs.serviceDigests` contains declared OCI digest identities.
An explicit VM `templatePin` selects that exact installed version even if the alias is later repointed.
Without it, alias resolution is covered by the reviewed plan fingerprint.
Changed artifact bytes reject apply before resource mutation with `stack_template_changed`.
The executor creates VMs from the approved version and retains these inputs in the apply journal.
Later starts of an owned VM reject changed approved content, including changes after API restart.
The plan fingerprint excludes live VM metrics, lifecycle state, service health samples and logs.
Apply checks stop permissions again when executing each affected resource.

The first version supports these operations:

| Action | Apply behavior |
| --- | --- |
| `create` network | Create and provision its bridge with a reserved, owned UUID |
| `create` VM | Create a standalone VM in `created` state; do not start it |
| `create` service | Save configuration, deploy replicas and wait for actual HTTP health |
| `update` network | Change internet access while preserving undeclared network settings |
| `update` VM | Save CPU, RAM, disk growth, egress and environment; preserve its security-group selection |
| `update` service | Stop owned replicas if needed, save the complete declared configuration, deploy and wait for health |
| `deploy` service | Deploy the declared placement; stop existing owned replicas if needed |
| `noop` | Keep matching configuration; also verify a declared service is healthy |
| `retain` | Keep the removed declaration's binding, resource and disks |
| `replace`, `blocked` | Reject the whole operation before reserving UUIDs or changing resources |

Standalone VM lifecycle is outside this schema. A newly created VM remains stopped until explicitly started.
Service replicas use the existing deployment controller and its image, capacity, port and policy checks.
Service deployment success requires the exact accepted deployment ID to reach `running` with healthy replicas.
Matching configuration alone does not repair an unhealthy service.

## Interrupting existing workloads

Running standalone VM changes require `--allow-stop`.
The executor stops that owned VM, updates it and starts it again with the same UUID and disk.
An inactive VM remains inactive. Paused or transitioning VMs must be explicitly resumed or stopped first.

Service changes that still own replicas also require `--allow-stop`.
The existing service controller drains and deletes those disposable replica VMs before deploying replacements.
Review the interruption and replica disk loss before opting in:

```sh
firecrab stack plan --file stack.yaml --json > plan.json
firecrab stack apply --file stack.yaml --expected-plan plan.json --allow-stop --json
```

If any planned change needs stop permission and it was not supplied, preflight rejects the entire operation.
Other planned creates do not start first.
Standalone VM/network replacement, disk shrinking, implicit adoption and pruning are unsupported.

## Durable status and retries

The CLI prints its operation UUID before sending the request.
Choose a UUID explicitly when a caller must retain the retry identity before launching the CLI:

```sh
firecrab stack apply --file stack.yaml --operation-id OPERATION_UUID --json
firecrab stack apply-status demo --id OPERATION_UUID
```

Use a real non-nil UUID in place of the uppercase placeholder.
An accepted request returns `202` with `operationId`, `stackId`, `name`, `state`, `changes` and optional `error`.
Each step includes its exact resource UUID and `pending`, `running`, `succeeded`, `failed` or `skipped` state.
Overall states are `queued`, `running`, `succeeded`, `failed` and `interrupted`.
Status records contain input identities, field names and static error categories, never environment values or backend error details.

The journal stores a hash of the declaration, stop permission and redacted status.
It does not store the manifest. The normal resource store still contains its usual configuration.
Only one stack apply can be queued or running on a host.
An identical operation ID, declaration, registration and stop permission returns its existing status.
It never repeats successful, failed or interrupted work.
Changing any of those inputs while reusing an operation ID returns `409 stack_apply_identity_conflict`.
Retried requests can recover status even when their own reservations now make a fresh plan blocked.

`--no-wait` exits after durable acceptance. Exit zero then means accepted, not completed.
Normal apply polls until success and exits nonzero for failure or interruption.
`--timeout SECONDS` bounds CLI polling (default 3600, maximum 86400).
Disconnecting or timing out does not cancel accepted work. Use the printed operation ID to inspect it.
Service wait timeouts can also leave the underlying service controller finishing its owned operation; inspect its deployment before recovery.

## Failure and explicit recovery

All create UUIDs are reserved in the stack's ownership set before resource creation begins.
A failure leaves completed work and all reserved identities available for inspection.
Apply does not roll back by deleting resources or silently release missing reservations.
After API restart, unfinished operations become `interrupted`; they are never replayed automatically.

Inspect the exact resource IDs in `stack apply-status` and `stack status`.
Check service deployment status for its owned replica IDs and any cleanup still in progress.
Keep completed resources by leaving their bindings in place.
If a reserved resource never appeared, release only that exact missing binding after inspection.
Then review a fresh plan and use a new operation ID.

For complete test cleanup, stop a service and wait for replica cleanup, delete its saved configuration, stop/delete standalone VMs, and delete their network last.
Release each exact binding and delete the empty registration with its exact stack UUID.
That final metadata deletion removes only this registration's terminal apply history.
Active applies prevent binding changes and registration deletion.

## REST and limits

| Method | Path | Body / result |
| --- | --- | --- |
| `POST` | `/api/stacks/{name}/apply` | `{ "stack": STACK_OBJECT, "expectedFingerprint": "PLAN_HASH", "operationId": "UUID", "allowStop": false }`; return `202` with redacted status |
| `GET` | `/api/stacks/{name}/applies/{id}` | Read that exact operation's redacted status; unknown name/ID returns `404` |

The existing 64 KiB API request body limit applies, including the JSON request envelope.
Offline validation's 1 MiB YAML limit does not raise that API limit.
The planner and executor perform optimistic configuration checks; a multi-resource apply is not an atomic transaction with manual API edits.
Existing per-resource operations still enforce their locks and validation.
Partial success is recorded rather than hidden.
Git sources use [source synchronization](stack-git-source.md) as their sole apply writer.
Their services reject independent edit, deploy, rollback, retry, stop and delete requests, including while polling is paused.
Explicitly release the source or exact service binding before direct recovery operations.
Automatic replacement/pruning and a stack dashboard remain follow-up work for [#437](https://github.com/SteelCrab/firecrab/issues/437).
