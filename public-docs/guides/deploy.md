# Deploy

Changing a referenced [PipeSecret](secrets.md#automatic-value-rotation) automatically
enters this Deploy flow for running domain services. Rotation keeps the current
image digest and configuration, uses the shared queue, and reports progress here.

Deploy updates a running MicroService through its existing Nginx domain.
The service UUID and domain stay the same.
New replicas must pass their HTTP health check before they receive traffic.
Use a compatible application and database schema across both releases.
Deploy does not restore external database or object data.

## Requirements

- A running service with a deployment-owned Nginx domain.
- The current API, helper, CLI host bundle, and Pipe configuration.
- A matching [Auth connection](pipe-auth.md) for the requested image.
- Host CPU, memory, and disk space for the rollout and recovery surge.

Apply the current Pipe configuration before the first update.
It sets `worker_shutdown_timeout 0` so Nginx does not force existing connections to close during reload.
Wait for an earlier reload to finish draining before starting another Deploy.
Services with direct host TCP ports must use the existing Stop and initial Deploy flow.
Changing the service name, domain, or network requires that flow too.

## Update and rollback

```sh
# Save the complete desired service configuration in web-v2.json.
firecrab microservice rollout SERVICE_UUID --file web-v2.json

# Inspect the current job and successful releases.
firecrab microservice status SERVICE_UUID
firecrab microservice releases SERVICE_UUID

# Restore one successful release by UUID.
firecrab microservice rollback SERVICE_UUID --release RELEASE_UUID

# Resume an interrupted operation, including a drain timeout.
firecrab microservice retry SERVICE_UUID
```

The default `--strategy rolling` starts one replacement at a time.
Each replacement passes its health check before Nginx stops sending new requests to one old replica.
The executor drains the old Nginx workers before deleting the retired VM.
Use `--strategy blue_green` to prepare the full replacement generation before switching traffic.
Blue-green needs enough capacity for both generations at once.

The CLI prints the operation UUID before submitting the request.
After a lost response, repeat the same command with `--operation-id UUID` and the same inputs.
The API returns the recorded operation instead of creating a second deployment.
Use `--expected-release UUID` to reject an update if the active release changed.
`--no-wait` returns after acceptance.
A client timeout does not cancel an accepted operation.

## Queue and cancellation

Deploy, YAML apply, Secrets rotation, rollback, and replica recovery share one
host-local worker permit. A waiting rollback runs before waiting ordinary Deploy
work. Requests in the same priority class keep their saved acceptance order.
An operation that has already started keeps its permit until it finishes or pauses.
After API restart, interrupted updates and replica repairs resume before waiting
work. Queued initial deployments also resume.

Use the **Cancel** icon while an operation is queued. The API also accepts
`POST /api/micro-services/{id}/deployments/{operationId}/cancel` without a body.
Repeat cancellation with the same operation UUID after a lost response.
The recorded attempt becomes `cancelled`. Cancelling an update restores the
previous active release without stopping its VMs or changing its Nginx routes.
An initial queued cancellation creates no VM. A later Deploy uses a new operation UUID.

Cancellation returns `409` after the worker claims execution. Inspect the operation
before choosing Retry or Stop for work that has already started.
Direct cancellation cannot change Git/Stack or controller-owned service intent.

## Automatic registry Deploy

Open **Automation** in the service's Deploy panel. Save an explicitly tagged
source such as `ghcr.io/team/app:stable` or `ghcr.io/team/app:v1.*`.
The connected Auth account must own that namespace or ECR registry.
Polling is opt-in, every 60–3600 seconds (default 300). Registry reads use normal
TLS verification, including installed system CA roots for private registry trust.

Exact tags follow their changed manifest digest. Version rules `*`, `v*`, `1.*`,
`v1.*`, `1.2.*` and `v1.2.*` select the highest matching stable semantic version.
Prereleases are excluded. Each scan reads at most ten pages and 1000 tags.
Metadata reads have a 30-second deadline. Errors back off up to one hour.
The selected host-platform digest enters the existing serial Deploy queue.
A lost acknowledgement or API restart keeps the same recorded operation UUID.

Rollback keeps the current policy and pauses its observed digest. A different
digest resumes monitoring. **Resume** explicitly permits the same digest again.
A failed or cancelled automatic operation uses the same pause rule. Interrupted
work that still owns resources must finish recovery first. **Pause** requires an
explicit Resume; a new digest cannot override it. Pause does not cancel a queued
operation. Use its Cancel icon separately. **Disable** removes monitoring.

Use `GET` or `PUT /api/micro-services/{id}/auto-deploy` to inspect or configure it:

```json
{"reference":"ghcr.io/team/app:v1.*","intervalSeconds":60}
```

Send JSON `null` with PUT to disable. Bodyless POST to `/auto-deploy/pause` or
`/auto-deploy/resume` changes monitoring. Status contains the last verified tag,
digest, pause reason, operation UUID, failure count and next check time. It never
contains registry credentials or remote response bodies.

YAML services can declare the same object under `config.autoDeploy`, a domain,
`hostPort: 0`, and an initial digest-pinned `config.image`. That image bootstraps a
new service. Reapplying YAML or Git preserves an automated running digest while
reconciling other declared settings. Removing the policy returns image ownership
to the declared pin. Git-owned policy edits still require Git sync; local runtime
pause/resume remains available. Active Stack work excludes automatic admission.
Controller-owned multi-host services are excluded.

## Dashboard

Use [Service CI Deploy](ci-deploy.md) for image-only CLI/GitHub Action deployment
with expiring service/repository tokens and exact pipeline outcomes.

In the dashboard, open a service and choose **Deploy update**.
The guided form offers the same configuration and strategy choices.
The **Releases** tab lists successful releases and offers **Roll back to selected release**.
The Overview shows the latest three, with the same roll back on the archived ones.
**Retry Deploy** resumes a failed operation that still owns resources.
The dashboard and YAML apply send the release they observed to prevent a stale update.

## Failure and recovery

Each operation stores its immutable OCI digest, service configuration, VM identifiers, and Nginx transition journal.
The API resumes a busy update after restart.
An explicitly failed recovery waits for Retry Deploy or Stop.
The executor reuses recorded VM identifiers and compares the exact route owner and upstream list before changing Nginx.
It confirms the new Nginx generation before draining old workers.
Worker identity includes the host boot ID, PID, and process start time.

An image, startup, or health failure restores the previous release through the same route transaction.
Already retired replicas can be recreated from the previous pinned image.
The failed attempt remains failed even after the previous release is running again.
Read the exact attempt endpoint to distinguish that result from the active release.

`drainTimeout` limits how long an operation waits for existing connections.
It does not force connections or VMs to close.
If the timeout expires, Deploy reports failure and retains the owned VMs and journal.
Retry after the connections finish.
A long-lived WebSocket can keep an old generation alive until it disconnects.
Normal Nginx connection and inactivity timeouts still apply.
Stop on an interrupted update withdraws its route and drains both generations before removing their VMs.

Rollback restores the recorded image digest and service configuration.
Queue bindings restore their references and access roles; broker data is unchanged.
Queues used by active or interrupted Deploy generations cannot be deleted.
It resolves the recorded Secret UUIDs and storage connection with their current values and credentials.
Automatic recovery keeps the previous generation's encrypted Secret snapshot until that generation is retired.
Missing credentials, Secrets, images, or capacity can prevent recovery.
Deploy keeps owned resources and reports the reason instead of discarding the recovery record.

## REST

| Method | Path | Body or result |
| --- | --- | --- |
| `POST` | `/api/micro-services/{id}/updates` | `{operationId, expectedReleaseId?, spec, strategy?}`; `202` |
| `GET` | `/api/micro-services/{id}/releases` | Up to 20 successful releases, newest first |
| `POST` | `/api/micro-services/{id}/rollback` | `{operationId, expectedReleaseId?, releaseId, strategy?}`; `202` |
| `POST` | `/api/micro-services/{id}/deployment/retry` | `{operationId}`; `202` |
| `GET` | `/api/micro-services/{id}/deployments/{operationId}` | Exact recorded attempt, including failed attempts |
| `POST` | `/api/micro-services/{id}/deployments/{operationId}/cancel` | No body; cancel an exact queued operation and retain its terminal outcome |

`spec` is a complete [MicroService configuration](microservices.md#fields).
`strategy` is `rolling` or `blue_green`.
An operation UUID cannot be reused with different inputs or for another service.
Concurrent operations, a stale expected release, or a stale Retry return `409`.
The active configuration changes only when the replacement succeeds.

Successful release metadata is immutable and limited to the newest 20 entries per service.
Attempt records remain available until the service is deleted.
Release records contain Secret references, not plaintext Secret values.
Deploy removes retired VM disks and private environment snapshots after verified drain.
New Deploy imports journal their own files under `rootfs/deploy/` before publication.
Cleanup removes a file only after VM, pool, current-operation, and retained-release references are gone.
It also protects active template readers and other registered aliases that share the file.
Manual images, existing imports, shared kernels, and OCI blob/layer caches stay in the [Images inventory](images.md).
Collection runs after completed deployments, at startup, and every minute.
Failed cleanup keeps its ownership journal for retry.

## Validate from source

Run `scripts/test-deploy-nginx.py --firecrab PATH --nginx PATH` for real Nginx HTTP and WebSocket reload tests.
The test uses disposable local application servers and does not boot MicroVMs.
Full acceptance also requires a Linux/KVM host with real MicroVMs, image pulls, restart recovery, and resource measurements.
The Installer CI job runs `sudo python3 scripts/ci-qa-deploy.py --disposable-host` for real MicroVM/Nginx updates, held requests, API restart, rollback, and injected failures.
It also uploads continuous runtime coverage, including counters retained across API restarts.
The script refuses hosts with existing VMs, services, pools, Auth connections, or ingress configuration.
It uses local OCI images and a private Auth fixture; it does not validate an external registry or public TLS.
