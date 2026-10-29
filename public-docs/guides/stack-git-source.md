# Git source synchronization

Connect one registered stack to a GitHub repository and compare its declared configuration periodically, even when the commit is unchanged.
The default mode only detects drift. Explicit synchronization and opt-in automatic mode reuse [native YAML apply](stack-apply.md).
Each run resolves an exact commit and verifies one manifest blob. Replacement and pruning require explicit manifest policies.

## Configure a source

Register the name declared by the repository's manifest:

```sh
firecrab stack register --name demo
```

Save `source.json` beside your local configuration:

```json
{
  "repository": "https://github.com/YOUR_ORGANIZATION/YOUR_INFRA_REPOSITORY",
  "ref": "heads/main",
  "manifestPath": "hosts/demo/stack.yaml",
  "pollIntervalSeconds": 900,
  "syncMode": "detect_only",
  "allowStop": false,
  "paused": true
}
```

Replace the repository and path with your own values. The remote manifest must declare `name: demo`.
Use a canonical github.com HTTPS URL without `.git`, credentials, query parameters, or fragments.
Other Git hosts and SSH URLs are not supported in this stage.
`ref` accepts a branch, tag, or complete commit SHA. Prefer `heads/main` or `tags/v1` when names collide.
Git revision expressions such as `main~1` are rejected.
Paths must be relative and have at most 16 components. Traversal, symlinks, and submodules are rejected.

```sh
firecrab stack source configure demo --file source.json --expected-generation 0
firecrab stack source check demo --generation 1
firecrab stack source status demo
firecrab stack source history demo
```

`configure` validates and saves metadata without contacting GitHub.
`check` returns after durable acceptance. Read `status` until `latest.state` leaves `checking`.
Exit zero from `check` means accepted, including when the eventual result is a failure.
Repeated requests during the same active check return that check rather than starting another.
Manual checks remain available while polling is paused.

## Apply Git changes

`check` never starts an apply, including for an automatic source.
To resolve and apply the current Git configuration explicitly:

```sh
firecrab stack source sync demo --generation 1
firecrab stack source status demo
```

`sync` also works in detect-only mode and while polling is paused.
It returns after durable acceptance, not after deployment success. Poll source status for the result.
An active synchronization shares its operation ID with concurrent requests.
If a read-only check is still active, wait for it before requesting synchronization.

For automatic synchronization, set `"syncMode": "automatic"` and `"paused": false`, then configure with the current generation.
Saving this configuration authorizes future polls to apply supported changes without a separate reviewed-plan submission.
Omitting `syncMode` preserves detect-only behavior for existing configurations.
Set `"allowStop": true` only when Git may interrupt owned VMs or replace a service's disposable replicas.
The default is false. A stop requirement fails admission before resource mutation.

The native executor rechecks the plan fingerprint, reserves exact create UUIDs, and serializes stack applies on the host.
Source provenance and native apply admission are committed in the same transaction.
A request timeout or lost response does not create a second operation.
A plan containing only `noop`/`retain` actions starts no apply operation, deployment, or extra restart.
Standalone VM creation still leaves the VM stopped; updates to an already running VM retain the native stop/restart rules.
The manifest's [lifecycle policy](stack-lifecycle.md) enables replacement, pruning and optional VM disk deletion separately.
Automatic mode treats a commit enabling those flags as authorization for future destructive synchronization.
Standalone VM disks are retained by default. `allowStop` alone does not enable destructive policies.

## Private repositories

Add `"credentialRef": "github"` to select the existing GitHub connection in **Pipe → Auth**.
Its classic personal access token must have access to the selected private repository.
The existing Auth connection also requires `read:packages` for GHCR; private repository access needs the appropriate repository scope and organization authorization.
The Git source never accepts a token in its own JSON file or URL.
Without `credentialRef`, every request is anonymous, even when Auth has a GitHub connection.

Credentials go only to the fixed `api.github.com` HTTPS origin.
Redirects are rejected. Provider response bodies and credential values stay out of check records and errors.
Removing the Auth connection causes subsequent checks to report `credential_unavailable`.
An already running request can finish with the credential it read before disconnection.

## Polling and configuration changes

```sh
firecrab stack source resume demo --generation 1
firecrab stack source status demo
firecrab stack source pause demo --generation 2
```

