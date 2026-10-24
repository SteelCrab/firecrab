# MicroService

MicroService stores the desired configuration of an OCI service on a MicroVM.
Configurations survive API restarts in the existing SQLite database.
Deploy imports the OCI image by digest, creates MicroVM replicas, and waits for HTTP app health.
Services with a domain use PipeIngress to reach guest app ports directly.
Services without a domain expose one host TCP port per replica.
The deployment record and owned VM identifiers survive API restarts.
PipeSecrets values are injected into every replica without appearing in VM or deployment responses.
Rolling updates, rollback, and automatic instance replacement remain separate work.

## Fields

| Field | Rule or default |
| --- | --- |
| `name` | Required unique DNS label; 1–63 lowercase letters, digits, or hyphens; start and end with a letter or digit |
| `image` | Required OCI image in a Pipe Auth-connected account; tag or SHA256 digest; ownership checked locally when saving |
| `runtime` | `vm` only; default `vm` |
| `port` | Required app port; 1–65535 |
| `healthPath` | Absolute HTTP path without query or fragment; default `/health` |
| `domain` | Optional lowercase DNS hostname; no scheme, port, path, or wildcard; default `null` |
| `ram` | Required RAM per replica in MiB; power of two from 128 to 32768 |
| `cpu` | Required vCPU count per replica; 1–32 |
| `replicas` | 1–100; default 1 |
| `securityGroupId` | Optional security group UUID; default `null`; every new replica inherits it |
| `startupTimeout` | 1–3600 seconds; default 300 |
| `drainTimeout` | 0–3600 seconds; default 30 |
| `env` | Variable name to plain value; default `{}` |
| `secrets` | Variable name to non-nil PipeSecrets UUID; default `{}`; values are never stored here |
| `storageConnectionId` | Optional [S3 connection](pipe-storage.md) UUID for the app AWS SDK; default `null` |

`env` and `secrets` together allow up to 64 entries.
Variable names follow `[A-Za-z_][A-Za-z0-9_]*` and allow up to 256 bytes.
Plain values allow up to 4096 bytes and must not contain NUL bytes.
A variable name cannot appear in both maps.
Secret references are saved without resolving them. Deployment validates them before accepting work.

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

Create, update, and deploy require a matching **Pipe Auth** connection:

- GitHub: `ghcr.io/ACCOUNT/REPOSITORY`, where `ACCOUNT` matches the connected
  GitHub login. Another user's or organization's namespace is rejected.
- AWS: the exact private ECR registry for the connected account and region.
  Other account IDs, regions, public ECR, and lookalike hosts are rejected.

A standalone registry login does not satisfy this policy. Invalid ownership
returns `400` with an `image` field error before saving or queuing a deployment.
Saving checks account scope without contacting the registry; the worker resolves
the requested tag or digest with that account's credentials when deploying.
Reading, stopping, and deleting existing services do not require the connection.

Example `web.json` using an image published to the connected GitHub account
(replace `your-account` with its lowercase login):

