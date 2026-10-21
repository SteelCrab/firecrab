# MicroService

MicroService stores the desired configuration of an OCI service on a MicroVM.
Configurations survive API restarts in the existing SQLite database.
Deploy imports the OCI image by digest, creates MicroVM replicas, and waits for HTTP app health.
Each replica exposes one host TCP port.
The deployment record and owned VM identifiers survive API restarts.
Domain routing, CrabSecrets injection, rolling updates, rollback, and automatic instance replacement remain separate work.

## Fields

| Field | Rule or default |
| --- | --- |
| `name` | Required unique DNS label; 1–63 lowercase letters, digits, or hyphens; start and end with a letter or digit |
| `image` | Required OCI image reference; tag or SHA256 digest; no registry request when saving |
| `runtime` | `vm` only; default `vm` |
| `port` | Required app port; 1–65535 |
| `healthPath` | Absolute HTTP path without query or fragment; default `/health` |
| `domain` | Optional lowercase DNS hostname; no scheme, port, path, or wildcard; default `null` |
| `ram` | Required RAM per replica in MiB; power of two from 128 to 32768 |
| `cpu` | Required vCPU count per replica; 1–32 |
| `replicas` | 1–100; default 1 |
| `startupTimeout` | 1–3600 seconds; default 300 |
| `drainTimeout` | 0–3600 seconds; default 30 |
| `env` | Variable name to plain value; default `{}` |
| `secrets` | Variable name to non-nil CrabSecrets UUID; default `{}`; values are never stored here |

`env` and `secrets` together allow up to 64 entries.
Variable names follow `[A-Za-z_][A-Za-z0-9_]*` and allow up to 256 bytes.
Plain values allow up to 4096 bytes and must not contain NUL bytes.
A variable name cannot appear in both maps.
Secret references are saved without resolving them in this stage.

## REST

| Method | Path | Result |
| --- | --- | --- |
| `GET` | `/api/micro-services` | List configurations in name order |
| `POST` | `/api/micro-services` | Create a configuration; `201` |
| `GET` | `/api/micro-services/{id}` | Read a configuration |
| `PUT` | `/api/micro-services/{id}` | Replace the complete configuration; keep its UUID |
| `DELETE` | `/api/micro-services/{id}` | Delete a configuration; `204` |
| `POST` | `/api/micro-services/{id}/deployment` | Start an asynchronous deployment; `202` |
| `GET` | `/api/micro-services/{id}/deployment` | Read the deployment, live VM states, HTTP health, and logs; `null` before first deploy |
| `POST` | `/api/micro-services/{id}/stop` | Stop and remove owned replicas; keep the configuration; `202` |

POST and PUT use the same JSON body.
Responses add `id` to that body.
Do not include `id` in a request body.
PUT resets omitted optional fields to their defaults.
Use `domain: null`, `env: {}`, and `secrets: {}` to clear those fields.
An invalid request or duplicate name returns `400` without changing the saved configuration.
An unknown UUID returns `404`.
The existing origin rules, request deadline, JSON errors, and 64 KiB body limit apply.
Stop the service before editing, deleting, or deploying it again.
Those requests return `409` while a deployment is active or its instances still exist.

Example `web.json` using a public image:

```json
{
  "name": "web",
  "image": "nginx:stable-alpine",
  "runtime": "vm",
  "port": 80,
  "healthPath": "/",
  "domain": null,
  "ram": 256,
  "cpu": 1,
  "replicas": 2,
  "startupTimeout": 300,
  "drainTimeout": 30,
  "env": {},
  "secrets": {}
}
```

## CLI and dashboard

The commands work on Linux, macOS, and Windows.
They use the normal `--api`, `--host`, and `FIRECRAB_API` endpoint selection.

```sh
firecrab microservice create --file web.json
firecrab microservice list
firecrab microservice list --json
firecrab microservice get SERVICE_UUID
firecrab microservice deploy SERVICE_UUID --host-port 30100
firecrab microservice status SERVICE_UUID
firecrab microservice stop SERVICE_UUID
firecrab microservice update SERVICE_UUID --file web.json
firecrab microservice delete SERVICE_UUID
```

Create, get, and update print JSON.
Delete prints a JSON receipt.
In the dashboard, open **MicroService** to create, inspect, edit, or delete configurations.
The form supports environment variables and CrabSecrets references.
Saving a configuration does not deploy it.
Select **Run**, then **Deploy and run** to pull the image and start its replicas.
The panel reports startup progress, app health, host-port links, and failures.
**Stop service** removes the owned MicroVMs.

## Deploy behavior

The deploy request body is `{"hostPort":30100,"microNetworkId":null}`.
Two replicas use host ports 30100 and 30101, both forwarding to the configured app port.
Omit the network UUID to create or reuse the `services` MicroNetwork.
Supply `microNetworkId` or CLI `--network UUID` to use an existing network.
The automatic network selects a free `/24` from `172.30.0.0/16`.
Concurrent deployments share one host queue.
The worker checks memory, disk space, and port conflicts before starting replicas.

The image's existing ENTRYPOINT and CMD run inside each VM.
Plain `env` values are applied through the existing guest service mechanism.
Each health request must receive HTTP 2xx; redirects do not count as healthy.
`startupTimeout` bounds the app health wait after each VM finishes booting.
The existing VM boot pipeline has its own disk, process, and network timeouts.
A health failure removes all replicas created by that deployment.
Cleanup errors preserve the remaining VM identifiers so Stop can retry them.

Stop withdraws host port forwards, waits up to `drainTimeout`, then stops and removes the VMs.
The macOS TCP relay can close its SSH forwards when those port records disappear.
An interrupted deploy or stop is marked failed after API restart; Stop cleans up remaining instances.
A completed deployment uses the existing VM re-adoption mechanism.

Deploy currently requires `domain: null` and empty `secrets`.
Unsupported domain or secret settings return a field error before the job starts.
Use this flow for direct host ports; it does not yet switch a stable domain between releases.
Deploy and Stop wait by default in the CLI.
Use `--no-wait` to return after acceptance and poll `microservice status` later.
Deploy exits nonzero when deployment or health fails.

## Verification

```sh
cargo test -p firecrab-api micro_services
cargo test -p firecrab-api service_runtime
cargo test -p firecrab-cli micro_services
FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm --prefix firecrab-e2e run test:services
```

API tests cover persistence, full replacement, concurrent name conflicts, and rejected inputs.
CLI tests cover request paths, JSON bodies, secret references, and failures.
Browser tests cover CRUD, validation, Korean labels, and the mobile layout.
They also cover deploy progress, healthy endpoints, and Stop.

Run the real HTTP app test on a Linux host with KVM and the API/helper running:
Install the normal OCI tools and `dnsmasq-utils` (`dhcp_release`) so stopped VM leases can be reused.

```sh
python3 scripts/test-microservice-runtime.py --api http://127.0.0.1:5523
```

It imports a local OCI fixture, boots two real MicroVMs, checks HTTP and env injection, and tests failed-health cleanup.
Use `--restart-unit UNIT` only for an isolated QA API unit to test re-adoption after restart.
Use `--keep-running` to retain the healthy service for manual inspection.

## Related

- [API](../reference/api.md)
- [CLI](../reference/cli.md)
- [Dashboard](dashboard.md)
- [OCI images](oci.md)