Every configuration change, including pause/resume, increments `generation` and clears the latest observation.
Older observations remain in history. Saving identical configuration keeps its generation and observation.
Use the current generation for each command. Stale updates and checks return `409 stale_stack_source`.
Wait for an active check or apply to finish before changing source configuration or deleting its registration.
These operations return `409 stack_source_busy` while a run is active.
Pause suppresses future polls; it does not cancel an already accepted operation.

The poll interval defaults to 900 seconds and accepts 60–86400 seconds.
The scheduler checks due sources every five seconds. At most four source checks contact GitHub concurrently.
Failures double the interval for each consecutive failure, up to a factor of 64 and a maximum delay of one day.
A successful observation resets backoff. Explicit checks bypass the periodic delay.
Each check has a 90-second network-work deadline, a 15-second request deadline, and bounded response sizes.
Queued checks wait for a concurrency slot before the network deadline starts.

GitHub applies API rate limits. One check uses a commit lookup, a commit-tree lookup, one tree lookup per path component, and a blob lookup.
Allow enough interval for all configured sources, especially with anonymous access.
Failed authentication, rate limits, missing paths, oversized files, invalid YAML, and network errors produce visible failure categories.
They never become an empty declaration or trigger resource deletion.

## Read observations

| Field | Meaning |
| --- | --- |
| `latest.source` | Source configuration captured for this check; history preserves earlier generations' repository, ref, path, and credential reference |
| `latest.commit` | Exact resolved Git commit; also available when later file validation fails |
| `latest.blob` | Regular-file Git object selected from that commit's tree |
| `latest.desiredHash` | SHA256 of validated canonical manifest configuration |
| `latest.plan` | Redacted configuration comparison, including ownership revision and fingerprint |
| `latest.error` | Static error category without provider messages or manifest values |
| `latest.sync` | Whether this run may apply supported changes |
| `latest.operationId` | Linked native apply identity; null when no apply was admitted |
| `desiredRevision` | Latest validated source, generation, commit, blob, and desired hash |
| `attemptedRevision` | Last revision accepted by the native apply journal |
| `successfulRevision` | Last successful native apply or synchronization that observed no changes |
| `lastApply` | Latest native operation state, per-resource redacted results, and immutable `gitRevision` provenance |
| `recoveryOperationId` | Failed/interrupted operation that must be inspected before explicit recovery |
| `nextCheckAt` | Earliest next periodic check, as Unix seconds; pause suppresses it |
| `failures` | Consecutive failed checks used for backoff |

The controller verifies the fetched bytes against the selected Git blob's object hash.
It never clones or checks out the repository and never executes hooks, scripts, or included files.
Manifests remain subject to the [stack schema](stacks.md) and the 1 MiB limit.
Only source metadata, hashes, and redacted observations are persisted.

`in_sync` means the declared configuration matches the planner's host observation.
It does not prove VM startup, service readiness, or successful deployment of that commit.
`drifted` means the planner found supported changes. `blocked` requires ownership or unsupported-change review.
`failed` means acquisition, validation, planning, or apply admission failed before a native operation was accepted.
`applying` follows the existing native operation. `applied` means that operation succeeded, including native service health checks.
A later observation can still detect a new change; `applied` is not a permanent readiness claim.
`recovery_required` means the accepted apply failed or was interrupted.
On API restart, unfinished read-only/acquisition work becomes `interrupted` and can safely retry.
Accepted applies follow their durable journal and are never replayed automatically after interruption.
The most recent 50 observations are retained per stack, newest first. Each native apply journal retains its own `gitRevision`, also available through `/api/stacks/{name}/applies/{operationId}`.
Evicting an observation does not delete that operation's provenance or the last attempted/successful revision.
Deleting an empty stack with its exact UUID also removes its source and check history.

## Manual edits and recovery

A dashboard or API configuration edit is detected at the same commit.
Detect-only mode leaves it in place. Automatic mode restores declared supported fields.
Changing or reverting the Git ref causes a fresh exact-commit comparison. It does not roll back guest disks or application data.
After a source error, correct the source or Auth connection and run an explicit check.
The previous completed observations remain available in history.

After an accepted apply fails or is interrupted, automatic mutation stops.
Read `lastApply`, its per-resource UUIDs, and the [native recovery guidance](stack-apply.md) before continuing.
Read-only checks and source configuration changes do not clear this stop condition.
After repairing the partial state, acknowledge the recorded operation and request a fresh exact-commit plan:

```sh
firecrab stack source sync demo --generation 1 --recover-operation-id FAILED_OPERATION_UUID
```

Recovery does not replay the old operation. Missing reserved resources remain blocked until ownership is explicitly repaired; there is no adoption by name.
A successful apply or an acknowledged synchronization with no changes clears the stop condition.
Automatic polling can then continue. Git errors before apply admission use bounded backoff instead.

## Immutable inputs and controller ownership

Each Git-managed standalone VM must declare `templatePin` beside `template`.
Obtain the installed content identity from a read-only plan on the target host:

```sh
firecrab stack plan --file stack.yaml --json > plan.json
```

Copy that VM's `inputs.templates.LOGICAL_KEY` object into the manifest:

```yaml
template: alpine-3.24.1
templatePin:
  version: REPLACE_WITH_INSTALLED_VERSION
  kernelSha256: REPLACE_WITH_64_LOWERCASE_HEX_CHARACTERS
  rootfsSha256: REPLACE_WITH_64_LOWERCASE_HEX_CHARACTERS
  initrdSha256: null
  bootArgsSha256: REPLACE_WITH_64_LOWERCASE_HEX_CHARACTERS
```

These placeholders are not valid pins. Copy real hashes, including the initrd hash if present.
The exact version and all hashes must match an installed template.
Changing the current alias does not change an explicit pin while the approved version remains installed.
A missing or changed version blocks synchronization. There is no fallback to another image.
An existing VM with different recorded template content requires explicit `lifecycle.replace: true`.
Templates are checked before apply mutations. Later VM starts also check their journaled approval.
Local plans without `templatePin` select the current alias and bind its resolved hashes into the plan fingerprint.
Service declarations continue to require an OCI SHA256 digest; the registry verifies that digest before deployment.
For a multi-platform OCI index, that digest fixes the platform descriptors; deployment records the selected host-platform digest.
Apply status retains `inputs` and exact Git provenance after observation history is evicted.
Pins identify boot inputs; they do not snapshot guest disk contents or external data.

A configured Git source owns synchronization, including in detect-only or paused mode.
Independent YAML apply returns `409 stack_source_managed`; use `stack source sync` instead.
Direct service edit, deploy, rollback, retry, stop and delete requests return the same conflict.
This prevents a dashboard, tag watcher or another API client from competing for its image identity.
Internal service deployment is allowed only for the accepted stack operation's running UUID step.
To recover or manage a service directly, wait for active operations, then explicitly unbind that exact service or remove the Git source.
Pause alone does not release ownership. There is no independent registry tag watcher enabled for Git services.

Manual VM and network configuration edits remain available.
Detect-only reports their declared-field drift on the next check.
Automatic mode restores supported declared fields at the same Git commit; stop permission still applies.
Edits after a plan or during apply invalidate the affected observation and stop further execution.
Undeclared fields, VM lifecycle state and guest data remain outside drift correction.

Follow-up work remains tracked in [#437](https://github.com/SteelCrab/firecrab/issues/437).

## Repository layout and CI

```text
hosts/
  demo/
    stack.yaml
.github/
  workflows/
    validate-stacks.yml
```

Install a trusted Firecrab CLI in CI, then validate each manifest before merging:

```yaml
name: Validate stacks
on: [push, pull_request]
jobs:
  validate:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
      # Install your pinned, trusted Firecrab CLI here.
      - run: firecrab stack validate --file hosts/demo/stack.yaml --json
```

Offline validation does not require host credentials or access to the runtime API.

## REST

| Method | Path | Body / result |
| --- | --- | --- |
| `PUT` | `/api/stacks/{name}/source` | `{ "expectedGeneration": 0, "source": SOURCE_CONFIG }`; returns complete source status |
| `GET` | `/api/stacks/{name}/source` | Configuration and latest observation; `404` if not configured |
| `POST` | `/api/stacks/{name}/source/check` | `{ "generation": 1 }`; returns `202` with the durably accepted check |
| `POST` | `/api/stacks/{name}/source/sync` | `{ "generation": 1, "recoverOperationId": null }`; returns `202`; use the exact failed operation UUID for explicit recovery |
| `GET` | `/api/stacks/{name}/source/history` | Latest 50 observations, newest first |

These endpoints use the existing host API transport and request protections.
Source configuration is host metadata and is not part of the resource manifest.