```json
{
  "name": "web",
  "image": "ghcr.io/your-account/web:1.0",
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
The form supports environment variables and [PipeSecrets](secrets.md) references.
Select **New MicroService** to follow four setup steps:

1. **App** — enter a name, choose an account connected in **Pipe Auth**, then
   select one of its app repositories and image versions. Connect GitHub or AWS
   in Auth first if no accounts are available. Manual image addresses and public
   examples are not offered in this picker.
2. **Connection** — set the app's internal port and a path that returns HTTP 2xx.
3. **Resources** — choose memory, CPU, and replica count. Advanced settings
   contain timeouts, environment variables, PipeSecrets references, and S3 connections.
4. **Review** — check the summary, return to any section to change it, then save.

Going back keeps the values entered in the form.
Switching accounts clears the selected app. The wizard checks the connected
account again before leaving App and before saving. If the account is disconnected
or replaced during setup, select an app from a connected account again.
The API enforces the same ownership rule for dashboard, CLI, and REST requests.
Saving opens deployment management but does not start any MicroVMs.
Keep the automatic services network or choose an existing one. Set the first
host port for a service without a domain, then select **Deploy and run**.
For an existing configuration, select **Run** to open the same panel.
The panel shows image preparation, app startup, health checks, and an **Open app**
link once the app responds successfully. Failed deployments show a log and next
steps. Services with a domain connect through PipeIngress. PipeSecrets values
are injected into every replica. Secret edits apply to the next deployment.
The panel checks Pipe Auth when opened, every three seconds, when returning to
this browser tab, and immediately before deployment. Deploy stays disabled during
the initial and pre-deployment checks, when the image's account is disconnected
or replaced, or when the Auth API cannot be reached. **Reconnect AUTH** opens
Pipe Auth to reconnect the account. It keeps the selected service, network, and
host port in this browser's history entry, including when the Auth page reloads.
A successful connection for the image's account returns to the same deployment
panel automatically. Select **Deploy and run** to continue; reconnection does not
start deployment. A different account or AWS region keeps you in Auth with
instructions. Browser Back also restores the choices if you cancel reconnection.
The panel loads the current service configuration on return and reports if the
service was deleted. Before enabling deployment, it verifies the selected network
and every consecutive host TCP port needed by the replicas. Deleted networks
remain visible with a field error; choose another network or **Use automatic
services network**. Occupied ports show a field error and the conflicting port.
Edit the first host port to choose a new range. Changes trigger a fresh check,
and late replies for older choices are ignored. Failed checks keep deployment
disabled and offer **Retry deployment check**. The choices remain editable.
**Check again** verifies the new connection without leaving the panel. An Auth
request failure offers **Retry AUTH check**. Reconnecting the matching account
allows deployment again. For AWS, the account and region must both match.
**Stop service** removes the owned MicroVMs, even when Auth is disconnected.

## Connect a domain

1. Set **Domain** when creating or editing a MicroService. A hostname such as
   `orders.example.com` is enough; the dashboard also accepts a pasted HTTPS URL
   without a path or query.
2. Select **Run**, then **Deploy and run**. No host port or network selection is
   required. Network selection remains available under **Advanced settings**.
3. Firecrab checks that ingress is installed and the hostname is free. After every
   replica passes its HTTP health check, it publishes one load-balanced root route.
   The dashboard then offers **Open domain**.

For public HTTPS, use the existing **Pipe → Named Tunnel** setup with this domain.
If a named tunnel is already configured, point the domain's proxied Cloudflare
CNAME to `<tunnel-id>.cfargotunnel.com`; the deployment panel shows that target.
DNS and public TLS remain external. A prepared local route does not prove that
public DNS, TLS, or the connector is working. This deployment does not modify DNS.

The domain owns all paths for that hostname. Existing manual routes or another
service on the hostname cause a field error before image import. Service routes
appear in Pipe as **Managed by MicroService**. Manual Pipe saves preserve them,
including saves from a stale dashboard draft.

Domain deployment needs no public replica ports:

```sh
firecrab microservice deploy SERVICE_UUID
```

REST accepts `{}` for a domain service, or `{"microNetworkId":"NETWORK_UUID"}`
to select an existing network. Domain deployments ignore `hostPort` and report
`hostPort: 0` for instances. Health checks send the service hostname in `Host`.

Stop withdraws only that deployment's route before draining and deleting its VMs.
The existing named connector and its DNS remain configured. A failed route
withdrawal retains instance ownership and allows **Stop** to retry. If a Quick
Tunnel references this hostname, remove that connector before stopping the service.
Interrupted publication is recorded durably, so Stop can clean it up after an API
restart. A completed domain route keeps serving independently of API restarts.

Changing the configured domain still requires stopping the service first. This
step does not add rolling updates or release rollback.

## Deploy behavior

Without a domain, the deploy request body is `{"hostPort":30100,"microNetworkId":null}`.
Two replicas use host ports 30100 and 30101, both forwarding to the configured app port.
Omit the network UUID to create or reuse the `services` MicroNetwork.
Supply `microNetworkId` or CLI `--network UUID` to use an existing network.
The automatic network selects a free `/24` from `172.30.0.0/16`.
`POST /api/micro-services/{id}/deployment/check` accepts the same request body.
It returns HTTP 200 with JSON `null` when placement is valid, or HTTP 400 with
`fields.microNetworkId` and/or `fields.hostPort` errors. It reads the current
service configuration and checks saved MicroVM TCP forwards and host listeners.
It does not save a deployment, create a network, or start MicroVMs. The check
briefly tests each TCP port; it does not reserve ports. The UI checks again
immediately before submitting. The deploy API repeats validation before queuing,
and the worker checks ports again before preparing the image.

When `securityGroupId` is set, select its IPv4-only network explicitly or reuse an existing matching `services` network.
The deployment captures the group selection and assigns it to every replica before startup.
See [security groups](security-groups.md) for rules and inactive-only changes.
Concurrent deployments share one host queue.
The worker rechecks Pipe Auth after leaving the queue. A disconnected or replaced
account fails the deployment before image resolution or VM creation. It never
falls back to anonymous pulls or an independently saved registry login.
Disconnecting Auth does not stop an already running service.
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

Domain services connect through PipeIngress after health
checks. Services without a domain keep using direct host ports. Neither flow
switches between running releases yet.
Deploy and Stop wait by default in the CLI.
Use `--no-wait` to return after acceptance and poll `microservice status` later.
Deploy exits nonzero when deployment or health fails.

## Inject Secrets

Create values in **Secrets**. Reference their UUIDs in **Resources → Advanced settings**.
Every replica receives one encrypted deployment snapshot. Secret edits apply at the next deployment.
See [Secret injection](secrets.md#microservice-injection) for limits, storage and restart behavior.

## Verification

```sh
cargo test -p firecrab-api micro_services
cargo test -p firecrab-api service_runtime
cargo test -p firecrab-cli micro_services
FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm --prefix firecrab-e2e run test:services
```

API, CLI and browser tests cover configuration, AUTH ownership and revocation,
validation, deployment health, Stop, Korean labels and mobile layout.

Run the real HTTP app test on a Linux host with KVM and the API/helper running:
Install the normal OCI tools and `dnsmasq-utils` (`dhcp_release`) so stopped VM leases can be reused.

```sh
python3 scripts/test-microservice-runtime.py --api http://127.0.0.1:5523 \
  --image ghcr.io/your-account/firecrab-service-test:qa
```

First publish a fixture image to the connected account. It must serve HTTP on
port 8080, return the `GREETING` environment value at `/health`, and return a
non-2xx response at `/missing`. Loopback registry images are rejected by the
service API. The script boots two MicroVMs, checks HTTP and env injection, and
tests failed-health cleanup using the supplied image.
Add `--with-secrets` to use a disposable PipeSecret and verify response redaction,
shared replica values, and a VM stop/start after editing the stored Secret.
Use `--restart-unit UNIT` only for an isolated QA API unit to test re-adoption after restart.
Use `--keep-running` to retain the healthy service for manual inspection.

For domain routing and public HTTPS, use an empty, dedicated Linux/KVM API with
the bundled ingress and cloudflared engines installed:

```sh
sudo python3 scripts/test-microservice-domain-live.py --api http://127.0.0.1:15335 \
  --image ghcr.io/your-account/service-domain-test:qa
```

The AUTH-owned image must serve `GREETING` at `/health` and its hostname at `/cgi-bin/identity` on port 8080.
The test checks two replicas, public TLS, restart continuity and failure, then removes its resources.
Use `--output PATH` for JSON. Stop the dedicated QA API/helper and ingress units after the run.
