# Replicated application state and credentials

[Controller services](multihost.md#controller-services) distribute one immutable
image, application configuration and credential version set to every target.
`releaseGeneration` identifies this configuration. Each host has its own deployment
ID and replica count. Reconcile and retry reuse the accepted release.

## Supported application state

The top-level `state` object declares how the application handles request movement.
These are application requirements, not automatic application inspection.

| Field | Supported contract |
| --- | --- |
| `mode: "stateless_http"` | No durable sessions, uploads or database files in the guest |
| `mode: "external_state"` | Shared external sessions, uploads or database connectivity |
| `sessions`, `uploads`, `database` | Environment names containing the corresponding shared store URLs |
| `requestsDuplicateSafe: true` | Requests tolerate retries and execution on another replica |
| `backgroundWork: "disabled"` | The application disables background jobs |
| `backgroundWork: "duplicate_safe"` | The application safely tolerates concurrent/repeated jobs |

Omitting `state` declares stateless HTTP, duplicate-safe requests and disabled
background work. It does not detect local state or turn off the application's
timers. Configure the application to meet its declaration.

External-state mode requires at least one of `sessions`, `uploads` or `database`.
Each references a name in `service.env` or top-level `credentials`.
Supported URL schemes are `http`, `https`, `redis`, `rediss`, `postgres`,
`postgresql`, `mysql` and `s3`. Guest paths, `file:`/`sqlite:` URLs, localhost,
loopback addresses and unspecified addresses are rejected. Configure reachable
remote stores and permit their traffic on every selected host network.
The application must use those stores for every declared durable state category.
Endpoint validation cannot verify the remote store's durability or application use.

For a shared session store, add these top-level fields to the existing service
specification. Keep its `service` and `targets` objects:

```json
{
  "state": {
    "mode": "external_state",
    "sessions": "SESSION_URL",
    "requestsDuplicateSafe": true,
    "backgroundWork": "disabled"
  },
  "credentials": {
    "SESSION_TOKEN": {
      "secretId": "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
      "version": 3
    }
  }
}
```

Set `service.env` in that specification:

```json
{
  "SESSION_URL": "https://sessions.example.internal"
}
```

The application reads its session endpoint and token from these environment names.
Both hosts receive the same values, so a session remains available when a request
moves between them. A session URL containing a password can itself use a credential
reference instead of plaintext `service.env`.
Uploads can use an external object store. Database clients can use an external
database URL. Credentials for either use the same scoped injection contract.

## Pin controller credentials

Create or import values into [PipeSecrets](secrets.md) on the controller.
Use the returned Secret UUID and exact positive `version` in `credentials`.
Names map directly to guest environment variables. A name cannot also appear in
`service.env`. The combined environment uses the normal MicroService limits.
Host-local `service.secrets`, storage connections, queues, security groups and
cross-host domain routing remain unsupported for controller services.

The controller resolves all credential references before recording or dispatching
any deployment intent. Missing Secrets, uncaptured stale versions, invalid values,
unsupported state and unsupported references reject the entire request.
No target is partially deployed by these validation failures.
Transport, capacity and guest failures can still produce a degraded deployment;
the durable lifecycle retains ownership and retries those hosts.

The controller retains an encrypted snapshot of the selected UUID/version/name
set. It sends only that scoped environment to enrolled targets over mutual TLS.
Workers store immutable encrypted snapshots by deployment ID and reuse the
MicroService guest injection path. Worker Secret UUIDs do not need to match or
exist locally. Credentials, value fingerprints and tokens do not enter service
responses, operation history, VM records or the SQLite deployment journal.

Source edits and imports do not change accepted releases. Timeout retries and
controller/worker restarts use the captured values, including when the source
Secret was edited or deleted. A previously captured version remains usable.
To rotate credentials, submit an explicit controller service Update with the new
version and the current `expectedGeneration`. This advances `releaseGeneration`
for all targets. Host-local automatic Secret rotation skips controller services.
The controller retains captured versions for durable retry; protect and retain
its vault and encryption key with its database.

Stop does not require reading controller credential values. Once local resources
are cleaned, each worker removes snapshots for all deployment IDs owned by the
stopped service. This includes snapshots staged before a failed local admission.
Missing or conflicting worker snapshots fail closed instead of resolving a
different host-local Secret.

The dashboard's **Application state and configuration** editor accepts the state,
environment and versioned credential maps as JSON. Service cards and operation
history show the accepted metadata. The CLI uses the same specification:

```sh
firecrab --host controller cluster service create --file service.json --idempotency-key web-create
firecrab --host controller cluster service update SERVICE_UUID --file service.json --expected-generation 1 --idempotency-key web-release-2
```

Upgrade the controller and workers together before using this contract. The
private worker RPC carries a public intent plus private scoped credential values;
worker receipts contain only the intent and deployment metadata.

## Concurrent work and recovery boundaries

Implement application idempotency for retried writes, such as a unique request key
checked in the shared database. Implement duplicate-safe background jobs with
equivalent application guarantees. Firecrab does not deduplicate HTTP requests or
jobs. `requestsDuplicateSafe: false` and `backgroundWork: "exclusive"` are rejected
before dispatch. Exclusive work requires the ownership and fencing protocol in
[#462](https://github.com/SteelCrab/firecrab/issues/462).

Service replicas do not replicate databases, back up application data, recover
host-local disks or migrate VMs. A local session cache, upload directory, SQLite
file or durable queue cannot become available on another host through service
failover. Manage external data replication, availability and backups separately.
Cross-host traffic routing and automatic failover remain separate milestones.

Automated acceptance uses real HTTPS/mTLS and encrypted vault restart recovery.
Fixture applications write a session through one replica and read it through
another using the injected endpoint/token. VM records are fixtures; these checks
do not boot guests on two physical hosts. Physical-host acceptance remains in
[#465](https://github.com/SteelCrab/firecrab/issues/465).
