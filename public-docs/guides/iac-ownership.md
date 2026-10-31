# Shared IaC ownership

This API is the stage 2 foundation of [#548](https://github.com/SteelCrab/firecrab/issues/548).
It observes, adopts, transfers and releases ownership metadata without changing resource configuration or data.
The full service contract remains in [the inventory](../reference/iac-services.md).

## Current support

Network, standalone VM, MicroService, registered storage root, volume, security group,
shell, schedule, pool, Queue, database connection, S3 connection, Secret, Secret connection,
Auth, registry, AI connection, project model-default, Sandbox environment and project ownership
can move between native Stack and a Terraform state owner.
Protected controller/pool/builder VMs and resources with active operations cannot be adopted.
Generated Sandbox environment networks, security groups and VMs retain their parent's authority.
Their reservation ledger blocks independent adoption and direct public edits, including retired environments.
Borrowed resources remain independent. Old independent child bindings can be released without deleting the child.
Other inventory kind names are recognized, but transition returns `unsupported_iac_resource` until their validation/activity adapter exists.
A Terraform ownership record does not mean the Terraform provider is implemented.
The Stack dashboard lists Terraform-owned resources and their ownership generation.

Storage roots, volumes, security groups, shells and schedules use their exact UUIDs.
Volume creation/deletion and active schedule executions block ownership transfers.
Immutable shell revisions remain pinned by their existing users.
These additional Stack ownership records appear in `/api/iac/owners`.
They count toward the same binding limit and prevent deleting the Stack registration.
Their native manifest deployment adapters remain part of the pending Stack coverage work.

Pool provisioning/draining and Queue or broker provisioning block transfers.
Database maintenance also blocks transfers through other saved aliases of the same target.
These checks include backups, migrations, restores and work awaiting recovery.
Pool leases and application row/object operations retain their existing permissions.
Connection ownership manages configuration and credentials, while external data stays with its existing owner.

Secret ownership uses the encrypted vault's file lock through the metadata commit.
Single-value edits and bulk imports check the current writer inside that same lock.
An import checks every selected destination before replacing any value.
Explicit operator reveal and external data transfers keep their existing permissions.
Values and provider credentials do not enter ownership records or acknowledgements.
A vault read failure rejects new ownership changes; an exact committed retry still returns its original result.

Auth uses the exact host-local provider slot: `firecrab`, `github` or `aws`.
Transfer preserves its currently saved account and credentials.
Native credential saves and removals check ownership in the same SQLite transaction.
An older dispatched save cannot overwrite a slot after it moves to Terraform.

Direct registry logins use lowercase hostnames and optional ports.
`registry-1.docker.io` and `docker.io` identify the same Docker Hub login.
The earlier Docker Hub API and registry API enforce the same ownership.
Shared Auth controls its derived registry connection; adopt the Auth slot instead.
A new Auth connection cannot mask an independently managed direct login.
These competing writes and transfers are checked within the same SQLite transaction.

AI connections and project model defaults use exact UUIDs.
Pending interactive login blocks a new transfer until completion or expiry.
Owned model defaults block removing their connection or verified model.
OAuth refresh keeps the same account and public configuration.
It matches the previous private credential snapshot, so a late refresh cannot undo rotation or recreate a deleted connection.
An acknowledged ownership retry returns its original result even while a newer login is pending.

Sandbox environment transfer takes the existing controller lock.
Provisioning, cleanup, VM transitions and unresolved environment tasks block transfer.
Retired environments cannot be adopted again. Controller observations and retained child authority stay intact.

Sandbox project transfer takes the existing admission lock.
Active or unresolved tasks and provisioning/cleanup of its environments block transfer.
Configuration writes also check the owner inside their SQLite transaction.
Task execution, retained task contents and borrowed source files retain their existing permissions.

The selected endpoint is the host/controller scope.
Use existing [mTLS](mtls.md) for remote requests.
Request-ID and idempotency headers do not authorize resource mutation.
Only operator API access can request ownership changes; delegated Sandbox callers retain their existing restrictions.

## Observe

| Method | Path | Result |
| --- | --- | --- |
| GET | `/api/iac/identity` | Persistent endpoint UUID, inventory kinds and implemented transfer adapters |
| GET | `/api/iac/owners` | Active native and Terraform ownership records |
| GET | `/api/iac/owners/{kind}/{id}` | Current owner and generation; null owner does not assert resource existence |
| GET | `/api/iac/owner-changes/{id}` | Immutable committed acknowledgement for an operation UUID, or 404 |
| POST | `/api/iac/owners/transition` | Atomically change ownership and persist its acknowledgement |
| GET | `/api/iac/native-operations/{id}` | Durable native operation state; observation never replays work |
| GET | `/api/iac/native-operations/resources/{kind}/{resource}` | Recorded operation IDs for an exact known resource, including completed results |
| POST | `/api/iac/native-operations/{id}/recover` | Operator recovery of the same accepted typed action |

The CLI uses the same selected host and remote mTLS as other resource commands:

```sh
firecrab --host production iac identity
firecrab --host production iac owners
firecrab --host production iac owner --kind network --id RESOURCE_UUID
firecrab --host production iac change-owner --file ownership.json
firecrab --host production iac owner-change --id OPERATION_UUID
```

Retain the exact JSON request file to recover an unknown response safely.
The CLI never generates a replacement operation UUID on retry.

UUID-backed resources use the exact UUID, not a display name.
Auth uses its provider slot key. Direct registries use their canonical host and optional port.
UUID aliases are normalized before looking up ownership.
A native owner has `manager: "stack"`, the registration UUID in `id`, and its logical binding `key`.
A Terraform owner has `manager: "terraform"`, a stable state-owner UUID, and the exact Terraform resource address in `key`.
The address is supplied by the operator; this API does not inspect a Terraform state backend.
The same Terraform owner/address cannot claim two resource identities.

## Transfer

Read the current owner and its `endpointId` first.
The API rejects a request for a different endpoint with `409 iac_endpoint_mismatch`.
The endpoint identity survives API restart and is independent of the URL or renewed TLS certificate.
Retain a fresh non-nil operation UUID before sending the request.
Replace every placeholder in this example with actual observed values:

```json
{
  "operationId": "OPERATION_UUID",
  "expectedEndpointId": "ENDPOINT_UUID",
  "resource": {"kind": "network", "id": "RESOURCE_UUID"},
  "expectedGeneration": 1,
  "expectedOwner": {
    "manager": "stack", "id": "STACK_UUID", "key": "app"
  },
  "owner": {
    "manager": "terraform", "id": "STATE_OWNER_UUID",
    "key": "firecrab_network.application"
  }
}
```

Success returns the new ownership observation.
The resource UUID, configuration and application data remain unchanged.
The old Stack binding is removed and its revision increases in the same transaction.
Transferring back adds the exact binding to the selected registered Stack.
Remove or update the old Git declaration before resuming automatic reconciliation for a transferred resource.
A paused Git source still owns its remaining bindings.

Set `owner` to null to release ownership while retaining the resource.
A released record keeps its generation to reject stale release/reclaim requests.
Legacy Stack bind/unbind/create/replace/prune paths update the same generation.
A new owner must supply both the current exact owner and generation.
A stale request returns `409 stale_iac_owner` and changes nothing.

## Retry and conflicts

An identical normalized request with the same operation UUID returns its original committed result.
Changing the request while reusing that UUID returns `409 iac_operation_identity_conflict`.
After a timeout, read the operation acknowledgement or repeat the exact request.
A later transfer does not rewrite an older acknowledgement.
Refresh current ownership before making another change.

Queued/running Stack applies and active Git checks block conflicting ownership transitions.
Direct VM/network/service edits cannot bypass Terraform ownership.
Explicitly transfer or release ownership before using independent mutation APIs.
Reads remain available.
Unsupported kinds, missing resources, protected VMs and occupied logical addresses fail before journal admission.
These metadata transactions do not implement the pending per-service deployment execution adapters.

## Native project deletion and recovery

The native mutation journal currently covers Sandbox project deletion only.
It is not the inventory-wide execution adapter required by stage 2.
The server stores the project UUID, endpoint, ownership generation and typed action before deleting its private workspace.
Its worker survives an HTTP disconnect. Ownership cannot move while the result is uncertain.
New configuration, tasks, environments and preview bindings cannot enter the reserved project.
Borrowed source directories and other projects' private workspaces are preserved.

Successful deletion returns `deleted: true` and `operationId`.
The project record and completed acknowledgement commit together after filesystem confirmation.
A filesystem failure leaves `needs_recovery` and a static error code.
Server startup changes unfinished native operations to `needs_recovery`; it does not replay them.
The project UUID remains retired after successful deletion, so a late retry cannot target a new incarnation.

After losing the response, find the original operation by the exact project UUID:

```sh
firecrab --host production iac native-operations --kind sandbox_project --id PROJECT_UUID
firecrab --host production iac native-operation --id OPERATION_UUID
firecrab --host production iac recover-native-operation --id OPERATION_UUID
```

Inspect and repair the underlying problem before explicit operator recovery.
Recovery checks the original authority and reuses the same operation, project and private workspace identity.
A completed recovery returns its immutable acknowledgement without another filesystem operation.
Operation metadata contains no project settings, prompts, credentials or filesystem paths.


## Reproduce dashboard evidence

Build the frontend and start the disposable test server from the repository root:

```sh
npm run build --prefix firecrab-frontend
cargo build -p firecrab-cli --bin firecrab --locked
FIRECRAB_IAC_QA_INFO=/tmp/firecrab-iac-qa.json \
FIRECRAB_IAC_QA_ASSETS="$PWD/firecrab-frontend/dist" \
cargo test -p firecrab-api --bin firecrab-api \
  handlers::iac::tests::iac_owner_browser_fixture -- --ignored --exact --nocapture
```

In another terminal, run the browser check before the ten-minute fixture deadline:

```sh
FIRECRAB_IAC_QA_CLI="$PWD/target/debug/firecrab" \
node firecrab-e2e/scripts/capture-iac-ownership.mjs \
  /tmp/firecrab-iac-qa.json firecrab-e2e/test-results/iac
# Stop and remove the temporary server/database after capture:
touch /tmp/firecrab-iac-qa.stop
```

Install the existing E2E browser dependencies first; `FIRECRAB_IAC_QA_CHANNEL=chrome` selects installed Chrome.
The check uses actual HTTP and compiled WASM, with isolated SQLite fixture records.
It verifies direct-mutation rejection, the durable acknowledgement, desktop and mobile rendering.
The actual CLI discovers a native cleanup failure, recovers its original operation and checks the immutable retry.
It does not boot Linux/KVM workloads or run a Terraform provider.
