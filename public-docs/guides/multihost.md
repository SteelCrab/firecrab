# Host registry and VM placement

One controller can register several workload hosts and place individual VMs on
them. Each placement records an immutable `{hostId, resourceId}` owner, an image
fingerprint, an operation generation, and capacity reservations in SQLite before
making a remote request.

This is the VM placement foundation in [#455](https://github.com/SteelCrab/firecrab/issues/455).
Shared MicroService identity follows in [#456](https://github.com/SteelCrab/firecrab/issues/456).
Networks, storage, VM processes, and pools remain host-local. Placement does not
provide service load balancing, application-state replication, or VM migration.

## Configure the controller and workers

Use [private-CA mutual TLS](mtls.md). Give the controller and every worker a
distinct UUID and certificate. Certificates need DNS/IP SANs for their reachable
HTTPS endpoints. The CA remains offline. All enrolled identities belong to the
existing trusted private-CA administrative boundary; enrollment does not add
tenant role-based authorization.

Configure the controller's API service environment:

```sh
export FIRECRAB_BIND_ADDR=0.0.0.0:5523
export FIRECRAB_TLS_CERT=/etc/firecrab/pki/controller.crt
export FIRECRAB_TLS_KEY=/etc/firecrab/pki/controller.key
export FIRECRAB_TLS_CA=/etc/firecrab/pki/ca.crt
export FIRECRAB_ALLOWED_ORIGINS=https://controller.example:5523
```

Configure each worker's API service environment, then restart that API:

```sh
export FIRECRAB_BIND_ADDR=0.0.0.0:5523
export FIRECRAB_TLS_CERT=/etc/firecrab/pki/host.crt
export FIRECRAB_TLS_KEY=/etc/firecrab/pki/host.key
export FIRECRAB_TLS_CA=/etc/firecrab/pki/ca.crt
export FIRECRAB_CONTROLLER_ID=CONTROLLER_CERTIFICATE_UUID
export FIRECRAB_CONTROLLER_URL=https://controller.example:5523
export FIRECRAB_HOST_ENDPOINT=https://worker-a.example:5523
export FIRECRAB_HOST_NAME=worker-a
export FIRECRAB_HOST_LABELS='{"zone":"zone-a"}'
export FIRECRAB_HOST_DISK_LIMITS='{"default":100}'
```

Replace the UUID and endpoints. Use a different host certificate, endpoint, and
name on the second worker. Labels and disk limits are optional JSON maps. All
three controller/endpoint settings must be present together. Invalid enrollment
or TLS configuration is rejected before loading or recovering host state.

Workers register and send inventory every 10 seconds. The controller derives
online state from its own receipt time; reports become stale after 30 seconds.
Report IDs must match the TLS peer. Worker RPCs accept only the configured
controller UUID. Both directions verify the certificate chain, endpoint hostname,
and expected host UUID. HTTP origins, redirects, URL credentials, and endpoint
paths are not allowed for cluster transport.

Install matching [images](images.md), create [local networks](networking.md),
and configure [storage roots](storage.md) on the candidates before placing VMs.
Every network/storage reference includes its host UUID. An ID from another host
cannot be substituted, even when display names match.

## Capacity and selection

CPU capacity follows the worker's available CPU count. Memory leaves 512 MiB for
the host. Existing unmanaged VMs and all nonterminal placement reservations count
against capacity, including stopped VMs.

Disk limits are allocatable GiB per storage-root ID, including existing VM
reservations. Leave space for the host and image cache. Without an explicit
limit, the worker records a conservative quota from free space at first
enrollment. Existing unmanaged VM sizes are also deducted. The quota stays
stable across VM disk writes and restarts. Explicit limits are useful for
preexisting workloads and expanded disks. A configured limit updates the durable
quota on the next worker report. Lowering it does not delete existing VMs.
Successful deletion releases reservations; stopping a VM does not.
For storage roots on the same filesystem, split its usable capacity between
their quotas. Each root has its own budget.

Candidates must have a fresh heartbeat, accept placement, match the exact image
fingerprint and required labels, own the selected resources, and have sufficient
CPU, memory, and disk capacity. Selection uses the lowest reserved-memory
fraction, then CPU fraction, then host UUID. One SQLite `IMMEDIATE` transaction
chooses the owner and reserves its capacity, including across independent
database connections.

## CLI and dashboard

Existing `firecrab host` commands still select saved API profiles. Use the
controller profile with the separate `cluster` namespace:

```sh
firecrab --host controller cluster host list --json > hosts.json
firecrab --host controller cluster host show HOST_UUID --json
firecrab --host controller cluster host drain HOST_UUID
firecrab --host controller cluster host resume HOST_UUID
```

Build a specification from inventory. This example uses each candidate's first
network/storage and pins the first host's image:

```sh
jq '{name:"demo", image:.[0].report.images[0], cpu:1, ram:512, diskGb:2,
  targets:map({network:{hostId:.report.id,resourceId:.report.networks[0].id},
               storage:{hostId:.report.id,resourceId:.report.storages[0].id}})}' \
  hosts.json > placement.json

firecrab --host controller cluster vm place --file placement.json \
  --idempotency-key demo-001 --json
firecrab --host controller cluster vm list --json
firecrab --host controller cluster vm show PLACEMENT_UUID --json
firecrab --host controller cluster vm start PLACEMENT_UUID
firecrab --host controller cluster vm stop PLACEMENT_UUID
firecrab --host controller cluster vm delete PLACEMENT_UUID
firecrab --host controller cluster vm reconcile PLACEMENT_UUID
```

Keys are unique across the controller. Reuse the **same key and specification**
after an unknown result. A different specification with the same key returns
`409`; an identical retry returns the existing placement. Use a new key only for
an intentionally new VM. `requiredLabels` restricts candidates; `env` and
`egressPolicy` follow normal VM semantics.

Open **Compute → Cluster** in the controller dashboard to inspect hosts,
drain/resume them, select per-host networks/storage, and place or manage VMs.
An unacknowledged request and retry key survive reload in session storage.
Heartbeat refreshes preserve the form's values, cursor, and focus.

Drain prevents new placement without stopping or migrating existing VMs. Later
operations still use their recorded host. Direct worker VM mutations, stack
adoption, and host-local schedules cannot override controller ownership.

## REST

| Endpoint | Purpose |
| --- | --- |
| `GET /api/cluster/hosts` | Hosts, inventory, heartbeat state, reservations |
| `GET /api/cluster/hosts/{id}` | One host |
| `PATCH /api/cluster/hosts/{id}` | Set `{"draining":true}` or `false` |
| `POST /api/cluster/hosts/register` | Authenticated worker registration/report |
| `POST /api/cluster/hosts/{id}/heartbeat` | Authenticated worker report |
| `GET /api/cluster/placements` | Durable placement journal |
| `POST /api/cluster/placements` | Admit a `PlacementSpec`; requires `Idempotency-Key` |
| `GET /api/cluster/placements/{id}` | Recorded owner and latest observed VM |
| `POST /api/cluster/placements/{id}/start` | Record and dispatch start intent |
| `POST /api/cluster/placements/{id}/stop` | Record and dispatch stop intent |
| `DELETE /api/cluster/placements/{id}` | Record and dispatch delete intent |
| `POST /api/cluster/placements/{id}/delete` | CLI/dashboard deletion alias |
| `POST /api/cluster/placements/{id}/reconcile` | Query/retry the recorded owner |

The private `GET, PUT /api/cluster/worker/placements/{id}` protocol authenticates
the configured controller and persists a receipt before local VM work. Its VM
UUID equals the placement UUID. Successful deletion leaves a tombstone; stale
creates and conflicting generations cannot resurrect it. Remote receipt bodies
are bounded to 1 MiB; detached worker execution is bounded to 16 operations.

## Unknown results and recovery

`pending` means intent is recorded. `uncertain` retains the same owner and
reservation because the result is unknown. `confirmed` acknowledges ownership
or an operation; inspect `observed.state` for actual VM startup status.
`rejected` releases a create reservation only after durable worker rejection
without a VM. `deleted` releases capacity after cleanup and a durable tombstone.

The controller checks owners every 3 seconds, with eight concurrent requests.
Claims expire after 30 seconds; acknowledged placements refresh at 10-second
intervals. Endpoint failures, timeouts, and missing heartbeats do not move a VM,
release an unresolved reservation, or choose another host.

After controller restart, the SQLite journal resumes reconciliation. After a
worker restart during create, the same receipt and VM UUID resume SSH key and
network-lease setup. Repeated deletion retries incomplete file and lease cleanup,
even if the VM row was removed before the worker restarted. Confirmed VMs
continue independently while the controller is unavailable.

Restore the recorded host's connectivity and certificates, then run
`cluster vm reconcile`. For removal, request deletion and wait for `deleted`.
Preserve both databases while recovering unresolved placements. This milestone
uses one controller; controller HA and service failover remain separate work in
the #292 dependency chain.
