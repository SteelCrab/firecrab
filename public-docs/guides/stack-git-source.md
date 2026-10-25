# Git source checks

Connect one registered stack to a GitHub repository and detect configuration drift.
Checks resolve an exact commit, read one manifest, and reuse the native stack planner.
They run periodically even when the commit is unchanged.
They **do not apply changes, deploy services, start VMs, replace resources, or prune**.
This is the source and detect-only stage of [GitOps #437](https://github.com/SteelCrab/firecrab/issues/437).

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
Wait for an active check to finish before changing source configuration or deleting its registration.
These operations return `409 stack_source_busy` while a check is active.

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
| `nextCheckAt` | Earliest next periodic check, as Unix seconds; pause suppresses it |
| `failures` | Consecutive failed checks used for backoff |

The controller verifies the fetched bytes against the selected Git blob's object hash.
It never clones or checks out the repository and never executes hooks, scripts, or included files.
Manifests remain subject to the [stack schema](stacks.md) and the 1 MiB limit.
Only source metadata, hashes, and redacted observations are persisted.

`in_sync` means the declared configuration matches the planner's host observation.
It does not prove VM startup, service readiness, or successful deployment of that commit.
`drifted` means the planner found supported changes. `blocked` requires ownership or unsupported-change review.
`failed` means acquisition, validation, or planning could not finish.
On API restart, unfinished checks become `interrupted`; a later read-only check can safely retry.
The most recent 50 checks are retained per stack, newest first. This is observation history, not deployment revision history.
Deleting an empty stack with its exact UUID also removes its source and check history.

## Manual edits and recovery

A dashboard or API configuration edit is detected by a later check of the same commit.
Checks leave that edit in place. Inspect the recorded plan before choosing to update Git or perform a normal [native YAML apply](stack-apply.md).
Changing or reverting the Git ref causes a fresh exact-commit comparison. It does not roll back guest disks or application data.
After a source error, correct the source or Auth connection and run an explicit check.
The previous completed observations remain available in history.

Git check plans are observations, not approved deployment artifacts.
They do not yet pin installed VM template content or coordinate image tag watchers.
Automatic apply, deployment revision history, immutable template approval, destructive policies, and dashboard Git status remain follow-up work.
Continue using the explicit apply operation's retry identity and recovery workflow for resource mutations.

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
| `GET` | `/api/stacks/{name}/source/history` | Latest 50 observations, newest first |

These endpoints use the existing host API transport and request protections.
Source configuration is host metadata and is not part of the resource manifest.
